//! Session host of the command line: service events and console commands
//! on one local executor, and the borrowed `present` future. See "Host and
//! dependency boundary" and "Asynchronous presentation" in
//! `doc/design/output_driver/overview.md`.
//!
//! Generic over the driver and the event source, so the unit tests use a
//! mock driver and a channel in place of the browser service.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;

use nitpick_browser::{ClientState, ServiceEvent, ServiceEvents};
use nitpick_core::driver::{DisplayId, Frame, OutputDriver, OutputError, OutputLimits, Surface};
use nitpick_core::image::Image;
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::Format;
use tokio::sync::mpsc;

use crate::command::{self, Command};

/// Service events in order. `None`: the source ended.
pub trait EventSource {
    fn next_event(&mut self) -> impl Future<Output = Option<ServiceEvent>>;
}

impl EventSource for ServiceEvents {
    fn next_event(&mut self) -> impl Future<Output = Option<ServiceEvent>> {
        self.next()
    }
}

impl EventSource for mpsc::UnboundedReceiver<ServiceEvent> {
    fn next_event(&mut self) -> impl Future<Output = Option<ServiceEvent>> {
        self.recv()
    }
}

/// Receives the console output, one line or block per call.
pub trait Output {
    fn line(&mut self, text: &str);
}

/// Standard output, flushed per line for a reading process.
pub struct Stdout;

impl Output for Stdout {
    fn line(&mut self, text: &str) {
        use std::io::Write as _;
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
}

impl Output for mpsc::UnboundedSender<String> {
    fn line(&mut self, text: &str) {
        let _ = self.send(text.into());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub pattern: PatternKind,
    pub format: Format,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// `quit` or the end of the console input.
    Quit,
    /// The service stopped by itself; `Some` with its error.
    Stopped(Option<String>),
}

type Presenting<'d> = Pin<Box<dyn Future<Output = Result<String, OutputError>> + 'd>>;

/// Runs until `quit`, the end of `console` or the end of the service.
/// Then it drops a pending frame and closes the surface; the caller stops
/// the service.
///
/// `driver` opens, presents and closes. A pending future holds its mutable
/// borrow, so `viewer`, a second driver on the same service state, answers
/// `displays` and `query` meanwhile.
pub async fn run<D: OutputDriver>(
    driver: &RefCell<D>,
    viewer: D,
    events: &mut impl EventSource,
    console: &mut mpsc::UnboundedReceiver<String>,
    settings: Settings,
    out: impl Output,
) -> Exit {
    let mut host = Host {
        driver,
        viewer,
        out,
        settings,
        selected: None,
        opened: None,
        pending: None,
        reshow: false,
    };
    let exit = loop {
        tokio::select! {
            event = events.next_event() => {
                let event = event.unwrap_or(ServiceEvent::Stopped {
                    error: Some("the event source ended".into()),
                });
                if let Some(exit) = host.on_event(event) {
                    break exit;
                }
            }
            line = console.recv() => {
                let Some(line) = line else { break Exit::Quit };
                if let Some(exit) = host.on_line(&line) {
                    break exit;
                }
            }
            result = async { host.pending.as_mut().expect("guarded").as_mut().await },
                if host.pending.is_some() =>
            {
                host.pending = None;
                match result {
                    Ok(status) => host.say(&format!("status: {status}")),
                    Err(e) => host.say(&format!("error: presentation failed: {e}")),
                }
            }
        }
    };
    host.close_surface();
    exit
}

struct Host<'d, D, O> {
    driver: &'d RefCell<D>,
    viewer: D,
    out: O,
    settings: Settings,
    selected: Option<DisplayId>,
    /// The surface stays open after `present`, so that the client keeps
    /// the current frame until replacement, `close` or exit.
    opened: Option<DisplayId>,
    /// Invariant: no other borrow of `driver` while `Some`.
    pending: Option<Presenting<'d>>,
    /// The open surface became stale: show again at the next `Ready` of
    /// the selected client.
    reshow: bool,
}

impl<D: OutputDriver, O: Output> Host<'_, D, O> {
    fn say(&mut self, text: &str) {
        self.out.line(text);
    }

    fn cancel(&mut self) {
        if self.pending.take().is_some() {
            self.say("status: presentation cancelled");
        }
    }

    /// Drops the pending future before the `close` borrow.
    fn close_surface(&mut self) {
        self.cancel();
        if let Some(display) = self.opened.take() {
            self.driver.borrow_mut().close(&display);
        }
    }

    fn on_event(&mut self, event: ServiceEvent) -> Option<Exit> {
        match event {
            ServiceEvent::Listening { url } => self.say(&format!("url: {url}")),
            ServiceEvent::PairingCode(code) => self.say(&format!("pairing code: {code}")),
            ServiceEvent::Client(ClientState::Connected) => self.say("client: connected"),
            ServiceEvent::Client(ClientState::Authenticated) => self.say("client: authenticated"),
            ServiceEvent::Client(ClientState::Ready(display)) => self.on_ready(&display),
            ServiceEvent::Client(ClientState::ViewportChanged(display)) => {
                self.say(&format!("client: viewport changed: {display}"));
                if self.opened.as_ref() == Some(&display) {
                    self.cancel();
                    self.reshow = true;
                }
            }
            ServiceEvent::Client(ClientState::Disconnected(reason)) => {
                self.say(&format!("client: disconnected: {reason}"));
                self.close_surface();
                self.selected = None;
                self.reshow = false;
            }
            ServiceEvent::Stopped { error } => {
                match &error {
                    Some(e) => self.say(&format!("service stopped: {e}")),
                    None => self.say("service stopped"),
                }
                return Some(Exit::Stopped(error));
            }
        }
        None
    }

