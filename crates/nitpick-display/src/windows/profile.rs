//! ICC profile assignment through the `ColorProfile*` display association
//! functions of `mscms.dll`.
//!
//! Each display has an association list and a default per scope (current
//! user or system-wide) and per role (ordinary or Advanced Color). A
//! per-user switch selects the scope that Windows applies; observed on
//! Windows 11 build 26200, a current-user association does not set it, so
//! a current-user assignment sets it and restoration resets it.

use nitpick_core::device::{
    ContentHash, DeviceError, DeviceInfo, IccProfile, PREPARED_CHANGE_VERSION, PlannedResource,
    PreparedProfileChange, ProfileRef, ProfileRole, ProfileScope, ProfileSelection, ProfileState,
    ProfileTarget, Report, ResourceKind, UserScope,
};

use super::{
    BACKEND, Entry, FALLBACK_KEY_REASON, Snapshot, WindowsDisplay, WindowsNative, is_fallback_key,
};
use crate::common::{fnv1a64, resolve, unsupported};

/// File name prefix of profiles that this backend installs. Restoration
/// removes only these, and only the one named in the change.
pub const INSTALLED_PREFIX: &str = "nitpick-";

/// Address of a device for the profile calls. Valid only for the
/// `QueryDisplayConfig` result that gave it: the adapter LUID and source ID
/// change after a restart or topology change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayAddress {
    /// `targetInfo.adapterId` of the path.
    pub target_adapter_luid: i64,
    pub source_id: u32,
    /// GDI source name; the legacy per-user switch finds its device name
    /// through it.
    pub gdi_name: Option<String>,
    /// Monitor device interface path, the device key.
    pub device_path: String,
}

