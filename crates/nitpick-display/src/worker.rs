//! The device worker of a host: one thread that owns a backend, the
//! recovery store and the active profile change. It runs each job to its
//! end, in request order.
//!
//! Dropping a reply receiver does not stop its job. A native write that
//! outlives the host wait thus ends before the next job of the queue
//! starts, so a restoration queued after an assignment waits for it, and
//! no other change of the device starts before.

use std::io;
use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use futures_channel::oneshot;
use nitpick_core::device::{
    DeviceError, DisplayDeviceControl, IccProfile, ProfileState, ProfileTarget,
};

use crate::common::unsupported;
use crate::restoration::{ProfileRun, Recovery, Restoration, recover};
use crate::store::RecoveryStore;

/// The backend of a worker.
pub type Backend = Box<dyn DisplayDeviceControl + Send>;

/// The outcome of the recovery at the start of a worker, for each record.
pub type Recovered = oneshot::Receiver<Result<Vec<Recovery>, DeviceError>>;

/// Result of [`DeviceWorker::assign`] after a saved record.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    /// The readback that selects the profile, or the assignment error.
    pub applied: Result<ProfileState, DeviceError>,
    /// After a failed assignment: the restoration in the same job. `None`
    /// after a successful assignment.
    pub restored: Option<Result<Restoration, DeviceError>>,
}

struct State {
    backend: Backend,
    /// An open failure fails each change; reads still work.
    store: Result<RecoveryStore, DeviceError>,
    /// The change to restore: applied, or with a failed restoration.
    run: Option<ProfileRun>,
}

type Job = Box<dyn FnOnce(&mut State) + Send>;

/// Handle of the worker thread. Dropping it lets the thread finish the
/// queued jobs and end; it does not wait for them. A process that ends
/// during a write leaves the saved record for the next [`recover`].
pub struct DeviceWorker {
    jobs: mpsc::Sender<Job>,
}

impl DeviceWorker {
    /// Starts the thread. Its first job opens the store in `dir` and
    /// recovers its records, so recovery precedes each later job; the
    /// receiver gets the outcome of each record.
    ///
    /// # Errors
    /// The thread cannot start.
    pub fn start(backend: Backend, dir: PathBuf) -> io::Result<(Self, Recovered)> {
        let (jobs, queue) = mpsc::channel::<Job>();
        let (tx, recovered) = oneshot::channel();
        thread::Builder::new()
            .name("nitpick-device".into())
            .spawn(move || {
                let mut state = State {
                    backend,
                    store: RecoveryStore::open(dir),
                    run: None,
                };
                let records = match &state.store {
                    Ok(store) => recover(state.backend.as_mut(), store),
                    Err(e) => Err(e.clone()),
                };
                let _ = tx.send(records);
                for job in queue {
                    job(&mut state);
                }
            })?;
        Ok((Self { jobs }, recovered))
    }

    fn submit<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut State) -> T + Send + 'static,
    ) -> oneshot::Receiver<T> {
        let (tx, rx) = oneshot::channel();
        // A stopped thread drops `tx`, so the receiver fails.
        let _ = self.jobs.send(Box::new(move |state: &mut State| {
            let _ = tx.send(job(state));
        }));
        rx
    }

    /// A read through the backend.
    pub fn read<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut dyn DisplayDeviceControl) -> T + Send + 'static,
    ) -> oneshot::Receiver<T> {
        self.submit(move |state| f(state.backend.as_mut()))
    }

    /// Prepares the change, saves its record, assigns and checks the
    /// readback. A failed assignment is restored in the same job. One
    /// change at a time: an active change must be restored first.
    ///
    /// The outer error: no record was saved, so nothing changed.
    pub fn assign(
        &self,
        target: ProfileTarget,
        profile: IccProfile,
    ) -> oneshot::Receiver<Result<Assignment, DeviceError>> {
        self.submit(move |state| {
            if let Some(run) = &state.run {
                return Err(DeviceError::StateConflict(format!(
                    "the profile change of {} is active; restore it first",
                    run.change().device.id
                )));
            }
            let store = state.store.as_ref().map_err(Clone::clone)?;
            let mut run = ProfileRun::begin(state.backend.as_mut(), store, &target, &profile)?;
            let applied = run.apply(state.backend.as_mut());
            let restored = applied
                .is_err()
                .then(|| run.restore(state.backend.as_mut(), store));
            if !matches!(restored, Some(Ok(_))) {
                state.run = Some(run);
            }
            Ok(Assignment { applied, restored })
        })
    }

    /// Restores the active change. A failed restoration keeps it active.
    pub fn restore(&self) -> oneshot::Receiver<Result<Restoration, DeviceError>> {
        self.submit(|state| {
            let store = state.store.as_ref().map_err(Clone::clone)?;
            let run = state
                .run
                .as_mut()
                .ok_or_else(|| unsupported("profile restoration", "no active profile change"))?;
            let restored = run.restore(state.backend.as_mut(), store)?;
            state.run = None;
            Ok(restored)
        })
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
