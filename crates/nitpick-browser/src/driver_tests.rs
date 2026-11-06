use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::io::Cursor;
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use nitpick_core::driver::{ContentLightLevel, Hdr10Request, Support};
use nitpick_core::image::Image;
use nitpick_core::signal::{Encoding, Format, Rgb};
use serde_json::{Value, json};
use zune_png::PngDecoder;

use super::*;
use crate::service::{ClientState, EventSender, ServiceEvent, ServiceEvents};
use crate::session::{ACK_TIMEOUT, Pairing, Reservation};
use crate::test_support::{self, Client, Outgoing, PEER, hello, hello_with};
use crate::worker::Job;

/// Runs each job at once, on the calling thread.
#[derive(Debug, Default)]
struct Inline;

impl Workers for Inline {
    fn spawn(&self, job: Job) {
        job();
    }
}

/// Keeps jobs until the test runs them.
#[derive(Clone, Default)]
struct Manual(Rc<RefCell<Vec<Job>>>);

impl std::fmt::Debug for Manual {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Manual")
    }
}

impl Workers for Manual {
    fn spawn(&self, job: Job) {
        self.0.borrow_mut().push(job);
    }
}

impl Manual {
    fn run(&self, index: usize) {
        let job = self.0.borrow_mut().remove(index);
        job();
    }

    fn len(&self) -> usize {
        self.0.borrow().len()
    }
}

struct Fixture {
    hub: Arc<SessionHub>,
    events: ServiceEvents,
    driver: BrowserDriver<Inline>,
}

impl Fixture {
    fn new() -> Self {
        let (sender, events) = EventSender::channel();
        let hub = SessionHub::new(sender, Pairing::Random);
        let driver = BrowserDriver::with_hub(Arc::clone(&hub), Inline);
        Self {
            hub,
            events,
            driver,
        }
    }

    /// An authenticated session without `hello` and without a session task.
    fn authenticate(&self) -> (Reservation, DisplayId) {
        let reservation = self.hub.reserve(PEER).expect("slot free");
        let (display, _commands) = self.hub.accept("token".into());
        (reservation, display)
    }

    fn hello(&self, display: &DisplayId, changes: &[(&str, Option<Value>)]) {
        self.hub.update_client(display, PEER, hello(changes));
    }

    fn drain(&mut self) -> Vec<ServiceEvent> {
        std::iter::from_fn(|| self.events.try_next()).collect()
    }
}

fn format(depth: BitDepth) -> Format {
    Format::new(Encoding::PqNarrow, depth)
}

/// Narrow-range black: no PNG clipping.
fn black(size: (usize, usize)) -> Image {
    Image::new(
        size.0,
        size.1,
        Rgb {
            r: 64,
            g: 64,
            b: 64,
        },
    )
}

/// Polls `present` once without an executor.
fn poll_present<J: Workers>(
    driver: &mut BrowserDriver<J>,
    surface: &Surface,
    depth: BitDepth,
    size: (usize, usize),
) -> Poll<Result<String, OutputError>> {
    let image = black(size);
    let frame = Frame {
        image: &image,
        format: format(depth),
        metadata: None,
    };
    let mut future = pin!(driver.present(surface, &frame));
    future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
}

/// The browser driver must answer stale, closed and disconnected surfaces
/// without a wait.
fn present_now<J: Workers>(
    driver: &mut BrowserDriver<J>,
    surface: &Surface,
    depth: BitDepth,
    size: (usize, usize),
) -> Result<String, OutputError> {
    match poll_present(driver, surface, depth, size) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("present waited"),
    }
}

/// The frame passed the surface checks and waits for the client.
fn submitted<J: Workers>(driver: &mut BrowserDriver<J>, surface: &Surface) -> bool {
    poll_present(driver, surface, BitDepth::Ten, surface.size).is_pending()
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
    assert!(submitted(&mut f.driver, &surface));
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
    assert!(submitted(&mut f.driver, &surface));
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
    assert!(
        submitted(&mut f.driver, &surface),
        "close of another display"
    );
    f.driver.close(&display);
    assert_eq!(
        present_now(&mut f.driver, &surface, BitDepth::Ten, surface.size),
        Err(OutputError::Disconnected)
    );
}