/// Native profile calls. Every write is one native call, so tests can fail
/// each step.
pub trait WindowsProfileNative {
    /// # Errors
    /// [`DeviceError::Unsupported`] if `mscms.dll` lacks the
    /// `ColorProfile*` functions (before Windows build 20348).
    fn profile_api(&mut self) -> Result<(), DeviceError>;
    /// `ColorProfileGetDisplayUserScope`: the scope Windows applies.
    ///
    /// # Errors
    /// Native failure.
    fn applied_scope(&mut self, display: &DisplayAddress) -> Result<UserScope, DeviceError>;
    /// `WcsSetUsePerUserProfiles`.
    ///
    /// # Errors
    /// No legacy device name for the display, or a native failure.
    fn set_per_user(&mut self, display: &DisplayAddress, per_user: bool)
    -> Result<(), DeviceError>;
    /// The association list of `role`, and the name of its source.
    ///
    /// The `ICMProfile` (ordinary) or `ICMProfileAC` registry value of the
    /// monitor, because `ColorProfileGetDisplayList` returned no profile.
    /// **Undocumented storage**: observed for the current user on Windows
    /// 11 build 26200; the system-wide Advanced Color value is unverified.
    ///
    /// # Errors
    /// Native failure.
    fn association_list(
        &mut self,
        display: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
    ) -> Result<(Vec<String>, &'static str), DeviceError>;
    /// `ColorProfileGetDisplayDefault`; `None` for `ERROR_FILE_NOT_FOUND`.
    ///
    /// # Errors
    /// Other native failure.
    fn default_profile(
        &mut self,
        display: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
    ) -> Result<Option<String>, DeviceError>;
    /// `ColorProfileAddDisplayAssociation` without `setAsDefault`.
    ///
    /// # Errors
    /// Permission or native failure.
    fn add_association(
        &mut self,
        display: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError>;
    /// `ColorProfileRemoveDisplayAssociation`.
    ///
    /// # Errors
    /// Permission or native failure.
    fn remove_association(
        &mut self,
        display: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError>;
    /// `ColorProfileSetDisplayDefaultAssociation`.
    ///
    /// # Errors
    /// Permission or native failure.
    fn set_default(
        &mut self,
        display: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError>;
    /// File names in the colour directory.
    ///
    /// # Errors
    /// Directory read failure.
    fn installed_profiles(&mut self) -> Result<Vec<String>, DeviceError>;
    /// Bytes of an installed profile; `None` if no such file.
    ///
    /// # Errors
    /// Read failure.
    fn read_installed(&mut self, name: &str) -> Result<Option<Vec<u8>>, DeviceError>;
    /// `InstallColorProfileW`: copies `bytes` into the colour directory as
    /// `name`.
    ///
    /// # Errors
    /// Permission or native failure.
    fn install(&mut self, name: &str, bytes: &[u8]) -> Result<(), DeviceError>;
    /// `UninstallColorProfileW` with file deletion.
    ///
    /// # Errors
    /// Permission or native failure.
    fn uninstall(&mut self, name: &str) -> Result<(), DeviceError>;
}

fn conflict(msg: impl Into<String>) -> DeviceError {
    DeviceError::StateConflict(msg.into())
}

fn scope_of(user: UserScope) -> ProfileScope {
    ProfileScope { user, host: None }
}

/// The addressable targets: both roles in both scopes. Windows has no host
/// scope.
pub(super) fn targets(device: &DeviceInfo) -> Vec<ProfileTarget> {
    let mut out = Vec::new();
    for role in [ProfileRole::Standard, ProfileRole::AdvancedColor] {
        for user in [UserScope::Current, UserScope::All] {
            out.push(ProfileTarget {
                device: device.id.clone(),
                role,
                scope: scope_of(user),
            });
        }
    }
    out
}

fn check_target(target: &ProfileTarget) -> Result<(), DeviceError> {
    if target.device.backend != BACKEND {
        return Err(unsupported(
            "profile target",
            format!("backend {}", target.device.backend),
        ));
    }
    if target.scope.host.is_some() {
        return Err(unsupported("profile target", "Windows has no host scope"));
    }
    Ok(())
}

/// A change needs a key that a recovery record finds after a restart.
/// Restoration does not check this, so a record of an earlier build is
/// still restored before a restart.
fn check_changeable(target: &ProfileTarget) -> Result<(), DeviceError> {
    if is_fallback_key(&target.device.key) {
        return Err(unsupported("profile change", FALLBACK_KEY_REASON));
    }
    Ok(())
}

fn address(entry: &Entry) -> DisplayAddress {
    DisplayAddress {
        target_adapter_luid: entry.path.target_adapter_luid,
        source_id: entry.path.source_id,
        gdi_name: entry.path.source_name.clone().ok(),
        device_path: entry.info.id.key.clone(),
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn names(state: &ProfileState) -> Vec<String> {
    state
        .associations
        .value()
        .map(|refs| refs.iter().map(|r| r.native_id.to_lowercase()).collect())
        .unwrap_or_default()
}

fn sorted_names(state: &ProfileState) -> Vec<String> {
    let mut n = names(state);
    n.sort_unstable();
    n
}

fn same_ref(a: &ProfileRef, b: &ProfileRef) -> bool {
    same_name(&a.native_id, &b.native_id) && a.hash == b.hash
}

fn same_selection(a: &ProfileSelection, b: &ProfileSelection) -> bool {
    match (a, b) {
        (ProfileSelection::NoOverride, ProfileSelection::NoOverride) => true,
        (ProfileSelection::Override(a), ProfileSelection::Override(b)) => same_ref(a, b),
        (
            ProfileSelection::Inherited {
                from: fa,
                profile: pa,
            },
            ProfileSelection::Inherited {
                from: fb,
                profile: pb,
            },
        ) => fa == fb && same_ref(pa, pb),
        _ => false,
    }
}

/// Equality after restoration. The association order is not compared:
/// setting a default can reorder the list, and Windows does not document
/// what the order means.
fn same_restored(a: &ProfileState, b: &ProfileState) -> bool {
    sorted_names(a) == sorted_names(b)
        && same_selection(&a.selection, &b.selection)
        && a.applied_scope == b.applied_scope
}

fn revision(state: &ProfileState) -> u64 {
    let mut parts = vec![format!("{:?}{:?}", state.target.role, state.target.scope)];
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
    parts.push(format!("u{:?}", state.applied_scope));
    fnv1a64(&parts)
}

fn is_profile_file(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("icc") || e.eq_ignore_ascii_case("icm"))
}

/// Unique per profile and target, so two targets never share a file
/// installed by this backend.
fn planned_name(target: &ProfileTarget, profile: &IccProfile) -> String {
    let target_tag = fnv1a64(&[
        target.device.key.as_str(),
        &format!("{:?}{:?}", target.role, target.scope),
    ]);
    let hash = profile.hash().to_string();
    format!("{INSTALLED_PREFIX}{}-{target_tag:016x}.icc", &hash[..16])
}

/// What a change does, derived from its record.
struct Plan {
    name: String,
    installs: bool,
    associates: bool,
}

impl Plan {
    /// The resources name the profile; without resources, the profile is
    /// the original association with the same content.
    fn of(change: &PreparedProfileChange) -> Result<Self, DeviceError> {
        let mut installs = None;
        let mut associates = None;
        for r in &change.resources {
            let slot = match r.kind {
                ResourceKind::InstalledProfile => &mut installs,
                ResourceKind::Association => &mut associates,
                ResourceKind::RegisteredProfile => {
                    return Err(unsupported(
                        "recovery record",
                        "registered profile on Windows",
                    ));
                }
            };
            if slot.replace(r.native_id.clone()).is_some() {
                return Err(unsupported("recovery record", "repeated resource kind"));
            }
        }
        if let (Some(i), Some(a)) = (&installs, &associates)
            && !same_name(i, a)
        {
            return Err(unsupported(
                "recovery record",
                "installed and associated profiles differ",
            ));
        }
        if installs
            .as_deref()
            .is_some_and(|n| !n.starts_with(INSTALLED_PREFIX))
        {
            return Err(unsupported(
                "recovery record",
                "installed profile without the backend prefix",
            ));
        }
        let name = match installs.clone().or_else(|| associates.clone()) {
            Some(name) => name,
            None => change
                .original
                .associations
                .value()
                .and_then(|refs| refs.iter().find(|r| r.hash == Some(change.profile.hash())))
                .map(|r| r.native_id.clone())
                .ok_or_else(|| {
                    unsupported(
                        "recovery record",
                        "no resource and no original association with the profile",
                    )
                })?,
        };
        Ok(Self {
            name,
            installs: installs.is_some(),
            associates: associates.is_some(),
        })
    }
}

impl<N: WindowsNative + WindowsProfileNative> WindowsDisplay<N> {
    fn profile_ref(&mut self, name: String) -> ProfileRef {
        let hash = self
            .native
            .read_installed(&name)
            .ok()
            .flatten()
            .map(|b| ContentHash::of(&b));
        ProfileRef {
            native_id: name,
            hash,
        }
    }

    fn read_selection(
        &mut self,
        display: &DisplayAddress,
        target: &ProfileTarget,
        applied: UserScope,
    ) -> ProfileSelection {
        let own = match self
            .native
            .default_profile(display, target.scope.user, target.role)
        {
            Ok(own) => own,
            Err(e) => return ProfileSelection::Unreadable(e.to_string()),
        };
        if let Some(name) = own {
            return ProfileSelection::Override(self.profile_ref(name));
        }
        // Unverified: whether a current-user read without a current-user
        // default falls back to the system-wide default. This mapping
        // assumes it does not; no system-wide default existed in the check.
        if target.scope.user == UserScope::Current && applied == UserScope::All {
            return match self
                .native
                .default_profile(display, UserScope::All, target.role)
            {
                Ok(Some(name)) => ProfileSelection::Inherited {
                    from: scope_of(UserScope::All),
                    profile: self.profile_ref(name),
                },
                Ok(None) => ProfileSelection::NoOverride,
                Err(e) => ProfileSelection::Unreadable(e.to_string()),
            };
        }
        ProfileSelection::NoOverride
    }

    fn read_state(&mut self, entry: &Entry, target: &ProfileTarget) -> ProfileState {
        let display = address(entry);
        let (selection, applied_scope) = match self.native.applied_scope(&display) {
            Ok(applied) => (
                self.read_selection(&display, target, applied),
                Some(scope_of(applied)),
            ),
            Err(e) => (
                ProfileSelection::Unreadable(format!("per-user switch: {e}")),
                None,
            ),
        };
        let associations =
            match self
                .native
                .association_list(&display, target.scope.user, target.role)
            {
                Ok((list, source)) => Report::known(
                    list.into_iter().map(|n| self.profile_ref(n)).collect(),
                    source,
                ),
                Err(e) => Report::unknown(e.to_string()),
            };
        let mut state = ProfileState {
            target: target.clone(),
            associations,
            selection,
            applied_scope,
            revision: 0,
        };
        state.revision = revision(&state);
        state
    }

    /// Snapshot and entry of `target`, after the target and API checks.
    fn locate(&mut self, target: &ProfileTarget) -> Result<(Snapshot, Entry), DeviceError> {
        check_target(target)?;
        let snapshot = self.snapshot()?;
        let entry = resolve(&snapshot.entries, &target.device, |e| &e.info)?.clone();
        self.native.profile_api()?;
        Ok((snapshot, entry))
    }

    pub(super) fn profile_targets_of(&mut self, info: &DeviceInfo) -> Vec<ProfileTarget> {
        if self.native.profile_api().is_err() || is_fallback_key(&info.id.key) {
            return Vec::new();
        }
        targets(info)
    }

    pub(super) fn profile_state_of(
        &mut self,
        target: &ProfileTarget,
    ) -> Result<ProfileState, DeviceError> {
        let (_, entry) = self.locate(target)?;
        Ok(self.read_state(&entry, target))
    }

    /// The first identical profile in the colour directory that this
    /// backend did not install. Sorted, so the choice is deterministic.
    fn find_installed(&mut self, profile: &IccProfile) -> Result<Option<String>, DeviceError> {
        let mut files = self.native.installed_profiles()?;
        files.sort_unstable();
        for name in files {
            if !is_profile_file(&name) || name.starts_with(INSTALLED_PREFIX) {
                continue;
            }
            if let Some(bytes) = self.native.read_installed(&name)?
                && ContentHash::of(&bytes) == profile.hash()
            {
                return Ok(Some(name));
            }
        }
        Ok(None)
    }

    pub(super) fn prepare_of(
        &mut self,
        target: &ProfileTarget,
        profile: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError> {
        check_changeable(target)?;
        let (snapshot, entry) = self.locate(target)?;
        let entry = &entry;
        let original = self.read_state(entry, target);
        original.check_restorable()?;
        let associated = original.associations.value().and_then(|refs| {
            refs.iter()
                .find(|r| r.hash == Some(profile.hash()))
                .map(|r| r.native_id.clone())
        });
        let mut resources = Vec::new();
        if associated.is_some() {
            // Selecting an associated profile cannot be undone to "no
            // selection" without removing the user's association.
            if !matches!(original.selection, ProfileSelection::Override(_)) {
                return Err(unsupported(
                    "profile preparation",
                    "the profile is associated without a selection; restoration could not \
                     remove the selection",
                ));
            }
        } else {
            let name = if let Some(name) = self.find_installed(profile)? {
                name
            } else {
                let name = planned_name(target, profile);
                if self.native.read_installed(&name)?.is_some() {
                    return Err(conflict(format!(
                        "{name} exists; recover the change that installed it first"
                    )));
                }
                resources.push(PlannedResource {
                    kind: ResourceKind::InstalledProfile,
                    native_id: name.clone(),
                });
                name
            };
            resources.push(PlannedResource {
                kind: ResourceKind::Association,
                native_id: name,
            });
        }
        Ok(PreparedProfileChange {
            version: PREPARED_CHANGE_VERSION,
            device: entry.info.clone(),
            topology_revision: snapshot.revision,
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
        check_changeable(target)?;
        let (snapshot, entry) = self.locate(target)?;
        let entry = &entry;
        if entry.info != change.device {
            return Err(conflict("device identity evidence changed"));
        }
        if snapshot.revision != change.topology_revision {
            return Err(conflict("display topology changed after preparation"));
        }
        if self.read_state(entry, target) != change.original {
            return Err(conflict("profile state changed after preparation"));
        }
        let display = address(entry);
        let (scope, role) = (target.scope.user, target.role);
        if plan.installs {
            if self.native.read_installed(&plan.name)?.is_some() {
                return Err(conflict(format!(
                    "{} exists before installation",
                    plan.name
                )));
            }
            self.native.install(&plan.name, change.profile.bytes())?;
        }
        let per_user = scope_of(UserScope::Current);
        if scope == UserScope::Current && change.original.applied_scope != Some(per_user) {
            self.native.set_per_user(&display, true)?;
        }
        if plan.associates {
            self.native
                .add_association(&display, scope, role, &plan.name)?;
        }
        self.native.set_default(&display, scope, role, &plan.name)?;

        let state = self.read_state(entry, target);
        let wanted = ProfileRef {
            native_id: plan.name.clone(),
            hash: Some(change.profile.hash()),
        };
        let selected =
            matches!(&state.selection, ProfileSelection::Override(r) if same_ref(r, &wanted));
        let listed = names(&state).contains(&plan.name.to_lowercase());
        let applied = scope == UserScope::All || state.applied_scope == Some(per_user);
        if !(selected && listed && applied) {
            return Err(conflict(format!(
                "readback after assignment: selected {selected}, listed {listed}, per-user \
                 scope applied {applied}"
            )));
        }
        Ok(state)
    }

    /// Whether any current display lists or selects `name` in any scope or
    /// role.
    fn in_use(&mut self, snapshot: &Snapshot, name: &str) -> Result<bool, DeviceError> {
        for entry in &snapshot.entries {
            let display = address(entry);
            for scope in [UserScope::Current, UserScope::All] {
                for role in [ProfileRole::Standard, ProfileRole::AdvancedColor] {
                    let (list, _) = self.native.association_list(&display, scope, role)?;
                    if list.iter().any(|n| same_name(n, name)) {
                        return Ok(true);
                    }
                    let default = self.native.default_profile(&display, scope, role)?;
                    if default.is_some_and(|d| same_name(&d, name)) {
                        return Ok(true);
                    }
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
        let (snapshot, entry) = self.locate(target)?;
        let entry = &entry;
        if entry.info != change.device {
            return Err(conflict("device identity evidence changed"));
        }
        let current = self.read_state(entry, target);
        current
            .check_restorable()
            .map_err(|e| conflict(format!("current state: {e}")))?;
        check_reachable(original, &current, &plan, target)?;

        let display = address(entry);
        let (scope, role) = (target.scope.user, target.role);
        let ours = plan.name.to_lowercase();
        if plan.associates && names(&current).contains(&ours) {
            self.native
                .remove_association(&display, scope, role, &plan.name)?;
        }
        if let ProfileSelection::Override(previous) = &original.selection {
            let now = self.native.default_profile(&display, scope, role)?;
            if !now.is_some_and(|n| same_name(&n, &previous.native_id)) {
                self.native
                    .set_default(&display, scope, role, &previous.native_id)?;
            }
        }
        if current.applied_scope != original.applied_scope {
            let per_user = original.applied_scope == Some(scope_of(UserScope::Current));
            self.native.set_per_user(&display, per_user)?;
        }
        if plan.installs
            && self.native.read_installed(&plan.name)?.is_some()
            && !self.in_use(&snapshot, &plan.name)?
        {
            self.native.uninstall(&plan.name)?;
            if self.native.read_installed(&plan.name)?.is_some() {
                return Err(conflict(format!(
                    "{} remains after uninstallation",
                    plan.name
                )));
            }
        }

        let state = self.read_state(entry, target);
        if !same_restored(&state, original) {
            return Err(conflict(format!(
                "readback after restoration differs: associations {:?}, selection {:?}, \
                 applied scope {:?}",
                names(&state),
                state.selection,
                state.applied_scope
            )));
        }
        Ok(state)
    }
}

/// `current` must be a state that some prefix of the change's writes gives
/// from `original`. Anything else is another actor's change, which
/// restoration must not overwrite.
fn check_reachable(
    original: &ProfileState,
    current: &ProfileState,
    plan: &Plan,
    target: &ProfileTarget,
) -> Result<(), DeviceError> {
    let ours = plan.name.to_lowercase();
    let mut now = names(current);
    if plan.associates {
        now.retain(|n| *n != ours);
    }
    now.sort_unstable();
    if now != sorted_names(original) {
        return Err(conflict(format!(
            "associations changed by another actor: {:?}",
            names(current)
        )));
    }
    if let ProfileSelection::Override(r) = &current.selection {
        let original_choice =
            matches!(&original.selection, ProfileSelection::Override(o) if same_ref(o, r));
        if !same_name(&r.native_id, &plan.name) && !original_choice {
            return Err(conflict(format!(
                "selection changed by another actor: {}",
                r.native_id
            )));
        }
    }
    let changed_by_us = target.scope.user == UserScope::Current
        && current.applied_scope == Some(scope_of(UserScope::Current));
    if current.applied_scope != original.applied_scope && !changed_by_us {
        return Err(conflict(format!(
            "per-user switch changed by another actor: {:?}",
            current.applied_scope
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
