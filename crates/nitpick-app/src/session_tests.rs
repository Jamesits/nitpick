use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use futures_channel::{mpsc as channel, oneshot};
use futures_util::StreamExt;
use nitpick_browser::{ClientState, ServiceConfig, ServiceEvent};
use nitpick_core::device::Report;
use nitpick_core::driver::{
    Capabilities, Conversion, DepthSupport, DisplayId, DisplayInfo, EncodingSupport, Frame,
    Hdr10Request, ModeSettings, OutputDriver, OutputError, PresentFuture, Query, Support, Surface,
};
use nitpick_core::pattern::{MIN_SIZE, PatternKind};
use nitpick_core::signal::{BitDepth, Encoding, Format};

use super::{Busy, DeviceReader, Event, Session, ShowRequest, WebviewEvent};
use crate::browser::{self, EventSource, StartService, Started, WebviewFlag};
use crate::delay::DelayTimer;
use crate::display::{Backend, DeviceReports, display_for_key, ffi};
use crate::qrhi_driver::Job;
use crate::qrhi_driver::tests::{ChannelPost, FakeWindow, HeldWorkers, id, identity};
use crate::submission;
use crate::webview::{HELLO_TIMEOUT_MS, LaunchWebview, Launched};

type Read = (Vec<ffi::ScreenIdentity>, oneshot::Sender<DeviceReports>);

/// Device reads that complete only when the test replies.
#[derive(Clone, Default)]
struct HeldReader(Rc<RefCell<Vec<Read>>>);

impl DeviceReader for HeldReader {
    fn read(&self, screens: Vec<ffi::ScreenIdentity>) -> oneshot::Receiver<DeviceReports> {
        let (tx, rx) = oneshot::channel();
        self.0.borrow_mut().push((screens, tx));
        rx
    }
}

impl HeldReader {
    fn take(&self) -> Read {
        self.0.borrow_mut().remove(0)
    }

    fn len(&self) -> usize {
        self.0.borrow().len()
    }
}

type Timer = (u32, oneshot::Sender<()>);

/// Controlled clock: timers expire only when the test fires them.
#[derive(Clone, Default)]
struct HeldTimer(Rc<RefCell<Vec<Timer>>>);

impl DelayTimer for HeldTimer {
    fn start(&self, ms: u32) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.0.borrow_mut().push((ms, tx));
        rx
    }
}

impl HeldTimer {
    fn started(&self) -> Vec<u32> {
        self.0.borrow().iter().map(|(ms, _)| *ms).collect()
    }

    /// Expires the oldest timer. `false` if it was cancelled.
    fn fire(&self) -> bool {
        let (_, tx) = self.0.borrow_mut().remove(0);
        tx.send(()).is_ok()
    }

    /// Expires the newest timer. `false` if it was cancelled.
    fn fire_last(&self) -> bool {
        let (_, tx) = self.0.borrow_mut().pop().expect("a timer");
        tx.send(()).is_ok()
    }
}

/// The result of a pending [`MockBrowser`] presentation.
type Reply = oneshot::Sender<Result<String, OutputError>>;

/// Browser client state of [`MockBrowser`], shared by the driver and the
/// viewer as the session hub is.
#[derive(Default)]
struct BrowserLog {
    /// Clients with a valid `hello`: key and size.
    clients: Vec<(String, (usize, usize))>,
    opens: Vec<(DisplayId, (usize, usize))>,
    closes: Vec<DisplayId>,
    /// Image size of each `present`, and its reply.
    presents: Vec<((usize, usize), Reply)>,
    /// Keys of the clients with the `webview` flag.
    webview: Vec<String>,
}

#[derive(Clone, Default)]
struct MockBrowser(Rc<RefCell<BrowserLog>>);

fn client(n: u32) -> DisplayId {
    DisplayId {
        driver: browser::DRIVER,
        key: format!("client-{n}"),
    }
}

