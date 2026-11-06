//! Test host for the client page: starts the service and prints each
//! service event and the driver view of each `Ready` client as one JSON
//! line on stdout. The page tests in `tests/browser_page` use it.
//!
//! Usage: `page_host [--code <fixed code>] [--port <port>]`. It stops when
//! stdin closes.

use std::io::{BufRead, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use nitpick_browser::{BrowserDriver, BrowserService, ClientState, ServiceConfig, ServiceEvent};
use nitpick_core::driver::{DisplayId, OutputDriver, Support};
use serde_json::{Value, json};

/// Event poll period. The service events have no blocking receive
/// without an executor.
const POLL: Duration = Duration::from_millis(5);

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
    let (service, mut events) =
        BrowserService::start(&config).unwrap_or_else(|e| fail(&e.to_string()));
    let mut driver = BrowserDriver::new(&service);

    // The reader thread ends, and drops the sender, when stdin closes.
    let (stdin_tx, stdin_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let _tx = stdin_tx;
        for _ in std::io::stdin().lock().lines().map_while(Result::ok) {}
    });

    loop {
        let Some(event) = events.try_next() else {
            match stdin_rx.recv_timeout(POLL) {
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                _ => break,
            }
        };
        print(&event_json(&event));
        if let ServiceEvent::Client(ClientState::Ready(id)) = &event {
            print(&client_json(&mut driver, id));
        }
        if matches!(event, ServiceEvent::Stopped { .. }) {
            return;
        }
    }
    service.shutdown();
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
