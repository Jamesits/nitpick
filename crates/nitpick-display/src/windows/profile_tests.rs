use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use nitpick_core::device::{
    ContentHash, DeviceError, DeviceId, DisplayDeviceControl, IccProfile, PreparedProfileChange,
    ProfileRef, ProfileRole, ProfileScope, ProfileSelection, ProfileTarget, Report, ResourceKind,
    UserScope,
};

use super::{DisplayAddress, INSTALLED_PREFIX, WindowsProfileNative};
use crate::common::Field;
use crate::recovery;
use crate::windows::{
    AdvancedColorInfo, AdvancedColorInfo2, DxgiOutput, PathReport, TargetName, WindowsDisplay,
    WindowsNative, WindowsSetupNative, color_mode, info_2_flags, info_flags,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Lists {
    list: Vec<String>,
    default: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Display {
    per_user: bool,
    lists: HashMap<(UserScope, ProfileRole), Lists>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    /// The call fails without an effect.
    Before,
    /// The call has its effect, then fails, as after a timeout.
    After,
}

/// Associations by display, then the colour directory.
type NativeState = (Vec<(String, Display)>, BTreeMap<String, Vec<u8>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Defect {
    NoApi,
    UnreadableScope,
    UnreadableList,
    IgnoresSetDefault,
}

/// Native association model. Writes at the system-wide scope need an
/// administrator.
#[derive(Default)]
struct World {
    paths: Vec<PathReport>,
    displays: HashMap<String, Display>,
    files: BTreeMap<String, Vec<u8>>,
    writes: Vec<String>,
    fail: Option<(usize, When, DeviceError)>,
    admin: bool,
    defect: Option<Defect>,
}

impl World {
    fn display(&mut self, d: &DisplayAddress) -> &mut Display {
        self.displays.entry(d.device_path.clone()).or_default()
    }

    fn lists(&mut self, d: &DisplayAddress, scope: UserScope, role: ProfileRole) -> &mut Lists {
        self.display(d).lists.entry((scope, role)).or_default()
    }

    fn file(&self, name: &str) -> Option<&Vec<u8>> {
        self.files
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, b)| b)
    }

    /// The comparable native state: associations, switches and files.
    fn state(&self) -> NativeState {
        let mut displays: Vec<_> = self
            .displays
            .iter()
            .map(|(k, v)| {
                let mut v = v.clone();
                v.lists.retain(|_, l| *l != Lists::default());
                (k.clone(), v)
            })
            .filter(|(_, v)| *v != Display::default())
            .collect();
        displays.sort_by(|a, b| a.0.cmp(&b.0));
        (displays, self.files.clone())
    }
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<World>>);

fn denied() -> DeviceError {
    DeviceError::PermissionDenied("system-wide association".into())
}

fn native_error(api: &str, message: &str) -> DeviceError {
    DeviceError::Native {
        api: api.into(),
        code: Some(0x8007_0490),
        message: message.into(),
    }
}

impl Fake {
    fn with(paths: Vec<PathReport>) -> Self {
        let fake = Self::default();
        fake.0.borrow_mut().paths = paths;
        fake
    }

    fn backend(&self) -> WindowsDisplay<Self> {
        WindowsDisplay::with_native(self.clone())
    }

    /// One native write. `check` rejects it without an effect.
    fn write(
        &self,
        op: String,
        system_wide: bool,
        check: impl FnOnce(&World) -> Result<(), DeviceError>,
        effect: impl FnOnce(&mut World),
    ) -> Result<(), DeviceError> {
        let mut w = self.0.borrow_mut();
        let index = w.writes.len();
        w.writes.push(op);
        if system_wide && !w.admin {
            return Err(denied());
        }
        check(&w)?;
        match w.fail.clone() {
            Some((i, When::Before, e)) if i == index => Err(e),
            Some((i, When::After, e)) if i == index => {
                effect(&mut w);
                Err(e)
            }
            _ => {
                effect(&mut w);
                Ok(())
            }
        }
    }

    fn writes(&self) -> usize {
        self.0.borrow().writes.len()
    }

    fn state(&self) -> NativeState {
        self.0.borrow().state()
    }

    fn fail_at(&self, index: usize, when: When, error: DeviceError) {
        self.0.borrow_mut().fail = Some((index, when, error));
    }

    fn clear_fail(&self) {
        self.0.borrow_mut().fail = None;
    }

    fn put_file(&self, name: &str, bytes: &[u8]) {
        self.0
            .borrow_mut()
            .files
            .insert(name.into(), bytes.to_vec());
    }

    fn set_lists(&self, key: &str, scope: UserScope, role: ProfileRole, lists: Lists) {
        let mut w = self.0.borrow_mut();
        w.displays
            .entry(key.into())
            .or_default()
            .lists
            .insert((scope, role), lists);
    }

    fn set_per_user_flag(&self, key: &str, on: bool) {
        self.0
            .borrow_mut()
            .displays
            .entry(key.into())
            .or_default()
            .per_user = on;
    }
}

/// The setup read has its own fake in `windows/setup_tests.rs`.
impl WindowsSetupNative for Fake {
    fn gamma_ramp(&mut self, _: &str) -> Result<Vec<u16>, DeviceError> {
        Err(native_error("GetDeviceGammaRamp", "not in this fake"))
    }
    fn calibration_management(&mut self) -> Result<bool, DeviceError> {
        Err(native_error(
            "WcsGetCalibrationManagementState",
            "not in this fake",
        ))
    }
}

impl WindowsNative for Fake {
    fn display_paths(&mut self) -> Result<Vec<PathReport>, DeviceError> {
        Ok(self.0.borrow().paths.clone())
    }

    fn dxgi_outputs(&mut self) -> Field<Vec<DxgiOutput>> {
        Ok(Vec::new())
    }
}

impl WindowsProfileNative for Fake {
    fn profile_api(&mut self) -> Result<(), DeviceError> {
        if self.0.borrow().defect == Some(Defect::NoApi) {
            return Err(DeviceError::Unsupported {
                operation: "ICC profile control".into(),
                reason: "no ColorProfile functions".into(),
            });
        }
        Ok(())
    }

    fn applied_scope(&mut self, d: &DisplayAddress) -> Result<UserScope, DeviceError> {
        let mut w = self.0.borrow_mut();
        if w.defect == Some(Defect::UnreadableScope) {
            return Err(native_error("ColorProfileGetDisplayUserScope", "fake"));
        }
        Ok(if w.display(d).per_user {
            UserScope::Current
        } else {
            UserScope::All
        })
    }

    fn set_per_user(&mut self, d: &DisplayAddress, per_user: bool) -> Result<(), DeviceError> {
        let d = d.clone();
        self.write(
            format!("per_user {per_user}"),
            false,
            |_| Ok(()),
            |w| w.display(&d).per_user = per_user,
        )
    }

    fn association_list(
        &mut self,
        d: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
    ) -> Result<(Vec<String>, &'static str), DeviceError> {
        let mut w = self.0.borrow_mut();
        if w.defect == Some(Defect::UnreadableList) {
            return Err(native_error("RegGetValueW", "fake"));
        }
        Ok((w.lists(d, scope, role).list.clone(), "fake list"))
    }

    fn default_profile(
        &mut self,
        d: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
    ) -> Result<Option<String>, DeviceError> {
        Ok(self.0.borrow_mut().lists(d, scope, role).default.clone())
    }

    fn add_association(
        &mut self,
        d: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError> {
        let d = d.clone();
        self.write(
            format!("add {scope:?} {role:?} {name}"),
            scope == UserScope::All,
            |w| match w.file(name) {
                Some(_) => Ok(()),
                None => Err(native_error(
                    "ColorProfileAddDisplayAssociation",
                    "not installed",
                )),
            },
            |w| {
                let l = w.lists(&d, scope, role);
                if !l.list.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                    l.list.push(name.into());
                    // Observed: the first association becomes the default.
                    if l.default.is_none() {
                        l.default = Some(name.into());
                    }
                }
            },
        )
    }

    fn remove_association(
        &mut self,
        d: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError> {
        let d = d.clone();
        self.write(
            format!("remove {scope:?} {role:?} {name}"),
            scope == UserScope::All,
            |_| Ok(()),
            |w| {
                let l = w.lists(&d, scope, role);
                l.list.retain(|n| !n.eq_ignore_ascii_case(name));
                if l.default
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
                {
                    l.default = l.list.last().cloned();
                }
            },
        )
    }

    fn set_default(
        &mut self,
        d: &DisplayAddress,
        scope: UserScope,
        role: ProfileRole,
        name: &str,
    ) -> Result<(), DeviceError> {
        let d = d.clone();
        let ignore = self.0.borrow().defect == Some(Defect::IgnoresSetDefault);
        let check_d = d.clone();
        self.write(
            format!("default {scope:?} {role:?} {name}"),
            scope == UserScope::All,
            |w| {
                let listed = w
                    .displays
                    .get(&check_d.device_path)
                    .and_then(|x| x.lists.get(&(scope, role)))
                    .is_some_and(|l| l.list.iter().any(|n| n.eq_ignore_ascii_case(name)));
                if listed {
                    Ok(())
                } else {
                    Err(native_error(
                        "ColorProfileSetDisplayDefaultAssociation",
                        "not associated",
                    ))
                }
            },
            |w| {
                if !ignore {
                    w.lists(&d, scope, role).default = Some(name.into());
                }
            },
        )
    }

    fn installed_profiles(&mut self) -> Result<Vec<String>, DeviceError> {
        Ok(self.0.borrow().files.keys().cloned().collect())
    }

    fn read_installed(&mut self, name: &str) -> Result<Option<Vec<u8>>, DeviceError> {
        Ok(self.0.borrow().file(name).cloned())
    }

    fn install(&mut self, name: &str, bytes: &[u8]) -> Result<(), DeviceError> {
        let bytes = bytes.to_vec();
        self.write(
            format!("install {name}"),
            false,
            |_| Ok(()),
            |w| {
                w.files.insert(name.into(), bytes);
            },
        )
    }

    fn uninstall(&mut self, name: &str) -> Result<(), DeviceError> {
        self.write(
            format!("uninstall {name}"),
            false,
            |_| Ok(()),
            |w| w.files.retain(|n, _| !n.eq_ignore_ascii_case(name)),
        )
    }
}

fn key(n: u32) -> String {
    format!(r"\\?\DISPLAY#ABC1234#5&1&0&UID{n}#{{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}}")
}

fn path(n: u32, mode: i32) -> PathReport {
    PathReport {
        adapter_luid: 0x1_0000_2000,
        target_adapter_luid: 0x1_0000_2000,
        source_id: n,
        target_id: 100 + n,
        source_name: Ok(format!(r"\\.\DISPLAY{n}")),
        target_name: Ok(TargetName {
            friendly_name: "Monitor".into(),
            device_path: key(n),
            edid_ids: Some((0x1234, 0x5678)),
            connector_instance: n,
            output_technology: 10,
        }),
        color_info: Ok(AdvancedColorInfo {
            flags: info_flags::SUPPORTED | info_flags::ENABLED,
            bits_per_color_channel: 10,
        }),
        color_info_2: Ok(AdvancedColorInfo2 {
            flags: info_2_flags::SUPPORTED | info_2_flags::HDR_SUPPORTED,
            bits_per_color_channel: 10,
            active_color_mode: mode,
        }),
        sdr_white_level: Ok(1000),
        target_active_size: Some((3840, 2160)),
    }
}

fn hdr(n: u32) -> PathReport {
    path(n, color_mode::HDR)
}

fn device(n: u32) -> DeviceId {
    DeviceId {
        backend: "windows".into(),
        key: key(n),
    }
}

fn target(n: u32, role: ProfileRole, user: UserScope) -> ProfileTarget {
    ProfileTarget {
        device: device(n),
        role,
        scope: ProfileScope { user, host: None },
    }
}

fn ac_user(n: u32) -> ProfileTarget {
    target(n, ProfileRole::AdvancedColor, UserScope::Current)
}

fn icc_bytes(tag: u8) -> Vec<u8> {
    let mut b = vec![0u8; 132];
    b[..4].copy_from_slice(&132u32.to_be_bytes());
    b[36..40].copy_from_slice(b"acsp");
    b[131] = tag;
    b
}

fn icc(tag: u8) -> IccProfile {
    IccProfile::new(icc_bytes(tag)).unwrap()
}

fn lists(list: &[&str], default: Option<&str>) -> Lists {
    Lists {
        list: list.iter().map(|s| (*s).to_string()).collect(),
        default: default.map(Into::into),
    }
}

fn reference(name: &str, tag: u8) -> ProfileRef {
    ProfileRef {
        native_id: name.into(),
        hash: Some(ContentHash::of(&icc_bytes(tag))),
    }
}

fn installed_name(change: &PreparedProfileChange) -> &str {
    &change
        .resources
        .iter()
        .find(|r| r.kind == ResourceKind::InstalledProfile)
        .expect("planned installation")
        .native_id
}

fn is_conflict<T: std::fmt::Debug>(r: &Result<T, DeviceError>) -> bool {
    matches!(r, Err(DeviceError::StateConflict(_)))
}

#[test]
fn targets_cover_both_roles_and_scopes() {
    let fake = Fake::with(vec![hdr(1)]);
    let mut b = fake.backend();
    let targets = b.profile_targets(&device(1)).unwrap();
    assert_eq!(targets.len(), 4);
    for role in [ProfileRole::Standard, ProfileRole::AdvancedColor] {
        for user in [UserScope::Current, UserScope::All] {
            assert!(targets.contains(&target(1, role, user)));
        }
    }
    let caps = b.query_device(&device(1)).unwrap().capabilities;
    assert_eq!(caps.icc_profiles.value(), Some(&true));
}

#[test]
fn host_scope_and_foreign_backend_are_unsupported() {
    let fake = Fake::with(vec![hdr(1)]);
    let mut b = fake.backend();
    let mut t = ac_user(1);
    t.scope.host = Some(nitpick_core::device::HostScope::Current);
    assert!(matches!(
        b.prepare_icc_profile(&t, &icc(1)),
        Err(DeviceError::Unsupported { .. })
    ));
    let mut t = ac_user(1);
    t.device.backend = "macos".into();
    assert!(matches!(
        b.profile_state(&t),
        Err(DeviceError::Unsupported { .. })
    ));
}

#[test]
fn state_without_prior_override() {
    let fake = Fake::with(vec![hdr(1)]);
    let s = fake.backend().profile_state(&ac_user(1)).unwrap();
    assert_eq!(s.selection, ProfileSelection::NoOverride);
    assert_eq!(s.associations.value(), Some(&vec![]));
    assert_eq!(
        s.applied_scope,
        Some(ProfileScope {
            user: UserScope::All,
            host: None
        })
    );
}

#[test]
fn state_reports_inherited_override_and_hashes() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.put_file("sys.icc", &icc_bytes(7));
    fake.set_lists(
        &key(1),
        UserScope::All,
        ProfileRole::AdvancedColor,
        lists(&["sys.icc"], Some("sys.icc")),
    );
    let mut b = fake.backend();
    let s = b.profile_state(&ac_user(1)).unwrap();
    assert_eq!(
        s.selection,
        ProfileSelection::Inherited {
            from: ProfileScope {
                user: UserScope::All,
                host: None
            },
            profile: reference("sys.icc", 7),
        }
    );
    // With the per-user switch on, the system-wide selection does not
    // apply.
    fake.set_per_user_flag(&key(1), true);
    let s = b.profile_state(&ac_user(1)).unwrap();
    assert_eq!(s.selection, ProfileSelection::NoOverride);
    let system = b
        .profile_state(&target(1, ProfileRole::AdvancedColor, UserScope::All))
        .unwrap();
    assert_eq!(
        system.selection,
        ProfileSelection::Override(reference("sys.icc", 7))
    );
    assert_eq!(
        system.associations.value(),
        Some(&vec![reference("sys.icc", 7)])
    );
    // A missing file has no hash.
    fake.set_lists(
        &key(1),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&["gone.icc"], Some("gone.icc")),
    );
    let s = b.profile_state(&ac_user(1)).unwrap();
    assert_eq!(
        s.selection,
        ProfileSelection::Override(ProfileRef {
            native_id: "gone.icc".into(),
            hash: None
        })
    );
}

