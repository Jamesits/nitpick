//! Device scenarios of the shared host contract: call order and
//! recovery, the device run and its setup checks, profile changes and
//! their effect on frames, captures and the surface.

use nitpick_core::device::{
    ContentHash, DeviceInfo, DisplaySetup, GammaRamp, ProfileRef, ProfileRole, ProfileScope,
    ProfileSelection, ProfileState, ProfileTarget, Report, SetupField, SetupFinding, UserScope,
};

use super::*;
use crate::device::{Assigned, RecoveredRecord, Restored};

pub(super) const SCENARIOS: &[Scenario] = scenarios![
    device_requests_recover_first_and_reply_in_order,
    device_map_gives_the_local_device,
    profile_assign_needs_a_fixture_and_a_run,
    profile_change_ends_frames_and_needs_open,
    failed_assignment_is_restored,
    timed_out_assignment_reports_the_kept_record,
    restoration_waits_for_the_assignment,
    quit_restores_the_active_change,
    quit_waits_for_a_queued_assignment,
    competing_baseline_fails_the_run,
    setup_change_fails_the_run,
    capture_passes_after_an_unchanged_setup,
    capture_fails_after_a_setup_change,
];

pub(super) const WITHOUT_DEVICES: &[Scenario] =
    scenarios![device_requests_without_device_control_are_unsupported];

const ORDINARY: &str = "ordinary_srgb";
const MHC2: &str = "mhc2_identity";

pub(super) fn device_id() -> DeviceId {
    DeviceId {
        backend: "mock".into(),
        key: "device-1".into(),
    }
}

fn target() -> ProfileTarget {
    ProfileTarget {
        device: device_id(),
        role: ProfileRole::Standard,
        scope: ProfileScope {
            user: UserScope::Current,
            host: None,
        },
    }
}

fn clean_setup() -> DisplaySetup {
    let off = || Report::known(false, "mock");
    DisplaySetup {
        device: device_id(),
        gamma_ramp: Report::known(GammaRamp::Identity, "mock"),
        os_calibration_loader: off(),
        external_calibration_loader: off(),
        gpu_gamut_clamp: off(),
        dithering: off(),
    }
}

fn changed_setup() -> DisplaySetup {
    DisplaySetup {
        gamma_ramp: Report::known(GammaRamp::Custom { digest: 7 }, "mock"),
        ..clean_setup()
    }
}

fn state(selected: Option<u8>) -> ProfileState {
    let profile = selected.map(|b| ProfileRef {
        native_id: format!("profile-{b}"),
        hash: Some(ContentHash([b; 32])),
    });
    ProfileState {
        target: target(),
        associations: Report::known(profile.iter().cloned().collect(), "mock"),
        selection: profile.map_or(ProfileSelection::NoOverride, ProfileSelection::Override),
        applied_scope: None,
        revision: 0,
    }
}

fn applied(selected: u8) -> DeviceReply {
    DeviceReply::Assigned(Assigned {
        applied: Ok(state(Some(selected))),
        restored: None,
    })
}

fn restored(record_kept: bool) -> DeviceReply {
    DeviceReply::Restored(Restored {
        state: state(None),
        record_kept,
    })
}

fn assign() -> Request {
    Request::ProfileAssign {
        target: target(),
        profile: ORDINARY.into(),
    }
}

fn device_error(event: &Event) -> (ErrorKind, Option<crate::protocol::DeviceErrorKind>) {
    match event {
        Event::Error {
            kind, device_error, ..
        } => (*kind, *device_error),
        other => panic!("expected `error`, got {other:?}"),
    }
}

impl Harness {
    fn device_calls(&self) -> Vec<DeviceCall> {
        self.devices
            .borrow()
            .iter()
            .map(|(c, _)| c.clone())
            .collect()
    }

    /// Waits for the `n`-th device call.
    async fn device_call(&self, n: usize) -> DeviceCall {
        self.wait("device call", |h| h.devices.borrow().len() > n)
            .await;
        self.devices.borrow()[n].0.clone()
    }

    /// Completes the `n`-th device call. `false`: its future is gone.
    fn device_reply(&self, n: usize, result: DeviceResult) -> bool {
        let tx = self.devices.borrow_mut()[n]
            .1
            .take()
            .expect("one reply per call");
        tx.send(result).is_ok()
    }

    /// Completes the last device read of the hooks.
    fn finish_refresh(&self) {
        let tx = self.refreshes.borrow_mut().pop().expect("a device read");
        tx.send(()).expect("device read pending");
    }

