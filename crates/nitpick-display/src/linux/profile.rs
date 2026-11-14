//! ICC profile assignment through colord.
//!
//! colord keeps an ordered profile list for each device: hard relations
//! first, then soft ones, newest first in each relation (colord source;
//! not documented). The first entry is the default profile that a
//! colour-managed compositor applies. The backend registers its own
//! profile object, adds it with a hard relation and makes it the default.
//! These calls change only the entry of that profile, so its removal gives
//! the original list again.

use nitpick_core::device::{
    ContentHash, DeviceError, DeviceId, DeviceInfo, IccProfile, PREPARED_CHANGE_VERSION,
    PlannedResource, PreparedProfileChange, ProfileRef, ProfileRole, ProfileScope,
    ProfileSelection, ProfileState, ProfileTarget, Report, ResourceKind, UserScope,
};

use super::{
    BACKEND, ColordDevice, ColordReport, LinuxDisplay, LinuxNative, NoMatch, colord_match,
};
use crate::common::{fnv1a64, resolve, unsupported};

/// Prefix of the profile files and colord profile IDs that this backend
/// creates. Restoration removes only these, and only the ones named in the
/// change.
pub const INSTALLED_PREFIX: &str = "nitpick-";

/// Source name of the device profile list.
const PROFILES: &str = "colord Device.Profiles";

/// The only target scope. colord display devices belong to the session
/// user that registered them; see [`LinuxProfileNative`] for the scope of
/// the stored relation.
const SCOPE: ProfileScope = ProfileScope {
    user: UserScope::Current,
    host: None,
};

/// `Device.GetProfileRelation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Relation {
    /// Added by a client. colord stores it in its mapping database.
    Hard,
    /// Added by colord from profile metadata. Not stored.
    Soft,
}

/// One colord profile object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColordProfile {
    /// D-Bus object path.
    pub path: String,
    /// `ProfileId`.
    pub id: String,
    /// `Filename`; empty if the profile has no file.
    pub filename: String,
    /// `Owner` UID.
    pub owner: u32,
}

/// Native profile calls. Every write is one native call, so tests can fail
/// each step.
///
/// colord stores a hard relation by device ID, not by device object or
/// user. A session of another user that registers a device with the same
/// ID gets the relation too, if it can see the profile.
pub trait LinuxProfileNative {
    /// `Profiles` of the device object `device`, in colord order, each with
    /// its relation.
    ///
    /// # Errors
    /// The device object or a profile object does not answer.
    fn device_profiles(
        &mut self,
        device: &str,
    ) -> Result<Vec<(ColordProfile, Relation)>, DeviceError>;
    /// `FindProfileById`; `None` if no profile has the ID. colord prefers a
    /// profile of the calling user, and otherwise returns one of any owner.
    ///
    /// # Errors
    /// colord does not answer.
    fn find_profile(&mut self, id: &str) -> Result<Option<ColordProfile>, DeviceError>;
    /// `CreateProfileWithFd` with the scope `normal`, the file `file` as
    /// the descriptor and its path as `Filename`. Returns the object path.
    /// The profile stays registered until `DeleteProfile` or a colord
    /// restart.
    ///
    /// # Errors
    /// [`DeviceError::PermissionDenied`] if polkit denies it;
    /// [`DeviceError::StateConflict`] if the ID exists.
    fn create_profile(&mut self, id: &str, file: &str) -> Result<String, DeviceError>;
    /// `Device.AddProfile` with the relation `hard`.
    ///
    /// # Errors
    /// [`DeviceError::PermissionDenied`] if polkit denies it.
    fn add_profile(&mut self, device: &str, profile: &str) -> Result<(), DeviceError>;
    /// `Device.MakeProfileDefault`.
    ///
    /// # Errors
    /// [`DeviceError::PermissionDenied`] if polkit denies it.
    fn make_profile_default(&mut self, device: &str, profile: &str) -> Result<(), DeviceError>;
    /// `Device.RemoveProfile`. colord keeps the mapping database row with
    /// timestamp 0, so that metadata cannot add the profile again.
    ///
    /// # Errors
    /// [`DeviceError::PermissionDenied`] if polkit denies it.
    fn remove_profile(&mut self, device: &str, profile: &str) -> Result<(), DeviceError>;
    /// `DeleteProfile`. colord also removes the profile from every device.
    ///
    /// # Errors
    /// [`DeviceError::PermissionDenied`] if polkit denies it.
    fn delete_profile(&mut self, profile: &str) -> Result<(), DeviceError>;
    /// `$XDG_DATA_HOME/nitpick/icc`. Not `$XDG_DATA_HOME/icc`: session
    /// daemons register each file there as a second profile object.
    ///
    /// # Errors
    /// No data directory.
    fn profile_dir(&mut self) -> Result<String, DeviceError>;
    /// File contents; `None` if no such file.
    ///
    /// # Errors
    /// Read failure.
    fn read_file(&mut self, path: &str) -> Result<Option<Vec<u8>>, DeviceError>;
    /// Creates a new file, and its directory if missing.
    ///
    /// # Errors
    /// [`DeviceError::PermissionDenied`]; [`DeviceError::StateConflict`] if
    /// the file exists; other write failure.
    fn create_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), DeviceError>;
    /// # Errors
    /// [`DeviceError::PermissionDenied`] or other failure.
    fn remove_file(&mut self, path: &str) -> Result<(), DeviceError>;
}

