//! [`Devices`] on the device worker of `nitpick-display`.

use std::path::PathBuf;

use futures_channel::oneshot;
use nitpick_core::device::DeviceError;
use nitpick_display::restoration::Restoration;
use nitpick_display::worker::{Backend, DeviceWorker};

use super::{Assigned, DeviceCall, DeviceReply, Devices, RecoveredRecord, Restored};
use crate::controller::LocalFuture;

/// Starts the worker at the first call, so a host without device requests
/// does not open the recovery store. The store directory is locked while
/// the host runs; a second host on it fails each change.
pub struct WorkerDevices {
    start: Option<(Box<dyn FnOnce() -> Backend>, PathBuf)>,
    worker: Option<DeviceWorker>,
}

impl WorkerDevices {
    /// `backend` runs on the host thread at the first call; `dir` is the
    /// recovery store directory.
    #[must_use]
    pub fn new(backend: impl FnOnce() -> Backend + 'static, dir: PathBuf) -> Self {
        Self {
            start: Some((Box::new(backend), dir)),
            worker: None,
        }
    }
}

fn stopped() -> DeviceError {
    DeviceError::Native {
        api: "device worker".into(),
        code: None,
        message: "the device worker stopped".into(),
    }
}

fn restored(r: Restoration) -> Restored {
    Restored {
        state: r.state,
        record_kept: r.record_kept,
    }
}

/// Waits for a reply of the worker.
fn reply<T: 'static>(
    rx: oneshot::Receiver<T>,
    map: impl FnOnce(T) -> Result<DeviceReply, DeviceError> + 'static,
) -> LocalFuture<Result<DeviceReply, DeviceError>> {
    Box::pin(async move { rx.await.map_err(|_| stopped()).and_then(map) })
}

impl Devices for WorkerDevices {
    fn call(&mut self, call: DeviceCall) -> LocalFuture<Result<DeviceReply, DeviceError>> {
        let mut recovered = None;
        if let Some((backend, dir)) = self.start.take() {
            match DeviceWorker::start(backend(), dir) {
                Ok((worker, rx)) => {
                    self.worker = Some(worker);
                    recovered = Some(rx);
                }
                Err(e) => {
                    let e = DeviceError::Native {
                        api: "spawn".into(),
                        code: e.raw_os_error().map(i64::from),
                        message: format!("cannot start the device worker: {e}"),
                    };
                    return Box::pin(async move { Err(e) });
                }
            }
        }
        let Some(worker) = &self.worker else {
            return Box::pin(async { Err(stopped()) });
        };
        match call {
            DeviceCall::Recover => {
                let Some(rx) = recovered else {
                    // Only the first call recovers; later ones have nothing to do.
                    return Box::pin(async { Ok(DeviceReply::Recovered(Vec::new())) });
                };
                reply(rx, |records| {
                    Ok(DeviceReply::Recovered(
                        records?
                            .into_iter()
                            .map(|r| RecoveredRecord {
                                record: r.key.to_string(),
                                result: r.result,
                            })
                            .collect(),
                    ))
                })
            }
            DeviceCall::List => reply(worker.read(|b| b.devices()), |r| r.map(DeviceReply::List)),
            DeviceCall::Query(id) => reply(worker.read(move |b| b.query_device(&id)), |r| {
                r.map(|q| DeviceReply::Query(Box::new(q)))
            }),
            DeviceCall::Setup(id) => reply(worker.read(move |b| b.display_setup(&id)), |r| {
                r.map(DeviceReply::Setup)
            }),
            DeviceCall::Targets(id) => reply(worker.read(move |b| b.profile_targets(&id)), |r| {
                r.map(DeviceReply::Targets)
            }),
            DeviceCall::State(target) => {
                reply(worker.read(move |b| b.profile_state(&target)), |r| {
                    r.map(DeviceReply::State)
                })
            }
            DeviceCall::Assign { target, profile } => reply(worker.assign(target, profile), |r| {
                r.map(|a| {
                    DeviceReply::Assigned(Assigned {
                        applied: a.applied,
                        restored: a.restored.map(|r| r.map(restored)),
                    })
                })
            }),
            DeviceCall::Restore => reply(worker.restore(), |r| {
                r.map(|r| DeviceReply::Restored(restored(r)))
            }),
        }
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