    /// Starts a device run of [`device_id`] with `setup` as its first
    /// device request: the recovery comes first.
    async fn run_started(&mut self, setup: DisplaySetup) {
        self.send(&Request::DeviceRun {
            device: device_id(),
        });
        assert_eq!(self.device_call(0).await, DeviceCall::Recover);
        assert_eq!(self.device_call(1).await, DeviceCall::Setup(device_id()));
        assert!(self.device_reply(0, Ok(DeviceReply::Recovered(Vec::new()))));
        assert!(self.device_reply(1, Ok(DeviceReply::Setup(setup.clone()))));
        self.expect(Event::DeviceRun {
            findings: setup.findings(),
            setup,
        })
        .await;
    }

    /// Completes the setup check and the device read after a profile
    /// change; the check is call `check`. The events of the change follow.
    async fn change_settles(&mut self, check: usize) {
        assert_eq!(
            self.device_call(check).await,
            DeviceCall::Setup(device_id())
        );
        assert!(self.device_reply(check, Ok(DeviceReply::Setup(clean_setup()))));
        self.finish_refresh();
        assert!(matches!(self.next().await, Event::Displays { .. }));
    }

    /// A run and an applied change of the profile `selected`. Device calls
    /// 0 to 3 are used.
    async fn assigned(&mut self, selected: u8) {
        self.run_started(clean_setup()).await;
        self.send(&assign());
        assert!(matches!(
            self.device_call(2).await,
            DeviceCall::Assign { target: t, .. } if t == target()
        ));
        assert!(self.device_reply(2, Ok(applied(selected))));
        self.change_settles(3).await;
        self.expect(Event::ProfileAssigned {
            state: state(Some(selected)),
        })
        .await;
    }
}

async fn device_requests_recover_first_and_reply_in_order(mut h: Harness) {
    h.send(&Request::DeviceList {});
    h.send(&Request::DeviceMap {
        display: DISPLAY.into(),
    });
    assert_eq!(h.device_call(1).await, DeviceCall::List);
    assert_eq!(h.device_calls(), [DeviceCall::Recover, DeviceCall::List]);
    let info = DeviceInfo {
        id: device_id(),
        name: "Mock device".into(),
        evidence: Vec::new(),
    };
    assert!(h.device_reply(1, Ok(DeviceReply::List(vec![info.clone()]))));
    // The list waits for the recovery; the map waits for the list.
    h.sync().await;
    let kept = DeviceError::StateConflict("changed by another actor".into());
    let records = vec![
        RecoveredRecord {
            record: "profile-a.json".into(),
            result: Ok(state(None)),
        },
        RecoveredRecord {
            record: "profile-b.json".into(),
            result: Err(kept.clone()),
        },
    ];
    assert!(h.device_reply(0, Ok(DeviceReply::Recovered(records))));
    h.expect(Event::Recovery {
        record: "profile-a.json".into(),
        state: Some(state(None)),
        error: None,
    })
    .await;
    h.expect(Event::Recovery {
        record: "profile-b.json".into(),
        state: None,
        error: Some(kept.to_string()),
    })
    .await;
    h.expect(Event::DeviceList {
        devices: vec![info],
    })
    .await;
    h.expect(Event::DeviceMap {
        display: DISPLAY.into(),
        device: device_id(),
    })
    .await;
    // Only the first request recovers.
    h.send(&Request::DeviceQuery {
        device: device_id(),
    });
    assert_eq!(h.device_call(2).await, DeviceCall::Query(device_id()));
    assert!(h.device_reply(2, Err(DeviceError::Disconnected(device_id()))));
    assert_eq!(
        device_error(&h.next().await),
        (
            ErrorKind::Device,
            Some(crate::protocol::DeviceErrorKind::Disconnected)
        )
    );
}

async fn device_map_gives_the_local_device(mut h: Harness) {
    h.send(&Request::DeviceMap {
        display: "mock:other".into(),
    });
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::UnknownDisplay)
    );
    h.mock.borrow_mut().not_local = true;
    h.send(&Request::DeviceMap {
        display: DISPLAY.into(),
    });
    assert_eq!(error_kind(&h.next().await), (None, ErrorKind::NotLocal));
    // Mapping is not a device call.
    assert_eq!(h.device_calls(), Vec::<DeviceCall>::new());
}

