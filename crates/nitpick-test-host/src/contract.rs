//! The shared host contract ("Shared host contract" in
//! `doc/design/output_driver/testing.md`) over the test-host protocol, with
//! a mock driver, a controlled clock and controlled presentation results.
//! Each host runs [`run_all`] with its own executor.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::future::{self, Future};
use std::pin::Pin;
use std::rc::{Rc, Weak};
use std::task::{Context, Poll, Waker};

use futures_channel::{mpsc, oneshot};
use futures_util::StreamExt as _;
use nitpick_core::device::{DeviceError, DeviceId};
use nitpick_core::driver::{
    Capabilities, Conversion, DepthSupport, DisplayId, DisplayInfo, EncodingSupport, Hdr10Request,
    ModeSettings, OutputDriver, OutputError, PresentFuture, Query, Support, Surface, check_frame,
    check_open,
};
use nitpick_core::image::Image;
use nitpick_core::signal::{BitDepth, Encoding, Format, Rgb};
use nitpick_fixtures::{LightLevelRequest, MetadataRequest};

use crate::artifact::{Artifact, ArtifactSink, Artifacts, Environment, EnvironmentSource, Saved};
use crate::capture::{Capture, CaptureAdapter, CaptureFrames, CaptureTarget, Timestamp};
use crate::controller::{Clock, DriverEvent, Hooks, LocalFuture, NO_ARTIFACTS, TestHost};
use crate::device::{DeviceCall, DeviceReply, Devices};
use crate::fixture::{Fixtures, hdr10_request};
use crate::protocol::{
    CaptureOutcome, Cause, DisplayEntry, DisplayState, ErrorKind, Event, Request, SupportState,
    SurfaceState, VERSION,
};
use crate::replay;

/// Runs the host and the test script of one scenario.
pub trait LocalExecutor {
    /// Runs both futures on the current thread until both complete.
    /// Panics if they stop making progress.
    fn run(&mut self, host: LocalFuture<()>, script: LocalFuture<()>);
}

const DISPLAY: &str = "mock:screen-1";
const SIZE: (usize, usize) = (64, 40);
const DEFAULT_DELAY_MS: u32 = 250;
const FRAME_A: &str = "state-frame-a-pq-full-10";
const FRAME_B: &str = "state-frame-b-pq-full-10";
/// 16×9 px, PQ narrow range, with a metadata request.
const WITH_METADATA: &str = "lum-uniform-peak-pq-narrow-10";
/// 96×18 px: larger than [`SIZE`].
const TOO_LARGE: &str = "gamut-pq-full-10";
const NO_CAPTURE: &str = "the mock host has no capture adapter";
/// Of the held artifact sink.
const QUEUE: usize = 2;

