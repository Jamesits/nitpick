//! Test host for the client page: starts the service and prints each
//! service event and the driver view of each `Ready` client as one JSON
//! line on stdout. The page tests in `tests/browser_page` use it.
//!
//! Usage: `page_host [--code <fixed code>] [--port <port>]`. It stops when
//! stdin closes.
//!
//! Commands, one JSON object per stdin line:
//! - `{"cmd": "show", "seq": <n>, "depth": 10 | 12, "pattern": <pattern>}`
//!   opens the last `Ready` client and presents a PQ narrow-range frame at
//!   the surface size. `pattern` is `{"fill": [r, g, b]}` or
//!   `{"columns": [[r, g, b], [r, g, b]]}` (alternating 1-pixel columns).
//!   A pending frame is cancelled first. The result is a `presented` or a
//!   `present_failed` event with the same `seq`.
//! - `{"cmd": "close"}` cancels a pending frame and closes the surface.
//!
//! A cancelled frame gives a `cancelled` event.

use std::cell::RefCell;
use std::future::Future;
use std::io::{BufRead, Write};
use std::pin::Pin;
use std::thread;

use nitpick_browser::{BrowserDriver, BrowserService, ClientState, ServiceConfig, ServiceEvent};
use nitpick_core::driver::{DisplayId, Frame, OutputDriver, OutputError, Support, Surface};
use nitpick_core::image::Image;
use nitpick_core::signal::{BitDepth, Encoding, Format, Rgb};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    Show {
        seq: u64,
        depth: u32,
        pattern: Pattern,
    },
    Close,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Pattern {
    Fill([u16; 3]),
    Columns([[u16; 3]; 2]),
}

type Presenting<'d> = Pin<Box<dyn Future<Output = Result<String, OutputError>> + 'd>>;

fn main() {
    let mut config = ServiceConfig::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| fail(&format!("{arg} needs a value")));
        match arg.as_str() {
            "--code" => config.fixed_code = Some(value),
            "--port" => config.port = value.parse().unwrap_or_else(|_| fail("invalid port")),
            _ => fail(&format!("unknown argument {arg}")),
        }
    }
    let (service, events) = BrowserService::start(&config).unwrap_or_else(|e| fail(&e.to_string()));

    // The reader thread ends, and drops the sender, when stdin closes.
    let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<String>();
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if stdin_tx.send(line).is_err() {
                return;
            }
        }
    });

    // The host executor; the service has its own runtime thread.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap_or_else(|e| fail(&e.to_string()));
    if runtime.block_on(run(&service, events, stdin_rx)) {
        service.shutdown();
    }
}

/// `false` if the service stopped by itself.
async fn run(
    service: &BrowserService,
    mut events: nitpick_browser::ServiceEvents,
    mut commands: mpsc::UnboundedReceiver<String>,
) -> bool {
    // Queries use their own driver, because the pending future holds the
    // borrow of the presenting one. Both read the same session state. The
    // host drops the pending future before each other use of `driver`.
    let mut viewer = BrowserDriver::new(service);
    let driver = RefCell::new(BrowserDriver::new(service));
    let mut ready: Option<DisplayId> = None;
    let mut opened: Option<DisplayId> = None;
    let mut pending: Option<(u64, Presenting<'_>)> = None;
    loop {
        tokio::select! {
            event = events.next() => {
                let Some(event) = event else { return false };
                print(&event_json(&event));
                match &event {
                    ServiceEvent::Client(ClientState::Ready(id)) => {
                        ready = Some(id.clone());
                        print(&client_json(&mut viewer, id));
                    }
                    ServiceEvent::Client(ClientState::Disconnected(_)) => ready = None,
                    ServiceEvent::Stopped { .. } => return false,
                    _ => {}
                }
            }
            line = commands.recv() => {
                let Some(line) = line else { return true };
                let command = match serde_json::from_str::<Command>(&line) {
                    Ok(command) => command,
                    Err(e) => {
                        print(&json!({"event": "bad_command", "error": e.to_string()}));
                        continue;
                    }
                };
                if let Some((seq, _)) = pending.take() {
                    print(&json!({"event": "cancelled", "seq": seq}));
                }
                match command {
                    Command::Show { seq, depth, pattern } => {
                        match start(&driver, ready.as_ref(), depth, pattern) {
                            Ok((display, task)) => {
                                opened = Some(display);
                                pending = Some((seq, task));
                            }
                            Err(e) => print(&failed(seq, &e)),
                        }
                    }
                    Command::Close => {
                        if let Some(display) = opened.take() {
                            driver.borrow_mut().close(&display);
                        }
                        print(&json!({"event": "closed"}));
                    }
                }
            }
            result = async { pending.as_mut().expect("guarded").1.as_mut().await },
                if pending.is_some() =>
            {
                let (seq, _) = pending.take().expect("guarded");
                match result {
                    Ok(status) => print(&json!({"event": "presented", "seq": seq, "status": status})),
                    Err(e) => print(&failed(seq, &e)),
                }
            }
        }
    }
}

/// Opens the client and starts `present` with an owned surface and frame.
fn start<'d>(
    driver: &'d RefCell<BrowserDriver>,
    ready: Option<&DisplayId>,
    depth: u32,
    pattern: Pattern,
) -> Result<(DisplayId, Presenting<'d>), OutputError> {
    let depth = match depth {
        10 => BitDepth::Ten,
        12 => BitDepth::Twelve,
        _ => return Err(OutputError::Platform(format!("invalid depth {depth}"))),
    };
    let display = ready.ok_or(OutputError::Disconnected)?.clone();
    let surface = driver.borrow_mut().open(&display, depth)?;
    let image = render(surface.size, pattern);
    Ok((display, Box::pin(present(driver, surface, image, depth))))
}