fn mock_query() -> Query {
    let unknown = || Support::Unknown("mock".into());
    Query {
        capabilities: Capabilities {
            depths: BitDepth::ALL
                .into_iter()
                .map(|d| {
                    let path = DepthSupport {
                        available: Support::Yes,
                        path: "mock PNG".into(),
                        conversion: Conversion::Png16,
                        exact: unknown(),
                    };
                    (d, path)
                })
                .collect(),
            encodings: Encoding::ALL
                .into_iter()
                .map(|e| {
                    let support = EncodingSupport {
                        input: Support::Yes,
                        interpretation: unknown(),
                        auto_light_level: Support::Yes,
                    };
                    (e, support)
                })
                .collect(),
            hdr_metadata: unknown(),
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

impl MockBrowser {
    fn size(&self, display: &DisplayId) -> Result<(usize, usize), OutputError> {
        self.0
            .borrow()
            .clients
            .iter()
            .find(|(k, _)| *k == display.key)
            .map(|(_, size)| *size)
            .ok_or(OutputError::Disconnected)
    }

    fn set_client(&self, n: u32, size: (usize, usize)) {
        self.0.borrow_mut().clients = vec![(client(n).key, size)];
    }

    fn opens(&self) -> Vec<(DisplayId, (usize, usize))> {
        self.0.borrow().opens.clone()
    }

    /// The oldest `present`: its image size and reply.
    fn take_present(&self) -> ((usize, usize), Reply) {
        self.0.borrow_mut().presents.remove(0)
    }
}

impl OutputDriver for MockBrowser {
    fn id(&self) -> &'static str {
        browser::DRIVER
    }

    fn displays(&mut self) -> Vec<DisplayInfo> {
        self.0
            .borrow()
            .clients
            .iter()
            .map(|(key, size)| DisplayInfo {
                id: DisplayId {
                    driver: browser::DRIVER,
                    key: key.clone(),
                },
                name: format!("Mock {key}"),
                size: *size,
            })
            .collect()
    }

    fn query(&mut self, display: &DisplayId) -> Result<Query, OutputError> {
        self.size(display)?;
        Ok(mock_query())
    }

    fn open(&mut self, display: &DisplayId, _: BitDepth) -> Result<Surface, OutputError> {
        let size = self.size(display)?;
        self.0.borrow_mut().opens.push((display.clone(), size));
        Ok(Surface {
            display: display.clone(),
            size,
            description: "mock surface".into(),
        })
    }

    fn present<'a>(&'a mut self, _: &'a Surface, frame: &'a Frame<'a>) -> PresentFuture<'a> {
        let (tx, rx) = oneshot::channel();
        let size = (frame.image.width(), frame.image.height());
        self.0.borrow_mut().presents.push((size, tx));
        Box::pin(async move { rx.await.unwrap_or(Err(OutputError::Disconnected)) })
    }

    fn close(&mut self, display: &DisplayId) {
        self.0.borrow_mut().closes.push(display.clone());
    }
}

impl WebviewFlag for MockBrowser {
    fn webview(&self, display: &DisplayId) -> Option<bool> {
        let log = self.0.borrow();
        log.clients
            .iter()
            .any(|(k, _)| *k == display.key)
            .then(|| log.webview.contains(&display.key))
    }
}

/// The webview processes of [`FakeLauncher`]: the arguments, the exit
/// sender and the end count of each.
#[derive(Default)]
struct LaunchLog {
    launches: Vec<(String, (i64, i64))>,
    exits: Vec<oneshot::Sender<String>>,
    ended: Rc<Cell<u32>>,
    /// The next launch fails with this reason.
    fail: Option<String>,
}

#[derive(Clone, Default)]
struct FakeLauncher(Rc<RefCell<LaunchLog>>);

impl LaunchWebview for FakeLauncher {
    fn launch(&self, url: &str, at: (i64, i64)) -> Result<Launched, String> {
        let mut log = self.0.borrow_mut();
        if let Some(e) = log.fail.take() {
            return Err(e);
        }
        log.launches.push((url.to_owned(), at));
        let (tx, exit) = oneshot::channel();
        log.exits.push(tx);
        Ok(Launched {
            exit,
            process: Box::new(StopGuard(Rc::clone(&log.ended))),
        })
    }
}

impl FakeLauncher {
    fn launches(&self) -> Vec<(String, (i64, i64))> {
        self.0.borrow().launches.clone()
    }

    fn ended(&self) -> u32 {
        self.0.borrow().ended.get()
    }

    /// The newest process exits.
    fn exit(&self, text: &str) {
        let tx = self.0.borrow_mut().exits.pop().expect("a process");
        let _ = tx.send(text.into());
    }
}

struct Channel(channel::UnboundedReceiver<ServiceEvent>);

impl EventSource for Channel {
    fn next_event(&mut self) -> Pin<Box<dyn Future<Output = Option<ServiceEvent>> + '_>> {
        Box::pin(self.0.next())
    }
}

/// Counts its drops: a dropped service is stopped.
struct StopGuard(Rc<Cell<u32>>);

impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

/// Starts [`MockBrowser`] services. Port 1 fails to start.
#[derive(Clone, Default)]
struct MockService {
    browser: MockBrowser,
    /// The event sender of each start.
    senders: Rc<RefCell<Vec<channel::UnboundedSender<ServiceEvent>>>>,
    stops: Rc<Cell<u32>>,
}

impl StartService<MockBrowser> for MockService {
    fn start(&self, config: &ServiceConfig) -> Result<Started<MockBrowser>, String> {
        if config.port == 1 {
            return Err("port 1 is in use".into());
        }
        let (tx, rx) = channel::unbounded();
        self.senders.borrow_mut().push(tx);
        Ok(Started {
            url: format!("http://127.0.0.1:{}/", config.port),
            driver: self.browser.clone(),
            viewer: self.browser.clone(),
            events: Box::new(Channel(rx)),
            service: Box::new(StopGuard(Rc::clone(&self.stops))),
        })
    }
}

struct Setup<B = MockBrowser> {
    session: Session<FakeWindow, HeldWorkers, B>,
    window: FakeWindow,
    workers: HeldWorkers,
    reader: HeldReader,
    timer: HeldTimer,
    launcher: FakeLauncher,
    events: Rc<RefCell<Vec<Event>>>,
    /// The posted jobs, in place of the Qt event loop.
    jobs: mpsc::Receiver<Job>,
}

impl<B: OutputDriver + WebviewFlag + 'static> Setup<B> {
    /// One event loop pass. It never waits.
    fn pass(&self) {
        while let Ok(job) = self.jobs.try_recv() {
            job();
        }
    }

    /// Event loop passes until no service or webview event is left. Gives
    /// each of them back to the session, as the launcher does.
    fn deliver(&mut self) {
        loop {
            self.pass();
            let service: Vec<Event> = {
                let mut events = self.events.borrow_mut();
                let (service, rest) = events
                    .drain(..)
                    .partition(|e| matches!(e, Event::Service(..) | Event::Webview(..)));
                *events = rest;
                service
            };
            if service.is_empty() {
                return;
            }
            for e in service {
                match e {
                    Event::Service(n, e) => self.session.on_service_event(n, e),
                    Event::Webview(n, e) => self.session.on_webview_event(n, e),
                    _ => unreachable!(),
                }
            }
        }
    }

    fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    fn statuses(&self) -> Vec<String> {
        self.take_events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Status(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    /// Replies to the oldest device read with an unmapped report for each
    /// screen.
    fn reply(&self) {
        let (screens, tx) = self.reader.take();
        let reports = screens
            .iter()
            .map(|s| (crate::display::screen_key(s), Err("unmapped".into())))
            .collect();
        let _ = tx.send(reports);
    }
}

fn build<B: OutputDriver + WebviewFlag + 'static>(service: Box<dyn StartService<B>>) -> Setup<B> {
    let window = FakeWindow::default();
    window.0.borrow_mut().size = Some(MIN_SIZE);
    let workers = HeldWorkers::default();
    let reader = HeldReader::default();
    let timer = HeldTimer::default();
    let launcher = FakeLauncher::default();
    let events = Rc::new(RefCell::new(Vec::new()));
    let (tx, jobs) = mpsc::channel();
    let log = Rc::clone(&events);
    let session = Session::new(
        Backend::D3d11,
        window.clone(),
        workers.clone(),
        Box::new(reader.clone()),
        service,
        Box::new(launcher.clone()),
        Rc::new(timer.clone()),
        Arc::new(ChannelPost(Mutex::new(tx))),
        move |e| log.borrow_mut().push(e),
    );
    Setup {
        session,
        window,
        workers,
        reader,
        timer,
        launcher,
        events,
        jobs,
    }
}

fn setup() -> Setup {
    build(Box::new(MockService::default()))
}

fn request(screen: i32) -> ShowRequest {
    ShowRequest {
        kind: PatternKind::Grayscale,
        format: Format::new(Encoding::PqFull, BitDepth::Ten),
        metadata: None::<Hdr10Request>,
        display: id(screen),
        delay_ms: 0,
    }
}

fn delayed(screen: i32, delay_ms: u32) -> ShowRequest {
    ShowRequest {
        delay_ms,
        ..request(screen)
    }
}

/// Acknowledges the held submission.
fn acknowledge(s: &Setup) {
    s.workers.take()();
    s.pass();
    submission::ended(submission::take_upload(), "");
    s.pass();
}

/// Shows a pattern on screen 1 up to the held conversion.
fn submit(s: &mut Setup) {
    s.session.show(request(1));
    s.pass();
    s.reply();
    s.pass();
    assert_eq!(s.statuses().last().unwrap(), "Submitting pattern");
    assert_eq!(s.workers.len(), 1);
}

#[test]
fn refresh_lists_displays_and_reads_devices_on_a_worker() {
    let mut s = setup();
    assert_eq!(
        s.session
            .reference_table(PatternKind::Grayscale, request(1).format, &id(1)),
        "No display selected."
    );
    s.session.refresh().unwrap();
    let displays = s.session.displays();
    assert_eq!(displays.len(), 2);
    assert_eq!(displays[1].id, id(1));
    assert_eq!(displays[1].size, MIN_SIZE);
    assert_eq!(s.reader.len(), 1);
    // No status without a pending presentation.
    assert_eq!(s.take_events(), Vec::new());

    let format = request(1).format;
    let before = s
        .session
        .reference_table(PatternKind::Grayscale, format, &id(1));
    assert!(before.contains("display state not read yet"), "{before}");
    s.pass();
    s.reply();
    s.pass();
    assert_eq!(s.take_events(), vec![Event::Devices]);
    let after = s
        .session
        .reference_table(PatternKind::Grayscale, format, &id(1));
    assert!(after.contains("unmapped"), "{after}");
}

#[test]
fn an_older_refresh_does_not_replace_newer_reports() {
    let mut s = setup();
    s.session.refresh().unwrap();
    s.session.refresh().unwrap();
    // The first refresh task was replaced; its reply goes nowhere.
    s.reply();
    s.reply();
    s.pass();
    assert_eq!(s.take_events(), vec![Event::Devices]);
}

#[test]
fn show_reads_devices_before_query_and_open() {
    let mut s = setup();
    s.session.show(request(1));
    assert_eq!(s.statuses(), vec!["Reading display state"]);
    assert_eq!(s.reader.len(), 1);
    // The task waits for the worker; the event loop keeps running.
    for _ in 0..3 {
        s.pass();
    }
    assert_eq!(s.window.0.borrow().opens.len(), 0);

    s.reply();
    s.pass();
    assert_eq!(
        s.take_events(),
        vec![Event::Devices, Event::Status("Submitting pattern".into())]
    );
    assert_eq!(s.window.0.borrow().opens, vec![(1, false)]);

    s.workers.take()();
    s.pass();
    assert_eq!(s.window.0.borrow().queued.len(), 1);
    assert_eq!(s.take_events().len(), 0);
    submission::ended(submission::take_upload(), "");
    s.pass();
    let status = s.statuses();
    assert_eq!(status.len(), 1);
    assert!(
        status[0].starts_with(&format!(
            "Ready after delay: 0 ms. PQ grayscale / brightness ({}) at {}x{}",
            request(1).format,
            MIN_SIZE.0,
            MIN_SIZE.1
        )),
        "{status:?}"
    );
    assert!(status[0].contains("display state unknown: unmapped"));
    assert!(status[0].ends_with("Esc closes the pattern."));
}

#[test]
fn errors_before_presentation_reach_the_status() {
    let mut s = setup();
    s.window.0.borrow_mut().size = Some((4, 2));
    s.session.show(request(1));
    s.reply();
    s.pass();
    let status = s.statuses();
    assert!(status.last().unwrap().starts_with("Error: "), "{status:?}");
    assert_eq!(s.workers.len(), 0);

    s.session.show(ShowRequest {
        display: display_for_key("missing"),
        ..request(1)
    });
    s.reply();
    s.pass();
    assert_eq!(s.statuses().last().unwrap(), "Error: display disconnected");
}

#[test]
fn a_stopped_reader_fails_the_request() {
    let mut s = setup();
    s.session.show(request(1));
    drop(s.reader.take());
    s.pass();
    assert_eq!(
        s.statuses(),
        vec![
            "Reading display state",
            "Error: the display state reader stopped"
        ]
    );
    assert_eq!(s.window.0.borrow().opens.len(), 0);
}

#[test]
fn replacement_drops_the_old_presentation() {
    let mut s = setup();
    submit(&mut s);
    let old_job = s.workers.take();
    // The second request borrows the driver: the first task is dropped.
    s.session.show(request(0));
    assert_eq!(s.statuses(), vec!["Reading display state"]);
    // The late conversion of the old frame does not reach the status.
    old_job();
    s.pass();
    assert_eq!(s.statuses().len(), 0);
    assert_eq!(s.window.0.borrow().queued.len(), 0);

    s.reply();
    s.pass();
    assert_eq!(s.statuses(), vec!["Submitting pattern"]);
    assert_eq!(s.window.0.borrow().opens, vec![(1, false), (0, false)]);
    s.workers.take()();
    s.pass();
    submission::ended(submission::take_upload(), "");
    s.pass();
    assert!(s.statuses()[0].ends_with("Esc closes the pattern."));
}

#[test]
fn replacement_before_the_device_reply_ignores_the_old_reply() {
    let mut s = setup();
    s.session.show(request(1));
    s.session.show(request(0));
    s.take_events();
    // The old reply goes to a dropped receiver.
    s.reply();
    s.pass();
    assert_eq!(s.take_events().len(), 0);
    assert_eq!(s.window.0.borrow().opens.len(), 0);
    s.reply();
    s.pass();
    assert_eq!(s.window.0.borrow().opens, vec![(0, false)]);
}

#[test]
fn a_display_change_cancels_and_closes_a_removed_screen() {
    let mut s = setup();
    submit(&mut s);
    let old_job = s.workers.take();

    // Another display is selected: cancel, but keep the window.
    s.session.refresh().unwrap();
    assert_eq!(s.statuses(), vec!["Cancelled: display changed"]);
    assert_eq!(s.window.0.borrow().closes, 0);
    assert_eq!(s.session.driver().borrow().opened(), Some(&id(1)));

    // Screen 1 is removed: its window closes.
    s.window.0.borrow_mut().screens = Some(vec![0]);
    s.session.refresh().unwrap();
    assert_eq!(s.statuses().len(), 0);
    assert_eq!(s.window.0.borrow().closes, 1);
    assert_eq!(s.session.driver().borrow().opened(), None);
    assert_eq!(s.session.displays().len(), 1);

    old_job();
    s.pass();
    assert_eq!(s.statuses().len(), 0);
}

#[test]
fn a_borrowed_driver_is_busy() {
    let mut s = setup();
    let driver = Rc::clone(s.session.driver());
    let borrow = driver.borrow_mut();
    assert_eq!(s.session.refresh(), Err(Busy));
    s.session.show(request(1));
    assert_eq!(s.statuses(), vec!["Error: the display is busy"]);
    assert_eq!(s.reader.len(), 0);
    drop(borrow);
    s.session.refresh().unwrap();
}

#[test]
fn device_reports_reach_the_query() {
    let mut s = setup();
    s.session.refresh().unwrap();
    let mut off = crate::display::tests::hdr_on();
    off.mode.hdr_on = Report::known(false, "test");
    let (_, tx) = s.reader.take();
    tx.send(vec![(id(1).key, Ok(off))]).unwrap();
    s.pass();
    let table = s.session.reference_table(
        PatternKind::Grayscale,
        Format::new(Encoding::PqFull, BitDepth::Ten),
        &id(1),
    );
    assert!(table.contains("Windows HDR is off"), "{table}");
    // The other screen has no report in this refresh.
    let table = s.session.reference_table(
        PatternKind::Grayscale,
        Format::new(Encoding::PqFull, BitDepth::Ten),
        &id(0),
    );
    assert!(table.contains("display state not read yet"), "{table}");
}

#[test]
fn identities_go_to_the_reader() {
    let mut s = setup();
    s.session.show(request(1));
    let (screens, _) = s.reader.take();
    assert_eq!(screens, vec![identity(0), identity(1)]);
}

#[test]
fn ready_after_acknowledgement_plus_delay() {
    let mut s = setup();
    s.session.show(delayed(1, 1500));
    s.pass();
    s.reply();
    s.pass();
    s.take_events();
    acknowledge(&s);
    let status = s.statuses();
    assert_eq!(status.len(), 1);
    assert!(
        status[0].starts_with("Waiting for measurement delay: 1500 ms. PQ grayscale"),
        "{status:?}"
    );
    assert_eq!(s.timer.started(), vec![1500]);
    // The event loop keeps running; the driver is free during the delay.
    s.pass();
    assert_eq!(s.statuses().len(), 0);
    assert!(s.session.driver().try_borrow_mut().is_ok());

    assert!(s.timer.fire());
    s.pass();
    let ready = s.statuses();
    assert_eq!(ready.len(), 1);
    assert!(
        ready[0].starts_with("Ready after delay: 1500 ms. PQ grayscale"),
        "{ready:?}"
    );
    // The driver warnings stay in the ready status.
    assert!(
        ready[0].contains("display state unknown: unmapped"),
        "{ready:?}"
    );
    assert!(ready[0].ends_with("Esc closes the pattern."));
}

#[test]
fn zero_delay_is_ready_without_a_timer() {
    let mut s = setup();
    s.session.show(delayed(1, 0));
    s.reply();
    s.pass();
    s.take_events();
    acknowledge(&s);
    let status = s.statuses();
    assert_eq!(status.len(), 1);
    assert!(
        status[0].starts_with("Ready after delay: 0 ms. "),
        "{status:?}"
    );
    assert_eq!(s.timer.started(), Vec::<u32>::new());
}

#[test]
fn failed_presentation_starts_no_timer() {
    let mut s = setup();
    s.window.0.borrow_mut().reject_queue = Some("queue failed".into());
    s.session.show(delayed(1, 1000));
    s.reply();
    s.pass();
    s.workers.take()();
    s.pass();
    assert!(s.statuses().last().unwrap().starts_with("Error: "));
    assert_eq!(s.timer.started(), Vec::<u32>::new());

    // A failed acknowledgement.
    s.window.0.borrow_mut().reject_queue = None;
    s.session.show(delayed(1, 1000));
    s.reply();
    s.pass();
    s.workers.take()();
    s.pass();
    submission::ended(submission::take_upload(), "endFrame failed");
    s.pass();
    assert!(s.statuses().last().unwrap().starts_with("Error: "));
    assert_eq!(s.timer.started(), Vec::<u32>::new());
}

#[test]
fn a_new_frame_cancels_the_old_timer() {
    let mut s = setup();
    s.session.show(delayed(1, 1000));
    s.reply();
    s.pass();
    acknowledge(&s);
    s.take_events();
    s.session.show(delayed(1, 2000));
    s.take_events();
    // The old timer belongs to the dropped task.
    assert!(!s.timer.fire());
    s.pass();
    assert_eq!(s.statuses().len(), 0);

    s.reply();
    s.pass();
    acknowledge(&s);
    s.take_events();
    assert!(s.timer.fire());
    s.pass();
    assert!(s.statuses()[0].starts_with("Ready after delay: 2000 ms. "));
}

#[test]
fn a_display_change_cancels_the_delay() {
    let mut s = setup();
    s.session.show(delayed(1, 1000));
    s.reply();
    s.pass();
    acknowledge(&s);
    s.take_events();
    s.session.refresh().unwrap();
    assert_eq!(s.statuses(), vec!["Cancelled: display changed"]);
    assert!(!s.timer.fire());
    s.pass();
    assert_eq!(s.statuses().len(), 0);
}

#[test]
fn a_closed_window_cancels_the_delay() {
    let mut s = setup();
    s.session.show(delayed(1, 1000));
    s.reply();
    s.pass();
    acknowledge(&s);
    s.take_events();
    // Esc in the pattern window.
    submission::closed();
    s.pass();
    assert_eq!(s.statuses(), vec!["Cancelled: the pattern window closed"]);
    assert!(!s.timer.fire());
    s.pass();
    assert_eq!(s.statuses().len(), 0);
}

#[test]
fn a_close_before_the_task_resumes_cancels_the_delay() {
    let mut s = setup();
    s.session.show(delayed(1, 1000));
    s.reply();
    s.pass();
    s.workers.take()();
    s.pass();
    s.take_events();
    // The acknowledgement and the close arrive in one event loop pass.
    submission::ended(submission::take_upload(), "");
    submission::closed();
    s.pass();
    let status = s.statuses();
    assert_eq!(status.len(), 2, "{status:?}");
    assert!(status[0].starts_with("Waiting for measurement delay: "));
    assert_eq!(status[1], "Cancelled: the pattern window closed");
}

#[test]
fn a_close_before_open_does_not_cancel_the_new_delay() {
    let mut s = setup();
    s.session.show(delayed(1, 1000));
    // The old window closes while the device state is read.
    submission::closed();
    s.reply();
    s.pass();
    acknowledge(&s);
    s.take_events();
    assert!(s.timer.fire());
    s.pass();
    assert!(s.statuses()[0].starts_with("Ready after delay: 1000 ms. "));
}

const BIGGER: (usize, usize) = (MIN_SIZE.0 + 160, MIN_SIZE.1 + 90);

fn send(s: &mut Setup, m: &MockService, event: ServiceEvent) {
    m.senders
        .borrow()
        .last()
        .expect("started")
        .unbounded_send(event)
        .unwrap();
    s.deliver();
}

/// A started mock service with client 1 ready at `MIN_SIZE`.
fn browser_setup() -> (Setup, MockService) {
    let m = MockService::default();
    let mut s = build(Box::new(m.clone()));
    s.session.start_browser(&ServiceConfig::default()).unwrap();
    m.browser.set_client(1, MIN_SIZE);
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Ready(client(1))),
    );
    s.take_events();
    (s, m)
}

