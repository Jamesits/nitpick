//! One profile change with a durable recovery record, and recovery of the
//! records that an earlier process left.
//!
//! The record is saved before the first write and removed only after an
//! independent readback equals the original state. Any other outcome keeps
//! it, so the next [`recover`] restores again.

use nitpick_core::device::{
    DeviceError, DisplayDeviceControl, IccProfile, PreparedProfileChange, ProfileRef,
    ProfileSelection, ProfileState, ProfileTarget, Report,
};

use crate::store::{RecordKey, RecoveryStore};

/// State of a [`ProfileRun`]. A failed restoration keeps the phase, so
/// the host can restore again.
#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    /// The record is saved. No write has started.
    Saved,
    /// The readback selects the profile.
    Applied(ProfileState),
    /// The assignment failed. Some of its writes can have taken effect.
    ApplyFailed(DeviceError),
    /// The readback equals the original state.
    Restored(ProfileState),
}

/// Result of a verified restoration.
#[derive(Debug, Clone, PartialEq)]
pub struct Restoration {
    /// The independent readback after the restoration.
    pub state: ProfileState,
    /// The record stays after an assignment that timed out: its native
    /// write can still take effect, and the next [`recover`] then undoes
    /// it.
    pub record_kept: bool,
}

#[derive(Debug)]
pub struct ProfileRun {
    change: PreparedProfileChange,
    key: RecordKey,
    phase: Phase,
    /// Set by a timeout of the assignment; never cleared.
    write_may_follow: bool,
}

fn phase_name(phase: &Phase) -> &'static str {
    match phase {
        Phase::Saved => "saved",
        Phase::Applied(_) => "applied",
        Phase::ApplyFailed(_) => "failed",
        Phase::Restored(_) => "restored",
    }
}

impl ProfileRun {
    /// Prepares the change and saves its record. Makes no device write.
    ///
    /// # Errors
    /// A preparation failure, or a store failure; for example
    /// [`DeviceError::StateConflict`] if the device has an unresolved
    /// record.
    pub fn begin(
        backend: &mut dyn DisplayDeviceControl,
        store: &RecoveryStore,
        target: &ProfileTarget,
        profile: &IccProfile,
    ) -> Result<Self, DeviceError> {
        let change = backend.prepare_icc_profile(target, profile)?;
        let key = store.save(&change)?;
        Ok(Self {
            change,
            key,
            phase: Phase::Saved,
            write_may_follow: false,
        })
    }

    #[must_use]
    pub fn change(&self) -> &PreparedProfileChange {
        &self.change
    }

    #[must_use]
    pub fn key(&self) -> &RecordKey {
        &self.key
    }

    #[must_use]
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    /// Assigns the profile and checks that the readback selects it. Only
    /// once, from [`Phase::Saved`].
    ///
    /// # Errors
    /// [`DeviceError::Unsupported`] in another phase. The backend error,
    /// or [`DeviceError::StateConflict`] if the readback does not select
    /// the profile; the phase is then [`Phase::ApplyFailed`], and the host
    /// must restore.
    pub fn apply(
        &mut self,
        backend: &mut dyn DisplayDeviceControl,
    ) -> Result<ProfileState, DeviceError> {
        if self.phase != Phase::Saved {
            return Err(DeviceError::Unsupported {
                operation: "profile assignment".into(),
                reason: format!("the run is {}", phase_name(&self.phase)),
            });
        }
        let result = backend.set_icc_profile(&self.change).and_then(|state| {
            let hash = Some(self.change.profile.hash());
            match &state.selection {
                ProfileSelection::Override(p) if p.hash == hash => Ok(state),
                other => Err(DeviceError::StateConflict(format!(
                    "readback does not select the profile: {other:?}"
                ))),
            }
        });
        match &result {
            Ok(state) => self.phase = Phase::Applied(state.clone()),
            Err(e) => {
                self.write_may_follow |= *e == DeviceError::Timeout;
                self.phase = Phase::ApplyFailed(e.clone());
            }
        }
        result
    }

