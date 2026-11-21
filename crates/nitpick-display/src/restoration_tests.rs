use nitpick_core::device::{
    ContentHash, DeviceError, DeviceId, IccProfile, ProfileRef, ProfileScope, ProfileSelection,
    ProfileState, ProfileTarget, Report, UserScope,
};
use tempfile::TempDir;

use super::{Phase, ProfileRun, Restoration, check_original, recover};
use crate::fake::{FakeDisplay, Fault, Step, standard_target};
use crate::store::{RecordKey, RecoveryStore};

fn icc(tag: u8) -> IccProfile {
    let mut b = vec![0u8; 132];
    b[..4].copy_from_slice(&132u32.to_be_bytes());
    b[36..40].copy_from_slice(b"acsp");
    b[131] = tag;
    IccProfile::new(b).unwrap()
}

/// The fake is the OS state; it outlives a simulated process. The store
/// directory also outlives it.
struct World {
    fake: FakeDisplay,
    id: DeviceId,
    target: ProfileTarget,
    dir: TempDir,
}

fn world() -> World {
    let mut fake = FakeDisplay::new();
    let id = fake.add_device("a");
    World {
        target: standard_target(&id),
        fake,
        id,
        dir: tempfile::tempdir().unwrap(),
    }
}

impl World {
    fn store(&self) -> RecoveryStore {
        RecoveryStore::open(self.dir.path()).unwrap()
    }

    fn begin(&mut self, store: &RecoveryStore) -> ProfileRun {
        ProfileRun::begin(&mut self.fake, store, &self.target, &icc(1)).unwrap()
    }

    fn state(&self) -> ProfileState {
        self.fake.peek(&self.target)
    }
}

fn user_profile() -> (&'static str, Vec<u8>) {
    ("user.icc", icc(9).bytes().to_vec())
}

fn assert_restored(w: &World, original: &ProfileState, store: &RecoveryStore) {
    check_original(original, &w.state()).unwrap();
    assert_eq!(store.records().unwrap(), vec![]);
}

#[test]
fn record_precedes_the_first_write() {
    let mut w = world();
    let store = w.store();
    let run = w.begin(&store);
    assert_eq!(w.fake.writes(), &[] as &[String]);
    assert_eq!(*run.phase(), Phase::Saved);
    let records = store.records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].record.as_ref(), Ok(run.change()));
}

#[test]
fn success_without_prior_override() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    assert_eq!(original.selection, ProfileSelection::NoOverride);

    let set = run.apply(&mut w.fake).unwrap();
    assert!(matches!(&set.selection,
        ProfileSelection::Override(p) if p.hash == Some(icc(1).hash())));
    assert_eq!(w.fake.files().len(), 1);

    let r = run.restore(&mut w.fake, &store).unwrap();
    assert!(!r.record_kept);
    assert!(matches!(run.phase(), Phase::Restored(_)));
    assert_restored(&w, &original, &store);
    assert_eq!(w.fake.files(), Vec::<String>::new());
}

#[test]
fn prior_override_returns_and_user_file_stays() {
    let mut w = world();
    let (name, bytes) = user_profile();
    w.fake.select_external(&w.target, name, &bytes);
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    run.apply(&mut w.fake).unwrap();
    run.restore(&mut w.fake, &store).unwrap();
    assert_restored(&w, &original, &store);
    assert_eq!(w.fake.files(), vec![name.to_owned()]);
}

#[test]
fn partial_failure_at_each_step_is_restored() {
    for step in [Step::Install, Step::Associate, Step::Select] {
        let mut w = world();
        let store = w.store();
        let mut run = w.begin(&store);
        let original = run.change().original.clone();
        let error = DeviceError::Native {
            api: format!("{step:?}"),
            code: Some(5),
            message: "injected".into(),
        };
        w.fake.inject(step, Fault::Fail(error.clone()));
        assert_eq!(run.apply(&mut w.fake), Err(error.clone()), "{step:?}");
        assert_eq!(*run.phase(), Phase::ApplyFailed(error));
        let r = run.restore(&mut w.fake, &store).unwrap();
        assert!(!r.record_kept, "{step:?}");
        assert_restored(&w, &original, &store);
        assert_eq!(w.fake.files(), Vec::<String>::new(), "{step:?}");
    }
}

/// A timed-out write can still take effect, so the record stays after a
/// verified restoration, and the next start checks again.
#[test]
fn timeout_keeps_the_record_until_recovery() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    w.fake.inject(Step::Associate, Fault::TimeoutAfterWrite);
    assert_eq!(run.apply(&mut w.fake), Err(DeviceError::Timeout));
    let r = run.restore(&mut w.fake, &store).unwrap();
    assert!(r.record_kept);
    check_original(&original, &w.state()).unwrap();
    assert_eq!(store.records().unwrap().len(), 1);

    let recovered = recover(&mut w.fake, &store).unwrap();
    assert_eq!(recovered.len(), 1);
    assert!(recovered[0].result.is_ok());
    assert_restored(&w, &original, &store);
}