type Scenario = (&'static str, fn(Harness) -> LocalFuture<()>);

macro_rules! scenarios {
    ($($name:ident),* $(,)?) => {
        &[$((stringify!($name), |h| Box::pin($name(h)) as LocalFuture<()>)),*]
    };
}

pub const SCENARIOS: &[Scenario] = scenarios![
    reports_displays_and_each_support_state,
    open_rejects_unavailable_depths,
    show_rejects_invalid_requests_without_presenting,
    frame_ids_must_increase,
    renders_the_fixture_at_the_surface_size,
    readiness_follows_acknowledgement_and_delay,
    zero_delay_is_ready_at_once,
    failed_presentation_starts_no_timer,
    stopped_timer_is_an_error,
    replacement_cancels_the_presentation,
    replacement_cancels_the_delay,
    malformed_input_is_reported,
    close_cancels_and_closes,
    disconnect_fails_the_presentation,
    disconnect_after_the_driver_result_gives_the_same_error,
    disconnect_cancels_the_delay,
    display_change_fails_the_presentation_and_needs_open,
    display_change_cancels_the_delay,
    external_close_fails_the_presentation,
    external_close_cancels_the_delay,
    refresh_lists_displays_after_the_read,
    metadata_follows_the_request,
    driver_notices_pass_through,
    quit_ends_pending_work,
    end_of_input_is_quit,
    driver_stop_ends_the_host,
];

mod capture;

/// Capture sessions, frame acceptance and artifacts.
pub const CAPTURE_SCENARIOS: &[Scenario] = capture::SCENARIOS;

/// Scenarios of a host without [`Artifacts`].
pub const SCENARIOS_WITHOUT_ARTIFACTS: &[Scenario] = capture::WITHOUT_ARTIFACTS;

mod device;

/// Device requests, the device run and profile changes.
pub const DEVICE_SCENARIOS: &[Scenario] = device::SCENARIOS;

/// Scenarios of a host without [`Devices`].
pub const SCENARIOS_WITHOUT_DEVICES: &[Scenario] = device::WITHOUT_DEVICES;

/// Runs every scenario of [`SCENARIOS`], [`CAPTURE_SCENARIOS`],
/// [`DEVICE_SCENARIOS`], [`SCENARIOS_WITHOUT_ARTIFACTS`] and
/// [`SCENARIOS_WITHOUT_DEVICES`] with `exec`.
///
/// # Panics
/// A scenario fails.
pub fn run_all(exec: &mut impl LocalExecutor) {
    let fixtures = Rc::new(Fixtures::load().expect("output fixtures"));
    let all = SCENARIOS
        .iter()
        .chain(CAPTURE_SCENARIOS)
        .chain(DEVICE_SCENARIOS)
        .map(|s| (s, true, true))
        .chain(SCENARIOS_WITHOUT_ARTIFACTS.iter().map(|s| (s, false, true)))
        .chain(SCENARIOS_WITHOUT_DEVICES.iter().map(|s| (s, true, false)));
    for ((name, scenario), artifacts, devices) in all {
        eprintln!("contract scenario: {name}");
        let (host, mut harness) = setup(&fixtures, artifacts, devices);
        let script = Box::pin(async move {
            harness
                .expect(Event::Hello {
                    protocol: VERSION,
                    driver: "mock".into(),
                })
                .await;
            scenario(harness).await;
        });
        exec.run(host, script);
    }
}

fn display_id() -> DisplayId {
    DisplayId {
        driver: "mock",
        key: "screen-1".into(),
    }
}

/// 10-bit with unknown exactness; 12-bit unavailable; HLG rejected.
fn query() -> Query {
    let input = |input| EncodingSupport {
        input,
        interpretation: Support::Yes,
        auto_light_level: Support::Yes,
    };
    let depth = |available| DepthSupport {
        available,
        path: "mock path".into(),
        conversion: Conversion::Direct,
        exact: Support::Unknown("mock exactness".into()),
    };
    Query {
        capabilities: Capabilities {
            depths: vec![
                (BitDepth::Ten, depth(Support::Yes)),
                (
                    BitDepth::Twelve,
                    depth(Support::No("mock has no 12-bit".into())),
                ),
            ],
            encodings: vec![
                (Encoding::PqNarrow, input(Support::Yes)),
                (Encoding::PqFull, input(Support::Yes)),
                (
                    Encoding::HlgNarrow,
                    input(Support::No("mock has no HLG".into())),
                ),
            ],
            hdr_metadata: Support::Yes,
            pixel_exact: Support::Yes,
        },
        mode: ModeSettings {
            hdr_on: Some(true),
            gamut: None,
            wire_depth: None,
            peak_nits: None,
            min_nits: None,
            sdr_white_nits: None,
            pixel_ratio: 1.0,
            description: "mock mode".into(),
        },
    }
}

/// One `present` call of the mock.
pub struct Presented {
    pub image: Image,
    pub format: Format,
    pub metadata: Option<Hdr10Request>,
}

type Reply = oneshot::Sender<Result<String, OutputError>>;

#[derive(Default)]
struct MockState {
    size: (usize, usize),
    /// `open`, `present` and `close`, in order.
    calls: Vec<String>,
    presented: Vec<Presented>,
    /// `None` after the reply.
    replies: Vec<Option<Reply>>,
    /// `present` futures not yet dropped.
    live: usize,
    /// The hooks map the screen to no device, as for a browser client.
    not_local: bool,
}

type Shared = Rc<RefCell<MockState>>;

/// Counts a `present` future from creation to drop.
struct Live(Shared);

impl Drop for Live {
    fn drop(&mut self) {
        self.0.borrow_mut().live -= 1;
    }
}

struct Mock(Shared);

impl OutputDriver for Mock {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn displays(&mut self) -> Vec<DisplayInfo> {
        vec![DisplayInfo {
            id: display_id(),
            name: "Mock screen".into(),
            size: self.0.borrow().size,
        }]
    }

    fn query(&mut self, display: &DisplayId) -> Result<Query, OutputError> {
        if *display == display_id() {
            Ok(query())
        } else {
            Err(OutputError::Disconnected)
        }
    }

    fn open(&mut self, display: &DisplayId, depth: BitDepth) -> Result<Surface, OutputError> {
        self.0.borrow_mut().calls.push(format!("open {depth}"));
        self.query(display)?;
        check_open(&query().capabilities, depth)?;
        Ok(Surface {
            display: display.clone(),
            size: self.0.borrow().size,
            description: "mock surface".into(),
        })
    }

    fn present<'a>(
        &'a mut self,
        _: &'a Surface,
        frame: &'a nitpick_core::driver::Frame<'a>,
    ) -> PresentFuture<'a> {
        let mut state = self.0.borrow_mut();
        state.calls.push("present".into());
        if let Err(e) = check_frame(&query().capabilities, frame.format, frame.metadata) {
            return Box::pin(future::ready(Err(e)));
        }
        state.presented.push(Presented {
            image: frame.image.clone(),
            format: frame.format,
            metadata: frame.metadata.copied(),
        });
        let (tx, rx) = oneshot::channel();
        state.replies.push(Some(tx));
        state.live += 1;
        let live = Live(Rc::clone(&self.0));
        Box::pin(async move {
            let _live = live;
            rx.await.unwrap_or(Err(OutputError::Disconnected))
        })
    }

    fn close(&mut self, _: &DisplayId) {
        let mut state = self.0.borrow_mut();
        assert_eq!(state.live, 0, "close while a present future is alive");
        state.calls.push("close".into());
    }
}

type Timers = Rc<RefCell<Vec<(u32, Option<oneshot::Sender<()>>)>>>;

/// Timers expire only when the script fires them.
struct HeldClock(Timers);

impl Clock for HeldClock {
    fn sleep(&self, ms: u32) -> LocalFuture<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        self.0.borrow_mut().push((ms, Some(tx)));
        Box::pin(async move { rx.await.map_err(|_| "the mock timer stopped".into()) })
    }
}

type Senders = Rc<RefCell<Vec<oneshot::Sender<()>>>>;

/// Refreshes and surface closes complete only when the script says so.
struct HeldHooks {
    refreshes: Senders,
    watches: Senders,
    mock: Shared,
}

impl Hooks for HeldHooks {
    fn refresh(&mut self) -> LocalFuture<()> {
        let (tx, rx) = oneshot::channel();
        self.refreshes.borrow_mut().push(tx);
        Box::pin(async move {
            let _ = rx.await;
        })
    }