/// Each role has its own list and default, independent of the active
/// colour mode.
#[test]
fn roles_are_independent() {
    for mode in [color_mode::SDR, color_mode::HDR] {
        let fake = Fake::with(vec![path(1, mode)]);
        fake.put_file("ac.icc", &icc_bytes(3));
        fake.set_per_user_flag(&key(1), true);
        fake.set_lists(
            &key(1),
            UserScope::Current,
            ProfileRole::AdvancedColor,
            lists(&["ac.icc"], Some("ac.icc")),
        );
        let before = fake.state();
        let mut b = fake.backend();
        let t = target(1, ProfileRole::Standard, UserScope::Current);
        assert_eq!(
            b.profile_state(&t).unwrap().selection,
            ProfileSelection::NoOverride
        );
        let change = b.prepare_icc_profile(&t, &icc(1)).unwrap();
        b.set_icc_profile(&change).unwrap();
        let ac = b.profile_state(&ac_user(1)).unwrap();
        assert_eq!(
            ac.selection,
            ProfileSelection::Override(reference("ac.icc", 3))
        );
        assert_eq!(ac.associations.value().unwrap().len(), 1);
        b.restore_profile(&change).unwrap();
        assert_eq!(fake.state(), before);
    }
}

#[test]
fn unreadable_list_is_not_restorable() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.0.borrow_mut().defect = Some(Defect::UnreadableList);
    let mut b = fake.backend();
    let s = b.profile_state(&ac_user(1)).unwrap();
    assert!(matches!(s.associations, Report::Unknown(_)));
    assert!(matches!(
        b.prepare_icc_profile(&ac_user(1), &icc(1)),
        Err(DeviceError::Unsupported { .. })
    ));
}