/// The write of the timed-out call lands at the start of the restoration
/// call. The restoration undoes it.
#[test]
fn late_write_before_restoration_is_undone() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    w.fake.inject(Step::Select, Fault::LateWrite { calls: 1 });
    assert_eq!(run.apply(&mut w.fake), Err(DeviceError::Timeout));
    let r = run.restore(&mut w.fake, &store).unwrap();
    assert!(r.record_kept);
    check_original(&original, &w.state()).unwrap();
    assert_eq!(w.fake.files(), Vec::<String>::new());
}

/// The write lands after `restore_profile` checked its readback. The
/// independent readback finds it: the run fails, and the record stays for
/// recovery.
#[test]
fn restoration_mismatch_fails_the_run() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    w.fake
        .inject(Step::Associate, Fault::LateWrite { calls: 2 });
    assert_eq!(run.apply(&mut w.fake), Err(DeviceError::Timeout));
    let e = run.restore(&mut w.fake, &store).unwrap_err();
    assert!(
        matches!(&e, DeviceError::StateConflict(m) if m.starts_with("restoration mismatch in associations")),
        "{e:?}"
    );
    assert!(matches!(run.phase(), Phase::ApplyFailed(_)));
    assert_eq!(store.records().unwrap().len(), 1);

    let recovered = recover(&mut w.fake, &store).unwrap();
    assert!(recovered[0].result.is_ok());
    assert_restored(&w, &original, &store);
}

#[test]
fn cancellation_before_and_after_assignment() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    let r = run.restore(&mut w.fake, &store).unwrap();
    assert_eq!(
        r,
        Restoration {
            state: original.clone(),
            record_kept: false
        }
    );
    assert_eq!(w.fake.writes(), &[] as &[String]);
    assert_restored(&w, &original, &store);
    // A cancelled run does not assign later.
    assert!(matches!(
        run.apply(&mut w.fake),
        Err(DeviceError::Unsupported { .. })
    ));
    assert_eq!(w.fake.writes(), &[] as &[String]);

    let mut run = w.begin(&store);
    run.apply(&mut w.fake).unwrap();
    run.restore(&mut w.fake, &store).unwrap();
    assert_restored(&w, &original, &store);
}

#[test]
fn repeated_restoration_is_safe() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    run.apply(&mut w.fake).unwrap();
    let first = run.restore(&mut w.fake, &store).unwrap();
    let writes = w.fake.writes().len();
    let second = run.restore(&mut w.fake, &store).unwrap();
    assert_eq!(first, second);
    assert_eq!(w.fake.writes().len(), writes);
    assert_restored(&w, &original, &store);
}

#[test]
fn failed_restoration_keeps_the_record_and_can_retry() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    run.apply(&mut w.fake).unwrap();
    w.fake.inject(
        Step::Uninstall,
        Fault::Fail(DeviceError::PermissionDenied("injected".into())),
    );
    assert!(matches!(
        run.restore(&mut w.fake, &store),
        Err(DeviceError::PermissionDenied(_))
    ));
    assert!(matches!(run.phase(), Phase::Applied(_)));
    assert_eq!(store.records().unwrap().len(), 1);
    run.restore(&mut w.fake, &store).unwrap();
    assert_restored(&w, &original, &store);
    assert_eq!(w.fake.files(), Vec::<String>::new());
}

#[test]
fn external_change_is_kept_and_reported() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    run.apply(&mut w.fake).unwrap();
    let (name, bytes) = user_profile();
    w.fake.select_external(&w.target, name, &bytes);
    let external = w.state();
    assert!(matches!(
        run.restore(&mut w.fake, &store),
        Err(DeviceError::StateConflict(_))
    ));
    assert_eq!(w.state(), external);
    assert_eq!(store.records().unwrap().len(), 1);
    let recovered = recover(&mut w.fake, &store).unwrap();
    assert!(matches!(
        recovered[0].result,
        Err(DeviceError::StateConflict(_))
    ));
    assert_eq!(w.state(), external);
    assert_eq!(store.records().unwrap().len(), 1);
}