    fn watch_surface(&mut self) -> Option<LocalFuture<()>> {
        let (tx, rx) = oneshot::channel();
        self.watches.borrow_mut().push(tx);
        Some(Box::pin(async move {
            if rx.await.is_err() {
                future::pending::<()>().await;
            }
        }))
    }

    /// The mock screen is a local device unless `MockState::not_local`.
    fn device(&mut self, display: &DisplayId) -> LocalFuture<Result<DeviceId, String>> {
        let result = if *display == display_id() && !self.mock.borrow().not_local {
            Ok(device::device_id())
        } else {
            Err(format!("{display} is not a local display"))
        };
        Box::pin(future::ready(result))
    }
}

type DeviceCalls = RefCell<Vec<(DeviceCall, Option<oneshot::Sender<DeviceResult>>)>>;
type DeviceReplies = Rc<DeviceCalls>;
type DeviceResult = Result<DeviceReply, DeviceError>;

/// Device calls complete only when the script replies. The script owns
/// the calls: when it ends, each pending call fails, so the host can stop.
struct HeldDevices(Weak<DeviceCalls>);

impl Devices for HeldDevices {
    fn call(&mut self, call: DeviceCall) -> LocalFuture<DeviceResult> {
        let (tx, rx) = oneshot::channel();
        if let Some(calls) = self.0.upgrade() {
            calls.borrow_mut().push((call, Some(tx)));
        }
        Box::pin(async move {
            rx.await.unwrap_or_else(|_| {
                Err(DeviceError::Native {
                    api: "mock".into(),
                    code: None,
                    message: "the mock worker stopped".into(),
                })
            })
        })
    }
}

#[derive(Default)]
struct SinkState {
    submitted: Vec<Artifact>,
    /// Submitted and not yet returned by `poll_saved`.
    queued: usize,
    /// Writes the script completed.
    finished: usize,
    done: VecDeque<Saved>,
    waker: Option<Waker>,
}

/// Writes complete only when the script says so.
struct HeldSink(Rc<RefCell<SinkState>>);

impl ArtifactSink for HeldSink {
    fn submit(&mut self, artifact: Artifact) -> Result<String, String> {
        let mut s = self.0.borrow_mut();
        if s.queued >= QUEUE {
            return Err("mock queue full".into());
        }
        s.submitted.push(artifact);
        s.queued += 1;
        Ok(format!("artifact-{}", s.submitted.len()))
    }

    fn poll_saved(&mut self, cx: &mut Context<'_>) -> Poll<Option<Saved>> {
        let mut s = self.0.borrow_mut();
        if let Some(saved) = s.done.pop_front() {
            s.queued -= 1;
            return Poll::Ready(Some(saved));
        }
        if s.queued == 0 {
            return Poll::Ready(None);
        }
        s.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

type FrameSender = mpsc::UnboundedSender<Result<Capture, String>>;

#[derive(Default)]
struct CaptureState {
    /// `false`: `start` fails with [`NO_CAPTURE`].
    available: bool,
    /// The adapter clock in milliseconds; `None`: no clock.
    now: Option<u64>,
    /// Display, frame and generation of each `start`.
    targets: Vec<(String, u64, u64)>,
    /// One per session; `None` after the script ended the session.
    sessions: Vec<Option<FrameSender>>,
    /// Sessions whose stream is not dropped.
    live: usize,
}

type Captures = Rc<RefCell<CaptureState>>;

/// Sessions deliver the frames that the script sends.
struct HeldCapture(Captures);

impl CaptureAdapter for HeldCapture {
    fn now(&self) -> Option<Timestamp> {
        self.0.borrow().now.map(replay::time)
    }

    fn start(&mut self, target: &CaptureTarget<'_>) -> Result<CaptureFrames, String> {
        let mut state = self.0.borrow_mut();
        state
            .targets
            .push((target.display.to_string(), target.frame, target.generation));
        if !state.available {
            return Err(NO_CAPTURE.into());
        }
        let (tx, frames) = mpsc::unbounded();
        state.sessions.push(Some(tx));
        state.live += 1;
        Ok(Box::pin(Session {
            frames,
            captures: Rc::clone(&self.0),
        }))
    }
}

/// Counts the session from `start` to drop.
struct Session {
    frames: mpsc::UnboundedReceiver<Result<Capture, String>>,
    captures: Captures,
}

impl futures_core::Stream for Session {
    type Item = Result<Capture, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.frames.poll_next_unpin(cx)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.captures.borrow_mut().live -= 1;
    }
}

/// The environment that [`DisplayEnvironment`] gives for `display`.
fn environment(display: &str) -> Environment {
    Environment {
        os: Some("mock os".into()),
        renderer: Some(format!("mock renderer of {display}")),
        ..Environment::default()
    }
}

/// Gives an environment of the display that it gets, so that a run record
/// shows which display the host passed.
struct DisplayEnvironment;

impl EnvironmentSource for DisplayEnvironment {
    fn read(&mut self, display: &DisplayId) -> Environment {
        environment(&display.to_string())
    }
}

/// Yields once to the executor.
struct YieldNow(bool);

impl Future for YieldNow {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// The test side of one scenario.
pub struct Harness {
    lines: mpsc::UnboundedSender<String>,
    events: mpsc::UnboundedSender<DriverEvent>,
    out: mpsc::UnboundedReceiver<Event>,
    mock: Shared,
    timers: Timers,
    refreshes: Senders,
    watches: Senders,
    captures: Captures,
    sink: Rc<RefCell<SinkState>>,
    devices: DeviceReplies,
    /// The adapter time of the next frame of [`Self::accept`].
    ticks: Cell<u64>,
}

fn setup(
    fixtures: &Rc<Fixtures>,
    with_artifacts: bool,
    with_devices: bool,
) -> (LocalFuture<()>, Harness) {
    let devices = DeviceReplies::default();
    let mock: Shared = Rc::new(RefCell::new(MockState {
        size: SIZE,
        ..MockState::default()
    }));
    let timers = Timers::default();
    let (refreshes, watches) = (Senders::default(), Senders::default());
    let captures = Rc::new(RefCell::new(CaptureState::default()));
    let sink = Rc::new(RefCell::new(SinkState::default()));
    let (lines_tx, lines) = mpsc::unbounded();
    let (events_tx, events) = mpsc::unbounded();
    let (out_tx, out) = mpsc::unbounded();
    let host = TestHost {
        driver: Rc::new(RefCell::new(Mock(Rc::clone(&mock)))),
        viewer: Mock(Rc::clone(&mock)),
        clock: Box::new(HeldClock(Rc::clone(&timers))),
        hooks: Box::new(HeldHooks {
            refreshes: Rc::clone(&refreshes),
            watches: Rc::clone(&watches),
            mock: Rc::clone(&mock),
        }),
        capture: Box::new(HeldCapture(Rc::clone(&captures))),
        artifacts: with_artifacts.then(|| Artifacts {
            sink: Box::new(HeldSink(Rc::clone(&sink))),
            environment: Box::new(DisplayEnvironment),
        }),
        devices: with_devices
            .then(|| Box::new(HeldDevices(Rc::downgrade(&devices))) as Box<dyn Devices>),
        fixtures: Rc::clone(fixtures),
        delay_ms: DEFAULT_DELAY_MS,
    };
    let host = Box::pin(async move {
        host.run(lines, events, move |e| {
            let _ = out_tx.unbounded_send(e.clone());
        })
        .await;
    });
    let harness = Harness {
        lines: lines_tx,
        events: events_tx,
        out,
        mock,
        timers,
        refreshes,
        watches,
        captures,
        sink,
        devices,
        ticks: Cell::new(10_000),
    };
    (host, harness)
}

impl Harness {
    fn line(&self, line: &str) {
        self.lines
            .unbounded_send(line.into())
            .expect("host running");
    }