#[test]
fn unreadable_scope_is_not_restorable() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.0.borrow_mut().defect = Some(Defect::UnreadableScope);
    let mut b = fake.backend();
    let s = b.profile_state(&ac_user(1)).unwrap();
    assert!(matches!(s.selection, ProfileSelection::Unreadable(_)));
    assert_eq!(s.applied_scope, None);
    assert!(matches!(
        b.prepare_icc_profile(&ac_user(1), &icc(1)),
        Err(DeviceError::Unsupported { .. })
    ));
}

#[test]
fn prepare_plans_unique_resources_without_writes() {
    let fake = Fake::with(vec![hdr(1), hdr(2)]);
    let mut b = fake.backend();
    let first = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    let second = b.prepare_icc_profile(&ac_user(2), &icc(1)).unwrap();
    let system = b
        .prepare_icc_profile(
            &target(1, ProfileRole::AdvancedColor, UserScope::All),
            &icc(1),
        )
        .unwrap();
    assert_eq!(fake.writes(), 0);
    let kinds: Vec<_> = first.resources.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        [ResourceKind::InstalledProfile, ResourceKind::Association]
    );
    let name = installed_name(&first);
    assert!(name.starts_with(INSTALLED_PREFIX));
    assert!(
        std::path::Path::new(name)
            .extension()
            .is_some_and(|e| e == "icc")
    );
    assert_ne!(name, installed_name(&second));
    assert_ne!(name, installed_name(&system));
    // Deterministic.
    let again = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    assert_eq!(again, first);
}