async fn profile_assign_needs_a_fixture_and_a_run(mut h: Harness) {
    let request = |profile: &str| Request::ProfileAssign {
        target: target(),
        profile: profile.into(),
    };
    h.send(&request("no-such-profile"));
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::UnknownFixture)
    );
    h.send(&request(MHC2));
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::DeviceUnsupported)
    );
    h.send(&request(ORDINARY));
    assert_eq!(error_kind(&h.next().await), (None, ErrorKind::NoDeviceRun));
    h.send(&Request::ProfileRestore {});
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::NoProfileChange)
    );
    h.run_started(clean_setup()).await;
    let mut other = target();
    other.device.key = "device-2".into();
    h.send(&Request::ProfileAssign {
        target: other,
        profile: ORDINARY.into(),
    });
    assert_eq!(error_kind(&h.next().await), (None, ErrorKind::NoDeviceRun));
    h.sync().await;
    assert_eq!(h.device_calls().len(), 2);
}

async fn profile_change_ends_frames_and_needs_open(mut h: Harness) {
    h.adapter(100);
    h.open().await;
    h.run_started(clean_setup()).await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.start_capture(&capture(1)).await;
    h.send(&assign());
    h.expect(capture::cancelled(1, Cause::Profile)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Profile,
    })
    .await;
    h.expect(surface(1, SurfaceState::Stale, Cause::Profile))
        .await;
    assert_eq!(h.sessions_live(), 0);
    // Nothing shows the state before the change ends.
    h.send(&Request::Open {
        display: DISPLAY.into(),
        depth: 10,
    });
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::ProfilePending)
    );
    h.send(&Request::Show {
        frame: 2,
        fixture: FRAME_B.into(),
        delay_ms: None,
        metadata: None,
        marker: true,
    });
    assert_eq!(
        error_kind(&h.next().await),
        (Some(2), ErrorKind::ProfilePending)
    );
    h.send(&capture(1));
    assert_eq!(
        error_kind(&h.next().await),
        (Some(1), ErrorKind::StaleFrame)
    );
    h.send(&Request::DeviceRun {
        device: device_id(),
    });
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::ProfilePending)
    );
    assert!(!h.fire(0), "the delay timer is dropped");

    assert!(h.device_reply(2, Ok(applied(1))));
    // The events wait for the device read after the change.
    h.sync().await;
    h.change_settles(3).await;
    h.expect(Event::ProfileAssigned {
        state: state(Some(1)),
    })
    .await;
    h.send(&Request::Open {
        display: DISPLAY.into(),
        depth: 10,
    });
    h.expect(surface(1, SurfaceState::Closed, Cause::Opened))
        .await;
    h.expect(open_surface(2, SIZE)).await;
    h.show(3, FRAME_B, Some(0)).await;
    assert!(h.reply(1, Ok("ack 3".into())));
    h.expect(submitted(3, 2, 0)).await;
    h.expect(Event::Ready {
        frame: 3,
        delay_ms: 0,
    })
    .await;
    // An applied change is restored with the next one only.
    h.send(&assign());
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::ProfilePending)
    );
}

async fn failed_assignment_is_restored(mut h: Harness) {
    h.run_started(clean_setup()).await;
    h.send(&assign());
    h.device_call(2).await;
    let conflict = DeviceError::StateConflict("readback".into());
    let reply = Assigned {
        applied: Err(conflict),
        restored: Some(Ok(Restored {
            state: state(None),
            record_kept: false,
        })),
    };
    assert!(h.device_reply(2, Ok(DeviceReply::Assigned(reply))));
    h.change_settles(3).await;
    assert_eq!(
        device_error(&h.next().await),
        (
            ErrorKind::Device,
            Some(crate::protocol::DeviceErrorKind::StateConflict)
        )
    );
    h.expect(Event::ProfileRestored {
        state: state(None),
        record_kept: false,
    })
    .await;
    h.send(&Request::ProfileRestore {});
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::NoProfileChange)
    );
    // A failed restoration keeps the change active.
    h.send(&assign());
    h.device_call(4).await;
    let reply = Assigned {
        applied: Err(DeviceError::Timeout),
        restored: Some(Err(DeviceError::StateConflict("external".into()))),
    };
    assert!(h.device_reply(4, Ok(DeviceReply::Assigned(reply))));
    h.change_settles(5).await;
    assert_eq!(
        device_error(&h.next().await).1,
        Some(crate::protocol::DeviceErrorKind::Timeout)
    );
    assert_eq!(
        device_error(&h.next().await).1,
        Some(crate::protocol::DeviceErrorKind::StateConflict)
    );
    h.send(&Request::ProfileRestore {});
    assert_eq!(h.device_call(6).await, DeviceCall::Restore);
}