/// The process ends at each point after the save. The next process
/// recovers from the record alone.
#[test]
fn crash_recovers_on_next_run() {
    type Crash = fn(&mut FakeDisplay, &mut ProfileRun);
    let points: [(&str, Crash); 4] = [
        ("after save", |_, _| {}),
        ("after install", |f, run| {
            f.inject(Step::Associate, Fault::Fail(DeviceError::Timeout));
            let _ = run.apply(f);
        }),
        ("after association", |f, run| {
            f.inject(Step::Select, Fault::Fail(DeviceError::Timeout));
            let _ = run.apply(f);
        }),
        ("after assignment", |f, run| {
            run.apply(f).unwrap();
        }),
    ];
    for (label, crash) in points {
        let mut w = world();
        let original = {
            let store = w.store();
            let mut run = w.begin(&store);
            crash(&mut w.fake, &mut run);
            run.change().original.clone()
        };
        let store = w.store();
        let recovered = recover(&mut w.fake, &store).unwrap();
        assert_eq!(recovered.len(), 1, "{label}");
        assert_eq!(
            recovered[0].key,
            RecordKey::of(&w.id),
            "{label}: {recovered:?}"
        );
        assert!(recovered[0].result.is_ok(), "{label}: {recovered:?}");
        assert_restored(&w, &original, &store);
        assert_eq!(w.fake.files(), Vec::<String>::new(), "{label}");
    }
}

#[test]
fn unresolved_record_blocks_a_new_change() {
    let mut w = world();
    {
        let store = w.store();
        let mut run = w.begin(&store);
        run.apply(&mut w.fake).unwrap();
    }
    let store = w.store();
    let writes = w.fake.writes().len();
    assert!(matches!(
        ProfileRun::begin(&mut w.fake, &store, &w.target, &icc(2)),
        Err(DeviceError::StateConflict(_))
    ));
    assert_eq!(w.fake.writes().len(), writes);
}

#[test]
fn unreadable_record_stays_for_investigation() {
    let mut w = world();
    let store = w.store();
    let key = RecordKey::of(&w.id);
    std::fs::write(w.dir.path().join(key.file_name()), "not json").unwrap();
    let recovered = recover(&mut w.fake, &store).unwrap();
    assert!(matches!(
        recovered[0].result,
        Err(DeviceError::Unsupported { .. })
    ));
    assert_eq!(store.records().unwrap().len(), 1);
    assert_eq!(w.fake.writes(), &[] as &[String]);
}

#[test]
fn disconnected_device_recovers_after_reconnection() {
    let mut w = world();
    let store = w.store();
    let mut run = w.begin(&store);
    let original = run.change().original.clone();
    run.apply(&mut w.fake).unwrap();
    w.fake.set_connected(&w.id, false);
    assert_eq!(
        run.restore(&mut w.fake, &store),
        Err(DeviceError::Disconnected(w.id.clone()))
    );
    let recovered = recover(&mut w.fake, &store).unwrap();
    assert_eq!(
        recovered[0].result,
        Err(DeviceError::Disconnected(w.id.clone()))
    );
    // Restoration does not compare the topology revision.
    w.fake.set_connected(&w.id, true);
    assert!(recover(&mut w.fake, &store).unwrap()[0].result.is_ok());
    assert_restored(&w, &original, &store);
}

fn reference(name: &str, tag: u8) -> ProfileRef {
    ProfileRef {
        native_id: name.into(),
        hash: Some(ContentHash::of(&[tag])),
    }
}

fn state(associations: Vec<ProfileRef>, selection: ProfileSelection) -> ProfileState {
    ProfileState {
        target: standard_target(&DeviceId {
            backend: "fake".into(),
            key: "a".into(),
        }),
        associations: Report::known(associations, "test"),
        selection,
        applied_scope: None,
        revision: 0,
    }
}

#[test]
fn original_check_ignores_order_and_revision_only() {
    let a = reference("a.icc", 1);
    let b = reference("b.icc", 2);
    let original = state(
        vec![a.clone(), b.clone()],
        ProfileSelection::Override(a.clone()),
    );
    let mut reordered = state(
        vec![b.clone(), a.clone()],
        ProfileSelection::Override(a.clone()),
    );
    reordered.revision = 9;
    check_original(&original, &reordered).unwrap();

    let other_bytes = state(
        vec![reference("a.icc", 3), b.clone()],
        ProfileSelection::Override(a.clone()),
    );
    let mut scope = original.clone();
    scope.applied_scope = Some(ProfileScope {
        user: UserScope::Current,
        host: None,
    });
    let mut unreadable = original.clone();
    unreadable.associations = Report::unknown("read failed");
    let selection = state(vec![a, b.clone()], ProfileSelection::Override(b));
    for (field, now) in [
        ("associations", other_bytes),
        ("applied scope", scope),
        ("associations", unreadable),
        ("selection", selection),
    ] {
        assert!(
            matches!(check_original(&original, &now),
                Err(DeviceError::StateConflict(m)) if m.contains(field)),
            "{field}"
        );
    }
}