#[test]
fn leftover_installed_file_conflicts() {
    let fake = Fake::with(vec![hdr(1)]);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    fake.put_file(installed_name(&change), &icc_bytes(1));
    assert!(is_conflict(&b.prepare_icc_profile(&ac_user(1), &icc(1))));
}

#[test]
fn assign_and_restore_without_prior_override() {
    let fake = Fake::with(vec![hdr(1)]);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    let name = installed_name(&change).to_string();
    let set = b.set_icc_profile(&change).unwrap();
    assert_eq!(
        set.selection,
        ProfileSelection::Override(ProfileRef {
            native_id: name.clone(),
            hash: Some(icc(1).hash())
        })
    );
    // The per-user switch was off; a current-user selection needs it on.
    assert_eq!(
        set.applied_scope,
        Some(ProfileScope {
            user: UserScope::Current,
            host: None
        })
    );
    assert_eq!(
        fake.0.borrow().writes,
        [
            format!("install {name}"),
            "per_user true".into(),
            format!("add Current AdvancedColor {name}"),
            format!("default Current AdvancedColor {name}"),
        ]
    );
    let restored = b.restore_profile(&change).unwrap();
    assert_eq!(restored, change.original);
    assert_eq!(fake.state(), before);
    // Repeated restoration writes nothing.
    let writes = fake.writes();
    assert_eq!(b.restore_profile(&change).unwrap(), change.original);
    assert_eq!(fake.writes(), writes);
}