#[test]
fn frame_errors_fail_before_submission() {
    let mut f = Fixture::new();
    let (_reservation, display) = f.authenticate();
    f.hello(&display, &[]);
    let surface = f.driver.open(&display, BitDepth::Ten).unwrap();
    let auto = Hdr10Request {
        primaries: [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
        white: [0.3127, 0.3290],
        max_mastering_nits: 1000.0,
        min_mastering_nits: 0.0001,
        light_level: ContentLightLevel::FromFrame,
    };
    let image = black(surface.size);
    let hlg = Frame {
        image: &image,
        format: Format::new(Encoding::HlgNarrow, BitDepth::Ten),
        metadata: Some(&auto),
    };
    let result =
        pin!(f.driver.present(&surface, &hlg)).poll(&mut Context::from_waker(Waker::noop()));
    assert!(matches!(
        result,
        Poll::Ready(Err(OutputError::Unsupported { .. }))
    ));

    // A code above the 10-bit maximum fails in the encoder.
    let image = Image::new(
        surface.size.0,
        surface.size.1,
        Rgb {
            r: 1024,
            g: 0,
            b: 0,
        },
    );
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    let result =
        pin!(f.driver.present(&surface, &frame)).poll(&mut Context::from_waker(Waker::noop()));
    assert!(matches!(result, Poll::Ready(Err(OutputError::Platform(_)))));
    assert_eq!(f.hub.frame(1), None, "nothing stored");
}

// Presentation through a session task with a mock transport.

/// A session with a client after `hello` on 4×2 CSS px at pixel ratio 1.
struct Live<J> {
    hub: Arc<SessionHub>,
    events: ServiceEvents,
    client: Client,
    display: DisplayId,
    driver: BrowserDriver<J>,
}

const SMALL: (usize, usize) = (4, 2);

fn small_hello(changes: &[(&str, Option<Value>)]) -> Value {
    let mut all = vec![
        ("viewport.width", Some(json!(SMALL.0))),
        ("viewport.height", Some(json!(SMALL.1))),
        ("pixelRatio", Some(json!(1))),
    ];
    all.extend_from_slice(changes);
    hello_with(&all)
}

impl<J: Workers> Live<J> {
    async fn new(workers: J) -> Self {
        let (hub, mut events) = test_support::hub(Pairing::Fixed("1234".into()));
        let (client, _token) = test_support::authenticated_client(&hub, "1234").await;
        client.send_json(&small_hello(&[]));
        let display = loop {
            if let Some(ServiceEvent::Client(ClientState::Ready(d))) = events.next().await {
                break d;
            }
        };
        let driver = BrowserDriver::with_hub(Arc::clone(&hub), workers);
        Self {
            hub,
            events,
            client,
            display,
            driver,
        }
    }

    fn open(&mut self) -> Surface {
        self.driver.open(&self.display, BitDepth::Ten).unwrap()
    }
}

/// Lets the session task and the client run.
async fn settle() {
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }
}

async fn poll<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut future = future;
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx))).await
}

async fn expect_show(client: &mut Client) -> u64 {
    match client.next().await {
        Some(Outgoing::Text(v)) if v["type"] == "show" => {
            assert_eq!(
                (v["width"].clone(), v["height"].clone()),
                (json!(4), json!(2))
            );
            v["id"].as_u64().expect("id")
        }
        other => panic!("expected show, got {other:?}"),
    }
}

fn applied(id: u64, layout: (f64, f64)) -> Value {
    json!({
        "type": "applied",
        "id": id,
        "naturalWidth": SMALL.0,
        "naturalHeight": SMALL.1,
        "layoutWidth": layout.0,
        "layoutHeight": layout.1,
    })
}

fn decode(png: &[u8]) -> (usize, usize) {
    let mut decoder = PngDecoder::new(Cursor::new(png));
    decoder.decode().expect("decode");
    decoder.dimensions().expect("dimensions")
}

