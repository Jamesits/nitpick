//! Runs local (non-`Send`) futures on the Qt GUI thread. See "Asynchronous
//! presentation" in `doc/design/output_driver/overview.md`.
//!
//! `async-task` keeps the task state. A wake, also from a worker thread,
//! posts the task to the GUI event loop through [`Post`]; the event loop
//! then polls it. Nothing blocks the GUI thread or runs a nested event loop.

use std::cell::RefCell;
use std::future::{self, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::thread::{self, ThreadId};

use async_task::{Runnable, Task};
use cxx_qt::{CxxQtThread, Threading};

/// Posts a job to the event loop of the GUI thread.
pub trait Post: Send + Sync + 'static {
    /// The job runs later on the GUI thread, or is dropped if the event
    /// loop no longer accepts jobs.
    fn post(&self, job: Box<dyn FnOnce() + Send>);
}

/// Queues the job on the thread of the `QObject`.
impl<T: Threading + 'static> Post for CxxQtThread<T> {
    fn post(&self, job: Box<dyn FnOnce() + Send>) {
        // Fails only after the `QObject` is destroyed, at exit. The task
        // then never runs again.
        let _ = self.queue(move |_| job());
    }
}

impl Post for Arc<dyn Post> {
    fn post(&self, job: Box<dyn FnOnce() + Send>) {
        (**self).post(job);
    }
}

/// A task step that must run or drop on the thread that spawned the task.
struct Step {
    runnable: Option<Runnable>,
    home: ThreadId,
}

impl Step {
    fn run(mut self) {
        if let Some(r) = self.runnable.take() {
            r.run();
        }
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        if let Some(r) = self.runnable.take() {
            // `async-task` panics if a local runnable drops on another
            // thread. This occurs when the event loop rejects a job that
            // a worker posted, at exit; the task is then leaked.
            if thread::current().id() == self.home {
                drop(r);
            } else {
                std::mem::forget(r);
            }
        }
    }
}

type Slot = Rc<RefCell<Option<Pin<Box<dyn Future<Output = ()>>>>>>;

/// Handle of a spawned future. Dropping it drops the future at once, with
/// its wakers, timers, callback registrations and borrows.
pub struct LocalTask {
    slot: Slot,
    /// Kept for its drop, which cancels the task.
    task: Task<()>,
}

impl LocalTask {
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }
}

impl Drop for LocalTask {
    fn drop(&mut self) {
        // `Task::drop` alone only marks the task: the future would drop at
        // the next event loop pass, after the host borrows the driver
        // again. The slot is borrowed only while the future is polled; a
        // drop from inside that poll leaves the future to `async-task`.
        if let Ok(mut slot) = self.slot.try_borrow_mut() {
            drop(slot.take());
        }
    }
}

/// Spawns `future` on the calling thread, which must run the event loop of
/// `post`. The first poll is posted too, so the caller returns first.
pub fn spawn_local(post: impl Post, future: impl Future<Output = ()> + 'static) -> LocalTask {
    let home = thread::current().id();
    let schedule = move |runnable: Runnable| {
        let step = Step {
            runnable: Some(runnable),
            home,
        };
        post.post(Box::new(move || step.run()));
    };
    let slot: Slot = Rc::new(RefCell::new(Some(Box::pin(future))));
    let polled = Rc::clone(&slot);
    let wrapper = future::poll_fn(move |cx| match polled.borrow_mut().as_mut() {
        Some(f) => f.as_mut().poll(cx),
        None => Poll::Ready(()),
    });
    let (runnable, task) = async_task::spawn_local(wrapper, schedule);
    runnable.schedule();
    LocalTask { slot, task }
}

#[cfg(test)]
#[path = "gui_executor_tests.rs"]
mod tests;