    /// Prints the client state; each `Ready` can change the mode.
    fn on_ready(&mut self, display: &DisplayId) {
        let info = self
            .viewer
            .displays()
            .into_iter()
            .find(|d| d.id == *display);
        match (info, self.viewer.query(display)) {
            (Some(info), Ok(query)) => self.say(&format!(
                "client: ready: {display} ({}, {}×{} px): {}",
                info.name, info.size.0, info.size.1, query.mode.description
            )),
            (_, Err(e)) => self.say(&format!("client: ready: {display}: error: {e}")),
            (None, Ok(_)) => self.say(&format!("client: ready: {display}: not listed")),
        }
        if self.reshow && self.selected.as_ref() == Some(display) {
            self.reshow = false;
            self.show();
        }
    }

    fn on_line(&mut self, line: &str) -> Option<Exit> {
        match command::parse(line) {
            Ok(Some(command)) => self.on_command(command),
            Ok(None) => None,
            Err(e) => {
                self.say(&format!("error: {e}"));
                None
            }
        }
    }

    fn on_command(&mut self, command: Command) -> Option<Exit> {
        match command {
            Command::Help => self.say(command::HELP),
            Command::Displays => self.list(),
            Command::Select(text) => self.select(&text),
            Command::Pattern(pattern) => {
                self.settings.pattern = pattern;
                self.say(&format!("pattern: {}", pattern.label()));
            }
            Command::Format(encoding, depth) => {
                self.settings.format = Format::new(encoding, depth);
                self.say(&format!("format: {}", self.settings.format));
            }
            Command::Show => self.show(),
            Command::Table => self.table(),
            Command::Close => {
                self.close_surface();
                self.reshow = false;
                self.say("status: surface closed");
            }
            Command::Quit => return Some(Exit::Quit),
        }
        None
    }

    fn list(&mut self) {
        let displays = self.viewer.displays();
        if displays.is_empty() {
            self.say("displays: none");
        }
        for d in displays {
            self.say(&format!(
                "display: {} ({}, {}×{} px)",
                d.id, d.name, d.size.0, d.size.1
            ));
        }
    }

    fn select(&mut self, text: &str) {
        let Some(info) = self
            .viewer
            .displays()
            .into_iter()
            .find(|d| d.id.to_string() == text)
        else {
            self.say(&format!("error: no client {text}; type `displays`"));
            return;
        };
        if self.selected.as_ref() != Some(&info.id) {
            self.close_surface();
            self.reshow = false;
        }
        self.say(&format!("selected: {}", info.id));
        self.selected = Some(info.id);
    }

    /// Replaces the pending or current frame. A rejected request keeps the
    /// current frame.
    fn show(&mut self) {
        // Releases the borrow before `open`.
        self.cancel();
        let Some(display) = self.selected.clone() else {
            self.say("error: no client selected; type `select <display id>`");
            return;
        };
        let Settings { pattern, format } = self.settings;
        if !pattern.encodings().contains(&format.encoding) {
            self.say(&format!(
                "error: {} does not support {}",
                pattern.label(),
                format.encoding
            ));
            return;
        }
        // The user can change the browser mode at any time.
        let query = match self.viewer.query(&display) {
            Ok(query) => query,
            Err(e) => {
                self.say(&format!("error: query failed: {e}"));
                return;
            }
        };
        let limits = OutputLimits::for_frame(&query.capabilities, format, None);
        let Some(path) = &limits.path else {
            // The warnings hold the rejection reason.
            self.say(&format!("error: {}", limits.warnings.join("; ")));
            return;
        };
        self.say(&format!("output: {}", path.description));
        for warning in &limits.warnings {
            self.say(&format!("warning: {warning}"));
        }
        let surface = match self.driver.borrow_mut().open(&display, format.depth) {
            Ok(surface) => surface,
            Err(e) => {
                self.say(&format!("error: open failed: {e}"));
                return;
            }
        };
        self.opened = Some(display);
        let image = match pattern.render(format, surface.size.0, surface.size.1) {
            Ok(image) => image,
            Err(e) => {
                self.say(&format!("error: {e}"));
                return;
            }
        };
        self.say(&format!("surface: {}", surface.description));
        self.say("status: submitting pattern");
        self.pending = Some(Box::pin(present(self.driver, surface, image, format)));
    }

    fn table(&mut self) {
        let Some(display) = self.selected.clone() else {
            self.say("error: no client selected; type `select <display id>`");
            return;
        };
        let size = self
            .viewer
            .displays()
            .into_iter()
            .find(|d| d.id == display)
            .map(|d| d.size);
        let (size, limits) = match (size, self.viewer.query(&display)) {
            (Some(size), Ok(query)) => (
                size,
                OutputLimits::for_frame(&query.capabilities, self.settings.format, None),
            ),
            (_, Err(e)) => {
                self.say(&format!("error: query failed: {e}"));
                return;
            }
            (None, Ok(_)) => {
                self.say(&format!("error: {display} is not listed"));
                return;
            }
        };
        let Settings { pattern, format } = self.settings;
        match pattern.reference_table(format, size, &limits) {
            Ok(table) => self.say(table.trim_end()),
            Err(e) => self.say(&format!("error: {e}")),
        }
    }
}

/// Owns the surface and image, so the pending frame needs no host borrow.
#[allow(clippy::await_holding_refcell_ref)] // See `Host::pending`.
async fn present<D: OutputDriver>(
    driver: &RefCell<D>,
    surface: Surface,
    image: Image,
    format: Format,
) -> Result<String, OutputError> {
    let frame = Frame {
        image: &image,
        format,
        metadata: None,
    };
    driver.borrow_mut().present(&surface, &frame).await
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