#[tokio::test(start_paused = true)]
async fn applied_completes_the_frame_with_pixel_exact_layout() {
    let mut live = Live::new(Inline).await;
    let surface = live.open();
    let image = black(SMALL);
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    let mut future = Box::pin(live.driver.present(&surface, &frame));
    assert!(poll(future.as_mut()).await.is_pending());
    let id = expect_show(&mut live.client).await;
    assert_eq!(decode(&live.hub.frame(id).expect("stored")), SMALL);
    // Service events stay available during the mutable driver borrow.
    live.client
        .send_json(&small_hello(&[("dynamicRange", Some(json!("standard")))]));
    assert_eq!(
        live.events.next().await,
        Some(ServiceEvent::Client(ClientState::Ready(
            live.display.clone()
        )))
    );
    live.client.send_json(&applied(id, (4.0, 2.0)));
    let status = future.await.unwrap();
    assert!(
        status.starts_with(&format!("Frame {id} applied")),
        "{status}"
    );
    for text in [
        "PQ narrow range, 10-bit",
        "4×2 display px",
        "Pixel-exact layout",
        "SDR preview",
    ] {
        assert!(status.contains(text), "{text}: {status}");
    }
    assert!(!status.contains("pixel-exact layout: unknown"), "{status}");
    assert_eq!(
        live.driver
            .query(&live.display)
            .unwrap()
            .capabilities
            .pixel_exact,
        Support::Yes
    );
    // A reopened surface keeps the reported state.
    assert!(!live.open().description.contains("pixel-exact"));
}

#[tokio::test(start_paused = true)]
async fn other_layout_or_ratio_is_not_pixel_exact() {
    for (changes, layout, text) in [
        (vec![], (8.0, 4.0), "laid out"),
        (
            vec![("viewport.scale", Some(json!(2)))],
            (4.0, 2.0),
            "zoom 2",
        ),
    ] {
        let mut live = Live::new(Inline).await;
        live.client.send_json(&small_hello(&changes));
        settle().await;
        let surface = live.open();
        let image = black(SMALL);
        let frame = Frame {
            image: &image,
            format: format(BitDepth::Ten),
            metadata: None,
        };
        let mut future = pin!(live.driver.present(&surface, &frame));
        assert!(poll(future.as_mut()).await.is_pending());
        let id = expect_show(&mut live.client).await;
        live.client.send_json(&applied(id, layout));
        let status = future.await.unwrap();
        assert!(status.contains("pixel-exact layout: no"), "{status}");
        assert!(status.contains(text), "{text}: {status}");
        assert!(!status.contains("Pixel-exact layout."), "{status}");
    }
}

#[tokio::test(start_paused = true)]
async fn clipping_is_in_the_status() {
    let mut live = Live::new(Inline).await;
    let surface = live.open();
    let image = Image::new(
        SMALL.0,
        SMALL.1,
        Rgb {
            r: 1000,
            g: 0,
            b: 64,
        },
    );
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    let mut future = pin!(live.driver.present(&surface, &frame));
    assert!(poll(future.as_mut()).await.is_pending());
    let id = expect_show(&mut live.client).await;
    live.client.send_json(&applied(id, (4.0, 2.0)));
    let status = future.await.unwrap();
    assert!(
        status.contains("PNG clipped 8 pixels below black and 8 pixels above nominal peak"),
        "{status}"
    );
}

