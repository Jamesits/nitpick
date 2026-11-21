//! Device requests of the test host: the device run with its setup
//! checks, and profile changes.
//!
//! Device calls go to [`crate::device::Devices`] in request order, and their replies are
//! handled in that order. A profile change cannot be cancelled: a
//! restoration, `quit` and the end of the input queue a restoration after
//! it.

use std::collections::VecDeque;
use std::task::{Context, Poll};

use nitpick_core::device::{
    ContentHash, DeviceError, DeviceId, DisplaySetup, IccProfile, ProfileSelection, ProfileState,
    ProfileTarget, SetupField, SetupFinding,
};
use nitpick_core::driver::OutputDriver;
use nitpick_fixtures::profile::{ProfileUse, manifest};

use super::{LocalFuture, Machine, NO_DEVICES, surface_event};
use crate::device::{DeviceCall, DeviceReply};
use crate::protocol::{CaptureOutcome, Cause, ErrorKind, Event, SurfaceState};

/// What the controller does with a device reply.
pub(super) enum Purpose {
    /// The event of the request.
    Reply,
    Recover,
    /// The setup is the baseline of a new device run.
    RunStart,
    /// The setup is compared with the baseline; the text says when.
    Check(String),
    /// [`Self::Check`] before the capture session with this serial passes.
    Verify(u64),
    Assign,
    Restore,
}

pub(super) enum Done {
    Device(Purpose, Box<Result<DeviceReply, DeviceError>>),
    Mapped {
        display: String,
        result: Result<DeviceId, String>,
    },
}

/// The profile change to restore.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Applied {
    #[default]
    None,
    /// Applied, or its restoration failed.
    Active {
        /// Of the profile that the readback selects; `None` after a
        /// failed assignment.
        selected: Option<ContentHash>,
    },
}

struct Run {
    device: DeviceId,
    baseline: DisplaySetup,
    /// The last read; it goes to the artifact environment.
    latest: DisplaySetup,
    failed: Option<String>,
}

#[derive(Default)]
pub(super) struct DeviceSide {
    /// Replies are handled from the front only.
    queue: VecDeque<LocalFuture<Done>>,
    /// [`DeviceCall::Recover`] is queued.
    recovering: bool,
    run: Option<Run>,
    /// Queued assignments and restorations.
    changes: usize,
    /// The device read after a profile change runs; `after_change` waits
    /// for it.
    refreshing: bool,
    after_change: Vec<Event>,
    applied: Applied,
    /// The host stops: no device read after a change, no more checks.
    stopping: bool,
}

impl DeviceSide {
    pub(super) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<Done> {
        let Some(front) = self.queue.front_mut() else {
            return Poll::Pending;
        };
        let done = std::task::ready!(front.as_mut().poll(cx));
        self.queue.pop_front();
        Poll::Ready(done)
    }

    /// `open` and `show` wait: a frame must not show the state before the
    /// change ends.
    pub(super) fn changing(&self) -> bool {
        self.changes > 0 || self.refreshing
    }

    pub(super) fn setup(&self) -> Option<&DisplaySetup> {
        self.run.as_ref().map(|r| &r.latest)
    }

    pub(super) fn selected(&self) -> Option<ContentHash> {
        match self.applied {
            Applied::Active { selected } => selected,
            Applied::None => None,
        }
    }

    fn active(&self) -> bool {
        self.applied != Applied::None
    }
}

pub(super) fn pending(frame: Option<u64>) -> Event {
    Event::error(
        frame,
        ErrorKind::ProfilePending,
        "a profile change is in progress; wait for its event",
    )
}

fn unexpected(reply: &DeviceReply) -> DeviceError {
    DeviceError::Native {
        api: "device worker".into(),
        code: None,
        message: format!("unexpected reply {reply:?}"),
    }
}

fn setup(result: Result<DeviceReply, DeviceError>) -> Result<DisplaySetup, DeviceError> {
    match result? {
        DeviceReply::Setup(s) => Ok(s),
        other => Err(unexpected(&other)),
    }
}

fn selection_hash(state: &ProfileState) -> Option<ContentHash> {
    match &state.selection {
        ProfileSelection::Override(p) => p.hash,
        _ => None,
    }
}