#[test]
fn failure_after_each_write_is_restorable() {
    let errors = [
        DeviceError::Timeout,
        native_error("ColorProfileAddDisplayAssociation", "injected"),
    ];
    for step in 0..4 {
        for when in [When::Before, When::After] {
            for error in &errors {
                let fake = Fake::with(vec![hdr(1)]);
                let before = fake.state();
                let mut b = fake.backend();
                let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
                fake.fail_at(step, when, error.clone());
                assert_eq!(
                    b.set_icc_profile(&change).unwrap_err(),
                    *error,
                    "step {step} {when:?}"
                );
                fake.clear_fail();
                let restored = b.restore_profile(&change);
                assert_eq!(
                    restored,
                    Ok(change.original.clone()),
                    "step {step} {when:?}"
                );
                assert_eq!(fake.state(), before, "step {step} {when:?}");
            }
        }
    }
}

#[test]
fn failed_restoration_can_be_repeated() {
    let fake = Fake::with(vec![hdr(1)]);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    let writes = fake.writes();
    // Restoration writes: remove, per-user switch, uninstall.
    for step in 0..3 {
        for when in [When::Before, When::After] {
            let fake2 = Fake::with(vec![hdr(1)]);
            let mut b2 = fake2.backend();
            let change = b2.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
            b2.set_icc_profile(&change).unwrap();
            fake2.fail_at(writes + step, when, DeviceError::Timeout);
            assert_eq!(b2.restore_profile(&change), Err(DeviceError::Timeout));
            fake2.clear_fail();
            assert_eq!(b2.restore_profile(&change), Ok(change.original.clone()));
            assert_eq!(fake2.state(), before, "step {step} {when:?}");
        }
    }
}

