use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};

use futures_channel::oneshot;
use nitpick_core::device::Report;
use nitpick_core::driver::Hdr10Request;
use nitpick_core::pattern::{MIN_SIZE, PatternKind};
use nitpick_core::signal::{BitDepth, Encoding, Format};

use super::{Busy, DeviceReader, Event, Session, ShowRequest};
use crate::display::{Backend, DeviceReports, display_for_key, ffi};
use crate::qrhi_driver::Job;
use crate::qrhi_driver::tests::{ChannelPost, FakeWindow, HeldWorkers, id, identity};
use crate::submission;

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

struct Setup {
    session: Session<FakeWindow, HeldWorkers>,
    window: FakeWindow,
    workers: HeldWorkers,
    reader: HeldReader,
    events: Rc<RefCell<Vec<Event>>>,
    /// The posted jobs, in place of the Qt event loop.
    jobs: mpsc::Receiver<Job>,
}

impl Setup {
    /// One event loop pass. It never waits.
    fn pass(&self) {
        while let Ok(job) = self.jobs.try_recv() {
            job();
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
                Event::Devices => None,
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

fn setup() -> Setup {
    let window = FakeWindow::default();
    window.0.borrow_mut().size = Some(MIN_SIZE);
    let workers = HeldWorkers::default();
    let reader = HeldReader::default();
    let events = Rc::new(RefCell::new(Vec::new()));
    let (tx, jobs) = mpsc::channel();
    let log = Rc::clone(&events);
    let session = Session::new(
        Backend::D3d11,
        window.clone(),
        workers.clone(),
        Box::new(reader.clone()),
        Arc::new(ChannelPost(Mutex::new(tx))),
        move |e| log.borrow_mut().push(e),
    );
    Setup {
        session,
        window,
        workers,
        reader,
        events,
        jobs,
    }
}

fn request(screen: i32) -> ShowRequest {
    ShowRequest {
        kind: PatternKind::Grayscale,
        format: Format::new(Encoding::PqFull, BitDepth::Ten),
        metadata: None::<Hdr10Request>,
        display: id(screen),
    }
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
            .reference_table(PatternKind::Grayscale, request(1).format, &id(1).key),
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
        .reference_table(PatternKind::Grayscale, format, &id(1).key);
    assert!(before.contains("display state not read yet"), "{before}");
    s.pass();
    s.reply();
    s.pass();
    assert_eq!(s.take_events(), vec![Event::Devices]);
    let after = s
        .session
        .reference_table(PatternKind::Grayscale, format, &id(1).key);
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
            "PQ grayscale / brightness ({}) at {}x{}",
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
        &id(1).key,
    );
    assert!(table.contains("Windows HDR is off"), "{table}");
    // The other screen has no report in this refresh.
    let table = s.session.reference_table(
        PatternKind::Grayscale,
        Format::new(Encoding::PqFull, BitDepth::Ten),
        &id(0).key,
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