fn browser_request(delay_ms: u32) -> ShowRequest {
    ShowRequest {
        display: client(1),
        ..delayed(1, delay_ms)
    }
}

/// Shows on client 1 up to the pending `present`.
fn browser_submit(s: &mut Setup, delay_ms: u32) {
    s.session.show(browser_request(delay_ms));
    s.pass();
    assert_eq!(s.statuses(), vec!["Submitting pattern"]);
}

/// Shows on client 1 up to the measurement delay.
fn browser_delay(s: &mut Setup, m: &MockService) {
    browser_submit(s, 1000);
    let _ = m.browser.take_present().1.send(Ok("mock status".into()));
    s.pass();
    let status = s.statuses();
    assert!(status[0].starts_with("Waiting for measurement delay: 1000 ms."));
}

#[test]
fn browser_service_reports_url_pairing_and_clients() {
    let m = MockService::default();
    let mut s = build(Box::new(m.clone()));
    assert_eq!(s.session.browser_view().url, None);
    assert_eq!(s.session.displays(), Vec::new());

    s.session
        .start_browser(&ServiceConfig {
            port: 8080,
            ..ServiceConfig::default()
        })
        .unwrap();
    assert_eq!(s.take_events(), vec![Event::Browser, Event::Displays]);
    let view = s.session.browser_view().clone();
    assert_eq!(view.url.as_deref(), Some("http://127.0.0.1:8080/"));
    assert_eq!(view.client, "Waiting for a browser.");

    // The events reach the session only through the host event loop.
    send(&mut s, &m, ServiceEvent::Client(ClientState::Connected));
    send(&mut s, &m, ServiceEvent::PairingCode("0123".into()));
    let view = s.session.browser_view().clone();
    assert_eq!(view.pairing_code.as_deref(), Some("0123"));
    assert_eq!(view.client, "Enter the pairing code in the browser.");
    assert_eq!(s.take_events(), vec![Event::Browser, Event::Browser]);

    send(&mut s, &m, ServiceEvent::Client(ClientState::Authenticated));
    assert_eq!(s.session.browser_view().pairing_code, None);

    m.browser.set_client(1, MIN_SIZE);
    s.take_events();
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Ready(client(1))),
    );
    assert_eq!(
        s.take_events(),
        vec![Event::Displays, Event::Devices, Event::Browser]
    );
    assert_eq!(s.session.browser_view().client, "Ready: Mock client-1.");
    s.session.refresh().unwrap();
    let displays = s.session.displays();
    assert_eq!(displays.len(), 3);
    assert_eq!(displays[2].id, client(1));
    assert_eq!(displays[2].size, MIN_SIZE);

    m.browser.0.borrow_mut().clients.clear();
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Disconnected("client closed".into())),
    );
    assert_eq!(
        s.session.browser_view().client,
        "The browser disconnected: client closed."
    );
    assert_eq!(s.session.displays().len(), 2);
}

