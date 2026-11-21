use nitpick_core::device::{DeviceError, DisplayDeviceControl, IccProfile, PreparedProfileChange};

use super::{RecordKey, RecoveryStore};
use crate::fake::{FakeDisplay, standard_target};

fn icc(tag: u8) -> IccProfile {
    let mut b = vec![0u8; 132];
    b[..4].copy_from_slice(&132u32.to_be_bytes());
    b[36..40].copy_from_slice(b"acsp");
    b[131] = tag;
    IccProfile::new(b).unwrap()
}

fn change(key: &str) -> PreparedProfileChange {
    let mut fake = FakeDisplay::new();
    let id = fake.add_device(key);
    fake.prepare_icc_profile(&standard_target(&id), &icc(1))
        .unwrap()
}

#[test]
fn saved_record_reads_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let c = change("a");
    let key = store.save(&c).unwrap();
    assert_eq!(key, RecordKey::of(&c.device.id));
    let records = store.records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].key, key);
    assert_eq!(records[0].record, Ok(c));
}

#[test]
fn one_record_per_device() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    store.save(&change("a")).unwrap();
    assert!(matches!(
        store.save(&change("a")),
        Err(DeviceError::StateConflict(_))
    ));
    store.save(&change("b")).unwrap();
    assert_eq!(store.records().unwrap().len(), 2);
}

#[test]
fn unreadable_record_is_listed_and_blocks_its_device() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let c = change("a");
    let key = RecordKey::of(&c.device.id);
    std::fs::write(dir.path().join(key.file_name()), "{\"version\": 1").unwrap();
    let records = store.records().unwrap();
    assert_eq!(records.len(), 1);
    assert!(matches!(
        &records[0].record,
        Err(DeviceError::Unsupported { reason, .. }) if reason.starts_with("unreadable")
    ));
    assert!(matches!(store.save(&c), Err(DeviceError::StateConflict(_))));
}

#[test]
fn record_under_another_device_name_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let a = change("a");
    let key = store.save(&a).unwrap();
    let other = RecordKey::of(&change("b").device.id);
    std::fs::rename(
        dir.path().join(key.file_name()),
        dir.path().join(other.file_name()),
    )
    .unwrap();
    let records = store.records().unwrap();
    assert!(matches!(
        &records[0].record,
        Err(DeviceError::Unsupported { reason, .. }) if reason.contains("fake:a")
    ));
}

/// A crash during `save` leaves a partial file. No write follows a save
/// that did not return, so it is not a pending change.
#[test]
fn partial_file_is_not_a_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    let c = change("a");
    let key = RecordKey::of(&c.device.id);
    std::fs::write(dir.path().join(format!("{key}.partial")), "{\"vers").unwrap();
    assert_eq!(store.records().unwrap(), vec![]);
    store.save(&c).unwrap();
    assert_eq!(store.records().unwrap()[0].record, Ok(c));
}

#[test]
fn one_process_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path()).unwrap();
    assert!(matches!(
        RecoveryStore::open(dir.path()),
        Err(DeviceError::StateConflict(_))
    ));
    drop(store);
    RecoveryStore::open(dir.path()).unwrap();
}

#[test]
fn remove_is_repeatable() {
    let dir = tempfile::tempdir().unwrap();
    let store = RecoveryStore::open(dir.path().join("nested")).unwrap();
    let key = store.save(&change("a")).unwrap();
    store.remove(&key).unwrap();
    store.remove(&key).unwrap();
    assert_eq!(store.records().unwrap(), vec![]);
}
