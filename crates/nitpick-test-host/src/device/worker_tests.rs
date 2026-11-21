use nitpick_core::device::{DeviceError, IccProfile, ProfileSelection, ProfileTarget};
use nitpick_display::fake::{FakeDisplay, standard_target};
use nitpick_display::restoration::ProfileRun;
use nitpick_display::store::RecoveryStore;

use super::WorkerDevices;
use crate::device::{DeviceCall, DeviceReply, Devices};

fn icc(tag: u8) -> IccProfile {
    let mut b = vec![0u8; 132];
    b[..4].copy_from_slice(&132u32.to_be_bytes());
    b[36..40].copy_from_slice(b"acsp");
    b[131] = tag;
    IccProfile::new(b).unwrap()
}

fn block_on<T>(f: crate::LocalFuture<T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

fn call(d: &mut WorkerDevices, c: DeviceCall) -> Result<DeviceReply, DeviceError> {
    block_on(d.call(c))
}

/// An earlier process saved and applied a change, then ended.
fn crashed_run(dir: &std::path::Path) -> (FakeDisplay, ProfileTarget) {
    let mut fake = FakeDisplay::new();
    let target = standard_target(&fake.add_device("a"));
    let store = RecoveryStore::open(dir).unwrap();
    let mut run = ProfileRun::begin(&mut fake, &store, &target, &icc(1)).unwrap();
    run.apply(&mut fake).unwrap();
    (fake, target)
}

#[test]
fn the_first_call_starts_the_worker_and_recovers() {
    let dir = tempfile::tempdir().unwrap();
    let (fake, target) = crashed_run(dir.path());
    let mut d = WorkerDevices::new(move || Box::new(fake), dir.path().into());
    let Ok(DeviceReply::Recovered(records)) = call(&mut d, DeviceCall::Recover) else {
        panic!("expected the recovery");
    };
    assert_eq!(records.len(), 1);
    let state = records[0].result.clone().unwrap();
    assert_eq!(state.selection, ProfileSelection::NoOverride);
    assert!(records[0].record.starts_with("profile-"));
    // Only the first call recovers.
    assert_eq!(
        call(&mut d, DeviceCall::Recover),
        Ok(DeviceReply::Recovered(Vec::new()))
    );
    let Ok(DeviceReply::State(now)) = call(&mut d, DeviceCall::State(target)) else {
        panic!("expected the state");
    };
    assert_eq!(now.selection, ProfileSelection::NoOverride);
}

#[test]
fn assigns_and_restores_through_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = FakeDisplay::new();
    let target = standard_target(&fake.add_device("a"));
    let mut d = WorkerDevices::new(move || Box::new(fake), dir.path().into());
    assert_eq!(
        call(&mut d, DeviceCall::Recover),
        Ok(DeviceReply::Recovered(Vec::new()))
    );
    let Ok(DeviceReply::Assigned(a)) = call(
        &mut d,
        DeviceCall::Assign {
            target: target.clone(),
            profile: icc(2),
        },
    ) else {
        panic!("expected the assignment");
    };
    assert!(a.applied.is_ok());
    assert_eq!(a.restored, None);
    let Ok(DeviceReply::Restored(r)) = call(&mut d, DeviceCall::Restore) else {
        panic!("expected the restoration");
    };
    assert!(!r.record_kept);
    assert_eq!(r.state.selection, ProfileSelection::NoOverride);
    assert!(matches!(
        call(&mut d, DeviceCall::Restore),
        Err(DeviceError::Unsupported { .. })
    ));
}

#[test]
fn reads_map_to_their_replies() {
    let dir = tempfile::tempdir().unwrap();
    let mut fake = FakeDisplay::new();
    let id = fake.add_device("a");
    let mut d = WorkerDevices::new(move || Box::new(fake), dir.path().into());
    let Ok(DeviceReply::List(devices)) = call(&mut d, DeviceCall::List) else {
        panic!("expected the list");
    };
    assert_eq!(devices[0].id, id);
    assert!(matches!(
        call(&mut d, DeviceCall::Query(id.clone())),
        Ok(DeviceReply::Query(_))
    ));
    assert!(matches!(
        call(&mut d, DeviceCall::Setup(id.clone())),
        Ok(DeviceReply::Setup(s)) if s.device == id
    ));
    assert_eq!(
        call(&mut d, DeviceCall::Targets(id.clone())),
        Ok(DeviceReply::Targets(vec![standard_target(&id)]))
    );
}