/// A crash after a write: a new backend restores from the decoded record.
#[test]
fn recovery_record_restores_after_restart() {
    let fake = Fake::with(vec![hdr(1)]);
    let before = fake.state();
    let change = fake
        .backend()
        .prepare_icc_profile(&ac_user(1), &icc(1))
        .unwrap();
    let record = recovery::encode(&change).unwrap();
    fake.fail_at(2, When::After, DeviceError::Timeout);
    assert!(fake.backend().set_icc_profile(&change).is_err());
    fake.clear_fail();
    let decoded = recovery::decode(&record).unwrap();
    assert_eq!(
        fake.backend().restore_profile(&decoded),
        Ok(change.original)
    );
    assert_eq!(fake.state(), before);
}

#[test]
fn previous_override_is_restored() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.put_file("user.icc", &icc_bytes(9));
    fake.set_per_user_flag(&key(1), true);
    fake.set_lists(
        &key(1),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&["user.icc"], Some("user.icc")),
    );
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    assert_eq!(
        change.original.selection,
        ProfileSelection::Override(reference("user.icc", 9))
    );
    let set = b.set_icc_profile(&change).unwrap();
    assert_eq!(set.associations.value().unwrap().len(), 2);
    assert!(
        !fake
            .0
            .borrow()
            .writes
            .iter()
            .any(|w| w.starts_with("per_user"))
    );
    b.restore_profile(&change).unwrap();
    assert_eq!(fake.state(), before);
}

#[test]
fn inherited_selection_is_restored() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.0.borrow_mut().admin = true;
    fake.put_file("sys.icc", &icc_bytes(7));
    fake.set_lists(
        &key(1),
        UserScope::All,
        ProfileRole::AdvancedColor,
        lists(&["sys.icc"], Some("sys.icc")),
    );
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    assert!(matches!(
        change.original.selection,
        ProfileSelection::Inherited { .. }
    ));
    b.set_icc_profile(&change).unwrap();
    assert_eq!(
        b.restore_profile(&change).unwrap().selection,
        change.original.selection
    );
    assert_eq!(fake.state(), before);
}

