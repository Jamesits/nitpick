//! Worker for PNG encoding jobs. The jobs own their input, so the host
//! thread does not wait for the conversion.

use std::fmt;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;

pub type Job = Box<dyn FnOnce() + Send>;

/// Runs encoding jobs off the host thread. Tests inject controlled workers.
pub trait Workers {
    fn spawn(&self, job: Job);
}

#[derive(Default)]
struct Slot {
    job: Option<Job>,
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    ready: Condvar,
}

/// One thread and one queued job. A new job replaces a queued job that has
/// not started, so at most one job runs and one waits. The host has only
/// one pending frame; a replaced job belongs to a dropped future.
///
/// The thread starts at the first job and ends after its current job when
/// the worker is dropped. Drop does not wait for it.
#[derive(Default)]
pub struct EncodeWorker {
    shared: Arc<Shared>,
    started: Mutex<bool>,
}

impl fmt::Debug for EncodeWorker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EncodeWorker").finish_non_exhaustive()
    }
}

impl EncodeWorker {
    /// `false` if the thread cannot start.
    fn start(&self) -> bool {
        let mut started = self.started.lock().unwrap_or_else(PoisonError::into_inner);
        if !*started {
            let shared = Arc::clone(&self.shared);
            *started = thread::Builder::new()
                .name("nitpick-png".into())
                .spawn(move || run(&shared))
                .is_ok();
        }
        *started
    }
}

fn run(shared: &Shared) {
    loop {
        let job = {
            let mut slot = shared.slot.lock().unwrap_or_else(PoisonError::into_inner);
            loop {
                if slot.stopped {
                    return;
                }
                if let Some(job) = slot.job.take() {
                    break job;
                }
                slot = shared
                    .ready
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        job();
    }
}

impl Workers for EncodeWorker {
    fn spawn(&self, job: Job) {
        // Without a thread the job is dropped with its sender; the future
        // then fails instead of waiting.
        if self.start() {
            let mut slot = self
                .shared
                .slot
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            slot.job = Some(job);
            self.shared.ready.notify_one();
        }
    }
}

impl Drop for EncodeWorker {
    fn drop(&mut self) {
        let mut slot = self
            .shared
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slot.stopped = true;
        slot.job = None;
        self.shared.ready.notify_one();
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