#[test]
fn browser_show_has_no_device_read_and_waits_for_the_delay() {
    let (mut s, m) = browser_setup();
    browser_submit(&mut s, 1500);
    assert_eq!(s.reader.len(), 0);
    assert_eq!(m.browser.opens(), vec![(client(1), MIN_SIZE)]);
    let (size, reply) = m.browser.take_present();
    assert_eq!(size, MIN_SIZE);
    reply.send(Ok("mock status".into())).unwrap();
    s.pass();
    let status = s.statuses();
    assert_eq!(status.len(), 1);
    assert!(
        status[0].starts_with("Waiting for measurement delay: 1500 ms. PQ grayscale"),
        "{status:?}"
    );
    assert!(s.timer.fire());
    s.pass();
    let ready = s.statuses();
    assert_eq!(ready.len(), 1);
    assert!(
        ready[0].starts_with("Ready after delay: 1500 ms. "),
        "{ready:?}"
    );
    assert!(ready[0].ends_with("mock status"), "{ready:?}");
}

#[test]
fn a_browser_disconnect_during_the_delay_cancels_readiness() {
    let (mut s, m) = browser_setup();
    browser_delay(&mut s, &m);
    m.browser.0.borrow_mut().clients.clear();
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Disconnected("client closed".into())),
    );
    assert_eq!(s.statuses(), vec!["Cancelled: the browser disconnected"]);
    assert_eq!(m.browser.0.borrow().closes, vec![client(1)]);
    assert!(!s.timer.fire());
    s.pass();
    assert_eq!(s.statuses().len(), 0);
}