    fn send(&self, request: &Request) {
        self.line(&serde_json::to_string(request).expect("serializable"));
    }

    fn event(&self, event: DriverEvent) {
        self.events.unbounded_send(event).expect("host running");
    }

    async fn next(&mut self) -> Event {
        self.out.next().await.expect("host output open")
    }

    async fn expect(&mut self, want: Event) {
        assert_eq!(self.next().await, want);
    }

    /// Requests are handled in order: no event comes between the earlier
    /// output and the reply to `displays`.
    async fn sync(&mut self) {
        self.send(&Request::Displays {});
        match self.next().await {
            Event::Displays { .. } => {}
            other => panic!("expected `displays`, got {other:?}"),
        }
    }

    /// Lets the host run until `done` holds.
    async fn until(&self, what: &str, done: impl Fn(&MockState) -> bool) {
        for _ in 0..1000 {
            if done(&self.mock.borrow()) {
                return;
            }
            YieldNow(false).await;
        }
        panic!("not reached: {what}");
    }

    fn live(&self) -> usize {
        self.mock.borrow().live
    }

    fn calls(&self) -> Vec<String> {
        self.mock.borrow().calls.clone()
    }

    /// Completes the `n`-th `present` call. `false`: its future is gone.
    fn reply(&self, n: usize, result: Result<String, OutputError>) -> bool {
        let tx = self.mock.borrow_mut().replies[n]
            .take()
            .expect("one reply per call");
        tx.send(result).is_ok()
    }

    fn timers(&self) -> Vec<u32> {
        self.timers.borrow().iter().map(|(ms, _)| *ms).collect()
    }

    /// Expires the `n`-th timer. `false`: it was cancelled.
    fn fire(&self, n: usize) -> bool {
        let tx = self.timers.borrow_mut()[n].1.take().expect("fired once");
        tx.send(()).is_ok()
    }

    async fn open(&mut self) -> u64 {
        self.send(&Request::Open {
            display: DISPLAY.into(),
            depth: 10,
        });
        let event = self.next().await;
        let Event::Surface { generation, .. } = &event else {
            panic!("expected `surface`, got {event:?}");
        };
        let size = self.mock.borrow().size;
        assert_eq!(event, open_surface(*generation, size));
        *generation
    }

    /// Sends `show` and waits for its `present` call.
    async fn show(&mut self, frame: u64, fixture: &str, delay_ms: Option<u32>) {
        self.show_with(frame, fixture, delay_ms, false).await;
    }

    async fn show_with(&mut self, frame: u64, fixture: &str, delay_ms: Option<u32>, marker: bool) {
        let before = self.mock.borrow().presented.len();
        self.send(&Request::Show {
            frame,
            fixture: fixture.into(),
            delay_ms,
            metadata: None,
            marker,
        });
        self.until("present", |m| m.presented.len() > before).await;
    }

    /// Lets the host run until `done` holds.
    async fn wait(&self, what: &str, done: impl Fn(&Self) -> bool) {
        for _ in 0..1000 {
            if done(self) {
                return;
            }
            YieldNow(false).await;
        }
        panic!("not reached: {what}");
    }

