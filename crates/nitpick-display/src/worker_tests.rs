use std::future::Future;
use std::pin::pin;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};

use nitpick_core::device::{
    DeviceError, DeviceId, DeviceInfo, DeviceQuery, DisplayDeviceControl, DisplaySetup, IccProfile,
    PreparedProfileChange, ProfileSelection, ProfileState, ProfileTarget,
};
use tempfile::TempDir;

use super::{Assignment, DeviceWorker, Recovered};
use crate::fake::{FakeDisplay, Fault, Step, standard_target};
use crate::store::RecoveryStore;

struct Unpark(Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future>(f: F) -> F::Output {
    let waker = Waker::from(Arc::new(Unpark(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut f = pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        thread::park();
    }
}

fn icc(tag: u8) -> IccProfile {
    let mut b = vec![0u8; 132];
    b[..4].copy_from_slice(&132u32.to_be_bytes());
    b[36..40].copy_from_slice(b"acsp");
    b[131] = tag;
    IccProfile::new(b).unwrap()
}

type Fake = Arc<Mutex<FakeDisplay>>;

/// The fake as the OS state, shared with the test. `set_icc_profile`
/// signals `entered` and waits for `release` when they are set.
struct Shared {
    fake: Fake,
    gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
}

impl DisplayDeviceControl for Shared {
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, DeviceError> {
        self.fake.lock().unwrap().devices()
    }
    fn query_device(&mut self, d: &DeviceId) -> Result<DeviceQuery, DeviceError> {
        self.fake.lock().unwrap().query_device(d)
    }
    fn display_setup(&mut self, d: &DeviceId) -> Result<DisplaySetup, DeviceError> {
        self.fake.lock().unwrap().display_setup(d)
    }
    fn profile_targets(&mut self, d: &DeviceId) -> Result<Vec<ProfileTarget>, DeviceError> {
        self.fake.lock().unwrap().profile_targets(d)
    }
    fn profile_state(&mut self, t: &ProfileTarget) -> Result<ProfileState, DeviceError> {
        self.fake.lock().unwrap().profile_state(t)
    }
    fn prepare_icc_profile(
        &mut self,
        t: &ProfileTarget,
        p: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError> {
        self.fake.lock().unwrap().prepare_icc_profile(t, p)
    }
    fn set_icc_profile(&mut self, c: &PreparedProfileChange) -> Result<ProfileState, DeviceError> {
        if let Some((entered, release)) = &self.gate {
            entered.send(()).unwrap();
            release.recv().unwrap();
        }
        self.fake.lock().unwrap().set_icc_profile(c)
    }
    fn restore_profile(&mut self, c: &PreparedProfileChange) -> Result<ProfileState, DeviceError> {
        self.fake.lock().unwrap().restore_profile(c)
    }
}

struct World {
    fake: Fake,
    target: ProfileTarget,
    dir: TempDir,
}

fn world() -> World {
    let mut fake = FakeDisplay::new();
    let id = fake.add_device("a");
    World {
        target: standard_target(&id),
        fake: Arc::new(Mutex::new(fake)),
        dir: tempfile::tempdir().unwrap(),
    }
}

impl World {
    fn start(&self) -> DeviceWorker {
        let (worker, recovered) = self.start_with(None);
        assert_eq!(block_on(recovered).unwrap(), Ok(vec![]));
        worker
    }

    fn start_with(
        &self,
        gate: Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>,
    ) -> (DeviceWorker, Recovered) {
        let backend = Shared {
            fake: Arc::clone(&self.fake),
            gate,
        };
        DeviceWorker::start(Box::new(backend), self.dir.path().into()).unwrap()
    }

    fn state(&self) -> ProfileState {
        self.fake.lock().unwrap().peek(&self.target)
    }

    fn records(&self) -> usize {
        // The worker holds the lock; read the directory.
        std::fs::read_dir(self.dir.path())
            .unwrap()
            .filter(|e| {
                let name = e.as_ref().unwrap().file_name();
                let name = name.to_string_lossy();
                name.starts_with("profile-") && name.ends_with(".json")
            })
            .count()
    }
}

fn selects(state: &ProfileState, profile: &IccProfile) -> bool {
    matches!(&state.selection, ProfileSelection::Override(p) if p.hash == Some(profile.hash()))
}

#[test]
fn assigns_and_restores_with_a_saved_record() {
    let w = world();
    let original = w.state();
    let worker = w.start();
    let a = block_on(worker.assign(w.target.clone(), icc(1)))
        .unwrap()
        .unwrap();
    assert!(a.applied.as_ref().is_ok_and(|s| selects(s, &icc(1))));
    assert_eq!(a.restored, None);
    assert_eq!(w.records(), 1);
    let r = block_on(worker.restore()).unwrap().unwrap();
    assert!(!r.record_kept);
    assert_eq!(r.state.selection, original.selection);
    assert_eq!(w.records(), 0);
    assert_eq!(w.fake.lock().unwrap().files(), Vec::<String>::new());
}

#[test]
fn one_change_at_a_time() {
    let w = world();
    let worker = w.start();
    block_on(worker.assign(w.target.clone(), icc(1)))
        .unwrap()
        .unwrap();
    let second = block_on(worker.assign(w.target.clone(), icc(2))).unwrap();
    assert!(matches!(second, Err(DeviceError::StateConflict(_))));
    assert!(selects(&w.state(), &icc(1)));
    block_on(worker.restore()).unwrap().unwrap();
    let none = block_on(worker.restore()).unwrap();
    assert!(matches!(none, Err(DeviceError::Unsupported { .. })));
}

#[test]
fn a_failed_assignment_is_restored_in_the_same_job() {
    let w = world();
    let original = w.state();
    w.fake.lock().unwrap().inject(
        Step::Select,
        Fault::Fail(DeviceError::Native {
            api: "select".into(),
            code: None,
            message: "injected".into(),
        }),
    );
    let worker = w.start();
    let Assignment { applied, restored } = block_on(worker.assign(w.target.clone(), icc(1)))
        .unwrap()
        .unwrap();
    assert!(matches!(applied, Err(DeviceError::Native { .. })));
    let restored = restored.expect("restored").unwrap();
    assert!(!restored.record_kept);
    assert_eq!(w.state().associations, original.associations);
    assert_eq!(w.records(), 0);
    // Nothing is active.
    assert!(block_on(worker.restore()).unwrap().is_err());
}

/// The assignment wait is dropped while `set_icc_profile` runs. The
/// restoration queued after it starts only when the call returns.
#[test]
fn restoration_waits_for_a_cancelled_assignment() {
    let w = world();
    let original = w.state();
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let (worker, recovered) = w.start_with(Some((entered_tx, release_rx)));
    block_on(recovered).unwrap().unwrap();
    let assign = worker.assign(w.target.clone(), icc(1));
    entered.recv().unwrap();
    drop(assign);
    let restore = worker.restore();
    assert_eq!(w.records(), 1, "saved before the write");
    assert_eq!(w.fake.lock().unwrap().writes(), Vec::<String>::new());
    release.send(()).unwrap();
    let r = block_on(restore).unwrap().unwrap();
    assert!(!r.record_kept);
    let writes = w.fake.lock().unwrap().writes().to_vec();
    let select = writes.iter().position(|s| s.starts_with("Select"));
    let reselect = writes.iter().position(|s| s.starts_with("Reselect"));
    assert!(select < reselect, "{writes:?}");
    assert_eq!(w.state().selection, original.selection);
    assert_eq!(w.records(), 0);
}

/// A write that lands after the timeout: the record stays, and it blocks
/// a new change of the device until the next recovery.
#[test]
fn a_timed_out_assignment_keeps_its_record() {
    let w = world();
    let original = w.state();
    w.fake
        .lock()
        .unwrap()
        .inject(Step::Select, Fault::TimeoutAfterWrite);
    let worker = w.start();
    let a = block_on(worker.assign(w.target.clone(), icc(1)))
        .unwrap()
        .unwrap();
    assert_eq!(a.applied, Err(DeviceError::Timeout));
    let restored = a.restored.expect("restored").unwrap();
    assert!(restored.record_kept);
    assert_eq!(w.state().selection, original.selection);
    assert_eq!(w.records(), 1);
    let again = block_on(worker.assign(w.target.clone(), icc(2))).unwrap();
    assert!(
        matches!(again, Err(DeviceError::StateConflict(_))),
        "{again:?}"
    );
    drop(worker);

    // The next process recovers the kept record before its first job.
    let (worker, recovered) = loop {
        // The earlier thread releases the store lock when it ends.
        let (worker, recovered) = w.start_with(None);
        match block_on(recovered).unwrap() {
            Err(DeviceError::StateConflict(_)) => thread::yield_now(),
            other => break (worker, other),
        }
    };
    let recovered = recovered.unwrap();
    assert_eq!(recovered.len(), 1);
    assert!(recovered[0].result.is_ok());
    assert_eq!(w.records(), 0);
    let a = block_on(worker.assign(w.target.clone(), icc(2)))
        .unwrap()
        .unwrap();
    assert!(a.applied.is_ok());
}

#[test]
fn a_store_in_use_fails_changes_but_not_reads() {
    let w = world();
    let _other = RecoveryStore::open(w.dir.path()).unwrap();
    let (worker, recovered) = w.start_with(None);
    assert!(matches!(
        block_on(recovered).unwrap(),
        Err(DeviceError::StateConflict(_))
    ));
    let devices = block_on(worker.read(|b| b.devices())).unwrap().unwrap();
    assert_eq!(devices.len(), 1);
    let a = block_on(worker.assign(w.target.clone(), icc(1))).unwrap();
    assert!(matches!(a, Err(DeviceError::StateConflict(_))));
    assert_eq!(w.fake.lock().unwrap().writes(), Vec::<String>::new());
}