#[test]
fn a_browser_disconnect_during_presentation_gives_the_driver_error() {
    // The event before the driver result.
    let (mut s, m) = browser_setup();
    browser_submit(&mut s, 1000);
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Disconnected("client closed".into())),
    );
    assert_eq!(s.statuses(), vec!["Error: display disconnected"]);
    // The task is gone: the late result goes nowhere.
    assert!(
        m.browser
            .take_present()
            .1
            .send(Err(OutputError::Disconnected))
            .is_err()
    );
    s.pass();
    assert_eq!(s.statuses().len(), 0);

    // The driver result before the event: the same status, once.
    let (mut s, m) = browser_setup();
    browser_submit(&mut s, 1000);
    let _ = m
        .browser
        .take_present()
        .1
        .send(Err(OutputError::Disconnected));
    s.pass();
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Disconnected("client closed".into())),
    );
    assert_eq!(s.statuses(), vec!["Error: display disconnected"]);
    assert_eq!(s.timer.started(), Vec::<u32>::new());
}

const VIEWPORT_ERROR: &str =
    "Error: platform error: the browser viewport changed; open the surface again";

/// The client resizes to `BIGGER`: `ViewportChanged`, then `Ready`.
fn resize(s: &mut Setup, m: &MockService) -> Vec<String> {
    send(
        s,
        m,
        ServiceEvent::Client(ClientState::ViewportChanged(client(1))),
    );
    let before = s.statuses();
    m.browser.set_client(1, BIGGER);
    send(s, m, ServiceEvent::Client(ClientState::Ready(client(1))));
    s.pass();
    assert_eq!(s.statuses(), vec!["Submitting pattern"]);
    assert_eq!(m.browser.opens().last(), Some(&(client(1), BIGGER)));
    let (size, reply) = m.browser.take_present();
    assert_eq!(size, BIGGER);
    reply.send(Ok("new size".into())).unwrap();
    s.pass();
    assert!(s.statuses()[0].starts_with("Waiting for measurement delay: 1000 ms."));
    assert!(s.timer.fire_last());
    s.pass();
    assert!(s.statuses()[0].starts_with("Ready after delay: 1000 ms. "));
    before
}

#[test]
fn a_viewport_change_during_presentation_reopens_at_the_new_size() {
    let (mut s, m) = browser_setup();
    browser_submit(&mut s, 1000);
    let (_, old_reply) = m.browser.take_present();
    assert_eq!(resize(&mut s, &m), vec![VIEWPORT_ERROR]);
    assert!(old_reply.send(Ok("late".into())).is_err());
}

#[test]
fn a_viewport_change_during_the_delay_reopens_at_the_new_size() {
    let (mut s, m) = browser_setup();
    browser_delay(&mut s, &m);
    assert_eq!(
        resize(&mut s, &m),
        vec!["Cancelled: the browser viewport changed"]
    );
    // The first timer belongs to the dropped task.
    assert_eq!(s.timer.started(), vec![1000]);
}

#[test]
fn a_viewport_change_after_readiness_shows_the_pattern_again() {
    let (mut s, m) = browser_setup();
    browser_delay(&mut s, &m);
    assert!(s.timer.fire());
    s.pass();
    s.take_events();
    assert_eq!(resize(&mut s, &m), Vec::<String>::new());
}

#[test]
fn a_later_request_or_display_change_stops_the_reshow() {
    for change in ["show", "refresh"] {
        let (mut s, m) = browser_setup();
        browser_delay(&mut s, &m);
        send(
            &mut s,
            &m,
            ServiceEvent::Client(ClientState::ViewportChanged(client(1))),
        );
        if change == "show" {
            s.session.show(request(1));
        } else {
            s.session.refresh().unwrap();
        }
        s.take_events();
        m.browser.set_client(1, BIGGER);
        send(
            &mut s,
            &m,
            ServiceEvent::Client(ClientState::Ready(client(1))),
        );
        s.pass();
        assert_eq!(m.browser.opens().len(), 1, "{change}");
        assert_eq!(s.statuses().len(), 0, "{change}");
    }
}