    /// Makes `start` succeed, with the adapter clock at `ms`.
    fn adapter(&self, ms: u64) {
        let mut c = self.captures.borrow_mut();
        c.available = true;
        c.now = Some(ms);
    }

    fn set_now(&self, ms: u64) {
        self.captures.borrow_mut().now = Some(ms);
    }

    /// The image of the `n`-th `present` call.
    fn image(&self, n: usize) -> Image {
        self.mock.borrow().presented[n].image.clone()
    }

    /// Sends `request` and waits for the session that it starts.
    async fn start_capture(&self, request: &Request) {
        let before = self.captures.borrow().sessions.len();
        self.send(request);
        self.wait("capture session", |h| {
            h.captures.borrow().sessions.len() > before
        })
        .await;
    }

    /// Delivers `frame` in the last session. `false`: the host dropped
    /// the session.
    fn deliver(&self, frame: Result<Capture, String>) -> bool {
        let c = self.captures.borrow();
        let tx = c
            .sessions
            .last()
            .and_then(Option::as_ref)
            .expect("a session");
        tx.unbounded_send(frame).is_ok()
    }

    /// Delivers the `n`-th presented image at `ms`.
    fn replay(&self, n: usize, ms: u64) -> bool {
        self.deliver(Ok(replay::frame(&self.image(n), replay::time(ms))))
    }

    /// Ends the last session from the adapter side.
    fn end_session(&self) {
        let tx = self
            .captures
            .borrow_mut()
            .sessions
            .last_mut()
            .and_then(Option::take);
        drop(tx);
    }

    fn sessions_live(&self) -> usize {
        self.captures.borrow().live
    }

    /// Captures `frame` and delivers three current frames of the `n`-th
    /// presented image. `captured` follows.
    async fn accept(&mut self, frame: u64, n: usize) {
        self.start_capture(&capture(frame)).await;
        for _ in 0..3 {
            let ms = self.ticks.get() + 10;
            self.ticks.set(ms);
            assert!(self.replay(n, ms));
        }
    }

    /// Completes the oldest write in progress.
    fn finish_write(&self, result: Result<(), String>) {
        let mut s = self.sink.borrow_mut();
        let n = s.finished;
        s.finished += 1;
        let saved = Saved {
            name: format!("artifact-{}", n + 1),
            frame: s.submitted[n].run.frame,
            result,
        };
        s.done.push_back(saved);
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }

    /// Completes the writes in progress, which have the names `names`.
    async fn finish_saved(&mut self, names: &[&str]) {
        for name in names {
            let frame = {
                let s = self.sink.borrow();
                s.submitted[s.finished].run.frame
            };
            self.finish_write(Ok(()));
            self.expect(Event::Saved {
                frame,
                artifact: (*name).into(),
            })
            .await;
        }
    }

    /// Shows `frame`, acknowledges it and checks `submitted`.
    async fn acknowledged(&mut self, frame: u64, generation: u64, delay_ms: u32) {
        self.acknowledge(frame, FRAME_A, generation, delay_ms, false)
            .await;
    }

    /// [`Self::acknowledged`] of `fixture` with the frame marker.
    async fn marked(&mut self, frame: u64, fixture: &str, generation: u64, delay_ms: u32) {
        self.acknowledge(frame, fixture, generation, delay_ms, true)
            .await;
    }

    async fn acknowledge(
        &mut self,
        frame: u64,
        fixture: &str,
        generation: u64,
        delay_ms: u32,
        marker: bool,
    ) {
        let n = self.mock.borrow().presented.len();
        self.show_with(frame, fixture, Some(delay_ms), marker).await;
        assert!(self.reply(n, Ok(format!("ack {frame}"))));
        self.expect(submitted(frame, generation, delay_ms)).await;
    }
}

/// `capture` with the default deadline and no heartbeat.
fn capture(frame: u64) -> Request {
    Request::Capture {
        frame,
        heartbeat_ms: None,
        deadline_ms: None,
    }
}

fn open_surface(generation: u64, size: (usize, usize)) -> Event {
    Event::Surface {
        generation,
        state: SurfaceState::Open,
        display: DISPLAY.into(),
        size: Some([size.0, size.1]),
        description: Some("mock surface".into()),
        cause: None,
    }
}

fn surface(generation: u64, state: SurfaceState, cause: Cause) -> Event {
    Event::Surface {
        generation,
        state,
        display: DISPLAY.into(),
        size: None,
        description: None,
        cause: Some(cause),
    }
}

fn submitted(frame: u64, generation: u64, delay_ms: u32) -> Event {
    Event::Submitted {
        frame,
        generation,
        delay_ms,
        status: format!("ack {frame}"),
    }
}

fn error_kind(event: &Event) -> (Option<u64>, ErrorKind) {
    match event {
        Event::Error { frame, kind, .. } => (*frame, *kind),
        other => panic!("expected `error`, got {other:?}"),
    }
}

async fn reports_displays_and_each_support_state(mut h: Harness) {
    h.send(&Request::Displays {});
    h.expect(Event::Displays {
        displays: vec![DisplayEntry {
            id: DISPLAY.into(),
            name: "Mock screen".into(),
            size: [SIZE.0, SIZE.1],
        }],
    })
    .await;
    h.send(&Request::Query {
        display: DISPLAY.into(),
    });
    let Event::Query {
        display,
        capabilities,
        mode,
    } = h.next().await
    else {
        panic!("expected `query`");
    };
    assert_eq!(display, DISPLAY);
    let states: Vec<_> = capabilities
        .depths
        .iter()
        .map(|d| (d.depth, d.available.state, d.exact.state))
        .collect();
    assert_eq!(
        states,
        [
            (10, SupportState::Yes, SupportState::Unknown),
            (12, SupportState::No, SupportState::Unknown)
        ]
    );
    assert_eq!(
        capabilities.depths[1].available.reason.as_deref(),
        Some("mock has no 12-bit")
    );
    assert_eq!(capabilities.encodings[2].encoding, "hlg-narrow");
    assert_eq!(capabilities.encodings[2].input.state, SupportState::No);
    assert_eq!(mode.hdr_on, Some(true));
    assert_eq!(mode.description, "mock mode");

    h.send(&Request::Query {
        display: "mock:other".into(),
    });
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::UnknownDisplay)
    );
}