fn conflict(msg: impl Into<String>) -> DeviceError {
    DeviceError::StateConflict(msg.into())
}

fn target(device: &DeviceId) -> ProfileTarget {
    ProfileTarget {
        device: device.clone(),
        role: ProfileRole::Standard,
        scope: SCOPE,
    }
}

fn check_target(target: &ProfileTarget) -> Result<(), DeviceError> {
    if target.device.backend != BACKEND {
        return Err(unsupported(
            "profile target",
            format!("backend {}", target.device.backend),
        ));
    }
    if target.role != ProfileRole::Standard {
        return Err(unsupported(
            "profile target",
            "colord has no Advanced Color role",
        ));
    }
    if target.scope != SCOPE {
        return Err(unsupported(
            "profile target",
            "colord display devices have only the current-user scope without a host",
        ));
    }
    Ok(())
}

fn same_identity(a: &DeviceInfo, b: &DeviceInfo) -> bool {
    a.id == b.id && a.evidence == b.evidence
}

/// Unique per profile and target, so two targets never share a profile
/// object or a file created by this backend. A repeated change of the same
/// profile and target reuses the ID, so the mapping database rows that
/// restoration leaves do not accumulate.
fn planned_id(target: &ProfileTarget, profile: &IccProfile) -> String {
    let target_tag = fnv1a64(&[
        target.device.key.as_str(),
        &format!("{:?}{:?}", target.role, target.scope),
    ]);
    let hash = profile.hash().to_string();
    format!("{INSTALLED_PREFIX}{}-{target_tag:016x}", &hash[..16])
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// What a change writes, derived from its record. `None`: the default
/// profile already has the same content, and the change writes nothing.
struct Plan {
    /// Installed profile file.
    file: String,
    /// colord profile ID.
    profile_id: String,
    /// colord device ID of the association.
    device_id: String,
}

impl Plan {
    fn of(change: &PreparedProfileChange) -> Result<Option<Self>, DeviceError> {
        let mut file = None;
        let mut profile_id = None;
        let mut device_id = None;
        for r in &change.resources {
            let slot = match r.kind {
                ResourceKind::InstalledProfile => &mut file,
                ResourceKind::RegisteredProfile => &mut profile_id,
                ResourceKind::Association => &mut device_id,
            };
            if slot.replace(r.native_id.clone()).is_some() {
                return Err(unsupported("recovery record", "repeated resource kind"));
            }
        }
        match (file, profile_id, device_id) {
            (None, None, None) => Ok(None),
            (Some(file), Some(profile_id), Some(device_id)) => {
                let named = file_name(&file)
                    .strip_suffix(".icc")
                    .is_some_and(|n| n == profile_id);
                if !file.starts_with('/') || !profile_id.starts_with(INSTALLED_PREFIX) || !named {
                    return Err(unsupported(
                        "recovery record",
                        "the installed file must be an absolute path named after the \
                         registered profile, with the backend prefix",
                    ));
                }
                Ok(Some(Self {
                    file,
                    profile_id,
                    device_id,
                }))
            }
            _ => Err(unsupported(
                "recovery record",
                "an installed file, a registered profile and an association must occur \
                 together",
            )),
        }
    }
}

type Listed = Result<Vec<(ColordProfile, Relation)>, String>;

/// Covers the relations and owners, which the state does not show.
fn revision(state: &ProfileState, device: &ColordDevice, listed: &Listed) -> u64 {
    let mut parts = vec![
        format!("{:?}{:?}", state.target.role, state.target.scope),
        device.path.clone(),
        device.id.clone(),
    ];
    match listed {
        Ok(list) => parts.extend(list.iter().map(|(p, relation)| {
            format!(
                "p\0{}\0{}\0{}\0{}\0{relation:?}",
                p.path, p.id, p.filename, p.owner
            )
        })),
        Err(e) => parts.push(format!("p?\0{e}")),
    }
    let push_ref = |parts: &mut Vec<String>, tag: &str, r: &ProfileRef| {
        let hash = r.hash.map(|h| h.to_string()).unwrap_or_default();
        parts.push(format!("{tag}\0{}\0{hash}", r.native_id));
    };
    match &state.associations {
        Report::Known { value, .. } => value.iter().for_each(|r| push_ref(&mut parts, "a", r)),
        Report::Unknown(reason) => parts.push(format!("a?\0{reason}")),
    }
    match &state.selection {
        ProfileSelection::Override(r) => push_ref(&mut parts, "s=", r),
        other => parts.push(format!("s\0{other:?}")),
    }
    fnv1a64(&parts)
}

/// A resolved target.
struct Located {
    topology_revision: u64,
    info: DeviceInfo,
    device: ColordDevice,
    colord: ColordReport,
}

impl Located {
    /// Whether another device of this user, or of root, lists `profile`.
    /// Devices of other users are not visible.
    fn in_use_elsewhere(&self, profile: &str) -> bool {
        self.colord
            .devices
            .iter()
            .filter(|d| d.path != self.device.path)
            .any(|d| d.profiles.iter().any(|p| p == profile))
    }
}

impl<N: LinuxNative + LinuxProfileNative> LinuxDisplay<N> {
    /// Profile control is optional: without colord or a single colord
    /// device the device has no targets, and the device list still works.
    pub(super) fn targets_of(
        &mut self,
        device: &DeviceId,
    ) -> Result<Vec<ProfileTarget>, DeviceError> {
        let snapshot = self.snapshot()?;
        let entry = resolve(&snapshot.entries, device, |e| &e.info)?;
        let Ok(colord) = self.native.colord() else {
            return Ok(Vec::new());
        };
        Ok(match colord_match(entry, &snapshot.entries, &colord) {
            Ok(_) => vec![target(device)],
            Err(_) => Vec::new(),
        })
    }

    fn locate(&mut self, target: &ProfileTarget) -> Result<Located, DeviceError> {
        check_target(target)?;
        let snapshot = self.snapshot()?;
        let entry = resolve(&snapshot.entries, &target.device, |e| &e.info)?;
        let colord = self
            .native
            .colord()
            .map_err(|e| unsupported("profile target", format!("colord: {e}")))?;
        let device = match colord_match(entry, &snapshot.entries, &colord) {
            Ok(d) => d.clone(),
            Err(NoMatch::Absent) => {
                return Err(unsupported(
                    "profile target",
                    "no colord display device has the EDID MD5 of the output",
                ));
            }
            Err(NoMatch::Unknown(reason)) => return Err(unsupported("profile target", reason)),
            Err(NoMatch::Ambiguous(reason)) => return Err(DeviceError::AmbiguousIdentity(reason)),
        };
        Ok(Located {
            topology_revision: snapshot.revision,
            info: entry.info.clone(),
            device,
            colord,
        })
    }

    fn profile_ref(&mut self, profile: &ColordProfile) -> ProfileRef {
        let hash = if profile.filename.is_empty() {
            None
        } else {
            self.native
                .read_file(&profile.filename)
                .ok()
                .flatten()
                .map(|b| ContentHash::of(&b))
        };
        ProfileRef {
            native_id: profile.id.clone(),
            hash,
        }
    }

    /// colord has no selection separate from the list: the default is the
    /// first profile.
    fn read_state(
        &mut self,
        target: &ProfileTarget,
        device: &ColordDevice,
    ) -> (ProfileState, Listed) {
        let listed: Listed = self
            .native
            .device_profiles(&device.path)
            .map_err(|e| e.to_string());
        let (associations, selection) = match &listed {
            Ok(list) => {
                let refs: Vec<ProfileRef> = list.iter().map(|(p, _)| self.profile_ref(p)).collect();
                let selection = refs
                    .first()
                    .cloned()
                    .map_or(ProfileSelection::NoOverride, ProfileSelection::Override);
                (Report::known(refs, PROFILES), selection)
            }
            Err(e) => (
                Report::unknown(e.clone()),
                ProfileSelection::Unreadable(format!("{PROFILES}: {e}")),
            ),
        };
        let mut state = ProfileState {
            target: target.clone(),
            associations,
            selection,
            // colord has no setting that selects the scope.
            applied_scope: None,
            revision: 0,
        };
        state.revision = revision(&state, device, &listed);
        (state, listed)
    }

    pub(super) fn profile_state_of(
        &mut self,
        target: &ProfileTarget,
    ) -> Result<ProfileState, DeviceError> {
        let located = self.locate(target)?;
        Ok(self.read_state(target, &located.device).0)
    }

    pub(super) fn prepare_of(
        &mut self,
        target: &ProfileTarget,
        profile: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError> {
        let located = self.locate(target)?;
        let (original, _) = self.read_state(target, &located.device);
        original.check_restorable()?;
        let identical = matches!(&original.selection, ProfileSelection::Override(r) if r.hash == Some(profile.hash()));
        let resources = if identical {
            Vec::new()
        } else {
            // An identical colord profile that is not the default is not
            // reused: adding and removing it would change a soft relation
            // to hard and leave a mapping database row for it.
            let id = planned_id(target, profile);
            let dir = self.native.profile_dir()?;
            let file = format!("{}/{id}.icc", dir.trim_end_matches('/'));
            if self.native.read_file(&file)?.is_some() {
                return Err(conflict(format!(
                    "{file} exists; recover the change that installed it first"
                )));
            }
            if let Some(p) = self.native.find_profile(&id)? {
                return Err(conflict(format!(
                    "colord profile {id} exists at {}; recover the change that registered it \
                     first",
                    p.path
                )));
            }
            vec![
                PlannedResource {
                    kind: ResourceKind::InstalledProfile,
                    native_id: file,
                },
                PlannedResource {
                    kind: ResourceKind::RegisteredProfile,
                    native_id: id,
                },
                PlannedResource {
                    kind: ResourceKind::Association,
                    native_id: located.device.id.clone(),
                },
            ]
        };
        Ok(PreparedProfileChange {
            version: PREPARED_CHANGE_VERSION,
            device: located.info.clone(),
            topology_revision: located.topology_revision,
            target: target.clone(),
            original,
            profile: profile.clone(),
            resources,
        })
    }

    pub(super) fn set_of(
        &mut self,
        change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError> {
        change.check_version()?;
        let plan = Plan::of(change)?;
        let target = &change.target;
        let located = self.locate(target)?;
        if !same_identity(&located.info, &change.device) {
            return Err(conflict("device identity evidence changed"));
        }
        if located.topology_revision != change.topology_revision {
            return Err(conflict("display topology changed after preparation"));
        }
        if let Some(plan) = &plan
            && plan.device_id != located.device.id
        {
            return Err(conflict(format!(
                "colord device ID changed from {} to {}",
                plan.device_id, located.device.id
            )));
        }
        if self.read_state(target, &located.device).0 != change.original {
            return Err(conflict("profile state changed after preparation"));
        }
        if let Some(plan) = &plan {
            if self.native.read_file(&plan.file)?.is_some() {
                return Err(conflict(format!(
                    "{} exists before installation",
                    plan.file
                )));
            }
            if self.native.find_profile(&plan.profile_id)?.is_some() {
                return Err(conflict(format!(
                    "colord profile {} exists before registration",
                    plan.profile_id
                )));
            }
            self.native
                .create_file(&plan.file, change.profile.bytes())?;
            let profile = self.native.create_profile(&plan.profile_id, &plan.file)?;
            // colord adds a new profile to each device that its mapping
            // database names with a nonzero timestamp. After an interrupted
            // change and a colord restart, the device can list it already.
            let listed = self.native.device_profiles(&located.device.path)?;
            if !listed.iter().any(|(p, _)| p.path == profile) {
                self.native.add_profile(&located.device.path, &profile)?;
            }
            // A new hard relation is already the newest. The explicit call
            // does not depend on that undocumented order.
            self.native
                .make_profile_default(&located.device.path, &profile)?;
        }

        let (state, _) = self.read_state(target, &located.device);
        let selected = matches!(
            &state.selection,
            ProfileSelection::Override(r)
                if r.hash == Some(change.profile.hash())
                    && plan.as_ref().is_none_or(|p| r.native_id == p.profile_id)
        );
        if !selected {
            return Err(conflict(format!(
                "readback after assignment: default profile {:?}",
                state.selection
            )));
        }
        Ok(state)
    }

    pub(super) fn restore_of(
        &mut self,
        change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError> {
        change.check_version()?;
        let plan = Plan::of(change)?;
        let target = &change.target;
        let original = &change.original;
        let located = self.locate(target)?;
        if !same_identity(&located.info, &change.device) {
            return Err(conflict("device identity evidence changed"));
        }
        if let Some(plan) = &plan {
            if plan.device_id != located.device.id {
                return Err(conflict(format!(
                    "colord device ID changed from {} to {}",
                    plan.device_id, located.device.id
                )));
            }
            let (current, listed) = self.read_state(target, &located.device);
            current
                .check_restorable()
                .map_err(|e| conflict(format!("current state: {e}")))?;
            check_reachable(original, &current, plan)?;
            let own = listed
                .map_err(conflict)?
                .into_iter()
                .find(|(p, _)| p.id == plan.profile_id);
            if let Some((own, _)) = own {
                self.native
                    .remove_profile(&located.device.path, &own.path)?;
            }
            let registered = self
                .native
                .find_profile(&plan.profile_id)?
                .filter(|p| p.owner == located.colord.uid);
            let keep = match registered {
                Some(p) if located.in_use_elsewhere(&p.path) => true,
                Some(p) => {
                    self.native.delete_profile(&p.path)?;
                    false
                }
                None => false,
            };
            if !keep && self.native.read_file(&plan.file)?.is_some() {
                self.native.remove_file(&plan.file)?;
                if self.native.read_file(&plan.file)?.is_some() {
                    return Err(conflict(format!("{} remains after removal", plan.file)));
                }
            }
        }

        let (state, _) = self.read_state(target, &located.device);
        if state.associations != original.associations || state.selection != original.selection {
            return Err(conflict(format!(
                "readback after restoration differs: associations {:?}, selection {:?}",
                state.associations, state.selection
            )));
        }
        Ok(state)
    }
}

/// `current` must be a state that some prefix of the change's writes gives
/// from `original`: the original list, in its order, with the change's
/// profile at any position or absent. Anything else is another actor's
/// change, which restoration must not overwrite.
fn check_reachable(
    original: &ProfileState,
    current: &ProfileState,
    plan: &Plan,
) -> Result<(), DeviceError> {
    let (Report::Known { value: before, .. }, Report::Known { value: now, .. }) =
        (&original.associations, &current.associations)
    else {
        return Err(conflict("profile list unreadable"));
    };
    let others: Vec<&ProfileRef> = now
        .iter()
        .filter(|r| r.native_id != plan.profile_id)
        .collect();
    if others != before.iter().collect::<Vec<_>>() {
        return Err(conflict(format!(
            "profile list changed by another actor: {now:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