async fn timed_out_assignment_reports_the_kept_record(mut h: Harness) {
    h.run_started(clean_setup()).await;
    h.send(&assign());
    h.device_call(2).await;
    let reply = Assigned {
        applied: Err(DeviceError::Timeout),
        restored: Some(Ok(Restored {
            state: state(None),
            record_kept: true,
        })),
    };
    assert!(h.device_reply(2, Ok(DeviceReply::Assigned(reply))));
    h.change_settles(3).await;
    assert_eq!(
        device_error(&h.next().await).1,
        Some(crate::protocol::DeviceErrorKind::Timeout)
    );
    h.expect(Event::ProfileRestored {
        state: state(None),
        record_kept: true,
    })
    .await;
    // The kept record blocks the next change of the device until the
    // next recovery; the worker gives the conflict before any write.
    h.send(&assign());
    h.device_call(4).await;
    assert!(h.device_reply(
        4,
        Err(DeviceError::StateConflict("unresolved record".into()))
    ));
    h.change_settles(5).await;
    assert_eq!(
        device_error(&h.next().await).1,
        Some(crate::protocol::DeviceErrorKind::StateConflict)
    );
}

/// A restoration during the assignment is handled after it: the worker
/// runs it after the assignment call returns.
async fn restoration_waits_for_the_assignment(mut h: Harness) {
    h.run_started(clean_setup()).await;
    h.send(&assign());
    h.send(&Request::ProfileRestore {});
    assert_eq!(h.device_call(3).await, DeviceCall::Restore);
    assert!(h.device_reply(3, Ok(restored(false))));
    h.sync().await;
    assert!(h.device_reply(2, Ok(applied(1))));
    // One check after each change; the second device read replaces the
    // first.
    for n in [4, 5] {
        assert_eq!(h.device_call(n).await, DeviceCall::Setup(device_id()));
        assert!(h.device_reply(n, Ok(DeviceReply::Setup(clean_setup()))));
    }
    h.finish_refresh();
    assert!(matches!(h.next().await, Event::Displays { .. }));
    h.expect(Event::ProfileAssigned {
        state: state(Some(1)),
    })
    .await;
    h.expect(Event::ProfileRestored {
        state: state(None),
        record_kept: false,
    })
    .await;
    h.send(&Request::ProfileRestore {});
    assert_eq!(
        error_kind(&h.next().await),
        (None, ErrorKind::NoProfileChange)
    );
}

async fn quit_restores_the_active_change(mut h: Harness) {
    h.assigned(1).await;
    h.send(&Request::Quit {});
    assert_eq!(h.device_call(4).await, DeviceCall::Restore);
    assert!(h.device_reply(4, Ok(restored(false))));
    h.expect(Event::ProfileRestored {
        state: state(None),
        record_kept: false,
    })
    .await;
    h.expect(Event::Stopped { error: None }).await;
}

async fn quit_waits_for_a_queued_assignment(mut h: Harness) {
    h.run_started(clean_setup()).await;
    h.send(&assign());
    h.device_call(2).await;
    h.send(&Request::Quit {});
    assert!(h.device_reply(2, Ok(applied(1))));
    h.expect(Event::ProfileAssigned {
        state: state(Some(1)),
    })
    .await;
    // No setup check while the host stops.
    assert_eq!(h.device_call(3).await, DeviceCall::Restore);
    assert!(h.device_reply(3, Ok(restored(false))));
    h.expect(Event::ProfileRestored {
        state: state(None),
        record_kept: false,
    })
    .await;
    h.expect(Event::Stopped { error: None }).await;
}

async fn competing_baseline_fails_the_run(mut h: Harness) {
    let setup = DisplaySetup {
        os_calibration_loader: Report::known(true, "mock"),
        dithering: Report::unknown("not read"),
        ..clean_setup()
    };
    let findings = setup.findings();
    assert!(matches!(findings[0], SetupFinding::Competing { .. }));
    h.run_started(setup).await;
    let Event::RunFailed {
        device,
        reason,
        changes,
    } = h.next().await
    else {
        panic!("expected `run_failed`");
    };
    assert_eq!(device, device_id());
    assert!(reason.contains("OS calibration loader is on"), "{reason}");
    assert_eq!(changes, Vec::<SetupField>::new());
    // A failed run is not checked again.
    h.event(DriverEvent::SystemChanged {
        reason: "resume".into(),
    });
    h.sync().await;
    assert_eq!(h.device_calls().len(), 2);
}