async fn open_rejects_unavailable_depths(mut h: Harness) {
    for (depth, kind) in [(12, ErrorKind::Unsupported), (8, ErrorKind::Malformed)] {
        h.send(&Request::Open {
            display: DISPLAY.into(),
            depth,
        });
        assert_eq!(error_kind(&h.next().await), (None, kind));
    }
    h.send(&Request::Open {
        display: "mock:other".into(),
        depth: 10,
    });
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::UnknownDisplay)
    );
    assert_eq!(h.open().await, 1);
    // A new `open` ends the earlier generation first.
    h.send(&Request::Open {
        display: DISPLAY.into(),
        depth: 12,
    });
    h.expect(surface(1, SurfaceState::Closed, Cause::Opened))
        .await;
    assert_eq!(error_kind(&h.next().await), (None, ErrorKind::Unsupported));
    assert_eq!(h.open().await, 2);
    assert_eq!(
        h.calls(),
        [
            "open 12-bit",
            "open 10-bit",
            "open 12-bit",
            "close",
            "open 10-bit"
        ]
    );
}

async fn show_rejects_invalid_requests_without_presenting(mut h: Harness) {
    let show = |frame, fixture: &str, metadata| Request::Show {
        frame,
        fixture: fixture.into(),
        delay_ms: None,
        metadata,
        marker: false,
    };
    h.send(&show(1, FRAME_A, None));
    assert_eq!(error_kind(&h.next().await), (Some(1), ErrorKind::NoSurface));
    h.open().await;
    let invalid = MetadataRequest {
        primaries: [[0.708, 0.292], [0.17, 0.797], [0.131, 0.046]],
        white: [0.3127, 0.329],
        max_mastering_nits: 100.0,
        min_mastering_nits: 1000.0,
        light_level: LightLevelRequest::FromFrame,
    };
    for (request, kind) in [
        (show(2, "no-such-case", None), ErrorKind::UnknownFixture),
        (show(3, TOO_LARGE, None), ErrorKind::FixtureTooLarge),
        (
            show(4, FRAME_A, Some(Some(invalid))),
            ErrorKind::InvalidMetadata,
        ),
    ] {
        let frame = match &request {
            Request::Show { frame, .. } => *frame,
            _ => unreachable!(),
        };
        h.send(&request);
        assert_eq!(error_kind(&h.next().await), (Some(frame), kind));
    }
    // HLG input is `No` for the mock: the driver rejects it.
    h.send(&show(5, "codes-endpoints-hlg-narrow-10", None));
    assert_eq!(
        error_kind(&h.next().await),
        (Some(5), ErrorKind::Unsupported)
    );
    assert_eq!(h.mock.borrow().presented.len(), 0);
}

async fn frame_ids_must_increase(mut h: Harness) {
    h.open().await;
    h.show(5, FRAME_A, Some(0)).await;
    for frame in [5, 4] {
        h.send(&Request::Show {
            frame,
            fixture: FRAME_B.into(),
            delay_ms: None,
            metadata: None,
            marker: false,
        });
        assert_eq!(
            error_kind(&h.next().await),
            (Some(frame), ErrorKind::FrameOrder)
        );
    }
    // The rejected requests keep the pending frame.
    assert_eq!(h.live(), 1);
    assert!(h.reply(0, Ok("ack 5".into())));
    h.expect(submitted(5, 1, 0)).await;
    h.expect(Event::Ready {
        frame: 5,
        delay_ms: 0,
    })
    .await;
}

async fn renders_the_fixture_at_the_surface_size(mut h: Harness) {
    let fixtures = Fixtures::load().expect("output fixtures");
    h.open().await;
    for (frame, id) in [(1, FRAME_A), (2, WITH_METADATA)] {
        h.show(frame, id, Some(0)).await;
        let fx = fixtures.get(id).expect("fixture");
        let state = h.mock.borrow();
        let presented = state.presented.last().expect("presented");
        assert_eq!(presented.format, fx.format);
        let image = &presented.image;
        assert_eq!((image.width(), image.height()), SIZE);
        let expected = fx.render(SIZE).expect("fits");
        assert_eq!(*image, expected);
        // The area outside the fixture is nominal black.
        let black = Rgb::grey(fx.format.black());
        assert_eq!(image.get(SIZE.0 - 1, SIZE.1 - 1), black);
        assert_eq!(image.get(fx.size.0, 0), black);
    }
}

async fn readiness_follows_acknowledgement_and_delay(mut h: Harness) {
    let generation = h.open().await;
    h.show(1, FRAME_A, Some(1500)).await;
    // The host keeps handling requests while the frame is pending.
    h.sync().await;
    assert!(h.reply(0, Ok("ack 1".into())));
    h.expect(submitted(1, generation, 1500)).await;
    assert_eq!(h.timers(), [1500]);
    h.sync().await;
    assert!(h.fire(0));
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 1500,
    })
    .await;

    // Without `delay_ms`: the host default.
    h.show(2, FRAME_B, None).await;
    assert!(h.reply(1, Ok("ack 2".into())));
    h.expect(submitted(2, generation, DEFAULT_DELAY_MS)).await;
    assert_eq!(h.timers(), [1500, DEFAULT_DELAY_MS]);
}

