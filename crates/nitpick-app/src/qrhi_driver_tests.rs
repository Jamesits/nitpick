use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

use nitpick_core::driver::{
    ContentLightLevel, DisplayId, Frame, Hdr10Request, OutputDriver, OutputError, PresentFuture,
    Surface,
};
use nitpick_core::image::Image;
use nitpick_core::output::hdr10_bytes;
use nitpick_core::signal::{BT2020_PRIMARIES, BitDepth, D65, Encoding, Format, Rgb};

use super::{Job, PatternWindow, QrhiDriver, Workers};
use crate::display::{Backend, DeviceReport, display_id, ffi};
use crate::gui_executor::{self, Post};
use crate::hdr_metadata::{MetadataSink, NativeMetadata};
use crate::submission::{self, FrameId};

const SIZE: (usize, usize) = (4, 2);

/// Calls of the driver into the window, shared with the test.
#[derive(Default)]
struct WindowLog {
    fallback: String,
    opens: Vec<(i32, bool)>,
    closes: usize,
    queued: Vec<(FrameId, Vec<u8>)>,
    /// `set` or `clear`, in call order, with the number of frames queued
    /// before the call.
    metadata: Vec<(&'static str, usize)>,
    reject_queue: Option<String>,
}

#[derive(Clone, Default)]
struct FakeWindow(Rc<RefCell<WindowLog>>);

impl MetadataSink for FakeWindow {
    fn set(&mut self, _: &NativeMetadata) -> Result<(), String> {
        let mut log = self.0.borrow_mut();
        let n = log.queued.len();
        log.metadata.push(("set", n));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), String> {
        let mut log = self.0.borrow_mut();
        let n = log.queued.len();
        log.metadata.push(("clear", n));
        Ok(())
    }
}

impl PatternWindow for FakeWindow {
    fn screen_count(&self) -> i32 {
        2
    }

    fn screen_name(&self, screen: i32) -> Result<String, String> {
        Ok(format!("screen {screen}"))
    }

    fn screen_size(&self, _: i32) -> Result<(usize, usize), String> {
        Ok(SIZE)
    }

    fn device_report(&self, _: i32) -> DeviceReport {
        Err("no device in this test".into())
    }

    fn open(&mut self, screen: i32, fp16: bool) -> Result<ffi::PatternSurface, String> {
        let mut log = self.0.borrow_mut();
        log.opens.push((screen, fp16));
        Ok(ffi::PatternSurface {
            fallback: log.fallback.clone(),
            width: 4,
            height: 2,
            description: "HDR10 swap chain (test)".into(),
        })
    }

    fn close(&mut self) {
        self.0.borrow_mut().closes += 1;
        // As the C++ window destructor does.
        submission::closed();
    }

    fn queue(&mut self, pixels: &[u8], size: (i32, i32), frame: FrameId) -> Result<(), String> {
        assert_eq!(size, (4, 2));
        let mut log = self.0.borrow_mut();
        if let Some(e) = &log.reject_queue {
            return Err(e.clone());
        }
        log.queued.push((frame, pixels.to_vec()));
        Ok(())
    }
}

/// Workers that run a job only when the test releases it.
#[derive(Clone, Default)]
struct HeldWorkers(Rc<RefCell<Vec<Job>>>);

impl Workers for HeldWorkers {
    fn spawn(&self, job: Job) {
        self.0.borrow_mut().push(job);
    }
}

impl HeldWorkers {
    fn take(&self) -> Job {
        self.0.borrow_mut().remove(0)
    }

