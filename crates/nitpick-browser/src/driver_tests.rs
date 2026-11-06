use std::pin::pin;
use std::task::{Context, Poll, Waker};

use nitpick_core::image::Image;
use nitpick_core::signal::{Encoding, Format, Rgb};
use serde_json::json;

use super::*;
use crate::service::{ClientState, EventSender, ServiceEvent, ServiceEvents};
use crate::session::{Pairing, Reservation};
use crate::test_support::*;

struct Fixture {
    hub: Arc<SessionHub>,
    events: ServiceEvents,
    driver: BrowserDriver,
}

impl Fixture {
    fn new() -> Self {
        let (sender, events) = EventSender::channel();
        let hub = SessionHub::new(sender, Pairing::Random);
        let driver = BrowserDriver::with_hub(Arc::clone(&hub));
        Self {
            hub,
            events,
            driver,
        }
    }

    /// An authenticated session without `hello`.
    fn authenticate(&self) -> (Reservation, DisplayId) {
        let reservation = self.hub.reserve(PEER).expect("slot free");
        let display = self.hub.accept("token".into());
        (reservation, display)
    }

    fn hello(&self, display: &DisplayId, changes: &[(&str, Option<serde_json::Value>)]) {
        self.hub.update_client(display, PEER, hello(changes));
    }

    fn drain(&mut self) -> Vec<ServiceEvent> {
        std::iter::from_fn(|| self.events.try_next()).collect()
    }
}

fn format(depth: BitDepth) -> Format {
    Format::new(Encoding::PqNarrow, depth)
}

/// Polls `present` once. The browser driver must answer stale, closed
/// and disconnected surfaces without a wait.
fn present_now(
    driver: &mut BrowserDriver,
    surface: &Surface,
    depth: BitDepth,
    size: (usize, usize),
) -> Result<String, OutputError> {
    let image = Image::new(size.0, size.1, Rgb::default());
    let frame = Frame {
        image: &image,
        format: format(depth),
        metadata: None,
    };
    let mut future = pin!(driver.present(surface, &frame));
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("present waited"),
    }
}

fn stale() -> OutputError {
    OutputError::Platform(VIEWPORT_CHANGED.into())
}

#[test]
fn no_display_before_a_valid_hello() {
    let mut f = Fixture::new();
    assert_eq!(f.driver.displays(), []);
    let (_reservation, display) = f.authenticate();
    assert_eq!(f.driver.displays(), [], "authenticated only");
    assert_eq!(f.driver.query(&display), Err(OutputError::Disconnected));
    assert_eq!(
        f.driver.open(&display, BitDepth::Ten),
        Err(OutputError::Disconnected)
    );
}

#[test]
fn hello_adds_the_client_display() {
    let mut f = Fixture::new();
    let (_reservation, display) = f.authenticate();
    f.hello(&display, &[]);
    assert_eq!(display.driver, DRIVER);
    assert_eq!(f.driver.id(), DRIVER);
    assert_eq!(
        f.driver.displays(),
        vec![DisplayInfo {
            id: display.clone(),
            name: "Safari at 192.0.2.10".into(),
            size: (2360, 1640),
        }]
    );
    let q = f.driver.query(&display).unwrap();
    assert_eq!(q.mode.hdr_on, Some(true));
    let other = DisplayId {
        driver: DRIVER,
        key: "client-0".into(),
    };
    assert_eq!(f.driver.query(&other), Err(OutputError::Disconnected));
}

#[test]
fn disconnect_removes_the_display() {
    let mut f = Fixture::new();
    let (reservation, display) = f.authenticate();
    f.hello(&display, &[]);
    let surface = f.driver.open(&display, BitDepth::Ten).unwrap();
    drop(reservation);
    assert_eq!(f.driver.displays(), []);
    assert_eq!(f.driver.query(&display), Err(OutputError::Disconnected));
    assert_eq!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
        Err(OutputError::Disconnected)
    );

    // A new session is a new display; the old surface stays invalid.
    let (_reservation, second) = f.authenticate();
    f.hello(&second, &[]);
    assert_ne!(second, display);
    assert_eq!(f.driver.displays().len(), 1);
    assert_eq!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
        Err(OutputError::Disconnected)
    );
}