async fn zero_delay_is_ready_at_once(mut h: Harness) {
    h.open().await;
    h.acknowledged(1, 1, 0).await;
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 0,
    })
    .await;
    assert_eq!(h.timers(), Vec::<u32>::new());
}

async fn failed_presentation_starts_no_timer(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(1000)).await;
    assert!(h.reply(0, Err(OutputError::Timeout)));
    h.expect(Event::Error {
        frame: Some(1),
        kind: ErrorKind::Timeout,
        message: "driver acknowledgement timed out".into(),
        device_error: None,
    })
    .await;
    h.sync().await;
    assert_eq!(h.timers(), Vec::<u32>::new());
    assert_eq!(h.live(), 0);
}

async fn stopped_timer_is_an_error(mut h: Harness) {
    h.open().await;
    h.acknowledged(1, 1, 1000).await;
    // Drops the sender.
    h.timers.borrow_mut()[0].1 = None;
    h.expect(Event::Error {
        frame: Some(1),
        kind: ErrorKind::Timer,
        message: "the mock timer stopped".into(),
        device_error: None,
    })
    .await;
}

async fn replacement_cancels_the_presentation(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(0)).await;
    h.show(2, FRAME_B, Some(0)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Replaced,
    })
    .await;
    // The old future is gone: its late result goes nowhere.
    assert!(!h.reply(0, Ok("ack 1".into())));
    assert_eq!(h.live(), 1);
    assert!(h.reply(1, Ok("ack 2".into())));
    h.expect(submitted(2, 1, 0)).await;
    h.expect(Event::Ready {
        frame: 2,
        delay_ms: 0,
    })
    .await;
}

async fn replacement_cancels_the_delay(mut h: Harness) {
    h.open().await;
    h.acknowledged(1, 1, 1000).await;
    h.show(2, FRAME_B, Some(2000)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Replaced,
    })
    .await;
    assert!(!h.fire(0), "the old timer is dropped");
    assert!(h.reply(1, Ok("ack 2".into())));
    h.expect(submitted(2, 1, 2000)).await;
    assert!(h.fire(1));
    h.expect(Event::Ready {
        frame: 2,
        delay_ms: 2000,
    })
    .await;
}

async fn malformed_input_is_reported(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(0)).await;
    for line in [
        "not json",
        "[]",
        r#"{"request":"bogus"}"#,
        r#"{"request":"show","frame":2}"#,
        r#"{"request":"show","frame":-1,"fixture":"x"}"#,
        r#"{"request":"open","display":"mock:screen-1","depth":10,"extra":1}"#,
    ] {
        h.line(line);
        assert_eq!(
            error_kind(&h.next().await),
            (None, ErrorKind::Malformed),
            "{line}"
        );
    }
    // A blank line is no request.
    h.line("  ");
    h.sync().await;
    // Malformed input keeps the pending frame.
    assert!(h.reply(0, Ok("ack 1".into())));
    h.expect(submitted(1, 1, 0)).await;
}

async fn close_cancels_and_closes(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(0)).await;
    h.send(&Request::Close {});
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Closed,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Closed))
        .await;
    assert_eq!(h.calls().last().map(String::as_str), Some("close"));
    h.send(&Request::Close {});
    assert_eq!(error_kind(&h.next().await), (None, ErrorKind::NoSurface));
    h.send(&Request::Show {
        frame: 2,
        fixture: FRAME_B.into(),
        delay_ms: None,
        metadata: None,
        marker: false,
    });
    assert_eq!(error_kind(&h.next().await), (Some(2), ErrorKind::NoSurface));
}

fn disconnected() -> DriverEvent {
    DriverEvent::Disconnected {
        display: None,
        reason: "gone".into(),
    }
}

fn disconnected_display() -> Event {
    Event::Display {
        state: DisplayState::Disconnected,
        display: None,
        reason: Some("gone".into()),
    }
}

fn frame_disconnected(frame: u64) -> Event {
    Event::Error {
        frame: Some(frame),
        kind: ErrorKind::Disconnected,
        message: "display disconnected".into(),
        device_error: None,
    }
}

async fn disconnect_fails_the_presentation(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(0)).await;
    h.event(disconnected());
    h.expect(disconnected_display()).await;
    h.expect(frame_disconnected(1)).await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Disconnected))
        .await;
    assert_eq!(h.live(), 0);
}

/// The other order of the driver result and the event: the frame gets the
/// same `error` event.
async fn disconnect_after_the_driver_result_gives_the_same_error(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(0)).await;
    assert!(h.reply(0, Err(OutputError::Disconnected)));
    h.expect(frame_disconnected(1)).await;
    h.event(disconnected());
    h.expect(disconnected_display()).await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Disconnected))
        .await;
}

async fn disconnect_cancels_the_delay(mut h: Harness) {
    h.open().await;
    h.acknowledged(1, 1, 1000).await;
    h.event(disconnected());
    h.expect(disconnected_display()).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Disconnected,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Disconnected))
        .await;
    assert!(!h.fire(0));
}

fn changed() -> DriverEvent {
    DriverEvent::Changed {
        display: display_id(),
        error: OutputError::Platform("mock viewport changed".into()),
    }
}