    fn len(&self) -> usize {
        self.0.borrow().len()
    }
}

#[derive(Default)]
struct WakeFlag(AtomicBool);

impl Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl WakeFlag {
    fn take(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }
}

/// Polls once, as the host executor does after a wake.
fn poll(future: &mut PresentFuture<'_>, flag: &Arc<WakeFlag>) -> Poll<Result<String, OutputError>> {
    let waker = Waker::from(Arc::clone(flag));
    future.as_mut().poll(&mut Context::from_waker(&waker))
}

struct Setup {
    driver: QrhiDriver<FakeWindow, HeldWorkers>,
    window: FakeWindow,
    workers: HeldWorkers,
    display: DisplayId,
}

fn setup() -> Setup {
    let window = FakeWindow::default();
    let workers = HeldWorkers::default();
    Setup {
        driver: QrhiDriver::new(Backend::D3d11, window.clone(), workers.clone()),
        window,
        workers,
        display: display_id(1),
    }
}

fn open(s: &mut Setup) -> Surface {
    s.driver.query(&s.display).unwrap();
    s.driver.open(&s.display, BitDepth::Ten).unwrap()
}

fn image(code: u16) -> Image {
    Image::new(SIZE.0, SIZE.1, Rgb::new(code, code + 1, code + 2))
}

const PQ_10: Format = Format::new(Encoding::PqFull, BitDepth::Ten);

fn request() -> Hdr10Request {
    Hdr10Request {
        primaries: BT2020_PRIMARIES,
        white: D65,
        max_mastering_nits: 1000.0,
        min_mastering_nits: 0.0001,
        light_level: ContentLightLevel::FromFrame,
    }
}

#[test]
fn displays_and_open_use_the_screen_index() {
    let mut s = setup();
    let displays = s.driver.displays();
    assert_eq!(displays.len(), 2);
    assert_eq!(displays[1].id, s.display);
    assert_eq!(displays[1].name, "screen 1");
    assert_eq!(displays[1].size, SIZE);
    let surface = open(&mut s);
    assert_eq!(surface.size, SIZE);
    assert_eq!(s.window.0.borrow().opens, vec![(1, false)]);
    s.driver.open(&s.display, BitDepth::Twelve).unwrap();
    assert_eq!(s.window.0.borrow().opens[1], (1, true));
}

#[test]
fn unknown_displays_are_disconnected() {
    let mut s = setup();
    for id in [display_id(2), display_id(-1)] {
        assert_eq!(s.driver.query(&id), Err(OutputError::Disconnected));
        assert_eq!(
            s.driver.open(&id, BitDepth::Ten),
            Err(OutputError::Disconnected)
        );
    }
    assert_eq!(s.window.0.borrow().opens.len(), 0);
}

#[test]
fn conversion_runs_on_a_worker_and_the_future_yields() {
    let mut s = setup();
    let surface = open(&mut s);
    let img = image(100);
    let req = request();
    let frame = Frame {
        image: &img,
        format: PQ_10,
        metadata: Some(&req),
    };
    let flag = Arc::new(WakeFlag::default());
    let mut future = s.driver.present(&surface, &frame);

    // Conversion waits for the worker; the poll returns at once.
    assert!(poll(&mut future, &flag).is_pending());
    assert_eq!(s.workers.len(), 1);
    assert_eq!(s.window.0.borrow().queued.len(), 0);

    let job = s.workers.take();
    thread::spawn(job).join().unwrap();
    assert!(flag.take(), "the worker result must wake the task");

    // Metadata goes first, then the frame; the future waits for its
    // acknowledgement.
    assert!(poll(&mut future, &flag).is_pending());
    let (id, pixels) = s.window.0.borrow().queued[0].clone();
    assert_eq!(pixels, hdr10_bytes(&img).unwrap());
    assert_eq!(s.window.0.borrow().metadata, vec![("set", 0)]);

    assert_eq!(submission::take_upload(), id);
    assert!(!flag.take());
    submission::ended(id, "");
    assert!(flag.take(), "the acknowledgement must wake the task");
    let Poll::Ready(Ok(status)) = poll(&mut future, &flag) else {
        panic!("the frame is acknowledged");
    };
    assert!(status.starts_with("HDR10 swap chain (test). display state unknown"));
    assert!(status.contains("HDR10 metadata: MaxCLL"), "{status}");
    assert!(status.contains("Warnings: "), "{status}");
}

#[test]
fn dropping_during_conversion_does_not_wait_for_the_worker() {
    let mut s = setup();
    let surface = open(&mut s);
    let img = image(100);
    let frame = Frame {
        image: &img,
        format: PQ_10,
        metadata: None,
    };
    let flag = Arc::new(WakeFlag::default());
    let mut future = s.driver.present(&surface, &frame);
    assert!(poll(&mut future, &flag).is_pending());

    // The worker is busy and waits for the test.
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let job = s.workers.take();
    let worker = thread::spawn(move || {
        go_rx.recv().unwrap();
        job();
    });
    drop(future);
    go_tx.send(()).unwrap();
    worker.join().unwrap();

    // The late result reaches nothing; no frame or metadata call.
    assert!(!flag.take());
    assert_eq!(s.window.0.borrow().queued.len(), 0);
    assert_eq!(s.window.0.borrow().metadata.len(), 0);
}

#[test]
fn a_late_result_does_not_affect_a_new_frame() {
    let mut s = setup();
    let surface = open(&mut s);
    let old_img = image(100);
    let new_img = image(200);
    let flag = Arc::new(WakeFlag::default());

    // The old frame reaches the window, then the host replaces it.
    let old = Frame {
        image: &old_img,
        format: PQ_10,
        metadata: None,
    };
    let mut future = s.driver.present(&surface, &old);
    assert!(poll(&mut future, &flag).is_pending());
    s.workers.take()();
    assert!(poll(&mut future, &flag).is_pending());
    let old_id = s.window.0.borrow().queued[0].0;
    drop(future);

    // A conversion of a dropped future that finishes late.
    let mut future = s.driver.present(&surface, &old);
    assert!(poll(&mut future, &flag).is_pending());
    let late_job = s.workers.take();
    drop(future);

    let new = Frame {
        image: &new_img,
        format: PQ_10,
        metadata: None,
    };
    let mut future = s.driver.present(&surface, &new);
    assert!(poll(&mut future, &flag).is_pending());
    flag.take();
    late_job();
    assert!(!flag.take());
    s.workers.take()();
    assert!(poll(&mut future, &flag).is_pending());
    let queued = s.window.0.borrow().queued.clone();
    assert_eq!(queued.len(), 2);
    let (new_id, pixels) = queued[1].clone();
    assert_eq!(pixels, hdr10_bytes(&new_img).unwrap());

    // Events of the old frame do not complete the new one.
    submission::ended(old_id, "");
    submission::timed_out(old_id);
    assert!(poll(&mut future, &flag).is_pending());
    assert_eq!(submission::take_upload(), new_id);
    submission::ended(new_id, "");
    assert!(matches!(poll(&mut future, &flag), Poll::Ready(Ok(_))));
}

#[test]
fn render_failures_and_close_complete_the_future() {
    let mut s = setup();
    let surface = open(&mut s);
    let img = image(100);
    let frame = Frame {
        image: &img,
        format: PQ_10,
        metadata: None,
    };
    let flag = Arc::new(WakeFlag::default());
    let cases: [(fn(FrameId), OutputError); 3] = [
        (submission::timed_out, OutputError::Timeout),
        (
            |_| submission::failed("beginFrame failed: graphics device lost"),
            OutputError::Platform("beginFrame failed: graphics device lost".into()),
        ),
        (|_| submission::closed(), OutputError::Disconnected),
    ];
    for (event, expected) in cases {
        let mut future = s.driver.present(&surface, &frame);
        assert!(poll(&mut future, &flag).is_pending());
        s.workers.take()();
        assert!(poll(&mut future, &flag).is_pending());
        let id = s.window.0.borrow().queued.last().unwrap().0;
        event(id);
        assert!(flag.take());
        assert_eq!(poll(&mut future, &flag), Poll::Ready(Err(expected)));
    }
}

#[test]
fn a_rejected_queue_fails_without_a_completion() {
    let mut s = setup();
    let surface = open(&mut s);
    s.window.0.borrow_mut().reject_queue = Some("no pattern window is open".into());
    let img = image(100);
    let frame = Frame {
        image: &img,
        format: PQ_10,
        metadata: None,
    };
    let flag = Arc::new(WakeFlag::default());
    let mut future = s.driver.present(&surface, &frame);
    assert!(poll(&mut future, &flag).is_pending());
    s.workers.take()();
    assert_eq!(
        poll(&mut future, &flag),
        Poll::Ready(Err(OutputError::Platform(
            "no pattern window is open".into()
        )))
    );
    drop(future);
    // The completion was released; a close reports nothing.
    submission::closed();
}

#[test]
fn invalid_frames_fail_before_conversion() {
    let mut s = setup();
    let surface = open(&mut s);
    let flag = Arc::new(WakeFlag::default());
    let small = Image::new(2, 2, Rgb::new(0, 0, 0));
    let img = image(100);
    let other = Surface {
        display: display_id(0),
        ..surface.clone()
    };
    let cases = [
        (
            &surface,
            Frame {
                image: &img,
                format: Format::new(Encoding::PqFull, BitDepth::Twelve),
                metadata: None,
            },
        ),
        (
            &surface,
            Frame {
                image: &small,
                format: PQ_10,
                metadata: None,
            },
        ),
        (
            &other,
            Frame {
                image: &img,
                format: PQ_10,
                metadata: None,
            },
        ),
    ];
    for (surface, frame) in &cases {
        let mut future = s.driver.present(surface, frame);
        assert!(matches!(poll(&mut future, &flag), Poll::Ready(Err(_))));
    }
    assert_eq!(s.workers.len(), 0);
}

#[test]
fn close_ends_the_surface() {
    let mut s = setup();
    let surface = open(&mut s);
    s.driver.close(&display_id(0));
    assert_eq!(s.window.0.borrow().closes, 0);
    s.driver.close(&s.display);
    assert_eq!(s.window.0.borrow().closes, 1);
    let img = image(100);
    let frame = Frame {
        image: &img,
        format: PQ_10,
        metadata: None,
    };
    let flag = Arc::new(WakeFlag::default());
    let mut future = s.driver.present(&surface, &frame);
    assert_eq!(
        poll(&mut future, &flag),
        Poll::Ready(Err(OutputError::Disconnected))
    );
}

#[test]
fn metadata_state_follows_the_window_and_open_warnings_reach_the_status() {
    let mut s = setup();
    s.window.0.borrow_mut().fallback = "8-bit SDR".into();
    let img = image(100);
    let req = request();
    let flag = Arc::new(WakeFlag::default());
    let first = open(&mut s);
    // Metadata, then none on the same window, then none on a new window.
    for (surface, metadata) in [
        (Some(first.clone()), Some(&req)),
        (Some(first), None),
        (None, None),
    ] {
        let surface = surface.unwrap_or_else(|| open(&mut s));
        let frame = Frame {
            image: &img,
            format: PQ_10,
            metadata,
        };
        let mut future = s.driver.present(&surface, &frame);
        assert!(poll(&mut future, &flag).is_pending());
        s.workers.take()();
        assert!(poll(&mut future, &flag).is_pending());
        let id = submission::take_upload();
        submission::ended(id, "");
        let Poll::Ready(Ok(status)) = poll(&mut future, &flag) else {
            panic!("the frame is acknowledged");
        };
        assert!(
            status.contains("10-bit swap chain not supported by this display: 8-bit SDR"),
            "{status}"
        );
    }
    // The new window starts without metadata, so it needs no removal.
    assert_eq!(s.window.0.borrow().metadata, vec![("set", 0), ("clear", 1)]);
}

/// Posts to a channel that the test thread runs, in place of the Qt
/// event loop.
struct ChannelPost(Mutex<mpsc::Sender<Job>>);

impl Post for ChannelPost {
    fn post(&self, job: Job) {
        let _ = self.0.lock().unwrap().send(job);
    }
}

/// A worker thread per job that starts after the test opens the gate.
struct GatedThreads(Arc<Mutex<mpsc::Receiver<()>>>);

impl Workers for GatedThreads {
    fn spawn(&self, job: Job) {
        let gate = Arc::clone(&self.0);
        thread::spawn(move || {
            gate.lock().unwrap().recv().unwrap();
            job();
        });
    }
}

#[test]
// As in the launcher: the task holds the driver borrow until it completes.
#[allow(clippy::await_holding_refcell_ref)]
fn the_event_loop_runs_during_conversion_and_submission() {
    let (post_tx, events) = mpsc::channel::<Job>();
    let (gate, gate_rx) = mpsc::channel();
    let window = FakeWindow::default();
    let driver = Rc::new(RefCell::new(QrhiDriver::new(
        Backend::D3d11,
        window.clone(),
        GatedThreads(Arc::new(Mutex::new(gate_rx))),
    )));
    let surface = {
        let mut d = driver.borrow_mut();
        d.query(&display_id(0)).unwrap();
        d.open(&display_id(0), BitDepth::Ten).unwrap()
    };
    let result = Rc::new(RefCell::new(None));
    let r = Rc::clone(&result);
    let d = Rc::clone(&driver);
    let task = gui_executor::spawn_local(ChannelPost(Mutex::new(post_tx)), async move {
        let img = image(100);
        let frame = Frame {
            image: &img,
            format: PQ_10,
            metadata: None,
        };
        let status = d.borrow_mut().present(&surface, &frame).await;
        *r.borrow_mut() = Some(status);
    });
    // One pass of the event loop: posted jobs, then a timer event. It
    // never waits.
    let mut ticks = 0;
    let mut pass = || {
        for job in events.try_iter() {
            job();
        }
        ticks += 1;
    };

    // Conversion: the worker waits at the gate; the loop keeps running.
    for _ in 0..3 {
        pass();
    }
    assert_eq!(window.0.borrow().queued.len(), 0);
    gate.send(()).unwrap();
    // The worker's wake arrives from its thread.
    events.recv_timeout(Duration::from_secs(5)).unwrap()();
    assert_eq!(window.0.borrow().queued.len(), 1);

    // Submission: the frame waits for its render.
    for _ in 0..3 {
        pass();
    }
    assert!(result.borrow().is_none());
    let id = submission::take_upload();
    submission::ended(id, "");
    pass();
    assert!(matches!(*result.borrow(), Some(Ok(_))));
    assert_eq!(ticks, 7);
    assert!(task.is_finished());
    // The task released its driver borrow.
    driver.borrow_mut().close(&display_id(0));
}