#[test]
fn a_restart_ends_the_session_and_ignores_old_events() {
    let (mut s, m) = browser_setup();
    browser_delay(&mut s, &m);
    // An event of the first service in the host queue at the restart.
    let old = m.senders.borrow()[0].clone();
    old.unbounded_send(ServiceEvent::PairingCode("9999".into()))
        .unwrap();
    s.pass();
    let queued = s.take_events();
    assert_eq!(
        queued,
        vec![Event::Service(1, ServiceEvent::PairingCode("9999".into()))]
    );

    s.session.start_browser(&ServiceConfig::default()).unwrap();
    assert_eq!(
        s.statuses(),
        vec!["Cancelled: the browser service restarted"]
    );
    assert_eq!(m.stops.get(), 1);
    assert_eq!(m.browser.0.borrow().closes, vec![client(1)]);
    assert!(!s.timer.fire());
    // The first event source is gone.
    assert!(old.is_closed());

    s.events.borrow_mut().extend(queued);
    s.deliver();
    assert_eq!(s.session.browser_view().pairing_code, None);
    send(&mut s, &m, ServiceEvent::PairingCode("0001".into()));
    assert_eq!(
        s.session.browser_view().pairing_code.as_deref(),
        Some("0001")
    );
}

#[test]
fn a_failed_start_reports_the_reason() {
    let (mut s, m) = browser_setup();
    let e = s
        .session
        .start_browser(&ServiceConfig {
            port: 1,
            ..ServiceConfig::default()
        })
        .unwrap_err();
    assert_eq!(e, "port 1 is in use");
    assert_eq!(m.stops.get(), 1);
    let view = s.session.browser_view();
    assert_eq!(view.url, None);
    assert_eq!(
        view.client,
        "The browser service does not run: port 1 is in use"
    );
    s.session.show(browser_request(0));
    assert_eq!(
        s.statuses(),
        vec!["Error: the browser service does not run"]
    );
    assert_eq!(
        s.session.reference_table(
            PatternKind::Grayscale,
            browser_request(0).format,
            &client(1)
        ),
        "The browser service does not run."
    );
}

#[test]
fn a_service_stop_clears_the_browser_state() {
    let (mut s, m) = browser_setup();
    browser_submit(&mut s, 1000);
    send(
        &mut s,
        &m,
        ServiceEvent::Stopped {
            error: Some("accept failed".into()),
        },
    );
    assert_eq!(s.statuses(), vec!["Cancelled: the browser service stopped"]);
    assert_eq!(m.stops.get(), 1);
    let view = s.session.browser_view();
    assert_eq!(view.url, None);
    assert_eq!(view.client, "The browser service stopped: accept failed");
    assert_eq!(s.session.displays(), Vec::new());
}

/// A started mock service without a client, and the listed screens.
fn webview_setup() -> (Setup, MockService) {
    let m = MockService::default();
    let mut s = build(Box::new(m.clone()));
    s.session.start_browser(&ServiceConfig::default()).unwrap();
    s.session.refresh().unwrap();
    s.reply();
    s.pass();
    s.take_events();
    (s, m)
}

/// A page connects and authenticates.
fn authenticate(s: &mut Setup, m: &MockService) {
    send(s, m, ServiceEvent::Client(ClientState::Connected));
    send(s, m, ServiceEvent::PairingCode("0123".into()));
    send(s, m, ServiceEvent::Client(ClientState::Authenticated));
}

/// A page pairs as client `n` with the `webview` flag `flag`.
fn pair(s: &mut Setup, m: &MockService, n: u32, flag: bool) {
    authenticate(s, m);
    m.browser.set_client(n, MIN_SIZE);
    m.browser.0.borrow_mut().webview = if flag {
        vec![client(n).key]
    } else {
        Vec::new()
    };
    send(s, m, ServiceEvent::Client(ClientState::Ready(client(n))));
}

const URL: &str = "http://127.0.0.1:0/";

/// [`FakeWindow::desktop_point`] of screen `n`.
fn point(n: i64) -> (i64, i64) {
    (n * 1000 + 500, 500)
}

fn starting(status: &[String]) -> bool {
    status.len() == 1
        && status[0].starts_with("Starting the local webview on ")
        && status[0].ends_with(". Enter the pairing code in its window.")
}

#[test]
fn the_webview_starts_pairs_and_gets_the_pattern() {
    let (mut s, m) = webview_setup();
    s.session.show_webview(delayed(1, 1000));
    assert_eq!(s.launcher.launches(), vec![(URL.into(), point(1))]);
    assert!(starting(&s.statuses()));
    assert_eq!(s.session.webview_display(&id(1)), None);

    authenticate(&mut s, &m);
    assert_eq!(s.timer.started(), vec![HELLO_TIMEOUT_MS]);
    m.browser.set_client(1, MIN_SIZE);
    m.browser.0.borrow_mut().webview = vec![client(1).key];
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Ready(client(1))),
    );
    s.pass();
    assert_eq!(s.statuses(), vec!["Submitting pattern"]);
    assert_eq!(m.browser.opens(), vec![(client(1), MIN_SIZE)]);
    assert_eq!(s.session.webview_display(&id(1)), Some(client(1)));
    assert_eq!(s.session.webview_display(&id(0)), None);
    // The `hello` ended the timeout.
    assert!(!s.timer.fire());

    let _ = m.browser.take_present().1.send(Ok("mock status".into()));
    s.pass();
    assert!(s.statuses()[0].starts_with("Waiting for measurement delay: 1000 ms."));
    assert!(s.timer.fire());
    s.pass();
    assert!(s.statuses()[0].starts_with("Ready after delay: 1000 ms. "));

    // The ready webview gets the next request at once.
    s.session.show_webview(request(1));
    s.pass();
    assert_eq!(s.statuses(), vec!["Submitting pattern"]);
    assert_eq!(s.launcher.launches().len(), 1);
    assert_eq!(m.browser.opens().len(), 2);
    assert_eq!(s.launcher.ended(), 0);
}

#[test]
fn a_wrong_code_keeps_the_webview_and_its_request() {
    let (mut s, m) = webview_setup();
    s.session.show_webview(request(1));
    s.take_events();
    send(&mut s, &m, ServiceEvent::Client(ClientState::Connected));
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Disconnected("wrong code".into())),
    );
    assert_eq!(s.statuses(), Vec::<String>::new());
    // "Connect again" in the page.
    pair(&mut s, &m, 2, true);
    s.pass();
    assert_eq!(s.statuses(), vec!["Submitting pattern"]);
    assert_eq!(m.browser.opens(), vec![(client(2), MIN_SIZE)]);
    assert_eq!(s.launcher.launches().len(), 1);
}

#[test]
fn no_hello_after_authentication_ends_the_webview() {
    let (mut s, m) = webview_setup();
    s.session.show_webview(request(1));
    s.take_events();
    authenticate(&mut s, &m);
    assert!(s.timer.fire());
    s.deliver();
    assert_eq!(
        s.statuses(),
        vec![
            "Error: the local webview sent no display state within 10 s after \
             authentication; it was closed"
        ]
    );
    assert_eq!(s.launcher.ended(), 1);
    // The next request starts a new process.
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Disconnected("client closed".into())),
    );
    s.session.show_webview(request(1));
    assert_eq!(s.launcher.launches().len(), 2);
}