fn changed_display() -> Event {
    Event::Display {
        state: DisplayState::Changed,
        display: Some(DISPLAY.into()),
        reason: None,
    }
}

async fn display_change_fails_the_presentation_and_needs_open(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(0)).await;
    h.mock.borrow_mut().size = (80, 50);
    h.event(changed());
    h.expect(changed_display()).await;
    h.expect(Event::Error {
        frame: Some(1),
        kind: ErrorKind::Platform,
        message: "platform error: mock viewport changed".into(),
        device_error: None,
    })
    .await;
    h.expect(surface(1, SurfaceState::Stale, Cause::Changed))
        .await;
    h.send(&Request::Show {
        frame: 2,
        fixture: FRAME_B.into(),
        delay_ms: None,
        metadata: None,
        marker: false,
    });
    assert_eq!(
        error_kind(&h.next().await),
        (Some(2), ErrorKind::StaleSurface)
    );
    h.send(&Request::Open {
        display: DISPLAY.into(),
        depth: 10,
    });
    h.expect(surface(1, SurfaceState::Closed, Cause::Opened))
        .await;
    h.expect(open_surface(2, (80, 50))).await;
    h.show(3, FRAME_B, Some(0)).await;
    let state = h.mock.borrow();
    let image = &state.presented.last().expect("presented").image;
    assert_eq!((image.width(), image.height()), (80, 50));
}

async fn display_change_cancels_the_delay(mut h: Harness) {
    h.open().await;
    h.acknowledged(1, 1, 1000).await;
    h.event(changed());
    h.expect(changed_display()).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Changed,
    })
    .await;
    h.expect(surface(1, SurfaceState::Stale, Cause::Changed))
        .await;
    assert!(!h.fire(0));
}

fn close_outside(h: &Harness) {
    let tx = h.watches.borrow_mut().pop().expect("watched surface");
    tx.send(()).expect("watch alive");
}

async fn external_close_fails_the_presentation(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(0)).await;
    close_outside(&h);
    h.expect(frame_disconnected(1)).await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Disconnected))
        .await;
}

async fn external_close_cancels_the_delay(mut h: Harness) {
    h.open().await;
    h.acknowledged(1, 1, 1000).await;
    close_outside(&h);
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Disconnected,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Disconnected))
        .await;
    assert!(!h.fire(0));
}

fn captured(
    frame: u64,
    result: CaptureOutcome,
    reason: Option<&str>,
    artifact: Option<&str>,
) -> Event {
    Event::Captured {
        frame,
        result,
        reason: reason.map(Into::into),
        artifact: artifact.map(Into::into),
        cause: None,
    }
}

async fn refresh_lists_displays_after_the_read(mut h: Harness) {
    h.send(&Request::Refresh {});
    h.send(&Request::Query {
        display: DISPLAY.into(),
    });
    // The query reply comes before the end of the read.
    assert!(matches!(h.next().await, Event::Query { .. }));
    let tx = h.refreshes.borrow_mut().remove(0);
    tx.send(()).expect("refresh pending");
    assert!(matches!(h.next().await, Event::Displays { .. }));
}

async fn metadata_follows_the_request(mut h: Harness) {
    let fixtures = Fixtures::load().expect("output fixtures");
    let own = fixtures
        .get(WITH_METADATA)
        .and_then(|f| f.metadata.clone())
        .expect("fixture metadata");
    let explicit = MetadataRequest {
        light_level: LightLevelRequest::Explicit {
            max_cll: 800.0,
            max_fall: 200.0,
        },
        ..own.clone()
    };
    h.open().await;
    for (frame, metadata, want) in [
        (1, None, Some(hdr10_request(&own))),
        (2, Some(None), None),
        (
            3,
            Some(Some(explicit.clone())),
            Some(hdr10_request(&explicit)),
        ),
    ] {
        h.send(&Request::Show {
            frame,
            fixture: WITH_METADATA.into(),
            delay_ms: Some(0),
            metadata,
            marker: false,
        });
        let n = usize::try_from(frame).unwrap();
        h.until("present", |m| m.presented.len() == n).await;
        assert_eq!(h.mock.borrow().presented[n - 1].metadata, want);
    }
}

async fn driver_notices_pass_through(mut h: Harness) {
    let notice = Event::Listening {
        url: "http://127.0.0.1:1/".into(),
    };
    h.event(DriverEvent::Notice(notice.clone()));
    h.expect(notice).await;
    h.event(DriverEvent::Ready(display_id()));
    h.expect(Event::Display {
        state: DisplayState::Ready,
        display: Some(DISPLAY.into()),
        reason: None,
    })
    .await;
}

async fn quit_ends_pending_work(mut h: Harness) {
    h.open().await;
    h.show(1, FRAME_A, Some(0)).await;
    h.send(&Request::Quit {});
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Stopped,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Stopped))
        .await;
    h.expect(Event::Stopped { error: None }).await;
    assert!(h.out.next().await.is_none(), "`stopped` is the last event");
    assert_eq!(h.calls().last().map(String::as_str), Some("close"));
}

async fn end_of_input_is_quit(mut h: Harness) {
    h.open().await;
    h.acknowledged(1, 1, 1000).await;
    h.lines.close_channel();
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Stopped,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Stopped))
        .await;
    h.expect(Event::Stopped { error: None }).await;
}

async fn driver_stop_ends_the_host(mut h: Harness) {
    h.event(DriverEvent::Stopped {
        error: Some("mock failure".into()),
    });
    h.expect(Event::Stopped {
        error: Some("mock failure".into()),
    })
    .await;
    assert!(h.out.next().await.is_none());
}