#[allow(clippy::await_holding_refcell_ref)] // See `run`: no other borrow while pending.
async fn present(
    driver: &RefCell<BrowserDriver>,
    surface: Surface,
    image: Image,
    depth: BitDepth,
) -> Result<String, OutputError> {
    let frame = Frame {
        image: &image,
        format: Format::new(Encoding::PqNarrow, depth),
        metadata: None,
    };
    driver.borrow_mut().present(&surface, &frame).await
}

fn render((width, height): (usize, usize), pattern: Pattern) -> Image {
    let rgb = |[r, g, b]: [u16; 3]| Rgb { r, g, b };
    match pattern {
        Pattern::Fill(c) => Image::new(width, height, rgb(c)),
        Pattern::Columns([even, odd]) => {
            let mut image = Image::new(width, height, rgb(even));
            for x in (1..width).step_by(2) {
                image.fill_rect(x, 0, x + 1, height, rgb(odd));
            }
            image
        }
    }
}

fn failed(seq: u64, error: &OutputError) -> Value {
    json!({"event": "present_failed", "seq": seq, "error": error.to_string()})
}

fn fail(message: &str) -> ! {
    eprintln!("page_host: {message}");
    std::process::exit(2);
}

fn print(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn event_json(event: &ServiceEvent) -> Value {
    match event {
        ServiceEvent::Listening { url } => json!({"event": "listening", "url": url}),
        ServiceEvent::PairingCode(code) => json!({"event": "pairing", "code": code}),
        ServiceEvent::Client(state) => match state {
            ClientState::Connected => json!({"event": "connected"}),
            ClientState::Authenticated => json!({"event": "authenticated"}),
            ClientState::Ready(id) => json!({"event": "ready", "display": id.to_string()}),
            ClientState::ViewportChanged(id) => {
                json!({"event": "viewport_changed", "display": id.to_string()})
            }
            ClientState::Disconnected(reason) => {
                json!({"event": "disconnected", "reason": reason})
            }
        },
        ServiceEvent::Stopped { error } => json!({"event": "stopped", "error": error}),
    }
}

/// Display list entry and query result of `id`.
fn client_json(driver: &mut BrowserDriver, id: &DisplayId) -> Value {
    let info = driver.displays().into_iter().find(|d| d.id == *id);
    let query = driver.query(id);
    match (info, query) {
        (Some(info), Ok(query)) => json!({
            "event": "client",
            "display": id.to_string(),
            "name": info.name,
            "size": [info.size.0, info.size.1],
            "pixelRatio": query.mode.pixel_ratio,
            "hdrOn": query.mode.hdr_on,
            "description": query.mode.description,
            "pixelExact": support(&query.capabilities.pixel_exact),
        }),
        (_, Err(e)) => {
            json!({"event": "client", "display": id.to_string(), "error": e.to_string()})
        }
        (None, Ok(_)) => {
            json!({"event": "client", "display": id.to_string(), "error": "not listed"})
        }
    }
}

fn support(s: &Support) -> Value {
    match s {
        Support::Yes => json!("yes"),
        Support::No(reason) => json!({"no": reason}),
        Support::Unknown(reason) => json!({"unknown": reason}),
    }
}