#[tokio::test(start_paused = true)]
async fn late_reply_does_not_complete_a_new_frame() {
    let mut live = Live::new(Inline).await;
    let surface = live.open();
    let image = black(SMALL);
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    let old = {
        let mut future = pin!(live.driver.present(&surface, &frame));
        assert!(poll(future.as_mut()).await.is_pending());
        expect_show(&mut live.client).await
        // Dropping the future removes its registration.
    };
    let mut future = pin!(live.driver.present(&surface, &frame));
    assert!(poll(future.as_mut()).await.is_pending());
    let new = expect_show(&mut live.client).await;
    assert_ne!(old, new);
    live.client.send_json(&applied(old, (4.0, 2.0)));
    live.client
        .send_json(&json!({"type": "error", "id": old, "message": "late"}));
    settle().await;
    assert!(poll(future.as_mut()).await.is_pending());
    live.client.send_json(&applied(new, (4.0, 2.0)));
    assert!(future.await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn late_worker_result_is_discarded() {
    let workers = Manual::default();
    let mut live = Live::new(workers.clone()).await;
    let surface = live.open();
    let image = black(SMALL);
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    {
        let mut future = pin!(live.driver.present(&surface, &frame));
        assert!(poll(future.as_mut()).await.is_pending());
    }
    let mut future = pin!(live.driver.present(&surface, &frame));
    assert!(poll(future.as_mut()).await.is_pending());
    assert_eq!(workers.len(), 2);
    // The job of the dropped future finishes late: nothing is shown.
    workers.run(0);
    settle().await;
    assert!(live.client.rx.try_recv().is_err());
    workers.run(0);
    assert!(poll(future.as_mut()).await.is_pending());
    let id = expect_show(&mut live.client).await;
    assert_eq!(id, 1, "the late job took no frame ID");
    live.client.send_json(&applied(id, (4.0, 2.0)));
    assert!(future.await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn resize_during_encoding_fails_at_submission() {
    let workers = Manual::default();
    let mut live = Live::new(workers.clone()).await;
    let surface = live.open();
    let image = black(SMALL);
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    let mut future = pin!(live.driver.present(&surface, &frame));
    assert!(poll(future.as_mut()).await.is_pending());
    live.client
        .send_json(&small_hello(&[("viewport.width", Some(json!(5)))]));
    settle().await;
    workers.run(0);
    assert_eq!(future.await, Err(stale()));
    assert!(live.client.rx.try_recv().is_err(), "no show");
}

#[tokio::test(start_paused = true)]
async fn timeout_runs_on_the_server_runtime() {
    let mut live = Live::new(Inline).await;
    let surface = live.open();
    let image = black(SMALL);
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    let mut future = Box::pin(live.driver.present(&surface, &frame));
    assert!(poll(future.as_mut()).await.is_pending());
    let id = expect_show(&mut live.client).await;
    // The host does not poll during the wait.
    tokio::time::advance(ACK_TIMEOUT + Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(
        poll(future.as_mut()).await,
        Poll::Ready(Err(OutputError::Timeout))
    );
    drop(future);
    // A reply after the timeout changes nothing.
    live.client.send_json(&applied(id, (4.0, 2.0)));
    settle().await;
    assert!(matches!(
        live.driver
            .query(&live.display)
            .unwrap()
            .capabilities
            .pixel_exact,
        Support::Unknown(_)
    ));
}

#[tokio::test(start_paused = true)]
async fn resize_or_disconnect_fails_the_pending_frame() {
    let mut live = Live::new(Inline).await;
    let surface = live.open();
    let image = black(SMALL);
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    {
        let mut future = pin!(live.driver.present(&surface, &frame));
        assert!(poll(future.as_mut()).await.is_pending());
        expect_show(&mut live.client).await;
        live.client
            .send_json(&small_hello(&[("pixelRatio", Some(json!(2)))]));
        assert_eq!(future.await, Err(stale()));
    }
    let surface = live.open();
    assert_eq!(surface.size, (8, 4));
    let image = black(surface.size);
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    let mut future = pin!(live.driver.present(&surface, &frame));
    assert!(poll(future.as_mut()).await.is_pending());
    let Live { client, hub, .. } = live;
    client.finished().await;
    assert_eq!(future.await, Err(OutputError::Disconnected));
    assert_eq!(hub.frame(2), None, "released on disconnect");
}

#[tokio::test(start_paused = true)]
async fn close_after_cancellation_shows_black() {
    let mut live = Live::new(Inline).await;
    let surface = live.open();
    let image = black(SMALL);
    let frame = Frame {
        image: &image,
        format: format(BitDepth::Ten),
        metadata: None,
    };
    let id = {
        let mut future = pin!(live.driver.present(&surface, &frame));
        assert!(poll(future.as_mut()).await.is_pending());
        expect_show(&mut live.client).await
    };
    live.driver.close(&live.display);
    assert_eq!(
        live.client.next().await,
        Some(Outgoing::Text(json!({"type": "close"})))
    );
    assert_eq!(live.hub.frame(id), None);
    live.client.send_json(&applied(id, (4.0, 2.0)));
    settle().await;
    assert_eq!(
        present_now(&mut live.driver, &surface, BitDepth::Ten, SMALL),
        Err(OutputError::Disconnected)
    );
}
