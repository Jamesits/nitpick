use std::rc::Rc;
use std::time::Duration;

use nitpick_core::driver::{
    Capabilities, Conversion, DepthSupport, DisplayInfo, EncodingSupport, ModeSettings,
    PresentFuture, Query, Support, check_open,
};
use nitpick_core::signal::{BitDepth, Encoding};
use tokio::sync::oneshot;

use super::*;

const SETTINGS: Settings = Settings {
    pattern: PatternKind::ColourBars,
    format: Format::new(Encoding::PqNarrow, BitDepth::Ten),
    delay_ms: 0,
};

/// State shared by the presenting driver, the viewer and the test.
#[derive(Default)]
struct Log {
    displays: Vec<DisplayInfo>,
    calls: Vec<String>,
    replies: Vec<oneshot::Sender<Result<String, OutputError>>>,
    /// `present` futures not yet dropped.
    live: usize,
}

type Shared = Rc<RefCell<Log>>;

fn client(size: (usize, usize)) -> DisplayInfo {
    DisplayInfo {
        id: id(),
        name: "Mock client".into(),
        size,
    }
}

fn id() -> DisplayId {
    DisplayId {
        driver: "mock",
        key: "client-1".into(),
    }
}

fn shared(size: (usize, usize)) -> Shared {
    Rc::new(RefCell::new(Log {
        displays: vec![client(size)],
        ..Log::default()
    }))
}