    /// Restores the original state from any phase, also again. Removes the
    /// record after a verified restoration, unless
    /// [`Restoration::record_kept`].
    ///
    /// # Errors
    /// The backend error, or [`DeviceError::StateConflict`] if the
    /// readback differs from the original state. The record stays; the run
    /// fails.
    pub fn restore(
        &mut self,
        backend: &mut dyn DisplayDeviceControl,
        store: &RecoveryStore,
    ) -> Result<Restoration, DeviceError> {
        let state = restore_verified(backend, &self.change)?;
        if !self.write_may_follow {
            store.remove(&self.key)?;
        }
        self.phase = Phase::Restored(state.clone());
        Ok(Restoration {
            state,
            record_kept: self.write_may_follow,
        })
    }
}

/// Outcome for one record of [`recover`].
#[derive(Debug, Clone, PartialEq)]
pub struct Recovery {
    pub key: RecordKey,
    /// The verified original state; the record is removed. On an error
    /// the record stays for investigation or a later retry.
    pub result: Result<ProfileState, DeviceError>,
}

/// Restores every record of the store and removes each one whose
/// restoration is verified. Call it at host start, before any new change:
/// it also restores the record of a run that is still active.
///
/// # Errors
/// The store cannot list its records. A failure of one record is in its
/// [`Recovery`].
pub fn recover(
    backend: &mut dyn DisplayDeviceControl,
    store: &RecoveryStore,
) -> Result<Vec<Recovery>, DeviceError> {
    let mut out = Vec::new();
    for stored in store.records()? {
        let result = stored
            .record
            .and_then(|change| restore_verified(backend, &change))
            .and_then(|state| store.remove(&stored.key).map(|()| state));
        out.push(Recovery {
            key: stored.key,
            result,
        });
    }
    Ok(out)
}

/// `restore_profile`, then an independent `profile_state`. Both must equal
/// the original: the backend's own check does not cover a write that lands
/// after it.
fn restore_verified(
    backend: &mut dyn DisplayDeviceControl,
    change: &PreparedProfileChange,
) -> Result<ProfileState, DeviceError> {
    let returned = backend.restore_profile(change)?;
    check_original(&change.original, &returned)?;
    let reread = backend.profile_state(&change.target)?;
    check_original(&change.original, &reread)?;
    Ok(reread)
}

fn sorted(list: &[ProfileRef]) -> Vec<&ProfileRef> {
    let mut v: Vec<&ProfileRef> = list.iter().collect();
    v.sort_by(|a, b| (&a.native_id, a.hash.map(|h| h.0)).cmp(&(&b.native_id, b.hash.map(|h| h.0))));
    v
}

/// Equal target, selection, applied scope and associations. The
/// association order is not compared: Windows does not document a meaning
/// for it. A backend whose order has a meaning (colord) compares it in its
/// own readback.
///
/// # Errors
/// [`DeviceError::StateConflict`] with the first differing field.
pub fn check_original(original: &ProfileState, now: &ProfileState) -> Result<(), DeviceError> {
    let mismatch = |field: &str, a: &dyn std::fmt::Debug, b: &dyn std::fmt::Debug| {
        Err(DeviceError::StateConflict(format!(
            "restoration mismatch in {field}: original {a:?}, now {b:?}"
        )))
    };
    if original.target != now.target {
        return mismatch("target", &original.target, &now.target);
    }
    if original.selection != now.selection {
        return mismatch("selection", &original.selection, &now.selection);
    }
    if original.applied_scope != now.applied_scope {
        return mismatch("applied scope", &original.applied_scope, &now.applied_scope);
    }
    let same = match (&original.associations, &now.associations) {
        (Report::Known { value: a, .. }, Report::Known { value: b, .. }) => sorted(a) == sorted(b),
        _ => false,
    };
    if !same {
        return mismatch("associations", &original.associations, &now.associations);
    }
    Ok(())
}

#[cfg(test)]
#[path = "restoration_tests.rs"]
mod tests;