/// An identical profile that the user installed is reused and kept, with
/// its association on another display.
#[test]
fn existing_shared_profile_is_reused_and_kept() {
    let fake = Fake::with(vec![hdr(1), hdr(2)]);
    fake.put_file("shared.icc", &icc_bytes(1));
    fake.set_per_user_flag(&key(1), true);
    fake.set_per_user_flag(&key(2), true);
    fake.set_lists(
        &key(2),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&["shared.icc"], Some("shared.icc")),
    );
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    assert_eq!(change.resources.len(), 1);
    assert_eq!(change.resources[0].kind, ResourceKind::Association);
    assert_eq!(change.resources[0].native_id, "shared.icc");
    b.set_icc_profile(&change).unwrap();
    assert!(
        !fake
            .0
            .borrow()
            .writes
            .iter()
            .any(|w| w.starts_with("install"))
    );
    b.restore_profile(&change).unwrap();
    assert_eq!(fake.state(), before);
    assert!(
        !fake
            .0
            .borrow()
            .writes
            .iter()
            .any(|w| w.starts_with("uninstall"))
    );
}

#[test]
fn associated_profile_changes_only_the_selection() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.put_file("mine.icc", &icc_bytes(1));
    fake.put_file("other.icc", &icc_bytes(2));
    fake.set_per_user_flag(&key(1), true);
    fake.set_lists(
        &key(1),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&["mine.icc", "other.icc"], Some("other.icc")),
    );
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    assert_eq!(change.resources, vec![]);
    b.set_icc_profile(&change).unwrap();
    assert_eq!(
        fake.0.borrow().writes,
        ["default Current AdvancedColor mine.icc"]
    );
    b.restore_profile(&change).unwrap();
    assert_eq!(fake.state(), before);
}

/// Restoring "no selection" would need removal of the user's association.
#[test]
fn associated_profile_without_selection_is_unsupported() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.put_file("mine.icc", &icc_bytes(1));
    fake.set_lists(
        &key(1),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&["mine.icc"], None),
    );
    assert!(matches!(
        fake.backend().prepare_icc_profile(&ac_user(1), &icc(1)),
        Err(DeviceError::Unsupported { .. })
    ));
}

#[test]
fn system_wide_permission_error_is_restorable() {
    let fake = Fake::with(vec![hdr(1)]);
    let before = fake.state();
    let mut b = fake.backend();
    let t = target(1, ProfileRole::AdvancedColor, UserScope::All);
    let change = b.prepare_icc_profile(&t, &icc(1)).unwrap();
    assert!(matches!(
        b.set_icc_profile(&change),
        Err(DeviceError::PermissionDenied(_))
    ));
    // The installed file is removed; no system-wide write is needed.
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);

    fake.0.borrow_mut().admin = true;
    b.set_icc_profile(&change).unwrap();
    // The per-user switch is not changed for a system-wide target.
    assert!(
        !fake
            .0
            .borrow()
            .writes
            .iter()
            .any(|w| w.starts_with("per_user"))
    );
    b.restore_profile(&change).unwrap();
    assert_eq!(fake.state(), before);
}

#[test]
fn device_replacement_blocks_writes() {
    let fake = Fake::with(vec![hdr(1)]);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    let mut replaced = hdr(1);
    replaced.target_name.as_mut().unwrap().device_path = key(9);
    fake.0.borrow_mut().paths = vec![replaced.clone()];
    assert_eq!(
        b.set_icc_profile(&change),
        Err(DeviceError::Disconnected(device(1)))
    );
    assert_eq!(fake.writes(), 0);

    fake.0.borrow_mut().paths = vec![hdr(1)];
    b.set_icc_profile(&change).unwrap();
    let writes = fake.writes();
    fake.0.borrow_mut().paths = vec![replaced];
    assert_eq!(
        b.restore_profile(&change),
        Err(DeviceError::Disconnected(device(1)))
    );
    assert_eq!(fake.writes(), writes);
    // The record stays valid when the device returns.
    fake.0.borrow_mut().paths = vec![hdr(1)];
    assert_eq!(b.restore_profile(&change), Ok(change.original));
}