/// 10-bit with unknown exactness; 12-bit unavailable; HLG rejected.
fn query() -> Query {
    let input = |input| EncodingSupport {
        input,
        interpretation: Support::Yes,
        auto_light_level: Support::Yes,
    };
    Query {
        capabilities: Capabilities {
            depths: vec![
                (
                    BitDepth::Ten,
                    DepthSupport {
                        available: Support::Yes,
                        path: "mock PNG".into(),
                        conversion: Conversion::Png16,
                        exact: Support::Unknown("mock exactness".into()),
                    },
                ),
                (
                    BitDepth::Twelve,
                    DepthSupport {
                        available: Support::No("mock has no 12-bit".into()),
                        path: "mock PNG".into(),
                        conversion: Conversion::Png16,
                        exact: Support::Unknown("mock exactness".into()),
                    },
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
            hdr_metadata: Support::Unknown("mock metadata".into()),
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

/// Counts a `present` future from creation to drop.
struct Live(Shared);

impl Live {
    fn new(log: &Shared) -> Self {
        log.borrow_mut().live += 1;
        Self(Rc::clone(log))
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        self.0.borrow_mut().live -= 1;
    }
}

struct Mock(Shared);

impl Mock {
    fn find(&self, display: &DisplayId) -> Result<DisplayInfo, OutputError> {
        let log = self.0.borrow();
        log.displays
            .iter()
            .find(|d| d.id == *display)
            .cloned()
            .ok_or(OutputError::Disconnected)
    }

    fn call(&self, text: String) {
        self.0.borrow_mut().calls.push(text);
    }
}

impl OutputDriver for Mock {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn displays(&mut self) -> Vec<DisplayInfo> {
        self.0.borrow().displays.clone()
    }

    fn query(&mut self, display: &DisplayId) -> Result<Query, OutputError> {
        self.call(format!("query {display}"));
        self.find(display).map(|_| query())
    }

    fn open(&mut self, display: &DisplayId, depth: BitDepth) -> Result<Surface, OutputError> {
        self.call(format!("open {display} {depth}"));
        let info = self.find(display)?;
        check_open(&query().capabilities, depth)?;
        Ok(Surface {
            display: display.clone(),
            size: info.size,
            description: "mock surface".into(),
        })
    }

    fn present<'a>(&'a mut self, _: &'a Surface, frame: &'a Frame<'a>) -> PresentFuture<'a> {
        self.call(format!(
            "present {}x{}",
            frame.image.width(),
            frame.image.height()
        ));
        let (tx, rx) = oneshot::channel();
        self.0.borrow_mut().replies.push(tx);
        let live = Live::new(&self.0);
        Box::pin(async move {
            let _live = live;
            rx.await.unwrap_or(Err(OutputError::Disconnected))
        })
    }

    fn close(&mut self, display: &DisplayId) {
        assert_eq!(
            self.0.borrow().live,
            0,
            "close while a present future is alive"
        );
        self.call(format!("close {display}"));
    }
}

struct Harness {
    log: Shared,
    events: mpsc::UnboundedSender<ServiceEvent>,
    console: mpsc::UnboundedSender<String>,
    out: mpsc::UnboundedReceiver<String>,
    /// Every output line read so far.
    seen: Vec<String>,
}

impl Harness {
    fn event(&self, event: ServiceEvent) {
        self.events.send(event).expect("host running");
    }

    fn ready(&self) {
        self.event(ServiceEvent::Client(ClientState::Ready(id())));
    }

    fn type_line(&self, line: &str) {
        self.console.send(line.into()).expect("host running");
    }

    /// The next output line that starts with `prefix`. Skips other lines.
    /// On a paused clock, the timeout lets earlier host timers expire
    /// first.
    async fn expect(&mut self, prefix: &str) -> String {
        let wait = async {
            loop {
                let line = self.out.recv().await.expect("host output open");
                self.seen.push(line.clone());
                if line.starts_with(prefix) {
                    return line;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .unwrap_or_else(|_| panic!("no output line starting with `{prefix}`"))
    }

    /// Advances the paused clock by `ms` and gives the output lines of
    /// that interval.
    async fn advance(&mut self, ms: u64) -> Vec<String> {
        tokio::time::advance(Duration::from_millis(ms)).await;
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
        let mut lines = Vec::new();
        while let Ok(line) = self.out.try_recv() {
            self.seen.push(line.clone());
            lines.push(line);
        }
        lines
    }

    fn saw(&self, prefix: &str) -> bool {
        self.seen.iter().any(|l| l.starts_with(prefix))
    }

    /// Lets the host run until `done` holds.
    async fn until(&self, done: impl Fn(&Log) -> bool) {
        for _ in 0..1000 {
            if done(&self.log.borrow()) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("condition not reached");
    }

    /// Completes the `n`-th `present` call.
    async fn reply(&self, n: usize, result: Result<String, OutputError>) {
        self.until(|log| log.replies.len() > n).await;
        let tx = self.log.borrow_mut().replies.remove(n);
        assert!(tx.send(result).is_ok(), "frame {n} was cancelled");
        // Keeps the indices of later frames.
        let (placeholder, _) = oneshot::channel();
        self.log.borrow_mut().replies.insert(n, placeholder);
    }

    fn calls(&self) -> Vec<String> {
        self.log.borrow().calls.clone()
    }

    fn clear_calls(&self) {
        self.log.borrow_mut().calls.clear();
    }

    /// Selects the client and starts a frame; waits for the `present` call.
    async fn show(&mut self) {
        self.type_line("select mock:client-1");
        self.expect("selected: mock:client-1").await;
        let before = self.log.borrow().replies.len();
        self.type_line("show");
        self.expect("status: submitting pattern").await;
        self.until(|log| log.replies.len() > before && log.live == 1)
            .await;
    }
}

/// Runs the host against `script`. The console closes when the script
/// returns.
async fn session(log: Shared, script: impl AsyncFnOnce(&mut Harness)) -> (Exit, Harness) {
    session_with(log, SETTINGS, script).await
}

async fn session_with(
    log: Shared,
    settings: Settings,
    script: impl AsyncFnOnce(&mut Harness),
) -> (Exit, Harness) {
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (console_tx, mut console) = mpsc::unbounded_channel();
    let (out_tx, out) = mpsc::unbounded_channel();
    let mut harness = Harness {
        log: Rc::clone(&log),
        events: events_tx,
        console: console_tx,
        out,
        seen: Vec::new(),
    };
    let driver = RefCell::new(Mock(Rc::clone(&log)));
    let host = run(
        &driver,
        Mock(Rc::clone(&log)),
        &mut events,
        &mut console,
        settings,
        out_tx,
    );
    let script = async {
        script(&mut harness).await;
        harness.console = mpsc::unbounded_channel().0;
    };
    let (exit, ()) = tokio::join!(host, script);
    (exit, harness)
}

#[tokio::test]
async fn startup_prints_service_and_client_state() {
    let (exit, h) = session(shared((1280, 720)), async |h| {
        h.event(ServiceEvent::Listening {
            url: "http://127.0.0.1:1/".into(),
        });
        h.event(ServiceEvent::PairingCode("0042".into()));
        h.event(ServiceEvent::Client(ClientState::Connected));
        h.event(ServiceEvent::Client(ClientState::Authenticated));
        h.ready();
        h.expect("url: http://127.0.0.1:1/").await;
        h.expect("pairing code: 0042").await;
        h.expect("client: connected").await;
        h.expect("client: authenticated").await;
        let ready = h.expect("client: ready: ").await;
        assert_eq!(
            ready,
            "client: ready: mock:client-1 (Mock client, 1280×720 px): mock mode"
        );
        h.type_line("displays");
        h.expect("display: mock:client-1 (Mock client, 1280×720 px)")
            .await;
    })
    .await;
    assert_eq!(exit, Exit::Quit);
    assert!(h.calls().iter().all(|c| c.starts_with("query")));
}

#[tokio::test]
async fn selects_by_display_id_and_renders_at_the_surface_size() {
    let (exit, h) = session(shared((1280, 720)), async |h| {
        h.type_line("show");
        h.expect("error: no client selected").await;
        h.type_line("select mock:client-9");
        h.expect("error: no client mock:client-9").await;
        h.clear_calls();
        h.show().await;
        assert_eq!(
            h.calls(),
            [
                "query mock:client-1",
                "open mock:client-1 10-bit",
                "present 1280x720"
            ]
        );
        h.reply(0, Ok("frame 1 applied".into())).await;
        h.expect("status: ready after delay: 0 ms. frame 1 applied")
            .await;
        // The current frame stays until replacement or exit.
        assert!(!h.calls().iter().any(|c| c.starts_with("close")));
    })
    .await;
    assert_eq!(exit, Exit::Quit);
    assert_eq!(h.calls().last().unwrap(), "close mock:client-1");
}

#[tokio::test]
async fn prints_output_limits_before_the_frame() {
    session(shared((1280, 720)), async |h| {
        h.type_line("select mock:client-1");
        h.type_line("show");
        h.expect("output: mock PNG").await;
        h.expect("warning: 10-bit (mock PNG) exact codes: unknown (mock exactness)")
            .await;
        h.expect("surface: mock surface").await;
    })
    .await;
}

#[tokio::test]
async fn rejects_unsupported_requests() {
    let (_, h) = session(shared((1280, 720)), async |h| {
        h.type_line("select mock:client-1");
        h.type_line("format pq-narrow 12");
        h.type_line("show");
        h.expect("error: 12-bit not supported: mock has no 12-bit")
            .await;
        h.type_line("format hlg-narrow 10");
        h.type_line("show");
        h.expect("error: HLG").await;
        h.type_line("pattern pq-grayscale");
        h.type_line("show");
        h.expect("error: PQ grayscale / brightness does not support HLG")
            .await;
        h.type_line("frobnicate");
        h.expect("error: unknown command `frobnicate`").await;
    })
    .await;
    assert!(!h.calls().iter().any(|c| c.starts_with("open")));
}

#[tokio::test]
async fn rejects_a_surface_below_the_pattern_minimum() {
    let (_, h) = session(shared((320, 200)), async |h| {
        h.type_line("select mock:client-1");
        h.type_line("show");
        h.expect("error: output 320x200 is smaller than 640x360")
            .await;
    })
    .await;
    assert!(!h.calls().iter().any(|c| c.starts_with("present")));
}

#[tokio::test]
async fn reports_a_failed_presentation() {
    session(shared((1280, 720)), async |h| {
        h.show().await;
        h.reply(0, Err(OutputError::Timeout)).await;
        h.expect("error: presentation failed: driver acknowledgement timed out")
            .await;
    })
    .await;
}

#[tokio::test]
async fn replacement_cancels_the_pending_frame() {
    let (_, h) = session(shared((1280, 720)), async |h| {
        h.show().await;
        h.clear_calls();
        h.type_line("show");
        h.expect("status: presentation cancelled").await;
        h.expect("status: submitting pattern").await;
        h.until(|log| log.replies.len() == 2 && log.live == 1).await;
        // The first future is dropped: its reply has no receiver.
        let first = h.log.borrow_mut().replies.remove(0);
        assert!(first.send(Ok("late".into())).is_err());
        h.reply(0, Ok("second".into())).await;
        h.expect("status: ready after delay: 0 ms. second").await;
        assert_eq!(
            h.calls(),
            [
                "query mock:client-1",
                "open mock:client-1 10-bit",
                "present 1280x720"
            ]
        );
    })
    .await;
    assert_eq!(h.log.borrow().live, 0);
}

#[tokio::test]
async fn handles_console_and_events_while_pending() {
    session(shared((1280, 720)), async |h| {
        h.show().await;
        h.event(ServiceEvent::PairingCode("1234".into()));
        h.expect("pairing code: 1234").await;
        h.type_line("displays");
        h.expect("display: mock:client-1").await;
        // A `Ready` without a geometry change keeps the frame.
        h.ready();
        h.expect("client: ready: mock:client-1").await;
        h.type_line("table");
        h.expect("BT.2111-3 HDR colour bars").await;
        assert_eq!(h.log.borrow().live, 1);
        h.reply(0, Ok("applied".into())).await;
        h.expect("status: ready after delay: 0 ms. applied").await;
    })
    .await;
}

#[tokio::test]
async fn viewport_change_reopens_at_the_new_size() {
    let (_, h) = session(shared((1280, 720)), async |h| {
        h.show().await;
        h.log.borrow_mut().displays = vec![client((1920, 1080))];
        h.event(ServiceEvent::Client(ClientState::ViewportChanged(id())));
        h.expect("status: presentation cancelled").await;
        assert_eq!(h.log.borrow().live, 0);
        h.clear_calls();
        h.ready();
        h.expect("client: ready: mock:client-1 (Mock client, 1920×1080 px)")
            .await;
        h.expect("status: submitting pattern").await;
        h.until(|log| log.live == 1).await;
        assert_eq!(
            h.calls()[1..],
            [
                "query mock:client-1",
                "open mock:client-1 10-bit",
                "present 1920x1080"
            ]
        );
    })
    .await;
    assert_eq!(h.calls().last().unwrap(), "close mock:client-1");
}

#[tokio::test]
async fn disconnect_cancels_and_closes() {
    session(shared((1280, 720)), async |h| {
        h.show().await;
        h.event(ServiceEvent::Client(ClientState::Disconnected(
            "gone".into(),
        )));
        h.expect("client: disconnected: gone").await;
        h.expect("status: presentation cancelled").await;
        h.until(|log| log.calls.last().is_some_and(|c| c == "close mock:client-1"))
            .await;
        h.type_line("show");
        h.expect("error: no client selected").await;
    })
    .await;
}

#[tokio::test]
async fn close_command_releases_the_frame_first() {
    let (_, h) = session(shared((1280, 720)), async |h| {
        h.show().await;
        h.type_line("close");
        h.expect("status: presentation cancelled").await;
        h.expect("status: surface closed").await;
    })
    .await;
    // Exit closes nothing more.
    let closes = h.calls().iter().filter(|c| c.starts_with("close")).count();
    assert_eq!(closes, 1);
}

#[tokio::test]
async fn quit_drops_the_pending_frame_before_close() {
    let (exit, h) = session(shared((1280, 720)), async |h| {
        h.show().await;
        h.type_line("quit");
        h.expect("status: presentation cancelled").await;
    })
    .await;
    assert_eq!(exit, Exit::Quit);
    assert_eq!(h.log.borrow().live, 0);
    assert_eq!(h.calls().last().unwrap(), "close mock:client-1");
}

#[tokio::test]
async fn service_stop_ends_the_session() {
    let (exit, h) = session(shared((1280, 720)), async |h| {
        h.show().await;
        h.event(ServiceEvent::Stopped {
            error: Some("boom".into()),
        });
        h.expect("service stopped: boom").await;
    })
    .await;
    assert_eq!(exit, Exit::Stopped(Some("boom".into())));
    assert_eq!(h.calls().last().unwrap(), "close mock:client-1");
}

const DELAYED: Settings = Settings {
    delay_ms: 1000,
    ..SETTINGS
};

/// Shows a frame and acknowledges it; the delay of `settings` starts.
async fn acknowledged(h: &mut Harness, n: usize, status: &str) {
    h.show().await;
    h.reply(n, Ok(status.into())).await;
}

#[tokio::test(start_paused = true)]
async fn readiness_follows_the_delay_with_the_driver_status() {
    session_with(shared((1280, 720)), DELAYED, async |h| {
        acknowledged(h, 0, "frame 1 applied. Warnings: mock warning.").await;
        let start = tokio::time::Instant::now();
        h.expect("status: waiting for measurement delay: 1000 ms. frame 1 applied. Warnings: mock warning.")
            .await;
        assert!(!h.saw("status: ready"));
        // The host keeps processing the console during the delay.
        h.type_line("displays");
        h.expect("display: mock:client-1").await;
        assert!(h.advance(999).await.iter().all(|l| !l.starts_with("status:")));
        h.expect("status: ready after delay: 1000 ms. frame 1 applied. Warnings: mock warning.")
            .await;
        assert_eq!(start.elapsed(), Duration::from_millis(1000));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn zero_delay_reports_readiness_at_once() {
    session(shared((1280, 720)), async |h| {
        acknowledged(h, 0, "applied").await;
        let start = tokio::time::Instant::now();
        h.expect("status: ready after delay: 0 ms. applied").await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert!(!h.saw("status: waiting"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn delay_command_sets_the_delay_of_later_frames() {
    session(shared((1280, 720)), async |h| {
        h.show().await;
        h.type_line("delay -1");
        h.expect("error: the measurement delay must be an integer >= 0 ms, not `-1`")
            .await;
        h.type_line("delay 250");
        h.expect("delay: 250 ms").await;
        // The pending frame keeps the delay of its `show`.
        h.reply(0, Ok("first".into())).await;
        h.expect("status: ready after delay: 0 ms. first").await;
        acknowledged(h, 1, "second").await;
        h.expect("status: waiting for measurement delay: 250 ms. second")
            .await;
        h.expect("status: ready after delay: 250 ms. second").await;
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn failed_presentation_starts_no_timer() {
    session_with(shared((1280, 720)), DELAYED, async |h| {
        h.show().await;
        h.reply(0, Err(OutputError::Disconnected)).await;
        h.expect("error: presentation failed").await;
        h.advance(5000).await;
        assert!(!h.saw("status: waiting"));
        assert!(!h.saw("status: ready"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn replacement_during_the_delay_starts_a_new_timer() {
    session_with(shared((1280, 720)), DELAYED, async |h| {
        acknowledged(h, 0, "first").await;
        h.expect("status: waiting for measurement delay: 1000 ms. first")
            .await;
        h.advance(600).await;
        h.type_line("show");
        h.expect("status: measurement delay cancelled").await;
        h.expect("status: submitting pattern").await;
        h.reply(1, Ok("second".into())).await;
        h.expect("status: waiting for measurement delay: 1000 ms. second")
            .await;
        let start = tokio::time::Instant::now();
        // The expiry time of the first timer passes without readiness.
        assert_eq!(h.advance(999).await, Vec::<String>::new());
        h.expect("status: ready after delay: 1000 ms. second").await;
        assert_eq!(start.elapsed(), Duration::from_millis(1000));
        assert!(!h.saw("status: ready after delay: 1000 ms. first"));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn viewport_change_during_the_delay_shows_again() {
    session_with(shared((1280, 720)), DELAYED, async |h| {
        acknowledged(h, 0, "first").await;
        h.expect("status: waiting for measurement delay").await;
        h.log.borrow_mut().displays = vec![client((1920, 1080))];
        h.event(ServiceEvent::Client(ClientState::ViewportChanged(id())));
        h.expect("status: measurement delay cancelled").await;
        h.advance(2000).await;
        assert!(!h.saw("status: ready"));
        h.ready();
        h.expect("status: submitting pattern").await;
        h.reply(1, Ok("second".into())).await;
        h.expect("status: ready after delay: 1000 ms. second").await;
        assert!(h.calls().contains(&"present 1920x1080".to_string()));
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn disconnect_during_the_delay_cancels_readiness() {
    let (_, h) = session_with(shared((1280, 720)), DELAYED, async |h| {
        acknowledged(h, 0, "first").await;
        h.expect("status: waiting for measurement delay").await;
        h.event(ServiceEvent::Client(ClientState::Disconnected(
            "gone".into(),
        )));
        h.expect("status: measurement delay cancelled").await;
        h.advance(2000).await;
    })
    .await;
    assert!(!h.saw("status: ready"));
    assert_eq!(
        h.calls().iter().filter(|c| c.starts_with("close")).count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn quit_during_the_delay_cancels_it_and_closes() {
    let (exit, h) = session_with(shared((1280, 720)), DELAYED, async |h| {
        acknowledged(h, 0, "first").await;
        h.expect("status: waiting for measurement delay").await;
        h.type_line("quit");
        h.expect("status: measurement delay cancelled").await;
    })
    .await;
    assert_eq!(exit, Exit::Quit);
    assert!(!h.saw("status: ready"));
    assert_eq!(h.calls().last().unwrap(), "close mock:client-1");
}
