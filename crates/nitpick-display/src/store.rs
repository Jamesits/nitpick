//! Durable recovery records in one directory: one file per device, in the
//! text form of [`crate::recovery`].
//!
//! A record is complete on disk before [`RecoveryStore::save`] returns, so
//! the host can start the first write after it. One process at a time
//! uses a store: [`RecoveryStore::open`] takes an exclusive lock on the
//! file `lock`, which the OS releases when the process ends.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use nitpick_core::device::{DeviceError, DeviceId, PreparedProfileChange};

use crate::common::fnv1a64;
use crate::recovery;

const LOCK: &str = "lock";
const PREFIX: &str = "profile-";
const SUFFIX: &str = ".json";
/// A record that is being written. It never precedes a native write, so a
/// left-over file is not a pending change.
const PARTIAL: &str = ".partial";

/// File name of the record of one device.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RecordKey(String);

impl RecordKey {
    /// From the device identity only, so a record that cannot be read
    /// still blocks a new change of its device.
    #[must_use]
    pub fn of(device: &DeviceId) -> Self {
        Self(format!(
            "{PREFIX}{:016x}{SUFFIX}",
            fnv1a64(&[device.backend.as_str(), device.key.as_str()])
        ))
    }

    #[must_use]
    pub fn file_name(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RecordKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A record file and its contents. An unreadable record keeps its error.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredRecord {
    pub key: RecordKey,
    pub record: Result<PreparedProfileChange, DeviceError>,
}

#[derive(Debug)]
pub struct RecoveryStore {
    dir: PathBuf,
    _lock: File,
}

fn io_error(op: &str, path: &Path, e: &std::io::Error) -> DeviceError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return DeviceError::PermissionDenied(format!("{op} {}: {e}", path.display()));
    }
    DeviceError::Native {
        api: op.into(),
        code: e.raw_os_error().map(i64::from),
        message: format!("{}: {e}", path.display()),
    }
}

/// Makes a rename or removal in `dir` durable. Windows has no portable
/// directory sync; there the change is durable when NTFS commits its
/// metadata log. **Unverified:** that a power loss right after `save`
/// keeps the record on Windows.
/// The per-user store directory of the hosts: `nitpick/device-recovery`
/// in the state directory (Linux, `$XDG_STATE_HOME`) or else the local
/// data directory (`%LOCALAPPDATA%` on Windows, `~/Library/Application
/// Support` on macOS). Not a cache or a temporary directory: a record
/// must survive a restart.
///
/// # Errors
/// [`DeviceError::Unsupported`] if the OS gives no such directory.
pub fn user_dir() -> Result<PathBuf, DeviceError> {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .map(|d| d.join("nitpick").join("device-recovery"))
        .ok_or_else(|| DeviceError::Unsupported {
            operation: "recovery store".into(),
            reason: "no per-user state or local data directory".into(),
        })
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), DeviceError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io_error("sync", dir, &e))
}

impl RecoveryStore {
    /// Creates `dir` if it is absent.
    ///
    /// # Errors
    /// [`DeviceError::StateConflict`] if another open store holds the
    /// lock; an I/O failure.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, DeviceError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|e| io_error("create_dir_all", &dir, &e))?;
        let path = dir.join(LOCK);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| io_error("open", &path, &e))?;
        match lock.try_lock() {
            Ok(()) => Ok(Self { dir, _lock: lock }),
            Err(TryLockError::WouldBlock) => Err(DeviceError::StateConflict(format!(
                "recovery store {} is in use",
                dir.display()
            ))),
            Err(TryLockError::Error(e)) => Err(io_error("lock", &path, &e)),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes the record of the change and syncs it to disk.
    ///
    /// # Errors
    /// [`DeviceError::StateConflict`] if the device has a record: recover
    /// it first. [`DeviceError::Unsupported`] for a change of another
    /// record version. An I/O failure.
    pub fn save(&self, change: &PreparedProfileChange) -> Result<RecordKey, DeviceError> {
        let text = recovery::encode(change)?;
        let key = RecordKey::of(&change.device.id);
        let path = self.dir.join(key.file_name());
        if path.exists() {
            return Err(DeviceError::StateConflict(format!(
                "device {} has the unresolved recovery record {}",
                change.device.id,
                path.display()
            )));
        }
        let partial = self.dir.join(format!("{key}{PARTIAL}"));
        let write = || -> std::io::Result<()> {
            let mut f = File::create(&partial)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()
        };
        write().map_err(|e| io_error("write", &partial, &e))?;
        std::fs::rename(&partial, &path).map_err(|e| io_error("rename", &path, &e))?;
        #[cfg(unix)]
        sync_dir(&self.dir)?;
        Ok(key)
    }

    /// Every record, in file-name order.
    ///
    /// # Errors
    /// Directory read failure. A record that cannot be read is an entry
    /// with its error, not a failure of the list.
    pub fn records(&self) -> Result<Vec<StoredRecord>, DeviceError> {
        let entries =
            std::fs::read_dir(&self.dir).map_err(|e| io_error("read_dir", &self.dir, &e))?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_error("read_dir", &self.dir, &e))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !(name.starts_with(PREFIX) && name.ends_with(SUFFIX)) {
                continue;
            }
            let key = RecordKey(name);
            let record = self.read(&key);
            out.push(StoredRecord { key, record });
        }
        out.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(out)
    }

    fn read(&self, key: &RecordKey) -> Result<PreparedProfileChange, DeviceError> {
        let path = self.dir.join(key.file_name());
        let text = std::fs::read_to_string(&path).map_err(|e| io_error("read", &path, &e))?;
        let change = recovery::decode(&text)?;
        if RecordKey::of(&change.device.id) != *key {
            return Err(DeviceError::Unsupported {
                operation: "recovery record".into(),
                reason: format!("{key} holds a record of device {}", change.device.id),
            });
        }
        Ok(change)
    }

    /// Removes a record. An absent record is no error, so a repeated
    /// restoration can remove it again.
    ///
    /// # Errors
    /// I/O failure.
    pub fn remove(&self, key: &RecordKey) -> Result<(), DeviceError> {
        let path = self.dir.join(key.file_name());
        match std::fs::remove_file(&path) {
            Ok(()) => {
                #[cfg(unix)]
                sync_dir(&self.dir)?;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error("remove_file", &path, &e)),
        }
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