#[test]
fn changed_identity_evidence_conflicts() {
    let fake = Fake::with(vec![hdr(1)]);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    let mut other = hdr(1);
    other.target_name.as_mut().unwrap().edid_ids = Some((0x4321, 0x0001));
    fake.0.borrow_mut().paths = vec![other];
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes(), 0);
}

#[test]
fn topology_change_blocks_assignment_but_not_restoration() {
    let fake = Fake::with(vec![hdr(1)]);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    fake.0.borrow_mut().paths = vec![hdr(1), hdr(2)];
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), 0);

    fake.0.borrow_mut().paths = vec![hdr(1)];
    b.set_icc_profile(&change).unwrap();
    fake.0.borrow_mut().paths = vec![hdr(1), hdr(2)];
    assert_eq!(b.restore_profile(&change), Ok(change.original));
    assert_eq!(fake.state(), before);
}

#[test]
fn external_changes_conflict_without_writes() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.put_file("x.icc", &icc_bytes(5));
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    fake.set_per_user_flag(&key(1), true);
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), 0);
    fake.set_per_user_flag(&key(1), false);

    b.set_icc_profile(&change).unwrap();
    let name = installed_name(&change).to_string();
    fake.set_lists(
        &key(1),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&[&name, "x.icc"], Some("x.icc")),
    );
    let writes = fake.writes();
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes(), writes);

    // Another actor selected the original state's absence of a profile
    // but kept an extra association.
    fake.set_lists(
        &key(1),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&["x.icc"], Some("x.icc")),
    );
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes(), writes);
}

#[test]
fn readback_mismatch_fails_assignment() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.0.borrow_mut().defect = Some(Defect::IgnoresSetDefault);
    fake.put_file("user.icc", &icc_bytes(9));
    fake.set_lists(
        &key(1),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&["user.icc"], Some("user.icc")),
    );
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    assert!(
        matches!(b.set_icc_profile(&change), Err(DeviceError::StateConflict(m)) if m.contains("readback"))
    );
    assert_eq!(b.restore_profile(&change), Ok(change.original));
    assert_eq!(fake.state(), before);
}

/// The installed file stays if another display uses it after the
/// assignment.
#[test]
fn installed_profile_in_use_elsewhere_is_kept() {
    let fake = Fake::with(vec![hdr(1), hdr(2)]);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    let name = installed_name(&change).to_string();
    fake.set_lists(
        &key(2),
        UserScope::Current,
        ProfileRole::AdvancedColor,
        lists(&[&name], Some(&name)),
    );
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert!(fake.0.borrow().file(&name).is_some());
}

#[test]
fn other_record_versions_are_rejected_without_writes() {
    let fake = Fake::with(vec![hdr(1)]);
    let mut b = fake.backend();
    let mut change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    change.version += 1;
    assert!(matches!(
        b.set_icc_profile(&change),
        Err(DeviceError::Unsupported { .. })
    ));
    assert!(matches!(
        b.restore_profile(&change),
        Err(DeviceError::Unsupported { .. })
    ));
    assert_eq!(fake.writes(), 0);
}

/// A record must not make restoration remove a file that this backend
/// did not install.
#[test]
fn record_without_prefix_is_rejected() {
    let fake = Fake::with(vec![hdr(1)]);
    let mut b = fake.backend();
    let mut change = b.prepare_icc_profile(&ac_user(1), &icc(1)).unwrap();
    for r in &mut change.resources {
        r.native_id = "user.icc".into();
    }
    assert!(matches!(
        b.restore_profile(&change),
        Err(DeviceError::Unsupported { .. })
    ));
}

#[test]
fn missing_api_gives_no_targets() {
    let fake = Fake::with(vec![hdr(1)]);
    fake.0.borrow_mut().defect = Some(Defect::NoApi);
    let mut b = fake.backend();
    assert_eq!(b.profile_targets(&device(1)).unwrap(), vec![]);
    assert!(matches!(
        b.profile_state(&ac_user(1)),
        Err(DeviceError::Unsupported { .. })
    ));
}