async fn setup_change_fails_the_run(mut h: Harness) {
    h.run_started(clean_setup()).await;
    h.event(DriverEvent::SystemChanged {
        reason: "an HDR change".into(),
    });
    assert_eq!(h.device_call(2).await, DeviceCall::Setup(device_id()));
    assert!(h.device_reply(2, Ok(DeviceReply::Setup(clean_setup()))));
    h.sync().await;
    h.event(DriverEvent::SystemChanged {
        reason: "resume".into(),
    });
    h.device_call(3).await;
    assert!(h.device_reply(3, Ok(DeviceReply::Setup(changed_setup()))));
    h.expect(Event::RunFailed {
        device: device_id(),
        reason: "the display setup changed after resume".into(),
        changes: vec![SetupField::GammaRamp],
    })
    .await;
    // A setup read alone does not change the run.
    h.send(&Request::DeviceSetup {
        device: device_id(),
    });
    h.device_call(4).await;
    assert!(h.device_reply(4, Ok(DeviceReply::Setup(clean_setup()))));
    h.expect(Event::DeviceSetup {
        setup: clean_setup(),
        findings: Vec::new(),
    })
    .await;
    // A new run starts with a new baseline.
    h.send(&Request::DeviceRun {
        device: device_id(),
    });
    h.device_call(5).await;
    assert!(h.device_reply(5, Ok(DeviceReply::Setup(changed_setup()))));
    h.expect(Event::DeviceRun {
        findings: changed_setup().findings(),
        setup: changed_setup(),
    })
    .await;
    assert!(matches!(h.next().await, Event::RunFailed { .. }));
}

/// Accepts three frames of frame 1 after a run; the setup read follows.
async fn verify(h: &mut Harness, call: usize) {
    h.adapter(100);
    h.open().await;
    h.marked(1, FRAME_A, 1, 0).await;
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 0,
    })
    .await;
    h.accept(1, 0).await;
    assert_eq!(h.device_call(call).await, DeviceCall::Setup(device_id()));
    // The session ended; the capture waits for the read.
    assert_eq!(h.sessions_live(), 0);
}

async fn capture_passes_after_an_unchanged_setup(mut h: Harness) {
    h.run_started(clean_setup()).await;
    verify(&mut h, 2).await;
    assert!(h.device_reply(2, Ok(DeviceReply::Setup(clean_setup()))));
    h.expect(captured(1, CaptureOutcome::Pass, None, Some("artifact-1")))
        .await;
    {
        let sink = h.sink.borrow();
        let env = &sink.submitted[0].run.environment;
        assert_eq!(env.display_setup, Some(clean_setup()));
        assert_eq!(env.profile_sha256, None);
        assert_eq!(env.os.as_deref(), Some("mock os"));
    }
    h.finish_saved(&["artifact-1"]).await;
}

async fn capture_fails_after_a_setup_change(mut h: Harness) {
    h.assigned(3).await;
    verify(&mut h, 4).await;
    assert!(h.device_reply(4, Ok(DeviceReply::Setup(changed_setup()))));
    let reason = "the display setup changed before the capture passed";
    h.expect(Event::RunFailed {
        device: device_id(),
        reason: reason.into(),
        changes: vec![SetupField::GammaRamp],
    })
    .await;
    let failed = format!("the device run failed: {reason}");
    h.expect(captured(
        1,
        CaptureOutcome::Fail,
        Some(&failed),
        Some("artifact-1"),
    ))
    .await;
    {
        let sink = h.sink.borrow();
        let env = &sink.submitted[0].run.environment;
        assert_eq!(env.display_setup, Some(changed_setup()));
        assert_eq!(env.profile_sha256, Some(ContentHash([3; 32]).to_string()));
    }
    // A failed run fails the next capture without a read.
    h.marked(2, FRAME_B, 1, 0).await;
    h.expect(Event::Ready {
        frame: 2,
        delay_ms: 0,
    })
    .await;
    h.accept(2, 1).await;
    h.expect(captured(
        2,
        CaptureOutcome::Fail,
        Some(&failed),
        Some("artifact-2"),
    ))
    .await;
    assert_eq!(h.device_calls().len(), 5);
    h.finish_saved(&["artifact-1", "artifact-2"]).await;
}

async fn device_requests_without_device_control_are_unsupported(mut h: Harness) {
    for request in [
        Request::DeviceList {},
        Request::DeviceRun {
            device: device_id(),
        },
        assign(),
        Request::ProfileRestore {},
    ] {
        h.send(&request);
        assert_eq!(
            error_kind(&h.next().await),
            (None, ErrorKind::DeviceUnsupported)
        );
    }
    // The mapping needs no device control.
    h.send(&Request::DeviceMap {
        display: DISPLAY.into(),
    });
    h.expect(Event::DeviceMap {
        display: DISPLAY.into(),
        device: device_id(),
    })
    .await;
}
