//! `--test-host`: the test-host protocol of `nitpick-test-host` with the
//! browser driver, in place of the console commands. See "Host boundary"
//! in `doc/design/output_driver/testing.md`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use futures_channel::mpsc;
use futures_core::Stream;
use futures_util::stream;
use nitpick_browser::{BrowserDriver, BrowserService, ClientState, ServiceEvent, ServiceEvents};
use nitpick_test_host::protocol::DisplayState;
use nitpick_test_host::{
    Clock, DriverEvent, Event, Fixtures, Hooks, LocalFuture, NoCapture, TestHost,
};

use crate::host::Exit;

/// Reason of every `capture` request.
const NO_CAPTURE: &str = "the browser host has no capture adapter; \
                          a capture of the server desktop does not show a remote display";

/// The executor timer of the command-line host.
struct TokioClock;

impl Clock for TokioClock {
    fn sleep(&self, ms: u32) -> LocalFuture<Result<(), String>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(ms.into())).await;
            Ok(())
        })
    }
}

/// The service pushes the client state; there is nothing to read again.
struct BrowserHooks;

impl Hooks for BrowserHooks {
    fn refresh(&mut self) -> LocalFuture<()> {
        Box::pin(async {})
    }

    fn watch_surface(&mut self) -> Option<LocalFuture<()>> {
        None
    }
}

/// The protocol form of a service event.
pub fn driver_event(event: ServiceEvent) -> DriverEvent {
    let notice = |state| {
        DriverEvent::Notice(Event::Display {
            state,
            display: None,
            reason: None,
        })
    };
    match event {
        ServiceEvent::Listening { url } => DriverEvent::Notice(Event::Listening { url }),
        ServiceEvent::PairingCode(code) => DriverEvent::Notice(Event::PairingCode { code }),
        ServiceEvent::Client(ClientState::Connected) => notice(DisplayState::Connected),
        ServiceEvent::Client(ClientState::Authenticated) => notice(DisplayState::Authenticated),
        ServiceEvent::Client(ClientState::Ready(display)) => DriverEvent::Ready(display),
        ServiceEvent::Client(state @ ClientState::ViewportChanged(_)) => {
            let error = state
                .presentation_error()
                .expect("a viewport change ends a frame");
            let ClientState::ViewportChanged(display) = state else {
                unreachable!()
            };
            DriverEvent::Changed { display, error }
        }
        ServiceEvent::Client(ClientState::Disconnected(reason)) => DriverEvent::Disconnected {
            display: None,
            reason,
        },
        ServiceEvent::Stopped { error } => DriverEvent::Stopped { error },
    }
}

fn driver_events(events: ServiceEvents) -> impl Stream<Item = DriverEvent> + Unpin {
    Box::pin(stream::unfold(events, |mut events| async move {
        let event = events.next().await?;
        Some((driver_event(event), events))
    }))
}

/// Runs the protocol until `quit`, the end of `lines` or a service stop.
/// The caller stops the service afterwards.
pub async fn run(
    service: &BrowserService,
    events: ServiceEvents,
    lines: mpsc::UnboundedReceiver<String>,
    fixtures: Fixtures,
    delay_ms: u32,
) -> Exit {
    let host = TestHost {
        driver: Rc::new(RefCell::new(BrowserDriver::new(service))),
        viewer: BrowserDriver::new(service),
        clock: Box::new(TokioClock),
        hooks: Box::new(BrowserHooks),
        capture: Box::new(NoCapture(NO_CAPTURE.into())),
        fixtures: Rc::new(fixtures),
        delay_ms,
    };
    let exit = host
        .run(lines, driver_events(events), |event| {
            use std::io::Write as _;
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", event.to_line());
            let _ = out.flush();
        })
        .await;
    match exit {
        nitpick_test_host::Exit::Quit => Exit::Quit,
        nitpick_test_host::Exit::Stopped(error) => Exit::Stopped(error),
    }
}

#[cfg(test)]
#[path = "test_host_tests.rs"]
mod tests;
