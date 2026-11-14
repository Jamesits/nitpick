use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use nitpick_core::device::{
    ContentHash, DeviceError, DeviceId, DisplayDeviceControl, IccProfile, PlannedResource,
    PreparedProfileChange, ProfileRef, ProfileRole, ProfileSelection, ProfileTarget, Report,
    ResourceKind, UserScope,
};

use super::{INSTALLED_PREFIX, MacProfileNative, MacScope, PRECEDENCE, PREFERENCES};
use crate::macos::{ColorSyncDevice, DisplayReport, MacDisplay, MacNative, ScreenReport};
use crate::recovery;

const CC: MacScope = PRECEDENCE[0];
const CA: MacScope = PRECEDENCE[1];
const AC: MacScope = PRECEDENCE[2];
const AA: MacScope = PRECEDENCE[3];
const ALL: MacScope = AA;

const NO_WRITES: [String; 0] = [];

const USER_DIR: &str = "/Users/test/Library/ColorSync/Profiles";
const SYSTEM_DIR: &str = "/Library/ColorSync/Profiles";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Device {
    default_id: Option<String>,
    factory_ids: Vec<String>,
    scope: MacScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    /// The call fails without an effect.
    Before,
    /// The call has its effect, then fails, as after a timeout.
    After,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Defect {
    UnreadablePreferences,
    /// `ColorSyncDeviceCopyDeviceInfo` resolves a profile that no scope
    /// holds.
    ResolvesOther,
    IgnoresSet,
    RejectsProfile,
    DeniesFiles,
}

/// Custom profiles by (device UUID, scope index), then by profile ID.
type Preferences = BTreeMap<(String, usize), BTreeMap<String, String>>;

/// Native `ColorSync` model. The resolved custom profile is the first
/// scope entry in [`PRECEDENCE`] order.
#[derive(Default)]
struct World {
    displays: Vec<DisplayReport>,
    devices: HashMap<String, Device>,
    preferences: Preferences,
    files: BTreeMap<String, Vec<u8>>,
    writes: Vec<String>,
    fail: Option<(usize, When, DeviceError)>,
    root: bool,
    defect: Option<Defect>,
}

impl World {
    fn resolved(&self, uuid: &str, id: &str) -> Option<String> {
        if self.defect == Some(Defect::ResolvesOther) {
            return Some("/elsewhere.icc".into());
        }
        (0..PRECEDENCE.len()).find_map(|i| {
            self.preferences
                .get(&(uuid.to_owned(), i))
                .and_then(|m| m.get(id).cloned())
        })
    }

    fn device_info(&self, uuid: &str) -> Option<ColorSyncDevice> {
        let d = self.devices.get(uuid)?;
        let mut ids = d.factory_ids.clone();
        ids.extend(d.default_id.clone());
        ids.sort_unstable();
        ids.dedup();
        Some(ColorSyncDevice {
            description: Some("ColorSync name".into()),
            default_profile_id: d.default_id.clone(),
            factory_profile_ids: d.factory_ids.clone(),
            custom_profiles: ids
                .into_iter()
                .filter_map(|id| self.resolved(uuid, &id).map(|p| (id, Some(p))))
                .collect(),
            user_scope: Some(d.scope.user),
            host_scope: Some(d.scope.host),
        })
    }

    /// The comparable native state: custom profiles and files.
    fn state(&self) -> (Preferences, BTreeMap<String, Vec<u8>>) {
        let mut prefs = self.preferences.clone();
        prefs.retain(|_, m| !m.is_empty());
        (prefs, self.files.clone())
    }
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<World>>);

fn uuid(n: u32) -> String {
    format!("00000000-0000-0000-0000-00000000000{n}")
}

fn display(n: u32) -> DisplayReport {
    DisplayReport {
        display_id: n,
        uuid: Ok(uuid(n)),
        vendor: 0x10ac,
        model: 0x4321,
        serial: 0x1000 + n,
        builtin: false,
        modes: Ok(Vec::new()),
        colorsync: None,
    }
}

fn native_error(api: &str) -> DeviceError {
    DeviceError::Native {
        api: api.into(),
        code: None,
        message: "returned false".into(),
    }
}

impl Fake {
    /// Displays `1..=count`, each with one factory profile `1` registered
    /// at `scope`.
    fn with(count: u32, scope: MacScope) -> Self {
        let fake = Self::default();
        {
            let mut w = fake.0.borrow_mut();
            for n in 1..=count {
                w.displays.push(display(n));
                w.devices.insert(
                    uuid(n),
                    Device {
                        default_id: Some("1".into()),
                        factory_ids: vec!["1".into()],
                        scope,
                    },
                );
            }
        }
        fake
    }

    fn backend(&self) -> MacDisplay<Self> {
        MacDisplay::with_native(self.clone())
    }

    /// One native write. `check` rejects it without an effect.
    fn write(
        &self,
        op: String,
        check: impl FnOnce(&World) -> Result<(), DeviceError>,
        effect: impl FnOnce(&mut World),
    ) -> Result<(), DeviceError> {
        let mut w = self.0.borrow_mut();
        let index = w.writes.len();
        w.writes.push(op);
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

    fn writes(&self) -> Vec<String> {
        self.0.borrow().writes.clone()
    }

    fn state(&self) -> (Preferences, BTreeMap<String, Vec<u8>>) {
        self.0.borrow().state()
    }

    fn fail_at(&self, index: usize, when: When, error: DeviceError) {
        self.0.borrow_mut().fail = Some((index, when, error));
    }

    fn clear_fail(&self) {
        self.0.borrow_mut().fail = None;
    }

    fn defect(&self, defect: Option<Defect>) {
        self.0.borrow_mut().defect = defect;
    }

    fn root(&self, root: bool) {
        self.0.borrow_mut().root = root;
    }

    fn put_file(&self, path: &str, bytes: &[u8]) {
        self.0
            .borrow_mut()
            .files
            .insert(path.into(), bytes.to_vec());
    }

    /// An external custom profile entry; `None` removes it.
    fn put_custom(&self, n: u32, scope: MacScope, id: &str, path: Option<&str>) {
        let mut w = self.0.borrow_mut();
        let entry = w.preferences.entry((uuid(n), scope.index())).or_default();
        match path {
            Some(p) => entry.insert(id.into(), p.into()),
            None => entry.remove(id),
        };
    }

    fn custom(&self, n: u32, scope: MacScope, id: &str) -> Option<String> {
        self.0
            .borrow()
            .preferences
            .get(&(uuid(n), scope.index()))
            .and_then(|m| m.get(id).cloned())
    }
}

impl MacNative for Fake {
    fn displays(&mut self) -> Result<Vec<DisplayReport>, DeviceError> {
        let w = self.0.borrow();
        Ok(w.displays
            .iter()
            .cloned()
            .map(|mut d| {
                d.colorsync = d.uuid.as_ref().ok().and_then(|u| w.device_info(u));
                d
            })
            .collect())
    }

    fn screens(&mut self) -> Result<Vec<ScreenReport>, String> {
        Ok(Vec::new())
    }
}

impl MacProfileNative for Fake {
    fn device_info(&mut self, uuid: &str) -> Result<Option<ColorSyncDevice>, DeviceError> {
        Ok(self.0.borrow().device_info(uuid))
    }

    fn scoped_custom_profiles(
        &mut self,
        uuid: &str,
        scope: MacScope,
    ) -> Result<Vec<(String, String)>, DeviceError> {
        let w = self.0.borrow();
        if w.defect == Some(Defect::UnreadablePreferences) {
            return Err(native_error("CFPreferencesCopyValue"));
        }
        Ok(w.preferences
            .get(&(uuid.to_owned(), scope.index()))
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default())
    }

    fn set_custom_profile(
        &mut self,
        uuid: &str,
        scope: MacScope,
        profile_id: &str,
        path: Option<&str>,
    ) -> Result<(), DeviceError> {
        let key = (uuid.to_owned(), scope.index());
        let ignore = self.0.borrow().defect == Some(Defect::IgnoresSet);
        let (id, value) = (profile_id.to_owned(), path.map(str::to_owned));
        let check_uuid = uuid.to_owned();
        let check_value = value.clone();
        self.write(
            format!("set {scope:?} {profile_id} {path:?}"),
            |w| {
                let device = w.devices.get(&check_uuid);
                let within = device.is_some_and(|d| scope.within(d.scope));
                let present = check_value.as_ref().is_none_or(|p| w.files.contains_key(p));
                // Root is checked by the backend before the call.
                if within && present {
                    Ok(())
                } else {
                    Err(native_error("ColorSyncDeviceSetCustomProfiles"))
                }
            },
            |w| {
                if ignore {
                    return;
                }
                let entry = w.preferences.entry(key).or_default();
                match value {
                    Some(p) => entry.insert(id, p),
                    None => entry.remove(&id),
                };
            },
        )
    }

    fn verify_profile(&mut self, _: &[u8]) -> Result<(), DeviceError> {
        if self.0.borrow().defect == Some(Defect::RejectsProfile) {
            return Err(DeviceError::InvalidProfile(
                "ColorSyncProfileVerify: injected".into(),
            ));
        }
        Ok(())
    }

    fn is_root(&mut self) -> bool {
        self.0.borrow().root
    }

    fn profile_dir(&mut self, user: UserScope) -> Result<String, DeviceError> {
        Ok(match user {
            UserScope::Current => USER_DIR.into(),
            UserScope::All => SYSTEM_DIR.into(),
        })
    }

    fn read_file(&mut self, path: &str) -> Result<Option<Vec<u8>>, DeviceError> {
        Ok(self.0.borrow().files.get(path).cloned())
    }

    fn create_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), DeviceError> {
        let (path, bytes) = (path.to_owned(), bytes.to_vec());
        let check_path = path.clone();
        self.write(
            format!("create {path}"),
            |w| {
                if w.defect == Some(Defect::DeniesFiles) {
                    return Err(DeviceError::PermissionDenied(format!("open {check_path}")));
                }
                if w.files.contains_key(&check_path) {
                    return Err(DeviceError::StateConflict(format!("{check_path} exists")));
                }
                Ok(())
            },
            |w| {
                w.files.insert(path, bytes);
            },
        )
    }

    fn remove_file(&mut self, path: &str) -> Result<(), DeviceError> {
        let path = path.to_owned();
        self.write(
            format!("remove {path}"),
            |_| Ok(()),
            |w| {
                w.files.remove(&path);
            },
        )
    }
}

fn device(n: u32) -> DeviceId {
    DeviceId {
        backend: "macos".into(),
        key: uuid(n),
    }
}

fn target(n: u32, scope: MacScope) -> ProfileTarget {
    ProfileTarget {
        device: device(n),
        role: ProfileRole::Standard,
        scope: scope.profile_scope(),
    }
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

fn reference(path: &str, tag: u8) -> ProfileRef {
    ProfileRef {
        native_id: path.into(),
        hash: Some(ContentHash::of(&icc_bytes(tag))),
    }
}

fn installed(change: &PreparedProfileChange) -> &str {
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

fn is_unsupported<T: std::fmt::Debug>(r: &Result<T, DeviceError>, part: &str) -> bool {
    matches!(r, Err(DeviceError::Unsupported { reason, .. }) if reason.contains(part))
}

#[test]
fn targets_follow_device_scope() {
    let fake = Fake::with(1, ALL);
    let mut b = fake.backend();
    let targets = b.profile_targets(&device(1)).unwrap();
    assert_eq!(
        targets,
        PRECEDENCE.iter().map(|s| target(1, *s)).collect::<Vec<_>>()
    );

    let fake = Fake::with(1, CC);
    let mut b = fake.backend();
    assert_eq!(b.profile_targets(&device(1)).unwrap(), vec![target(1, CC)]);

    let fake = Fake::with(1, CA);
    let mut b = fake.backend();
    assert_eq!(
        b.profile_targets(&device(1)).unwrap(),
        vec![target(1, CC), target(1, CA)]
    );

    // No ColorSync device for the UUID.
    fake.0.borrow_mut().devices.clear();
    assert_eq!(b.profile_targets(&device(1)).unwrap(), vec![]);
}

#[test]
fn unaddressable_targets_are_unsupported() {
    let fake = Fake::with(1, CC);
    let mut b = fake.backend();
    let mut advanced = target(1, CC);
    advanced.role = ProfileRole::AdvancedColor;
    assert!(is_unsupported(
        &b.profile_state(&advanced),
        "Advanced Color"
    ));
    let mut no_host = target(1, CC);
    no_host.scope.host = None;
    assert!(is_unsupported(&b.profile_state(&no_host), "host scope"));
    assert!(is_unsupported(
        &b.prepare_icc_profile(&target(1, AA), &icc(1)),
        "wider than the device scope"
    ));
    let mut foreign = target(1, CC);
    foreign.device.backend = "windows".into();
    assert!(is_unsupported(&b.profile_state(&foreign), "backend"));
    assert!(matches!(
        b.profile_state(&target(2, CC)),
        Err(DeviceError::Disconnected(_))
    ));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn state_without_custom_profile() {
    let fake = Fake::with(1, ALL);
    let mut b = fake.backend();
    let s = b.profile_state(&target(1, CC)).unwrap();
    assert_eq!(s.associations, Report::known(Vec::new(), PREFERENCES));
    assert_eq!(s.selection, ProfileSelection::NoOverride);
    assert_eq!(s.applied_scope, None);
}

#[test]
fn state_reports_override_inherited_and_hashes() {
    let fake = Fake::with(1, ALL);
    let site = format!("{SYSTEM_DIR}/site.icc");
    let mine = format!("{USER_DIR}/mine.icc");
    fake.put_file(&site, &icc_bytes(9));
    fake.put_file(&mine, &icc_bytes(8));
    fake.put_custom(1, AA, "1", Some(&site));
    fake.put_custom(1, CC, "1", Some(&mine));
    // Another profile ID does not count.
    fake.put_custom(1, CA, "2", Some(&mine));
    let mut b = fake.backend();
    let state = |b: &mut MacDisplay<Fake>, s| b.profile_state(&target(1, s)).unwrap();

    let cc = state(&mut b, CC);
    assert_eq!(
        cc.selection,
        ProfileSelection::Override(reference(&mine, 8))
    );
    assert_eq!(cc.associations.value().unwrap(), &vec![reference(&mine, 8)]);
    let inherited = ProfileSelection::Inherited {
        from: AA.profile_scope(),
        profile: reference(&site, 9),
    };
    let ca = state(&mut b, CA);
    assert_eq!(ca.selection, inherited);
    assert_eq!(ca.associations.value().unwrap(), &vec![]);
    assert_eq!(state(&mut b, AC).selection, inherited);
    assert_eq!(
        state(&mut b, AA).selection,
        ProfileSelection::Override(reference(&site, 9))
    );

    // A missing file gives no hash.
    fake.0.borrow_mut().files.remove(&mine);
    let cc = state(&mut b, CC);
    assert_eq!(
        cc.selection,
        ProfileSelection::Override(ProfileRef {
            native_id: mine,
            hash: None
        })
    );
}

/// A change at another scope changes the revision of every target.
#[test]
fn revision_covers_every_scope() {
    let fake = Fake::with(1, ALL);
    fake.put_file("/p.icc", &icc_bytes(1));
    let mut b = fake.backend();
    let before = b.profile_state(&target(1, AA)).unwrap();
    fake.put_custom(1, CC, "1", Some("/p.icc"));
    let after = b.profile_state(&target(1, AA)).unwrap();
    assert_eq!(before.selection, after.selection);
    assert_ne!(before.revision, after.revision);
}

/// The scope reads must give the documented resolved value; otherwise the
/// undocumented storage is not understood and the state is unreadable.
#[test]
fn resolution_mismatch_is_unreadable() {
    let fake = Fake::with(1, CC);
    fake.defect(Some(Defect::ResolvesOther));
    let mut b = fake.backend();
    let s = b.profile_state(&target(1, CC)).unwrap();
    assert!(
        matches!(&s.selection, ProfileSelection::Unreadable(r) if r.contains("/elsewhere.icc"))
    );
    assert!(is_unsupported(
        &b.prepare_icc_profile(&target(1, CC), &icc(1)),
        "selection unreadable"
    ));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn unreadable_preferences_are_not_restorable() {
    let fake = Fake::with(1, CC);
    fake.defect(Some(Defect::UnreadablePreferences));
    let mut b = fake.backend();
    let s = b.profile_state(&target(1, CC)).unwrap();
    assert!(matches!(s.associations, Report::Unknown(_)));
    assert!(matches!(s.selection, ProfileSelection::Unreadable(_)));
    assert!(matches!(
        b.prepare_icc_profile(&target(1, CC), &icc(1)),
        Err(DeviceError::Unsupported { .. })
    ));
}

#[test]
fn prepare_plans_unique_resources_without_writes() {
    let fake = Fake::with(2, ALL);
    fake.root(true);
    let mut b = fake.backend();
    let first = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    let other_host = b.prepare_icc_profile(&target(1, CA), &icc(1)).unwrap();
    let other_device = b.prepare_icc_profile(&target(2, CC), &icc(1)).unwrap();
    let all_users = b.prepare_icc_profile(&target(1, AA), &icc(1)).unwrap();
    assert_eq!(fake.writes(), NO_WRITES);
    let path = installed(&first);
    assert_eq!(
        first.resources[1],
        PlannedResource {
            kind: ResourceKind::Association,
            native_id: "1".into(),
        }
    );
    let name = path.strip_prefix(&format!("{USER_DIR}/")).unwrap();
    assert!(name.starts_with(INSTALLED_PREFIX));
    assert_eq!(name.rsplit_once('.').map(|(_, e)| e), Some("icc"));
    assert_ne!(path, installed(&other_host));
    assert_ne!(path, installed(&other_device));
    assert!(installed(&all_users).starts_with(&format!("{SYSTEM_DIR}/{INSTALLED_PREFIX}")));
    assert_eq!(first.original.selection, ProfileSelection::NoOverride);
    // Deterministic.
    assert_eq!(
        b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap(),
        first
    );
}

#[test]
fn rejected_profile_fails_preparation() {
    let fake = Fake::with(1, CC);
    fake.defect(Some(Defect::RejectsProfile));
    let mut b = fake.backend();
    assert!(matches!(
        b.prepare_icc_profile(&target(1, CC), &icc(1)),
        Err(DeviceError::InvalidProfile(r)) if r.contains("ColorSyncProfileVerify")
    ));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn narrower_override_blocks_preparation() {
    let fake = Fake::with(1, ALL);
    fake.put_file("/p.icc", &icc_bytes(5));
    fake.put_custom(1, CC, "1", Some("/p.icc"));
    let mut b = fake.backend();
    assert!(is_unsupported(
        &b.prepare_icc_profile(&target(1, CA), &icc(1)),
        "narrower scope"
    ));
    assert!(b.prepare_icc_profile(&target(1, CC), &icc(1)).is_ok());
}

#[test]
fn leftover_installed_file_conflicts() {
    let fake = Fake::with(1, CC);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    fake.put_file(installed(&change), &icc_bytes(1));
    assert!(is_conflict(&b.prepare_icc_profile(&target(1, CC), &icc(1))));
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn assign_and_restore_without_prior_override() {
    let fake = Fake::with(1, CC);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    let path = installed(&change).to_owned();
    let set = b.set_icc_profile(&change).unwrap();
    assert_eq!(
        set.selection,
        ProfileSelection::Override(ProfileRef {
            native_id: path.clone(),
            hash: Some(icc(1).hash()),
        })
    );
    assert_eq!(set.applied_scope, None);
    assert_eq!(
        fake.writes(),
        [
            format!("create {path}"),
            format!("set {CC:?} 1 Some({path:?})"),
        ]
    );
    let restored = b.restore_profile(&change).unwrap();
    assert_eq!(restored, change.original);
    assert_eq!(fake.state(), before);
    assert_eq!(
        fake.writes()[2..],
        [format!("set {CC:?} 1 None"), format!("remove {path}")]
    );
    // Repeated restoration writes nothing.
    let writes = fake.writes().len();
    assert_eq!(b.restore_profile(&change).unwrap(), change.original);
    assert_eq!(fake.writes().len(), writes);
}

#[test]
fn previous_override_is_restored() {
    let fake = Fake::with(1, CC);
    let mine = format!("{USER_DIR}/mine.icc");
    fake.put_file(&mine, &icc_bytes(8));
    fake.put_custom(1, CC, "1", Some(&mine));
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    assert_eq!(
        change.original.selection,
        ProfileSelection::Override(reference(&mine, 8))
    );
    b.set_icc_profile(&change).unwrap();
    assert_eq!(fake.custom(1, CC, "1").as_deref(), Some(installed(&change)));
    b.restore_profile(&change).unwrap();
    assert_eq!(fake.state(), before);
}

/// The target had no entry and inherited a wider one. Restoration passes
/// `kCFNull`, and the wider entry applies again.
#[test]
fn inherited_selection_is_restored() {
    let fake = Fake::with(1, ALL);
    let site = format!("{SYSTEM_DIR}/site.icc");
    fake.put_file(&site, &icc_bytes(9));
    fake.put_custom(1, AA, "1", Some(&site));
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    assert!(matches!(
        change.original.selection,
        ProfileSelection::Inherited { .. }
    ));
    b.set_icc_profile(&change).unwrap();
    assert_eq!(b.restore_profile(&change).unwrap(), change.original);
    assert_eq!(fake.state(), before);
}

#[test]
fn identical_custom_profile_writes_nothing() {
    let fake = Fake::with(1, CC);
    let mine = format!("{USER_DIR}/mine.icc");
    fake.put_file(&mine, &icc_bytes(1));
    fake.put_custom(1, CC, "1", Some(&mine));
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    assert_eq!(change.resources, Vec::new());
    let set = b.set_icc_profile(&change).unwrap();
    assert_eq!(
        set.selection,
        ProfileSelection::Override(reference(&mine, 1))
    );
    assert_eq!(b.restore_profile(&change).unwrap(), change.original);
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn failure_after_each_write_is_restorable() {
    let errors = [
        DeviceError::Timeout,
        native_error("ColorSyncDeviceSetCustomProfiles"),
    ];
    for step in 0..2 {
        for when in [When::Before, When::After] {
            for error in &errors {
                let fake = Fake::with(1, CC);
                let before = fake.state();
                let mut b = fake.backend();
                let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
                fake.fail_at(step, when, error.clone());
                assert_eq!(
                    b.set_icc_profile(&change).unwrap_err(),
                    *error,
                    "step {step} {when:?}"
                );
                fake.clear_fail();
                assert_eq!(
                    b.restore_profile(&change),
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
    // Restoration writes: the original entry, then the file removal.
    for step in 0..2 {
        for when in [When::Before, When::After] {
            let fake = Fake::with(1, CC);
            let before = fake.state();
            let mut b = fake.backend();
            let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
            b.set_icc_profile(&change).unwrap();
            let writes = fake.writes().len();
            fake.fail_at(writes + step, when, DeviceError::Timeout);
            assert_eq!(b.restore_profile(&change), Err(DeviceError::Timeout));
            fake.clear_fail();
            assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
            assert_eq!(fake.state(), before, "step {step} {when:?}");
        }
    }
}

/// A crash after a write: a new backend restores from the decoded record.
#[test]
fn recovery_record_restores_after_restart() {
    let fake = Fake::with(1, CC);
    let before = fake.state();
    let change = fake
        .backend()
        .prepare_icc_profile(&target(1, CC), &icc(1))
        .unwrap();
    let record = recovery::encode(&change).unwrap();
    fake.fail_at(1, When::After, DeviceError::Timeout);
    assert!(fake.backend().set_icc_profile(&change).is_err());
    fake.clear_fail();
    let decoded = recovery::decode(&record).unwrap();
    assert_eq!(decoded, change);
    assert_eq!(
        fake.backend().restore_profile(&decoded),
        Ok(change.original)
    );
    assert_eq!(fake.state(), before);
}

#[test]
fn any_user_scope_needs_root() {
    let fake = Fake::with(1, ALL);
    let before = fake.state();
    let mut b = fake.backend();
    assert!(matches!(
        b.prepare_icc_profile(&target(1, AA), &icc(1)),
        Err(DeviceError::PermissionDenied(_))
    ));
    fake.root(true);
    let change = b.prepare_icc_profile(&target(1, AA), &icc(1)).unwrap();
    fake.root(false);
    assert!(matches!(
        b.set_icc_profile(&change),
        Err(DeviceError::PermissionDenied(_))
    ));
    assert_eq!(fake.writes(), NO_WRITES);

    fake.root(true);
    b.set_icc_profile(&change).unwrap();
    let writes = fake.writes().len();
    fake.root(false);
    assert!(matches!(
        b.restore_profile(&change),
        Err(DeviceError::PermissionDenied(_))
    ));
    assert_eq!(fake.writes().len(), writes);
    fake.root(true);
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
}

#[test]
fn file_permission_error_is_restorable() {
    let fake = Fake::with(1, CC);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    fake.defect(Some(Defect::DeniesFiles));
    assert!(matches!(
        b.set_icc_profile(&change),
        Err(DeviceError::PermissionDenied(_))
    ));
    fake.defect(None);
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
}

#[test]
fn readback_mismatch_fails_assignment() {
    let fake = Fake::with(1, CC);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    fake.defect(Some(Defect::IgnoresSet));
    assert!(is_conflict(&b.set_icc_profile(&change)));
    fake.defect(None);
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
}

#[test]
fn device_replacement_blocks_writes() {
    let fake = Fake::with(1, CC);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    {
        let mut w = fake.0.borrow_mut();
        w.displays[0].uuid = Ok("FFFFFFFF-0000-0000-0000-000000000001".into());
    }
    assert!(matches!(
        b.set_icc_profile(&change),
        Err(DeviceError::Disconnected(_))
    ));
    assert!(matches!(
        b.restore_profile(&change),
        Err(DeviceError::Disconnected(_))
    ));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn changed_identity_evidence_conflicts() {
    let fake = Fake::with(1, CC);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    fake.0.borrow_mut().displays[0].serial = 0x9999;
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn topology_change_blocks_assignment_but_not_restoration() {
    let fake = Fake::with(1, CC);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    fake.0.borrow_mut().displays.push(display(2));
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);

    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    fake.0.borrow_mut().displays.pop();
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
}

#[test]
fn external_changes_conflict_without_writes() {
    let fake = Fake::with(1, ALL);
    fake.put_file("/other.icc", &icc_bytes(7));
    let mut b = fake.backend();

    // Before assignment.
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    fake.put_custom(1, CC, "1", Some("/other.icc"));
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);
    fake.put_custom(1, CC, "1", None);

    // After assignment, at the target.
    b.set_icc_profile(&change).unwrap();
    let writes = fake.writes().len();
    fake.put_custom(1, CC, "1", Some("/other.icc"));
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes().len(), writes);

    // After assignment, at a wider scope.
    fake.put_custom(1, CC, "1", Some(installed(&change)));
    fake.put_custom(1, AA, "1", Some("/other.icc"));
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes().len(), writes);

    // The record is kept; restoration succeeds after the other change is
    // undone.
    fake.put_custom(1, AA, "1", None);
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
}

#[test]
fn installed_profile_in_use_elsewhere_is_kept() {
    let fake = Fake::with(2, CC);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    let path = installed(&change).to_owned();
    fake.put_custom(2, CC, "1", Some(&path));
    b.restore_profile(&change).unwrap();
    assert!(fake.0.borrow().files.contains_key(&path));
    assert!(!fake.writes().iter().any(|w| w.starts_with("remove")));
}

/// Restoration writes the profile ID of the record. Assignment requires
/// the device to keep that ID.
#[test]
fn profile_id_change() {
    let fake = Fake::with(1, CC);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    let renumber = |id: &str| {
        let mut w = fake.0.borrow_mut();
        let d = w.devices.get_mut(&uuid(1)).unwrap();
        d.default_id = Some(id.into());
        d.factory_ids = vec!["1".into(), "2".into()];
    };
    renumber("2");
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);

    renumber("1");
    b.set_icc_profile(&change).unwrap();
    renumber("2");
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
}

#[test]
fn malformed_records_are_rejected_without_writes() {
    let fake = Fake::with(1, CC);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CC), &icc(1)).unwrap();
    let mut bad = Vec::new();

    let mut c = change.clone();
    c.version += 1;
    bad.push(c);
    let mut c = change.clone();
    c.resources[0].kind = ResourceKind::RegisteredProfile;
    bad.push(c);
    let mut c = change.clone();
    c.resources.pop();
    bad.push(c);
    let mut c = change.clone();
    c.resources[1].kind = ResourceKind::InstalledProfile;
    bad.push(c);
    let mut c = change.clone();
    c.resources[0].native_id = format!("{USER_DIR}/user.icc");
    bad.push(c);
    let mut c = change.clone();
    c.resources[0].native_id = format!("{INSTALLED_PREFIX}relative.icc");
    bad.push(c);

    for c in &bad {
        assert!(
            matches!(b.set_icc_profile(c), Err(DeviceError::Unsupported { .. })),
            "{:?}",
            c.resources
        );
        assert!(matches!(
            b.restore_profile(c),
            Err(DeviceError::Unsupported { .. })
        ));
    }
    assert_eq!(fake.writes(), NO_WRITES);
}

/// The narrower scope check covers only the device's own scopes; an
/// entry of another device does not block preparation.
#[test]
fn other_device_entries_are_independent() {
    let fake = Fake::with(2, ALL);
    fake.put_file("/p.icc", &icc_bytes(5));
    fake.put_custom(2, CC, "1", Some("/p.icc"));
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1, CA), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    b.restore_profile(&change).unwrap();
    assert_eq!(fake.custom(2, CC, "1").as_deref(), Some("/p.icc"));
    assert_eq!(fake.custom(1, CA, "1"), None);
}
