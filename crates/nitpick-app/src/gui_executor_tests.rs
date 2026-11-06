use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use futures_channel::oneshot;

use super::{Post, spawn_local};

type Job = Box<dyn FnOnce() + Send>;

/// An event loop on the test thread, in place of the Qt event loop.
struct EventLoop {
    rx: mpsc::Receiver<Job>,
}

#[derive(Clone)]
struct Poster {
    tx: Arc<Mutex<mpsc::Sender<Job>>>,
    /// The loop rejects jobs, as Qt does after the `QObject` is destroyed.
    closed: Arc<AtomicBool>,
}

impl Post for Poster {
    fn post(&self, job: Job) {
        if !self.closed.load(Ordering::SeqCst) {
            let _ = self.tx.lock().unwrap().send(job);
        }
    }
}

fn event_loop() -> (EventLoop, Poster) {
    let (tx, rx) = mpsc::channel();
    let poster = Poster {
        tx: Arc::new(Mutex::new(tx)),
        closed: Arc::default(),
    };
    (EventLoop { rx }, poster)
}

impl EventLoop {
    /// Runs the jobs that are posted now. Never blocks.
    fn run_ready(&self) -> usize {
        self.rx.try_iter().map(|job| job()).count()
    }

    /// Waits for one job from another thread and runs it.
    fn run_next(&self) {
        let job = self
            .rx
            .recv_timeout(Duration::from_secs(5))
            .expect("no job posted");
        job();
    }
}

/// Sets its flag when dropped.
struct DropFlag(Rc<Cell<bool>>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

#[test]
fn the_first_poll_is_posted() {
    let (ev, poster) = event_loop();
    let polled = Rc::new(Cell::new(false));
    let p = Rc::clone(&polled);
    let task = spawn_local(poster, async move { p.set(true) });
    // The caller, a QML invokable, returns before the task runs.
    assert!(!polled.get());
    assert_eq!(ev.run_ready(), 1);
    assert!(polled.get());
    assert!(task.is_finished());
}

#[test]
fn a_wake_from_a_worker_polls_on_the_home_thread() {
    let (ev, poster) = event_loop();
    let (tx, rx) = oneshot::channel::<()>();
    let polled_on = Rc::new(Cell::new(None));
    let p = Rc::clone(&polled_on);
    let task = spawn_local(poster, async move {
        rx.await.unwrap();
        p.set(Some(thread::current().id()));
    });
    ev.run_ready();
    assert!(!task.is_finished());
    let worker = thread::spawn(move || tx.send(()).unwrap());
    ev.run_next();
    worker.join().unwrap();
    assert!(task.is_finished());
    assert_eq!(polled_on.get(), Some(thread::current().id()));
}

#[test]
fn other_jobs_run_while_a_task_waits() {
    let (ev, poster) = event_loop();
    let (tx, rx) = oneshot::channel::<()>();
    let task = spawn_local(poster.clone(), async move { rx.await.unwrap() });
    // Timer events of the event loop, posted while the task waits.
    let ticks = Arc::new(Mutex::new(0));
    for _ in 0..3 {
        let t = Arc::clone(&ticks);
        poster.post(Box::new(move || *t.lock().unwrap() += 1));
        ev.run_ready();
    }
    assert_eq!(*ticks.lock().unwrap(), 3);
    assert!(!task.is_finished());
    tx.send(()).unwrap();
    ev.run_ready();
    assert!(task.is_finished());
}

#[test]
fn dropping_the_task_drops_the_future_at_once() {
    let (ev, poster) = event_loop();
    let (tx, rx) = oneshot::channel::<()>();
    let dropped = Rc::new(Cell::new(false));
    let guard = DropFlag(Rc::clone(&dropped));
    let task = spawn_local(poster, async move {
        let _guard = guard;
        rx.await.unwrap();
    });
    ev.run_ready();
    drop(task);
    assert!(dropped.get());
    // The receiver is gone, so a worker sees the cancellation.
    assert!(tx.is_canceled());
    ev.run_ready();
}

#[test]
fn a_rejected_wake_on_a_worker_does_not_panic() {
    let (ev, poster) = event_loop();
    let (tx, rx) = oneshot::channel::<()>();
    let closed = Arc::clone(&poster.closed);
    let task = spawn_local(poster, async move { rx.await.unwrap() });
    ev.run_ready();
    closed.store(true, Ordering::SeqCst);
    // The worker's wake posts the step; the closed loop drops it on the
    // worker thread.
    thread::spawn(move || tx.send(()).unwrap())
        .join()
        .expect("the worker panicked");
    assert!(!task.is_finished());
}