#[test]
fn a_failed_webview_reports_its_exit() {
    let (mut s, _m) = webview_setup();
    s.session.show_webview(request(1));
    s.take_events();
    s.launcher
        .exit("exit status 1: error: no monitor contains the position 1500,500");
    s.deliver();
    assert_eq!(
        s.statuses(),
        vec![
            "Error: the local webview stopped: exit status 1: error: no monitor contains \
             the position 1500,500"
        ]
    );
    s.launcher.0.borrow_mut().fail = Some("nitpick-webview.exe not found".into());
    s.session.show_webview(request(1));
    assert_eq!(
        s.statuses(),
        vec!["Error: cannot start the local webview: nitpick-webview.exe not found"]
    );
    s.session.show_webview(request(1));
    assert!(starting(&s.statuses()));
    assert_eq!(s.launcher.launches().len(), 2);
}

#[test]
fn an_existing_browser_session_is_not_replaced() {
    let (mut s, m) = webview_setup();
    pair(&mut s, &m, 1, false);
    s.take_events();
    s.session.show_webview(request(1));
    assert_eq!(
        s.statuses(),
        vec!["Error: another browser holds the session; close it before the local webview starts"]
    );
    assert_eq!(s.launcher.launches(), Vec::new());
}

#[test]
fn another_browser_that_takes_the_slot_ends_the_webview() {
    let (mut s, m) = webview_setup();
    s.session.show_webview(request(1));
    s.take_events();
    pair(&mut s, &m, 1, false);
    s.pass();
    assert_eq!(
        s.statuses(),
        vec![
            "Error: another browser took the session before the local webview; the webview \
             was closed"
        ]
    );
    assert_eq!(s.launcher.ended(), 1);
    assert_eq!(m.browser.opens(), Vec::new());
    // The other browser stays a browser display.
    assert!(s.session.displays().iter().any(|d| d.id == client(1)));
}

#[test]
fn a_removed_screen_ends_the_webview() {
    let (mut s, m) = webview_setup();
    s.session.show_webview(request(1));
    s.take_events();
    s.window.0.borrow_mut().screens = Some(vec![0]);
    s.session.refresh().unwrap();
    assert_eq!(s.statuses(), vec!["Cancelled: display changed"]);
    assert_eq!(s.launcher.ended(), 1);
    // The page of the ended process gives no request to its client.
    pair(&mut s, &m, 1, true);
    s.pass();
    assert_eq!(m.browser.opens(), Vec::new());
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Disconnected("client closed".into())),
    );

    // A display change keeps a webview whose screen stays.
    s.session.show_webview(request(0));
    s.take_events();
    s.session.refresh().unwrap();
    assert_eq!(s.statuses(), vec!["Cancelled: display changed"]);
    assert_eq!(s.launcher.ended(), 1);
}

#[test]
fn a_webview_on_another_screen_is_replaced_after_its_session_ends() {
    let (mut s, m) = webview_setup();
    s.session.show_webview(request(0));
    pair(&mut s, &m, 1, true);
    s.pass();
    s.take_events();

    s.session.show_webview(request(1));
    assert_eq!(
        s.statuses(),
        vec!["Closing the local webview on the other screen"]
    );
    assert_eq!(s.launcher.ended(), 1);
    // The old page still holds the slot.
    assert_eq!(s.launcher.launches().len(), 1);
    m.browser.0.borrow_mut().clients.clear();
    send(
        &mut s,
        &m,
        ServiceEvent::Client(ClientState::Disconnected("client closed".into())),
    );
    assert_eq!(s.launcher.launches()[1], (URL.into(), point(1)));
    assert!(starting(&s.statuses()));
}

#[test]
fn a_service_restart_ends_the_webview() {
    let (mut s, m) = webview_setup();
    s.session.show_webview(request(1));
    s.take_events();
    send(&mut s, &m, ServiceEvent::Client(ClientState::Connected));
    s.session.start_browser(&ServiceConfig::default()).unwrap();
    assert_eq!(
        s.statuses(),
        vec!["Cancelled: the browser service restarted"]
    );
    assert_eq!(s.launcher.ended(), 1);
    // The new service has no session.
    s.session.show_webview(request(1));
    assert!(starting(&s.statuses()));
    // The exit of the ended process does nothing.
    s.launcher
        .0
        .borrow_mut()
        .exits
        .remove(0)
        .send("exit status 1".into())
        .unwrap_err();
    s.deliver();
    assert_eq!(s.statuses(), Vec::<String>::new());
}

#[test]
fn a_late_event_of_an_ended_process_does_nothing() {
    let (mut s, _m) = webview_setup();
    s.session.show_webview(request(1));
    s.take_events();
    s.session
        .on_webview_event(7, WebviewEvent::Exited("exit status 1".into()));
    s.session.on_webview_event(1, WebviewEvent::HelloTimeout);
    assert_eq!(s.statuses(), Vec::<String>::new());
    assert_eq!(s.launcher.ended(), 0);
}

/// The page of the native-service test: a WebSocket client on its own
/// Tokio thread.
mod page {
    use std::net::SocketAddr;
    use std::sync::mpsc;
    use std::time::Duration;

    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio::time::timeout;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

    pub const CODE: &str = "0042";
    const WAIT: Duration = Duration::from_secs(20);

    pub enum Command {
        /// Connect, authenticate and send `hello`.
        Connect {
            size: (usize, usize),
            fullscreen: bool,
        },
        Hello {
            size: (usize, usize),
            fullscreen: bool,
        },
        /// Wait for `show`, fetch the frame and send `applied`.
        Apply,
        Close,
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Reply {
        Done,
        /// Size of `show`, HTTP status and PNG header size of the frame.
        Applied {
            show: (usize, usize),
            status: u16,
            png: (usize, usize),
        },
    }

    type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

    /// Runs the page until the command channel closes.
    pub fn spawn(
        addr: SocketAddr,
        commands: mpsc::Receiver<Command>,
        replies: mpsc::Sender<Reply>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut ws: Option<Ws> = None;
                let mut token = String::new();
                while let Ok(command) = commands.recv() {
                    let reply = match command {
                        Command::Connect { size, fullscreen } => {
                            let (mut socket, _) = connect_async(format!("ws://{addr}/api/v1/ws"))
                                .await
                                .unwrap();
                            assert_eq!(next(&mut socket).await["type"], "auth_required");
                            send(&mut socket, &json!({"type": "auth", "code": CODE})).await;
                            let reply = next(&mut socket).await;
                            assert_eq!(reply["type"], "authenticated", "{reply}");
                            reply["token"].as_str().unwrap().clone_into(&mut token);
                            send(&mut socket, &hello(size, fullscreen)).await;
                            ws = Some(socket);
                            Reply::Done
                        }
                        Command::Hello { size, fullscreen } => {
                            send(ws.as_mut().unwrap(), &hello(size, fullscreen)).await;
                            Reply::Done
                        }
                        Command::Apply => {
                            let socket = ws.as_mut().unwrap();
                            let show = next(socket).await;
                            assert_eq!(show["type"], "show", "{show}");
                            let id = show["id"].as_u64().unwrap();
                            let dim = |k: &str| usize::try_from(show[k].as_u64().unwrap()).unwrap();
                            let size = (dim("width"), dim("height"));
                            let (status, png) = fetch(addr, &token, id).await;
                            let applied = json!({
                                "type": "applied", "id": id,
                                "naturalWidth": size.0, "naturalHeight": size.1,
                                "layoutWidth": size.0, "layoutHeight": size.1
                            });
                            send(socket, &applied).await;
                            Reply::Applied {
                                show: size,
                                status,
                                png,
                            }
                        }
                        Command::Close => {
                            ws.take().unwrap().close(None).await.unwrap();
                            Reply::Done
                        }
                    };
                    replies.send(reply).unwrap();
                }
            });
        })
    }

    fn hello((width, height): (usize, usize), fullscreen: bool) -> Value {
        json!({
            "type": "hello",
            "client": {
                "userAgent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36",
                "webview": false
            },
            "screen": {"width": width, "height": height},
            "viewport": {"width": width, "height": height, "scale": 1},
            "pixelRatio": 1,
            "dynamicRange": "high",
            "colorGamut": "rec2020",
            "fullscreen": fullscreen
        })
    }

    async fn send(ws: &mut Ws, value: &Value) {
        ws.send(Message::text(value.to_string())).await.unwrap();
    }

    async fn next(ws: &mut Ws) -> Value {
        loop {
            let message = timeout(WAIT, ws.next()).await.unwrap().unwrap().unwrap();
            match message {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) | Message::Pong(_) => {}
                other => panic!("expected a JSON message, got {other:?}"),
            }
        }
    }

    /// HTTP status and the IHDR size of the frame PNG.
    async fn fetch(addr: SocketAddr, token: &str, id: u64) -> (u16, (usize, usize)) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request = format!(
            "GET /api/v1/frame/{id}.png HTTP/1.1\r\nHost: {addr}\r\n\
             Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        timeout(WAIT, stream.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let status = String::from_utf8_lossy(&response[9..12]).parse().unwrap();
        let png = &response[split + 4..];
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let be = |i: usize| {
            usize::try_from(u32::from_be_bytes(png[i..i + 4].try_into().unwrap())).unwrap()
        };
        (status, (be(16), be(20)))
    }
}

