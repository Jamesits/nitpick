//! ICC profile assignment through `ColorSync` device custom profiles.
//!
//! `ColorSyncDeviceSetCustomProfiles` stores one custom profile file per
//! device profile ID and preference scope (user and host). The documented
//! read, `ColorSyncDeviceCopyDeviceInfo`, gives only the value resolved for
//! the current user and host. The backend therefore reads each scope from
//! the preference storage, which is undocumented, and rejects a state in
//! which the scopes do not give the resolved value.

use nitpick_core::device::{
    ContentHash, DeviceError, DeviceInfo, HostScope, IccProfile, PREPARED_CHANGE_VERSION,
    PlannedResource, PreparedProfileChange, ProfileRef, ProfileRole, ProfileScope,
    ProfileSelection, ProfileState, ProfileTarget, Report, ResourceKind, UserScope,
};

use super::{BACKEND, COLORSYNC_INFO, ColorSyncDevice, Entry, MacDisplay, MacNative, Snapshot};
use crate::common::{fnv1a64, resolve, unsupported};

/// File name prefix of profiles that this backend installs. Restoration
/// removes only these, and only the one named in the change.
pub const INSTALLED_PREFIX: &str = "nitpick-";

/// Source name of the per-scope custom profiles.
const PREFERENCES: &str = "CFPreferences com.apple.ColorSync.Devices";

/// Preference scope of a custom profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacScope {
    pub user: UserScope,
    pub host: HostScope,
}

/// Resolution order: the first scope with a custom profile applies. This
/// is the `CFPreferences` search order. **Unverified** for `ColorSync`; a
/// state read rejects an observed state that does not follow it.
pub const PRECEDENCE: [MacScope; 4] = [
    MacScope {
        user: UserScope::Current,
        host: HostScope::Current,
    },
    MacScope {
        user: UserScope::Current,
        host: HostScope::Any,
    },
    MacScope {
        user: UserScope::All,
        host: HostScope::Current,
    },
    MacScope {
        user: UserScope::All,
        host: HostScope::Any,
    },
];

impl MacScope {
    fn of(scope: ProfileScope) -> Option<Self> {
        scope.host.map(|host| Self {
            user: scope.user,
            host,
        })
    }

    fn profile_scope(self) -> ProfileScope {
        ProfileScope {
            user: self.user,
            host: Some(self.host),
        }
    }

    fn index(self) -> usize {
        PRECEDENCE
            .iter()
            .position(|s| *s == self)
            .expect("PRECEDENCE has every scope")
    }

    /// The `ColorSyncDevice.h` notes state that a custom profile scope
    /// cannot exceed the device scope.
    fn within(self, device: Self) -> bool {
        (self.user == UserScope::Current || device.user == UserScope::All)
            && (self.host == HostScope::Current || device.host == HostScope::Any)
    }
}