/// A profile fixture of use `ordinary`. The design installs `mhc2`
/// fixtures only in the Windows calibration tests, and never the adapter
/// cases.
fn ordinary_profile(id: &str) -> Result<IccProfile, Box<Event>> {
    let fixture_error =
        |message: String| Box::new(Event::error(None, ErrorKind::UnknownFixture, message));
    let manifest = manifest().map_err(|e| fixture_error(format!("profile manifest: {e}")))?;
    let entry = manifest
        .profiles
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| fixture_error(format!("no profile fixture {id}")))?;
    if entry.usage != ProfileUse::Ordinary {
        return Err(Box::new(Event::error(
            None,
            ErrorKind::DeviceUnsupported,
            format!("{id} is not a profile fixture of use `ordinary`"),
        )));
    }
    let bytes = entry
        .bytes()
        .map_err(|e| fixture_error(format!("{id}: {e}")))?;
    IccProfile::new(bytes).map_err(|e| Box::new(Event::device_error("profile fixture", &e)))
}

/// The event of a [`Purpose::Reply`].
fn reply_event(result: Result<DeviceReply, DeviceError>) -> Event {
    let reply = match result {
        Ok(reply) => reply,
        Err(e) => return Event::device_error("device request", &e),
    };
    match reply {
        DeviceReply::List(devices) => Event::DeviceList { devices },
        DeviceReply::Query(query) => Event::DeviceQuery { query },
        DeviceReply::Setup(setup) => Event::DeviceSetup {
            findings: setup.findings(),
            setup,
        },
        DeviceReply::Targets(targets) => Event::ProfileTargets { targets },
        DeviceReply::State(state) => Event::ProfileState { state },
        other => Event::device_error("device request", &unexpected(&other)),
    }
}