impl Setup<nitpick_browser::BrowserDriver> {
    /// Runs the event loop until `done`, at most 20 s. Returns the
    /// statuses that arrived.
    fn run_until(&mut self, mut done: impl FnMut(&Self, &[String]) -> bool) -> Vec<String> {
        let end = Instant::now() + Duration::from_secs(20);
        let mut statuses = Vec::new();
        loop {
            self.deliver();
            statuses.extend(self.statuses());
            if done(self, &statuses) {
                return statuses;
            }
            assert!(Instant::now() < end, "timeout; statuses: {statuses:?}");
            if let Ok(job) = self.jobs.recv_timeout(Duration::from_millis(10)) {
                job();
            }
        }
    }

    /// Runs the event loop until the page replies. The statuses stay for
    /// the next `run_until`.
    fn page(
        &mut self,
        commands: &mpsc::Sender<page::Command>,
        replies: &mpsc::Receiver<page::Reply>,
        command: page::Command,
    ) -> page::Reply {
        commands.send(command).unwrap();
        let mut reply = None;
        let statuses = self.run_until(|_, _| {
            reply = reply.take().or_else(|| replies.try_recv().ok());
            reply.is_some()
        });
        self.events
            .borrow_mut()
            .splice(0..0, statuses.into_iter().map(Event::Status));
        reply.unwrap()
    }
}

fn starts(prefix: &str) -> impl Fn(&Setup<nitpick_browser::BrowserDriver>, &[String]) -> bool {
    let prefix = prefix.to_owned();
    move |_, statuses| statuses.iter().any(|s| s.starts_with(&prefix))
}

/// The page fetches the next frame and applies it; it has `size`.
fn apply(
    s: &mut Setup<nitpick_browser::BrowserDriver>,
    commands: &mpsc::Sender<page::Command>,
    replies: &mpsc::Receiver<page::Reply>,
    size: (usize, usize),
) {
    let reply = s.page(commands, replies, page::Command::Apply);
    let expected = page::Reply::Applied {
        show: size,
        status: 200,
        png: size,
    };
    assert_eq!(reply, expected);
}

/// The real service and driver with a WebSocket page: URL, clients,
/// warnings, reference marks, a viewport change during the delay and a
/// disconnect during the delay.
#[test]
fn native_service_session() {
    let mut s = build(Box::new(browser::NativeService::default()));
    s.session
        .start_browser(&ServiceConfig {
            fixed_code: Some(page::CODE.into()),
            ..ServiceConfig::default()
        })
        .unwrap();
    let url = s.session.browser_view().url.clone().unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let addr = url["http://".len()..url.len() - 1].parse().unwrap();
    let (commands, page_commands) = mpsc::channel();
    let (page_replies, replies) = mpsc::channel();
    let page = page::spawn(addr, page_commands, page_replies);

    let first = (800, 450);
    s.page(
        &commands,
        &replies,
        page::Command::Connect {
            size: first,
            fullscreen: false,
        },
    );
    s.run_until(|s, _| {
        s.session
            .displays()
            .iter()
            .any(|d| d.id.driver == browser::DRIVER)
    });
    let info = s.session.displays().pop().unwrap();
    assert_eq!(info.name, "Chrome at 127.0.0.1");
    assert_eq!(info.size, first);
    assert!(s.session.browser_view().client.starts_with("Ready: Chrome"));
    let table = s.session.reference_table(
        PatternKind::ColourBars,
        Format::new(Encoding::PqNarrow, BitDepth::Ten),
        &info.id,
    );
    assert!(table.contains(" clipped"), "{table}");

    let request = ShowRequest {
        display: info.id.clone(),
        ..browser_request(500)
    };
    s.session.show(request.clone());
    apply(&mut s, &commands, &replies, first);
    let statuses = s.run_until(starts("Waiting for measurement delay"));
    let waiting = statuses.last().unwrap();
    assert!(waiting.contains("Pixel-exact layout"), "{waiting}");
    assert!(waiting.contains("not full screen"), "{waiting}");

    // Full screen at a new size during the delay.
    let second = (960, 540);
    s.page(
        &commands,
        &replies,
        page::Command::Hello {
            size: second,
            fullscreen: true,
        },
    );
    let statuses = s.run_until(starts("Submitting pattern"));
    assert!(
        statuses.contains(&"Cancelled: the browser viewport changed".to_owned()),
        "{statuses:?}"
    );
    apply(&mut s, &commands, &replies, second);
    let statuses = s.run_until(starts("Waiting for measurement delay"));
    assert!(!statuses.last().unwrap().contains("not full screen"));
    // The first and the second timer.
    assert_eq!(s.timer.started(), vec![500, 500]);
    assert!(!s.timer.fire());
    assert!(s.timer.fire());
    let statuses = s.run_until(starts("Ready after delay: 500 ms."));
    assert!(
        statuses.last().unwrap().contains("at 960x540"),
        "{statuses:?}"
    );

    // A disconnect during the delay.
    s.session.show(request);
    s.page(&commands, &replies, page::Command::Apply);
    s.run_until(starts("Waiting for measurement delay"));
    s.page(&commands, &replies, page::Command::Close);
    s.run_until(starts("Cancelled: the browser disconnected"));
    s.run_until(|s, _| s.session.displays().is_empty());
    assert!(!s.timer.fire());
    assert!(
        s.session
            .browser_view()
            .client
            .starts_with("The browser disconnected")
    );
    drop(commands);
    page.join().unwrap();
}