/// Native profile calls. Every write is one native call, so tests can fail
/// each step.
pub trait MacProfileNative {
    /// `ColorSyncDeviceCopyDeviceInfo` of the display device `uuid`; `None`
    /// if `ColorSync` has no such device.
    ///
    /// # Errors
    /// `uuid` is not a UUID.
    fn device_info(&mut self, uuid: &str) -> Result<Option<ColorSyncDevice>, DeviceError>;
    /// Custom profiles stored at `scope`, as (profile ID, file path).
    ///
    /// **Undocumented storage**: the `com.apple.ColorSync.Devices` value of
    /// the global preference domain, with the entry `Device.mntr.<uuid>` and
    /// its `CustomProfiles` dictionary. Reported by administrator tools for
    /// older macOS versions; unverified on current macOS.
    ///
    /// # Errors
    /// A value of an unexpected type.
    fn scoped_custom_profiles(
        &mut self,
        uuid: &str,
        scope: MacScope,
    ) -> Result<Vec<(String, String)>, DeviceError>;
    /// `ColorSyncDeviceSetCustomProfiles` for one profile ID at `scope`.
    /// `None` passes `kCFNull`, which reverts that scope to the factory
    /// profile.
    ///
    /// # Errors
    /// The call returned false; it gives no reason.
    fn set_custom_profile(
        &mut self,
        uuid: &str,
        scope: MacScope,
        profile_id: &str,
        path: Option<&str>,
    ) -> Result<(), DeviceError>;
    /// `ColorSyncProfileCreate` and `ColorSyncProfileVerify`.
    ///
    /// # Errors
    /// [`DeviceError::InvalidProfile`] with the `ColorSync` reason.
    fn verify_profile(&mut self, bytes: &[u8]) -> Result<(), DeviceError>;
    /// Whether the effective user is root. Any-user preferences need root.
    fn is_root(&mut self) -> bool;
    /// `~/Library/ColorSync/Profiles` for the current user;
    /// `/Library/ColorSync/Profiles` for all users.
    ///
    /// # Errors
    /// No home directory.
    fn profile_dir(&mut self, user: UserScope) -> Result<String, DeviceError>;
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

/// The profile ID that the backend assigns: the default ID, or the only
/// factory ID. `ColorSyncDevice.h` makes the default ID optional for a
/// device with one profile.
///
/// # Errors
/// The reason if no single ID applies.
pub(super) fn profile_id(device: &ColorSyncDevice) -> Result<String, String> {
    let ids = &device.factory_profile_ids;
    match (&device.default_profile_id, ids.as_slice()) {
        (Some(id), _) if ids.is_empty() || ids.contains(id) => Ok(id.clone()),
        (Some(id), _) => Err(format!(
            "default profile ID {id} is not a factory profile ID"
        )),
        (None, [id]) => Ok(id.clone()),
        (None, _) => Err(format!(
            "no default profile ID and {} factory profile IDs",
            ids.len()
        )),
    }
}

/// Registration scope. A missing value is the narrowest scope:
/// `ColorSyncRegisterDevice` defaults to the current user and host.
fn device_scope(device: &ColorSyncDevice) -> MacScope {
    MacScope {
        user: device.user_scope.unwrap_or(UserScope::Current),
        host: device.host_scope.unwrap_or(HostScope::Current),
    }
}

/// One target per scope within the device scope, in [`PRECEDENCE`] order.
/// `ColorSync` has no Advanced Color role.
pub(super) fn targets(entry: &Entry) -> Vec<ProfileTarget> {
    let (Ok(_), Some(device)) = (&entry.display.uuid, &entry.display.colorsync) else {
        return Vec::new();
    };
    if profile_id(device).is_err() {
        return Vec::new();
    }
    let limit = device_scope(device);
    PRECEDENCE
        .iter()
        .filter(|s| s.within(limit))
        .map(|s| ProfileTarget {
            device: entry.info.id.clone(),
            role: ProfileRole::Standard,
            scope: s.profile_scope(),
        })
        .collect()
}

fn check_target(target: &ProfileTarget) -> Result<MacScope, DeviceError> {
    if target.device.backend != BACKEND {
        return Err(unsupported(
            "profile target",
            format!("backend {}", target.device.backend),
        ));
    }
    if target.role != ProfileRole::Standard {
        return Err(unsupported(
            "profile target",
            "ColorSync has no Advanced Color role",
        ));
    }
    MacScope::of(target.scope)
        .ok_or_else(|| unsupported("profile target", "a ColorSync target needs a host scope"))
}

/// Identity without the name: the name falls back when `NSScreen` is not
/// read.
fn same_identity(a: &DeviceInfo, b: &DeviceInfo) -> bool {
    a.id == b.id && a.evidence == b.evidence
}

/// Unique per profile and target, so two targets never share a file
/// installed by this backend.
fn planned_path(dir: &str, target: &ProfileTarget, profile: &IccProfile) -> String {
    let target_tag = fnv1a64(&[
        target.device.key.as_str(),
        &format!("{:?}{:?}", target.role, target.scope),
    ]);
    let hash = profile.hash().to_string();
    format!(
        "{}/{INSTALLED_PREFIX}{}-{target_tag:016x}.icc",
        dir.trim_end_matches('/'),
        &hash[..16]
    )
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// What a change writes, derived from its record. `None`: the target
/// already has an identical custom profile, and the change writes nothing.
struct Plan {
    /// Installed profile file.
    path: String,
    /// The `ColorSync` profile ID whose custom profile the change sets.
    profile_id: String,
}

impl Plan {
    fn of(change: &PreparedProfileChange) -> Result<Option<Self>, DeviceError> {
        let mut path = None;
        let mut profile_id = None;
        for r in &change.resources {
            let slot = match r.kind {
                ResourceKind::InstalledProfile => &mut path,
                ResourceKind::Association => &mut profile_id,
                ResourceKind::RegisteredProfile => {
                    return Err(unsupported(
                        "recovery record",
                        "registered profile on macOS",
                    ));
                }
            };
            if slot.replace(r.native_id.clone()).is_some() {
                return Err(unsupported("recovery record", "repeated resource kind"));
            }
        }
        match (path, profile_id) {
            (None, None) => Ok(None),
            (Some(path), Some(profile_id)) => {
                if !path.starts_with('/') || !file_name(&path).starts_with(INSTALLED_PREFIX) {
                    return Err(unsupported(
                        "recovery record",
                        "installed profile without an absolute path and the backend prefix",
                    ));
                }
                Ok(Some(Self { path, profile_id }))
            }
            _ => Err(unsupported(
                "recovery record",
                "an installed profile and a custom profile entry must occur together",
            )),
        }
    }
}

/// The custom profile entries of one profile ID.
struct Observed {
    /// Path per scope, in [`PRECEDENCE`] order.
    scoped: Result<[Option<String>; 4], String>,
    /// `ColorSyncDeviceCopyDeviceInfo` value.
    resolved: Result<Option<String>, String>,
}

impl Observed {
    /// Index of the scope whose entry applies.
    fn effective(&self) -> Option<usize> {
        self.scoped.as_ref().ok()?.iter().position(Option::is_some)
    }

    fn at(&self, index: usize) -> Option<&String> {
        self.scoped.as_ref().ok()?[index].as_ref()
    }

    /// The first entry wider than `index`, with its scope.
    fn wider(&self, index: usize) -> Option<(MacScope, &String)> {
        let scoped = self.scoped.as_ref().ok()?;
        (index + 1..PRECEDENCE.len()).find_map(|i| scoped[i].as_ref().map(|p| (PRECEDENCE[i], p)))
    }
}

fn own_path(state: &ProfileState) -> Option<&str> {
    match &state.selection {
        ProfileSelection::Override(r) => Some(&r.native_id),
        _ => None,
    }
}

/// Covers every scope and the resolved value, so that a change at a
/// narrower scope changes the revision too.
fn revision(state: &ProfileState, observed: &Observed, profile_id: &str) -> u64 {
    let mut parts = vec![
        format!("{:?}{:?}", state.target.role, state.target.scope),
        profile_id.to_owned(),
    ];
    match &observed.scoped {
        Ok(scoped) => parts.extend(scoped.iter().map(|p| format!("c{p:?}"))),
        Err(e) => parts.push(format!("c?\0{e}")),
    }
    match &observed.resolved {
        Ok(p) => parts.push(format!("r{p:?}")),
        Err(e) => parts.push(format!("r?\0{e}")),
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
        ProfileSelection::NoOverride => parts.push("s-".into()),
        ProfileSelection::Override(r) => push_ref(&mut parts, "s=", r),
        ProfileSelection::Inherited { from, profile } => {
            push_ref(&mut parts, &format!("s<{from:?}"), profile);
        }
        ProfileSelection::Unreadable(reason) => parts.push(format!("s?\0{reason}")),
    }
    fnv1a64(&parts)
}

/// A resolved target.
struct Located {
    snapshot: Snapshot,
    info: DeviceInfo,
    uuid: String,
    scope: MacScope,
    /// Current profile ID of the device.
    profile_id: String,
}

impl<N: MacNative + MacProfileNative> MacDisplay<N> {
    fn locate(&mut self, target: &ProfileTarget) -> Result<Located, DeviceError> {
        let scope = check_target(target)?;
        let snapshot = self.snapshot_with(false)?;
        let entry = resolve(&snapshot.entries, &target.device, |e| &e.info)?.clone();
        let uuid = match &entry.display.uuid {
            Ok(uuid) if !uuid.is_empty() => uuid.clone(),
            Ok(_) => return Err(unsupported("profile target", "empty ColorSync device ID")),
            Err(e) => {
                return Err(unsupported(
                    "profile target",
                    format!("no ColorSync device ID: {e}"),
                ));
            }
        };
        let device = entry.display.colorsync.as_ref().ok_or_else(|| {
            unsupported(
                "profile target",
                format!("{COLORSYNC_INFO}: no ColorSync device for {uuid}"),
            )
        })?;
        let profile_id = profile_id(device)
            .map_err(|e| unsupported("profile target", format!("{COLORSYNC_INFO}: {e}")))?;
        let limit = device_scope(device);
        if !scope.within(limit) {
            return Err(unsupported(
                "profile target",
                format!("scope {scope:?} is wider than the device scope {limit:?}"),
            ));
        }
        Ok(Located {
            snapshot,
            info: entry.info,
            uuid,
            scope,
            profile_id,
        })
    }

    fn profile_ref(&mut self, path: &str) -> ProfileRef {
        let hash = self
            .native
            .read_file(path)
            .ok()
            .flatten()
            .map(|b| ContentHash::of(&b));
        ProfileRef {
            native_id: path.to_owned(),
            hash,
        }
    }

    fn observe(&mut self, uuid: &str, profile_id: &str) -> Observed {
        let mut scoped: [Option<String>; 4] = Default::default();
        let mut failure = None;
        for (slot, scope) in scoped.iter_mut().zip(PRECEDENCE) {
            match self.native.scoped_custom_profiles(uuid, scope) {
                Ok(list) => {
                    *slot = list
                        .into_iter()
                        .find(|(id, _)| id == profile_id)
                        .map(|(_, path)| path);
                }
                Err(e) => {
                    failure = Some(format!("{scope:?}: {e}"));
                    break;
                }
            }
        }
        let resolved = match self.native.device_info(uuid) {
            Ok(Some(device)) => Ok(device
                .custom_profiles
                .into_iter()
                .find(|(id, _)| id == profile_id)
                .and_then(|(_, path)| path)),
            Ok(None) => Err(format!("no ColorSync device for {uuid}")),
            Err(e) => Err(e.to_string()),
        };
        Observed {
            scoped: failure.map_or(Ok(scoped), Err),
            resolved,
        }
    }

    fn state_of(
        &mut self,
        target: &ProfileTarget,
        scope: MacScope,
        observed: &Observed,
        profile_id: &str,
    ) -> ProfileState {
        let index = scope.index();
        let associations = match &observed.scoped {
            Ok(scoped) => Report::known(
                scoped[index].iter().map(|p| self.profile_ref(p)).collect(),
                PREFERENCES,
            ),
            Err(e) => Report::unknown(e.clone()),
        };
        let selection = match (&observed.scoped, &observed.resolved) {
            (Err(e), _) => ProfileSelection::Unreadable(format!("{PREFERENCES}: {e}")),
            (_, Err(e)) => ProfileSelection::Unreadable(format!("{COLORSYNC_INFO}: {e}")),
            (Ok(scoped), Ok(resolved)) => {
                let effective = scoped.iter().flatten().next();
                if effective != resolved.as_ref() {
                    ProfileSelection::Unreadable(format!(
                        "{COLORSYNC_INFO} resolves {resolved:?}; the preference scopes give \
                         {effective:?}"
                    ))
                } else if let Some(path) = observed.at(index).cloned() {
                    ProfileSelection::Override(self.profile_ref(&path))
                } else if let Some((from, path)) = observed.wider(index) {
                    let path = path.clone();
                    ProfileSelection::Inherited {
                        from: from.profile_scope(),
                        profile: self.profile_ref(&path),
                    }
                } else {
                    ProfileSelection::NoOverride
                }
            }
        };
        let mut state = ProfileState {
            target: target.clone(),
            associations,
            selection,
            // macOS has no setting that selects the scope.
            applied_scope: None,
            revision: 0,
        };
        state.revision = revision(&state, observed, profile_id);
        state
    }

    fn read_state(
        &mut self,
        target: &ProfileTarget,
        located: &Located,
        profile_id: &str,
    ) -> (ProfileState, Observed) {
        let observed = self.observe(&located.uuid, profile_id);
        let state = self.state_of(target, located.scope, &observed, profile_id);
        (state, observed)
    }

    /// The `customdisplayprofiles` administrator tool requires root for
    /// the any-user scope. **Unverified:** whether a call without root
    /// returns false or returns true without a change; the readback detects
    /// the second case.
    fn check_permission(&mut self, scope: MacScope) -> Result<(), DeviceError> {
        if scope.user == UserScope::All && !self.native.is_root() {
            return Err(DeviceError::PermissionDenied(
                "the any-user ColorSync scope needs root".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn profile_state_of(
        &mut self,
        target: &ProfileTarget,
    ) -> Result<ProfileState, DeviceError> {
        let located = self.locate(target)?;
        let profile_id = located.profile_id.clone();
        Ok(self.read_state(target, &located, &profile_id).0)
    }

    pub(super) fn prepare_of(
        &mut self,
        target: &ProfileTarget,
        profile: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError> {
        let located = self.locate(target)?;
        let profile_id = located.profile_id.clone();
        let (original, observed) = self.read_state(target, &located, &profile_id);
        original.check_restorable()?;
        let index = located.scope.index();
        if let Some(i) = observed.effective().filter(|&i| i < index) {
            return Err(unsupported(
                "profile preparation",
                format!(
                    "a custom profile at the narrower scope {:?} overrides this target",
                    PRECEDENCE[i]
                ),
            ));
        }
        self.check_permission(located.scope)?;
        self.native.verify_profile(profile.bytes())?;
        let identical = matches!(&original.selection, ProfileSelection::Override(r) if r.hash == Some(profile.hash()));
        let resources = if identical {
            Vec::new()
        } else {
            // An identical file elsewhere is not reused: the record names
            // one installed file, and its removal is the only cleanup.
            let dir = self.native.profile_dir(located.scope.user)?;
            let path = planned_path(&dir, target, profile);
            if self.native.read_file(&path)?.is_some() {
                return Err(conflict(format!(
                    "{path} exists; recover the change that installed it first"
                )));
            }
            vec![
                PlannedResource {
                    kind: ResourceKind::InstalledProfile,
                    native_id: path,
                },
                PlannedResource {
                    kind: ResourceKind::Association,
                    native_id: profile_id,
                },
            ]
        };
        Ok(PreparedProfileChange {
            version: PREPARED_CHANGE_VERSION,
            device: located.info.clone(),
            topology_revision: located.snapshot.revision,
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
        if located.snapshot.revision != change.topology_revision {
            return Err(conflict("display topology changed after preparation"));
        }
        let profile_id = located.profile_id.clone();
        if let Some(plan) = &plan
            && plan.profile_id != profile_id
        {
            return Err(conflict(format!(
                "ColorSync profile ID changed from {} to {profile_id}",
                plan.profile_id
            )));
        }
        if self.read_state(target, &located, &profile_id).0 != change.original {
            return Err(conflict("profile state changed after preparation"));
        }
        self.check_permission(located.scope)?;
        if let Some(plan) = &plan {
            if self.native.read_file(&plan.path)?.is_some() {
                return Err(conflict(format!(
                    "{} exists before installation",
                    plan.path
                )));
            }
            self.native
                .create_file(&plan.path, change.profile.bytes())?;
            self.native.set_custom_profile(
                &located.uuid,
                located.scope,
                &profile_id,
                Some(&plan.path),
            )?;
        }

        let (state, observed) = self.read_state(target, &located, &profile_id);
        let selected = matches!(
            &state.selection,
            ProfileSelection::Override(r)
                if r.hash == Some(change.profile.hash())
                    && plan.as_ref().is_none_or(|p| r.native_id == p.path)
        );
        let applied = observed.effective() == Some(located.scope.index());
        if !(selected && applied) {
            return Err(conflict(format!(
                "readback after assignment: selected {selected}, applied {applied}"
            )));
        }
        Ok(state)
    }

    /// Whether a custom profile entry of any current display, in any scope
    /// that this process can read, names `path`. Other users' current-user
    /// entries are not readable; the file name is unique to the change.
    fn in_use(&mut self, snapshot: &Snapshot, path: &str) -> Result<bool, DeviceError> {
        for entry in &snapshot.entries {
            let Ok(uuid) = &entry.display.uuid else {
                continue;
            };
            for scope in PRECEDENCE {
                let list = self.native.scoped_custom_profiles(uuid, scope)?;
                if list.iter().any(|(_, p)| p == path) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
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
        // Restoration writes the profile ID of the record, also if the
        // device now has another one.
        let profile_id = plan
            .as_ref()
            .map_or_else(|| located.profile_id.clone(), |p| p.profile_id.clone());
        let (current, observed) = self.read_state(target, &located, &profile_id);
        if let Some(plan) = &plan {
            current
                .check_restorable()
                .map_err(|e| conflict(format!("current state: {e}")))?;
            check_reachable(original, &current, &observed, plan, located.scope)?;
            let wanted = own_path(original);
            if own_path(&current) != wanted {
                self.check_permission(located.scope)?;
                self.native.set_custom_profile(
                    &located.uuid,
                    located.scope,
                    &profile_id,
                    wanted,
                )?;
            }
            if self.native.read_file(&plan.path)?.is_some()
                && !self.in_use(&located.snapshot, &plan.path)?
            {
                self.native.remove_file(&plan.path)?;
                if self.native.read_file(&plan.path)?.is_some() {
                    return Err(conflict(format!("{} remains after removal", plan.path)));
                }
            }
        }

        let (state, _) = self.read_state(target, &located, &profile_id);
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
/// from `original`: the target holds the original entry or the installed
/// file, and the wider scopes are unchanged. Anything else is another
/// actor's change, which restoration must not overwrite.
fn check_reachable(
    original: &ProfileState,
    current: &ProfileState,
    observed: &Observed,
    plan: &Plan,
    scope: MacScope,
) -> Result<(), DeviceError> {
    let own = own_path(current);
    if own != own_path(original) && own != Some(plan.path.as_str()) {
        return Err(conflict(format!(
            "custom profile changed by another actor: {own:?}"
        )));
    }
    let wider = observed
        .wider(scope.index())
        .map(|(s, p)| (s.profile_scope(), p.as_str()));
    let original_wider = match &original.selection {
        ProfileSelection::Inherited { from, profile } => Some((*from, profile.native_id.as_str())),
        ProfileSelection::NoOverride => None,
        // An override hides the wider scopes; the record has no value.
        _ => return Ok(()),
    };
    if wider != original_wider {
        return Err(conflict(format!(
            "wider scope changed by another actor: {wider:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