impl<D: OutputDriver + 'static, O: FnMut(&Event)> Machine<D, O> {
    /// Queues `call`, after [`DeviceCall::Recover`] if it is the first.
    pub(super) fn device_call(&mut self, call: DeviceCall, purpose: Purpose) {
        let Some(devices) = self.host.devices.as_mut() else {
            let e = Event::error(None, ErrorKind::DeviceUnsupported, NO_DEVICES);
            return self.emit(&e);
        };
        if !self.dev.recovering {
            self.dev.recovering = true;
            let f = devices.call(DeviceCall::Recover);
            self.dev.queue.push_back(Box::pin(async move {
                Done::Device(Purpose::Recover, Box::new(f.await))
            }));
        }
        let f = devices.call(call);
        self.dev.queue.push_back(Box::pin(
            async move { Done::Device(purpose, Box::new(f.await)) },
        ));
    }

    /// Through the hooks of the host, in the order of the device calls.
    pub(super) fn device_map(&mut self, text: &str) {
        let Some(display) = self.find(text) else {
            return;
        };
        let f = self.host.hooks.device(&display);
        let display = text.to_owned();
        self.dev.queue.push_back(Box::pin(async move {
            Done::Mapped {
                display,
                result: f.await,
            }
        }));
    }

    pub(super) fn device_run(&mut self, device: DeviceId) {
        if self.dev.changing() || self.dev.active() {
            let e = Event::error(
                None,
                ErrorKind::ProfilePending,
                "restore the profile change before a new device run",
            );
            return self.emit(&e);
        }
        self.device_call(DeviceCall::Setup(device), Purpose::RunStart);
    }

    pub(super) fn profile_assign(&mut self, target: ProfileTarget, id: &str) {
        if self.host.devices.is_none() {
            let e = Event::error(None, ErrorKind::DeviceUnsupported, NO_DEVICES);
            return self.emit(&e);
        }
        let profile = match ordinary_profile(id) {
            Ok(p) => p,
            Err(e) => return self.emit(&e),
        };
        if self
            .dev
            .run
            .as_ref()
            .is_none_or(|r| r.device != target.device)
        {
            let e = Event::error(
                None,
                ErrorKind::NoDeviceRun,
                format!("start a device run of {} first", target.device),
            );
            return self.emit(&e);
        }
        if self.dev.changing() || self.dev.active() {
            let e = Event::error(
                None,
                ErrorKind::ProfilePending,
                "restore the active profile change first",
            );
            return self.emit(&e);
        }
        self.change_starts();
        self.device_call(DeviceCall::Assign { target, profile }, Purpose::Assign);
    }

    pub(super) fn profile_restore(&mut self) {
        if self.host.devices.is_none() {
            let e = Event::error(None, ErrorKind::DeviceUnsupported, NO_DEVICES);
            return self.emit(&e);
        }
        if !(self.dev.active() || self.dev.changes > 0) {
            let e = Event::error(None, ErrorKind::NoProfileChange, "no profile change");
            return self.emit(&e);
        }
        self.change_starts();
        self.device_call(DeviceCall::Restore, Purpose::Restore);
    }

    /// Old frames, readiness and captures cannot qualify the new profile
    /// state: the frame and the capture end, and the surface is stale
    /// until the next `open`.
    fn change_starts(&mut self) {
        self.end_capture(Cause::Profile);
        self.cancel(Cause::Profile);
        self.current = None;
        if let Some(open) = self.open.as_mut().filter(|o| !o.stale) {
            open.stale = true;
            let event = surface_event(open, SurfaceState::Stale, Some(Cause::Profile));
            self.emit(&event);
        }
        self.dev.changes += 1;
    }

    /// Reads the setup of the device run again; a change fails the run.
    pub(super) fn check_run(&mut self, reason: String) {
        let Some(run) = self.dev.run.as_ref().filter(|r| r.failed.is_none()) else {
            return;
        };
        let device = run.device.clone();
        self.device_call(DeviceCall::Setup(device), Purpose::Check(reason));
    }

    /// The gate accepted a frame. With a device run, the setup is read
    /// before the capture passes.
    pub(super) fn accepted(&mut self) {
        let Some(run) = &self.dev.run else {
            return self.finish_capture(CaptureOutcome::Pass, String::new(), true);
        };
        if let Some(reason) = &run.failed {
            let reason = format!("the device run failed: {reason}");
            return self.finish_capture(CaptureOutcome::Fail, reason, true);
        }
        let device = run.device.clone();
        let c = self.capturing.as_mut().expect("a frame of a capture");
        c.verifying = true;
        c.frames = Box::pin(futures_util::stream::pending());
        c.heartbeat = None;
        let serial = c.serial;
        self.device_call(DeviceCall::Setup(device), Purpose::Verify(serial));
    }

    pub(super) fn device_done(&mut self, done: Done) {
        let (purpose, result) = match done {
            Done::Mapped { display, result } => {
                let e = match result {
                    Ok(device) => Event::DeviceMap { display, device },
                    Err(reason) => Event::error(None, ErrorKind::NotLocal, reason),
                };
                return self.emit(&e);
            }
            Done::Device(purpose, result) => (purpose, *result),
        };
        match purpose {
            Purpose::Reply => self.emit(&reply_event(result)),
            Purpose::Recover => self.recovered(result),
            Purpose::RunStart => match setup(result) {
                Ok(s) => self.start_run(s),
                Err(e) => self.emit(&Event::device_error("device run", &e)),
            },
            Purpose::Check(reason) => self.checked(setup(result), &reason),
            Purpose::Verify(serial) => {
                self.checked(setup(result), "before the capture passed");
                self.verified(serial);
            }
            Purpose::Assign => self.assigned(result),
            Purpose::Restore => self.restored(result),
        }
    }

    fn recovered(&mut self, result: Result<DeviceReply, DeviceError>) {
        let records = match result {
            Ok(DeviceReply::Recovered(records)) => records,
            Ok(other) => return self.emit(&Event::device_error("recovery", &unexpected(&other))),
            Err(e) => return self.emit(&Event::device_error("recovery", &e)),
        };
        for r in records {
            let (state, error) = match r.result {
                Ok(state) => (Some(state), None),
                Err(e) => (None, Some(e.to_string())),
            };
            self.emit(&Event::Recovery {
                record: r.record,
                state,
                error,
            });
        }
    }

    /// **Conservative rule:** a `competing` finding fails the run at once.
    fn start_run(&mut self, setup: DisplaySetup) {
        let findings = setup.findings();
        let competing: Vec<&str> = findings
            .iter()
            .filter_map(|f| match f {
                SetupFinding::Competing { detail, .. } => Some(detail.as_str()),
                SetupFinding::Unknown { .. } => None,
            })
            .collect();
        let reason = (!competing.is_empty())
            .then(|| format!("competing display setup: {}", competing.join("; ")));
        self.dev.run = Some(Run {
            device: setup.device.clone(),
            baseline: setup.clone(),
            latest: setup.clone(),
            failed: None,
        });
        self.emit(&Event::DeviceRun { setup, findings });
        if let Some(reason) = reason {
            self.fail_run(reason, Vec::new());
        }
    }

    fn fail_run(&mut self, reason: String, changes: Vec<SetupField>) {
        let Some(run) = self.dev.run.as_mut().filter(|r| r.failed.is_none()) else {
            return;
        };
        run.failed = Some(reason.clone());
        let device = run.device.clone();
        self.emit(&Event::RunFailed {
            device,
            reason,
            changes,
        });
    }

    /// A read of another device, for example of an earlier run, does not
    /// count.
    fn checked(&mut self, result: Result<DisplaySetup, DeviceError>, when: &str) {
        let Some(run) = self.dev.run.as_mut() else {
            return;
        };
        match result {
            Ok(s) => {
                let Ok(changes) = run.baseline.changes(&s) else {
                    return;
                };
                run.latest = s;
                if !changes.is_empty() {
                    self.fail_run(format!("the display setup changed {when}"), changes);
                }
            }
            Err(e) => self.fail_run(
                format!("the display setup read {when} failed: {e}"),
                Vec::new(),
            ),
        }
    }

    fn verified(&mut self, serial: u64) {
        if !self
            .capturing
            .as_ref()
            .is_some_and(|c| c.serial == serial && c.verifying)
        {
            return;
        }
        match self.dev.run.as_ref().and_then(|r| r.failed.clone()) {
            Some(reason) => {
                let reason = format!("the device run failed: {reason}");
                self.finish_capture(CaptureOutcome::Fail, reason, true);
            }
            None => self.finish_capture(CaptureOutcome::Pass, String::new(), true),
        }
    }

    fn assigned(&mut self, result: Result<DeviceReply, DeviceError>) {
        self.dev.changes -= 1;
        let mut events = Vec::new();
        match result {
            Ok(DeviceReply::Assigned(a)) => {
                // A record exists: restore after success and after failure.
                self.dev.applied = Applied::Active { selected: None };
                match a.applied {
                    Ok(state) => {
                        self.dev.applied = Applied::Active {
                            selected: selection_hash(&state),
                        };
                        events.push(Event::ProfileAssigned { state });
                    }
                    Err(e) => events.push(Event::device_error("profile assignment", &e)),
                }
                match a.restored {
                    Some(Ok(r)) => {
                        self.dev.applied = Applied::None;
                        events.push(Event::ProfileRestored {
                            state: r.state,
                            record_kept: r.record_kept,
                        });
                    }
                    Some(Err(e)) => events.push(Event::device_error("profile restoration", &e)),
                    None => {}
                }
            }
            Ok(other) => {
                events.push(Event::device_error(
                    "profile assignment",
                    &unexpected(&other),
                ));
            }
            // No record was saved, so nothing changed.
            Err(e) => events.push(Event::device_error("profile assignment", &e)),
        }
        self.after_change(events);
    }

    fn restored(&mut self, result: Result<DeviceReply, DeviceError>) {
        self.dev.changes -= 1;
        let event = match result {
            Ok(DeviceReply::Restored(r)) => {
                self.dev.applied = Applied::None;
                Event::ProfileRestored {
                    state: r.state,
                    record_kept: r.record_kept,
                }
            }
            Ok(other) => Event::device_error("profile restoration", &unexpected(&other)),
            Err(e) => Event::device_error("profile restoration", &e),
        };
        self.after_change(vec![event]);
    }

    /// Checks the setup and reads the display state again. The events of
    /// the change follow that read, so that `open` after them queries the
    /// new state.
    fn after_change(&mut self, events: Vec<Event>) {
        if self.dev.stopping {
            return events.iter().for_each(|e| self.emit(e));
        }
        self.check_run("after the profile change".into());
        self.dev.after_change.extend(events);
        self.dev.refreshing = true;
        self.refreshing = Some(self.host.hooks.refresh());
    }

    /// The device read after a change ended.
    pub(super) fn refreshed(&mut self) {
        if !self.dev.refreshing {
            return;
        }
        self.dev.refreshing = false;
        for e in std::mem::take(&mut self.dev.after_change) {
            self.emit(&e);
        }
    }

    /// Handles each queued call, then restores an active change.
    pub(super) async fn stop_devices(&mut self) {
        self.dev.stopping = true;
        self.refreshed();
        self.drain().await;
        if self.dev.active() {
            self.dev.changes += 1;
            self.device_call(DeviceCall::Restore, Purpose::Restore);
            self.drain().await;
        }
    }

    async fn drain(&mut self) {
        while let Some(f) = self.dev.queue.pop_front() {
            let done = f.await;
            self.device_done(done);
        }
    }
}