#[test]
fn open_uses_cached_state_and_reports_warnings() {
    let mut f = Fixture::new();
    let (_reservation, display) = f.authenticate();
    f.hello(
        &display,
        &[
            ("dynamicRange", Some(json!("standard"))),
            ("fullscreen", Some(json!(false))),
        ],
    );
    let surface = f.driver.open(&display, BitDepth::Twelve).unwrap();
    assert_eq!(surface.display, display);
    assert_eq!(surface.size, (2360, 1640));
    for text in ["2360×1640", "SDR preview", "not full screen", "exact codes"] {
        assert!(
            surface.description.contains(text),
            "{}",
            surface.description
        );
    }
    // `open` sends nothing and emits no event.
    f.drain();
    f.driver.open(&display, BitDepth::Ten).unwrap();
    assert_eq!(f.drain(), []);
}

#[test]
fn resize_invalidates_the_surface_and_notifies_the_host() {
    let mut f = Fixture::new();
    let (_reservation, display) = f.authenticate();
    f.drain();
    f.hello(&display, &[]);
    assert_eq!(
        f.drain(),
        vec![ServiceEvent::Client(ClientState::Ready(display.clone()))]
    );
    let surface = f.driver.open(&display, BitDepth::Ten).unwrap();

    f.hello(&display, &[("viewport.width", Some(json!(1000)))]);
    assert_eq!(
        f.drain(),
        vec![
            ServiceEvent::Client(ClientState::ViewportChanged(display.clone())),
            ServiceEvent::Client(ClientState::Ready(display.clone())),
        ]
    );
    assert_eq!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
        Err(stale())
    );

    // Reopen at the new size.
    let surface = f.driver.open(&display, BitDepth::Ten).unwrap();
    assert_eq!(surface.size, (2000, 1640));
    assert_ne!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
        Err(stale())
    );
}

#[test]
fn pixel_ratio_and_zoom_changes_invalidate_the_surface() {
    for change in [("pixelRatio", json!(3)), ("viewport.scale", json!(1.5))] {
        let mut f = Fixture::new();
        let (_reservation, display) = f.authenticate();
        f.hello(&display, &[]);
        let surface = f.driver.open(&display, BitDepth::Ten).unwrap();
        f.drain();
        f.hello(&display, &[(change.0, Some(change.1.clone()))]);
        assert!(
            f.drain()
                .contains(&ServiceEvent::Client(ClientState::ViewportChanged(
                    display.clone()
                ))),
            "{change:?}"
        );
        assert_eq!(
            present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
            Err(stale()),
            "{change:?}"
        );
    }
}

#[test]
fn mode_change_keeps_the_surface() {
    let mut f = Fixture::new();
    let (_reservation, display) = f.authenticate();
    f.hello(&display, &[]);
    let surface = f.driver.open(&display, BitDepth::Ten).unwrap();
    f.drain();
    f.hello(
        &display,
        &[
            ("dynamicRange", Some(json!("standard"))),
            ("fullscreen", Some(json!(false))),
        ],
    );
    assert_eq!(
        f.drain(),
        vec![ServiceEvent::Client(ClientState::Ready(display.clone()))]
    );
    assert_eq!(f.driver.query(&display).unwrap().mode.hdr_on, Some(false));
    assert_ne!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
        Err(stale())
    );
}

#[test]
fn closed_or_mismatched_surface_fails_at_once() {
    let mut f = Fixture::new();
    let (_reservation, display) = f.authenticate();
    f.hello(&display, &[]);
    let surface = f.driver.open(&display, BitDepth::Ten).unwrap();
    assert!(matches!(
        present_now(&mut f.driver, &surface, BitDepth::Twelve, surface.size),
        Err(OutputError::Unsupported { .. })
    ));
    assert_eq!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, (16, 16)),
        Err(OutputError::Platform(
            "image size differs from the surface size".into()
        ))
    );
    let other = DisplayId {
        driver: DRIVER,
        key: "other".into(),
    };
    f.driver.close(&other);
    assert_ne!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
        Err(OutputError::Disconnected),
        "close of another display"
    );
    f.driver.close(&display);
    assert_eq!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
        Err(OutputError::Disconnected)
    );
}
