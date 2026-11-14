use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use nitpick_core::device::{
    ContentHash, DeviceError, DeviceId, DisplayDeviceControl, HostScope, IccProfile,
    PreparedProfileChange, ProfileRef, ProfileRole, ProfileScope, ProfileSelection, ProfileTarget,
    Report, ResourceKind, UserScope,
};

use super::{ColordProfile, INSTALLED_PREFIX, LinuxProfileNative, PROFILES, Relation};
use crate::common::NativeFailure;
use crate::edid::md5_hex;
use crate::edid::tests::{Spec, build};
use crate::linux::{
    COLORD_EDID_MD5, ColordDevice, ColordReport, ConnectorReport, KmsConnector, LinuxDisplay,
    LinuxNative,
};
use crate::recovery;

const UID: u32 = 1000;
const OTHER_UID: u32 = 1001;
const DIR: &str = "/home/test/.local/share/nitpick/icc";
const NO_WRITES: [String; 0] = [];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum When {
    /// The call fails without an effect.
    Before,
    /// The call has its effect, then fails, as after a timeout.
    After,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Profile {
    id: String,
    filename: String,
    owner: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    profile: String,
    relation: Relation,
    timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Device {
    path: String,
    id: String,
    owner: u32,
    md5: String,
    items: Vec<Item>,
}

/// Device-to-profile rows of the colord mapping database: (device ID,
/// profile ID) to timestamp.
type Mapping = BTreeMap<(String, String), u64>;

/// Device lists in colord order, profiles and files.
type State = (
    Vec<Vec<Item>>,
    BTreeMap<String, Profile>,
    BTreeMap<String, Vec<u8>>,
);

/// The colord model from its source code (`cd-device.c`, `cd-main.c`): a
/// device lists hard relations before soft ones, newest first.
#[derive(Default)]
struct World {
    connectors: Vec<ConnectorReport>,
    /// `Some(reason)`: colord is not running.
    stopped: Option<String>,
    devices: Vec<Device>,
    profiles: BTreeMap<String, Profile>,
    mapping: Mapping,
    files: BTreeMap<String, Vec<u8>>,
    clock: u64,
    writes: Vec<String>,
    fail: Option<(usize, When, DeviceError)>,
    /// polkit denies every colord write.
    denied: bool,
    /// Device profile reads fail.
    unreadable: bool,
}

impl World {
    fn device(&self, path: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.path == path)
    }

    fn device_mut(&mut self, path: &str) -> &mut Device {
        self.devices
            .iter_mut()
            .find(|d| d.path == path)
            .expect("device")
    }

    fn sorted(&self, path: &str) -> Vec<Item> {
        let mut items = self
            .device(path)
            .map(|d| d.items.clone())
            .unwrap_or_default();
        items.sort_by_key(|i| (i.relation == Relation::Soft, std::cmp::Reverse(i.timestamp)));
        items
    }

    /// `cd_profile_array_get_by_id_owner`: the caller's profile first, then
    /// a profile of any owner.
    fn find(&self, id: &str) -> Option<(String, Profile)> {
        let all: Vec<_> = self.profiles.iter().filter(|(_, p)| p.id == id).collect();
        all.iter()
            .find(|(_, p)| p.owner == UID)
            .or(all.first())
            .map(|(k, p)| ((*k).clone(), (*p).clone()))
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Comparable state: device lists, profiles and files. The mapping
    /// database is compared separately.
    fn state(&self) -> State {
        (
            self.devices.iter().map(|d| self.sorted(&d.path)).collect(),
            self.profiles.clone(),
            self.files.clone(),
        )
    }
}

#[derive(Clone, Default)]
struct Fake(Rc<RefCell<World>>);

fn edid(n: u32) -> Vec<u8> {
    let serial = format!("SN000{n}");
    build(&Spec {
        serial_text: Some(&serial),
        ..Spec::default()
    })
}

fn connector(n: u32, edid: Vec<u8>) -> ConnectorReport {
    ConnectorReport {
        card: "card1".into(),
        connector: format!("DP-{n}"),
        bus: Ok("0000:01:00.0".into()),
        connector_id: Ok(100 + n),
        status: "connected".into(),
        edid: Ok(edid),
    }
}

fn device_path(n: u32) -> String {
    format!("/org/freedesktop/ColorManager/devices/xrandr_{n}_{UID}")
}

fn profile_path(id: &str, owner: u32) -> String {
    format!(
        "/org/freedesktop/ColorManager/profiles/{}_{owner}",
        id.replace('-', "_")
    )
}

fn denied() -> DeviceError {
    DeviceError::PermissionDenied("failed to obtain org.freedesktop.color-manager auth".into())
}

fn colord_error(api: &str, message: &str) -> DeviceError {
    DeviceError::Native {
        api: api.into(),
        code: None,
        message: message.into(),
    }
}

impl Fake {
    /// Connectors `DP-1..=count`, each with one colord device of this user
    /// without profiles.
    fn with(count: u32) -> Self {
        let fake = Self::default();
        {
            let mut w = fake.0.borrow_mut();
            for n in 1..=count {
                let e = edid(n);
                w.devices.push(Device {
                    path: device_path(n),
                    id: format!("xrandr-Dell Inc.-DELL U2417H-SN000{n}"),
                    owner: UID,
                    md5: md5_hex(&e),
                    items: Vec::new(),
                });
                w.connectors.push(connector(n, e));
            }
        }
        fake
    }

    fn backend(&self) -> LinuxDisplay<Self> {
        LinuxDisplay::with_native(self.clone())
    }

    /// One native write. `check` rejects it without an effect.
    fn write<T>(
        &self,
        op: String,
        check: impl FnOnce(&World) -> Result<(), DeviceError>,
        effect: impl FnOnce(&mut World) -> T,
    ) -> Result<T, DeviceError> {
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
            _ => Ok(effect(&mut w)),
        }
    }

    /// A colord write that polkit checks first.
    fn colord_write<T>(
        &self,
        op: String,
        check: impl FnOnce(&World) -> Result<(), DeviceError>,
        effect: impl FnOnce(&mut World) -> T,
    ) -> Result<T, DeviceError> {
        self.write(
            op,
            |w| {
                if w.denied {
                    return Err(denied());
                }
                check(w)
            },
            effect,
        )
    }

    fn writes(&self) -> Vec<String> {
        self.0.borrow().writes.clone()
    }

    fn state(&self) -> State {
        self.0.borrow().state()
    }

    fn mapping(&self) -> Mapping {
        self.0.borrow().mapping.clone()
    }

    fn fail_at(&self, index: usize, when: When, error: DeviceError) {
        self.0.borrow_mut().fail = Some((index, when, error));
    }

    fn clear_fail(&self) {
        self.0.borrow_mut().fail = None;
    }

    /// An external profile with a file; returns its object path.
    fn put_profile(&self, id: &str, tag: u8, owner: u32) -> String {
        let path = profile_path(id, owner);
        let filename = format!("/usr/share/color/icc/{id}.icc");
        let mut w = self.0.borrow_mut();
        w.files.insert(filename.clone(), icc_bytes(tag));
        w.profiles.insert(
            path.clone(),
            Profile {
                id: id.into(),
                filename,
                owner,
            },
        );
        path
    }

    /// An external association, as colord adds it.
    fn associate(&self, n: u32, profile: &str, relation: Relation) {
        let mut w = self.0.borrow_mut();
        let timestamp = w.tick();
        w.device_mut(&device_path(n)).items.push(Item {
            profile: profile.into(),
            relation,
            timestamp,
        });
    }

    fn dissociate(&self, n: u32, profile: &str) {
        self.0
            .borrow_mut()
            .device_mut(&device_path(n))
            .items
            .retain(|i| i.profile != profile);
    }

    /// colord forgets `normal` profiles; devices and the mapping database
    /// remain.
    fn restart_colord(&self) {
        let mut w = self.0.borrow_mut();
        let ours: Vec<String> = w
            .profiles
            .iter()
            .filter(|(_, p)| p.id.starts_with(INSTALLED_PREFIX))
            .map(|(k, _)| k.clone())
            .collect();
        for path in ours {
            w.profiles.remove(&path);
            for d in &mut w.devices {
                d.items.retain(|i| i.profile != path);
            }
        }
    }

    fn profile(&self, path: &str) -> ColordProfile {
        let w = self.0.borrow();
        let p = &w.profiles[path];
        ColordProfile {
            path: path.into(),
            id: p.id.clone(),
            filename: p.filename.clone(),
            owner: p.owner,
        }
    }
}

impl LinuxNative for Fake {
    fn connectors(&mut self) -> Result<Vec<ConnectorReport>, DeviceError> {
        Ok(self.0.borrow().connectors.clone())
    }

    fn kms(&mut self, _: &str) -> Result<Vec<KmsConnector>, NativeFailure> {
        Ok(Vec::new())
    }

    fn colord(&mut self) -> Result<ColordReport, String> {
        let w = self.0.borrow();
        if let Some(reason) = &w.stopped {
            return Err(reason.clone());
        }
        Ok(ColordReport {
            uid: UID,
            devices: w
                .devices
                .iter()
                .map(|d| ColordDevice {
                    path: d.path.clone(),
                    id: d.id.clone(),
                    owner: d.owner,
                    metadata: BTreeMap::from([(COLORD_EDID_MD5.to_owned(), d.md5.clone())]),
                    profiles: w.sorted(&d.path).into_iter().map(|i| i.profile).collect(),
                })
                .collect(),
        })
    }
}

impl LinuxProfileNative for Fake {
    fn device_profiles(
        &mut self,
        device: &str,
    ) -> Result<Vec<(ColordProfile, Relation)>, DeviceError> {
        let items = {
            let w = self.0.borrow();
            if w.unreadable || w.device(device).is_none() {
                return Err(colord_error("colord Device.Profiles", "UnknownObject"));
            }
            w.sorted(device)
        };
        Ok(items
            .into_iter()
            .map(|i| (self.profile(&i.profile), i.relation))
            .collect())
    }

    fn find_profile(&mut self, id: &str) -> Result<Option<ColordProfile>, DeviceError> {
        let found = self.0.borrow().find(id);
        Ok(found.map(|(path, _)| self.profile(&path)))
    }

    fn create_profile(&mut self, id: &str, file: &str) -> Result<String, DeviceError> {
        let (id, file) = (id.to_owned(), file.to_owned());
        let (check_id, check_file) = (id.clone(), file.clone());
        self.colord_write(
            format!("create profile {id}"),
            |w| {
                if !w.files.contains_key(&check_file) {
                    return Err(colord_error("open", "No such file"));
                }
                if w.find(&check_id).is_some() {
                    return Err(DeviceError::StateConflict(format!(
                        "profile id '{check_id}' already exists"
                    )));
                }
                Ok(())
            },
            |w| {
                let path = profile_path(&id, UID);
                w.profiles.insert(
                    path.clone(),
                    Profile {
                        id: id.clone(),
                        filename: file,
                        owner: UID,
                    },
                );
                // `cd_main_profile_auto_add_from_db`.
                let rows: Vec<(String, u64)> = w
                    .mapping
                    .iter()
                    .filter(|((_, p), t)| *p == id && **t > 0)
                    .map(|((d, _), t)| (d.clone(), *t))
                    .collect();
                for (device_id, timestamp) in rows {
                    if let Some(d) = w.devices.iter_mut().find(|d| d.id == device_id) {
                        d.items.push(Item {
                            profile: path.clone(),
                            relation: Relation::Hard,
                            timestamp,
                        });
                    }
                }
                path
            },
        )
    }

    fn add_profile(&mut self, device: &str, profile: &str) -> Result<(), DeviceError> {
        let (device, profile) = (device.to_owned(), profile.to_owned());
        let (check_device, check_profile) = (device.clone(), profile.clone());
        self.colord_write(
            format!("add {profile}"),
            |w| {
                if !w.profiles.contains_key(&check_profile) {
                    return Err(colord_error(
                        "colord Device.AddProfile",
                        "ProfileDoesNotExist",
                    ));
                }
                let d = w.device(&check_device).expect("device");
                if d.items
                    .iter()
                    .any(|i| i.profile == check_profile && i.relation == Relation::Hard)
                {
                    return Err(DeviceError::StateConflict("already added".into()));
                }
                Ok(())
            },
            |w| {
                let timestamp = w.tick();
                let profile_id = w.profiles[&profile].id.clone();
                let d = w.device_mut(&device);
                let device_id = d.id.clone();
                if let Some(item) = d.items.iter_mut().find(|i| i.profile == profile) {
                    item.relation = Relation::Hard;
                } else {
                    d.items.push(Item {
                        profile,
                        relation: Relation::Hard,
                        timestamp,
                    });
                }
                w.mapping.insert((device_id, profile_id), timestamp);
            },
        )
    }

    fn make_profile_default(&mut self, device: &str, profile: &str) -> Result<(), DeviceError> {
        let (device, profile) = (device.to_owned(), profile.to_owned());
        let (check_device, check_profile) = (device.clone(), profile.clone());
        self.colord_write(
            format!("default {profile}"),
            |w| {
                let listed = w
                    .device(&check_device)
                    .is_some_and(|d| d.items.iter().any(|i| i.profile == check_profile));
                if listed {
                    Ok(())
                } else {
                    Err(colord_error(
                        "colord Device.MakeProfileDefault",
                        "ProfileDoesNotExist",
                    ))
                }
            },
            |w| {
                let timestamp = w.tick();
                let profile_id = w.profiles[&profile].id.clone();
                let d = w.device_mut(&device);
                let device_id = d.id.clone();
                let item = d.items.iter_mut().find(|i| i.profile == profile).unwrap();
                item.timestamp = timestamp;
                item.relation = Relation::Hard;
                w.mapping.insert((device_id, profile_id), timestamp);
            },
        )
    }

    fn remove_profile(&mut self, device: &str, profile: &str) -> Result<(), DeviceError> {
        let (device, profile) = (device.to_owned(), profile.to_owned());
        let (check_device, check_profile) = (device.clone(), profile.clone());
        self.colord_write(
            format!("remove {profile}"),
            |w| {
                let listed = w
                    .device(&check_device)
                    .is_some_and(|d| d.items.iter().any(|i| i.profile == check_profile));
                if listed {
                    Ok(())
                } else {
                    Err(colord_error(
                        "colord Device.RemoveProfile",
                        "ProfileDoesNotExist",
                    ))
                }
            },
            |w| {
                let profile_id = w.profiles[&profile].id.clone();
                let d = w.device_mut(&device);
                d.items.retain(|i| i.profile != profile);
                let device_id = d.id.clone();
                // `cd_mapping_db_clear_timestamp`.
                w.mapping.insert((device_id, profile_id), 0);
            },
        )
    }

    fn delete_profile(&mut self, profile: &str) -> Result<(), DeviceError> {
        let profile = profile.to_owned();
        let check_profile = profile.clone();
        self.colord_write(
            format!("delete {profile}"),
            |w| {
                if w.profiles.contains_key(&check_profile) {
                    Ok(())
                } else {
                    Err(colord_error("colord DeleteProfile", "NotFound"))
                }
            },
            |w| {
                w.profiles.remove(&profile);
                for d in &mut w.devices {
                    d.items.retain(|i| i.profile != profile);
                }
            },
        )
    }

    fn profile_dir(&mut self) -> Result<String, DeviceError> {
        Ok(DIR.into())
    }

    fn read_file(&mut self, path: &str) -> Result<Option<Vec<u8>>, DeviceError> {
        Ok(self.0.borrow().files.get(path).cloned())
    }

    fn create_file(&mut self, path: &str, bytes: &[u8]) -> Result<(), DeviceError> {
        let (path, bytes) = (path.to_owned(), bytes.to_vec());
        let check_path = path.clone();
        self.write(
            format!("create file {path}"),
            |w| {
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
            format!("remove file {path}"),
            |_| Ok(()),
            |w| {
                w.files.remove(&path);
            },
        )
    }
}

fn device(n: u32) -> DeviceId {
    DeviceId {
        backend: "linux".into(),
        key: format!("0000:01:00.0/DP-{n}/DEL40E7-SN000{n}"),
    }
}

fn target(n: u32) -> ProfileTarget {
    ProfileTarget {
        device: device(n),
        role: ProfileRole::Standard,
        scope: ProfileScope {
            user: UserScope::Current,
            host: None,
        },
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

fn reference(id: &str, tag: u8) -> ProfileRef {
    ProfileRef {
        native_id: id.into(),
        hash: Some(ContentHash::of(&icc_bytes(tag))),
    }
}

fn resource(change: &PreparedProfileChange, kind: ResourceKind) -> &str {
    &change
        .resources
        .iter()
        .find(|r| r.kind == kind)
        .expect("planned resource")
        .native_id
}

fn registered(change: &PreparedProfileChange) -> &str {
    resource(change, ResourceKind::RegisteredProfile)
}

fn installed(change: &PreparedProfileChange) -> &str {
    resource(change, ResourceKind::InstalledProfile)
}

fn is_conflict<T: std::fmt::Debug>(r: &Result<T, DeviceError>) -> bool {
    matches!(r, Err(DeviceError::StateConflict(_)))
}

fn is_unsupported<T: std::fmt::Debug>(r: &Result<T, DeviceError>, part: &str) -> bool {
    matches!(r, Err(DeviceError::Unsupported { reason, .. }) if reason.contains(part))
}

/// Mapping rows that restoration leaves: timestamp 0 for the change's
/// profile, so that colord never adds it again.
fn cleared(change: &PreparedProfileChange, n: u32) -> Mapping {
    BTreeMap::from([(
        (
            format!("xrandr-Dell Inc.-DELL U2417H-SN000{n}"),
            registered(change).to_owned(),
        ),
        0,
    )])
}

#[test]
fn one_target_per_matched_device() {
    let fake = Fake::with(2);
    let mut b = fake.backend();
    assert_eq!(b.profile_targets(&device(1)).unwrap(), vec![target(1)]);
    assert_eq!(b.profile_targets(&device(2)).unwrap(), vec![target(2)]);

    // A colord device of another user is not this process's device.
    fake.0.borrow_mut().devices[0].owner = OTHER_UID;
    assert_eq!(b.profile_targets(&device(1)).unwrap(), vec![]);
    assert!(is_unsupported(&b.profile_state(&target(1)), "owner 1001"));

    fake.0.borrow_mut().devices.clear();
    assert_eq!(b.profile_targets(&device(1)).unwrap(), vec![]);
    assert!(is_unsupported(
        &b.profile_state(&target(1)),
        "no colord display device"
    ));
}

/// Without the service, the device list, the query and the targets still
/// work; profile operations fail without writes.
#[test]
fn missing_service() {
    let fake = Fake::with(1);
    fake.0.borrow_mut().stopped = Some("colord is not running".into());
    let mut b = fake.backend();
    assert_eq!(b.devices().unwrap().len(), 1);
    assert!(b.query_device(&device(1)).is_ok());
    assert_eq!(b.profile_targets(&device(1)).unwrap(), vec![]);
    assert!(is_unsupported(
        &b.profile_state(&target(1)),
        "colord: colord is not running"
    ));
    assert!(is_unsupported(
        &b.prepare_icc_profile(&target(1), &icc(1)),
        "not running"
    ));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn ambiguous_mapping_fails_without_writes() {
    // Two colord devices of this user with the output's EDID.
    let fake = Fake::with(1);
    {
        let mut w = fake.0.borrow_mut();
        let mut twin = w.devices[0].clone();
        twin.path.push_str("_twin");
        twin.id.push_str("-twin");
        w.devices.push(twin);
    }
    let mut b = fake.backend();
    assert_eq!(b.profile_targets(&device(1)).unwrap(), vec![]);
    assert!(matches!(
        b.profile_state(&target(1)),
        Err(DeviceError::AmbiguousIdentity(m)) if m.contains("2 colord devices")
    ));

    // Two outputs with the same EDID.
    let fake = Fake::with(1);
    {
        let mut w = fake.0.borrow_mut();
        let mut copy = w.connectors[0].clone();
        copy.connector = "DP-9".into();
        w.connectors.push(copy);
    }
    let mut b = fake.backend();
    assert!(matches!(
        b.prepare_icc_profile(&target(1), &icc(1)),
        Err(DeviceError::AmbiguousIdentity(m)) if m.contains("2 outputs")
    ));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn unaddressable_targets_are_unsupported() {
    let fake = Fake::with(1);
    let mut b = fake.backend();
    let mut advanced = target(1);
    advanced.role = ProfileRole::AdvancedColor;
    assert!(is_unsupported(
        &b.profile_state(&advanced),
        "Advanced Color"
    ));
    let mut all_users = target(1);
    all_users.scope.user = UserScope::All;
    assert!(is_unsupported(&b.profile_state(&all_users), "current-user"));
    let mut host = target(1);
    host.scope.host = Some(HostScope::Current);
    assert!(is_unsupported(
        &b.prepare_icc_profile(&host, &icc(1)),
        "current-user"
    ));
    let mut foreign = target(1);
    foreign.device.backend = "windows".into();
    assert!(is_unsupported(&b.profile_state(&foreign), "backend"));
    assert!(matches!(
        b.profile_state(&target(2)),
        Err(DeviceError::Disconnected(_))
    ));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn state_without_profiles() {
    let fake = Fake::with(1);
    let s = fake.backend().profile_state(&target(1)).unwrap();
    assert_eq!(s.associations, Report::known(Vec::new(), PROFILES));
    assert_eq!(s.selection, ProfileSelection::NoOverride);
    assert_eq!(s.applied_scope, None);
}

/// The list is in colord order: hard relations first, newest first. The
/// first entry is the default.
#[test]
fn state_reports_colord_order_and_hashes() {
    let fake = Fake::with(1);
    let edid_profile = fake.put_profile("edid-0123", 9, UID);
    let old = fake.put_profile("icc-old", 8, UID);
    let new = fake.put_profile("icc-new", 7, UID);
    fake.associate(1, &edid_profile, Relation::Soft);
    fake.associate(1, &old, Relation::Hard);
    fake.associate(1, &new, Relation::Hard);
    let mut b = fake.backend();
    let s = b.profile_state(&target(1)).unwrap();
    assert_eq!(
        s.associations.value().unwrap(),
        &vec![
            reference("icc-new", 7),
            reference("icc-old", 8),
            reference("edid-0123", 9),
        ]
    );
    assert_eq!(
        s.selection,
        ProfileSelection::Override(reference("icc-new", 7))
    );

    // A profile without a readable file gives no hash.
    let filename = fake.0.borrow().profiles[&new].filename.clone();
    fake.0.borrow_mut().files.remove(&filename);
    let s = b.profile_state(&target(1)).unwrap();
    assert_eq!(
        s.selection,
        ProfileSelection::Override(ProfileRef {
            native_id: "icc-new".into(),
            hash: None,
        })
    );
}

/// The relation is not in the state, but a change of it changes the
/// revision, so assignment detects it.
#[test]
fn revision_covers_relations() {
    let fake = Fake::with(1);
    let p = fake.put_profile("icc-user", 8, UID);
    fake.associate(1, &p, Relation::Soft);
    let mut b = fake.backend();
    let before = b.profile_state(&target(1)).unwrap();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    fake.0.borrow_mut().devices[0].items[0].relation = Relation::Hard;
    let after = b.profile_state(&target(1)).unwrap();
    assert_eq!(before.associations, after.associations);
    assert_ne!(before.revision, after.revision);
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn unreadable_profile_list_is_not_restorable() {
    let fake = Fake::with(1);
    fake.0.borrow_mut().unreadable = true;
    let mut b = fake.backend();
    let s = b.profile_state(&target(1)).unwrap();
    assert!(matches!(s.associations, Report::Unknown(_)));
    assert!(matches!(s.selection, ProfileSelection::Unreadable(_)));
    assert!(is_unsupported(
        &b.prepare_icc_profile(&target(1), &icc(1)),
        "unreadable"
    ));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn prepare_plans_unique_resources_without_writes() {
    let fake = Fake::with(2);
    let mut b = fake.backend();
    let first = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    let other_device = b.prepare_icc_profile(&target(2), &icc(1)).unwrap();
    let other_profile = b.prepare_icc_profile(&target(1), &icc(2)).unwrap();
    assert_eq!(fake.writes(), NO_WRITES);
    let id = registered(&first);
    assert!(id.starts_with(INSTALLED_PREFIX));
    assert_eq!(installed(&first), format!("{DIR}/{id}.icc"));
    assert_eq!(
        resource(&first, ResourceKind::Association),
        "xrandr-Dell Inc.-DELL U2417H-SN0001"
    );
    assert_ne!(id, registered(&other_device));
    assert_ne!(id, registered(&other_profile));
    assert_eq!(first.original.selection, ProfileSelection::NoOverride);
    // Deterministic.
    assert_eq!(b.prepare_icc_profile(&target(1), &icc(1)).unwrap(), first);
}

#[test]
fn identical_default_writes_nothing() {
    let fake = Fake::with(1);
    let p = fake.put_profile("icc-user", 1, UID);
    fake.associate(1, &p, Relation::Hard);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    assert_eq!(change.resources, Vec::new());
    let set = b.set_icc_profile(&change).unwrap();
    assert_eq!(
        set.selection,
        ProfileSelection::Override(reference("icc-user", 1))
    );
    assert_eq!(b.restore_profile(&change).unwrap(), change.original);
    assert_eq!(fake.writes(), NO_WRITES);
}

/// An identical profile that is not the default is not reused. Adding it
/// would make a soft relation hard, and its removal would leave a mapping
/// row for the user's profile.
#[test]
fn shared_profile_is_not_reused() {
    let fake = Fake::with(1);
    let user_default = fake.put_profile("icc-default", 8, UID);
    let shared = fake.put_profile("icc-shared", 1, UID);
    fake.associate(1, &shared, Relation::Soft);
    fake.associate(1, &user_default, Relation::Hard);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    assert_eq!(change.resources.len(), 3);
    b.set_icc_profile(&change).unwrap();
    b.restore_profile(&change).unwrap();
    assert_eq!(fake.state(), before);
    assert_eq!(fake.mapping(), cleared(&change, 1));
}

#[test]
fn leftovers_conflict() {
    let fake = Fake::with(1);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    let file = installed(&change).to_owned();
    fake.0.borrow_mut().files.insert(file.clone(), icc_bytes(1));
    assert!(is_conflict(&b.prepare_icc_profile(&target(1), &icc(1))));
    assert!(is_conflict(&b.set_icc_profile(&change)));
    fake.0.borrow_mut().files.remove(&file);

    // A registered profile of another user with the same ID.
    let id = registered(&change).to_owned();
    fake.put_profile(&id, 1, OTHER_UID);
    assert!(is_conflict(&b.prepare_icc_profile(&target(1), &icc(1))));
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn assign_and_restore_without_prior_profile() {
    let fake = Fake::with(1);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    let (id, file) = (
        registered(&change).to_owned(),
        installed(&change).to_owned(),
    );
    let path = profile_path(&id, UID);
    let set = b.set_icc_profile(&change).unwrap();
    assert_eq!(
        set.selection,
        ProfileSelection::Override(ProfileRef {
            native_id: id.clone(),
            hash: Some(icc(1).hash()),
        })
    );
    assert_eq!(set.applied_scope, None);
    assert_eq!(
        fake.writes(),
        [
            format!("create file {file}"),
            format!("create profile {id}"),
            format!("add {path}"),
            format!("default {path}"),
        ]
    );
    let restored = b.restore_profile(&change).unwrap();
    assert_eq!(restored, change.original);
    assert_eq!(fake.state(), before);
    assert_eq!(fake.mapping(), cleared(&change, 1));
    assert_eq!(
        fake.writes()[4..],
        [
            format!("remove {path}"),
            format!("delete {path}"),
            format!("remove file {file}"),
        ]
    );
    // Repeated restoration writes nothing.
    let writes = fake.writes().len();
    assert_eq!(b.restore_profile(&change).unwrap(), change.original);
    assert_eq!(fake.writes().len(), writes);
}

/// The original order returns: the change does not touch the timestamps
/// of other profiles.
#[test]
fn previous_profiles_are_restored_in_order() {
    let fake = Fake::with(1);
    let edid_profile = fake.put_profile("edid-0123", 9, UID);
    let old = fake.put_profile("icc-old", 8, UID);
    let new = fake.put_profile("icc-new", 7, UID);
    fake.associate(1, &edid_profile, Relation::Soft);
    fake.associate(1, &old, Relation::Hard);
    fake.associate(1, &new, Relation::Hard);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    let set = b.set_icc_profile(&change).unwrap();
    assert_eq!(set.associations.value().unwrap().len(), 4);
    assert_eq!(b.restore_profile(&change).unwrap(), change.original);
    assert_eq!(fake.state(), before);
}

#[test]
fn failure_after_each_write_is_restorable() {
    let errors = [DeviceError::Timeout, denied()];
    for step in 0..4 {
        for when in [When::Before, When::After] {
            for error in &errors {
                let fake = Fake::with(1);
                let before = fake.state();
                let mut b = fake.backend();
                let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
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
    // Restoration writes: the association, the registration, the file.
    for step in 0..3 {
        for when in [When::Before, When::After] {
            let fake = Fake::with(1);
            let before = fake.state();
            let mut b = fake.backend();
            let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
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
    let fake = Fake::with(1);
    let before = fake.state();
    let change = fake
        .backend()
        .prepare_icc_profile(&target(1), &icc(1))
        .unwrap();
    let record = recovery::encode(&change).unwrap();
    fake.fail_at(2, When::After, DeviceError::Timeout);
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

/// polkit denies colord writes, for example outside an active local
/// session. Restoration needs no colord write when the change registered
/// nothing.
#[test]
fn authorization_failure() {
    let fake = Fake::with(1);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    fake.0.borrow_mut().denied = true;
    assert!(matches!(
        b.set_icc_profile(&change),
        Err(DeviceError::PermissionDenied(_))
    ));
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);

    // Denied after the assignment: the record is kept until a retry.
    fake.0.borrow_mut().denied = false;
    b.set_icc_profile(&change).unwrap();
    fake.0.borrow_mut().denied = true;
    assert!(matches!(
        b.restore_profile(&change),
        Err(DeviceError::PermissionDenied(_))
    ));
    fake.0.borrow_mut().denied = false;
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
}

/// A hard relation with a timestamp in the future, for example after a
/// clock change, stays first. The readback detects it.
#[test]
fn readback_mismatch_fails_assignment() {
    let fake = Fake::with(1);
    let p = fake.put_profile("icc-future", 8, UID);
    fake.associate(1, &p, Relation::Hard);
    fake.0.borrow_mut().devices[0].items[0].timestamp = u64::MAX;
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
}

#[test]
fn device_replacement_blocks_writes() {
    let fake = Fake::with(1);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    fake.0.borrow_mut().connectors[0] = connector(1, edid(7));
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

/// Same key, other EDID bytes: the identity evidence differs.
#[test]
fn changed_identity_evidence_conflicts() {
    let fake = Fake::with(1);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    let renamed = build(&Spec {
        name: Some("Other name"),
        ..Spec::default()
    });
    {
        let mut w = fake.0.borrow_mut();
        w.devices[0].md5 = md5_hex(&renamed);
        w.connectors[0] = connector(1, renamed);
    }
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);
}

/// The session registered the output under another colord device ID: the
/// stored relation belongs to the old ID.
#[test]
fn changed_colord_device_id_conflicts() {
    let fake = Fake::with(1);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    fake.0.borrow_mut().devices[0].id = "xrandr-DP-1".into();
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);
}

#[test]
fn topology_change_blocks_assignment_but_not_restoration() {
    let fake = Fake::with(1);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    fake.0.borrow_mut().connectors.push(connector(2, edid(2)));
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);

    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    fake.0.borrow_mut().connectors.pop();
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
}

#[test]
fn external_changes_conflict_without_writes() {
    let fake = Fake::with(1);
    let other = fake.put_profile("icc-other", 7, UID);
    let mut b = fake.backend();

    // Before assignment.
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    fake.associate(1, &other, Relation::Hard);
    assert!(is_conflict(&b.set_icc_profile(&change)));
    assert_eq!(fake.writes(), NO_WRITES);
    fake.dissociate(1, &other);

    // After assignment.
    b.set_icc_profile(&change).unwrap();
    let writes = fake.writes().len();
    fake.associate(1, &other, Relation::Hard);
    assert!(is_conflict(&b.restore_profile(&change)));
    assert_eq!(fake.writes().len(), writes);

    // The record is kept; restoration succeeds after the other change is
    // undone.
    fake.dissociate(1, &other);
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
}

/// Another actor made a previous profile the default again. The original
/// list remains after the change's entry is removed.
#[test]
fn reordering_by_another_actor_is_accepted() {
    let fake = Fake::with(1);
    let user = fake.put_profile("icc-user", 8, UID);
    fake.associate(1, &user, Relation::Hard);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    fake.0.borrow_mut().devices[0]
        .items
        .iter_mut()
        .find(|i| i.profile == user)
        .unwrap()
        .timestamp = u64::MAX;
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
}

/// A colord restart forgets the `normal` profile, but the mapping row
/// remains. Restoration removes the file; a new change re-registers the
/// ID, which colord adds to the device from the row.
#[test]
fn colord_restart_after_assignment() {
    let fake = Fake::with(1);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    fake.restart_colord();
    let writes = fake.writes().len();
    assert_eq!(b.restore_profile(&change), Ok(change.original.clone()));
    assert_eq!(
        fake.writes()[writes..],
        [format!("remove file {}", installed(&change))]
    );
    assert_eq!(fake.state(), before);
    assert!(fake.mapping().values().all(|&t| t > 0));

    let again = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    assert_eq!(again, change);
    let writes = fake.writes().len();
    b.set_icc_profile(&again).unwrap();
    let path = profile_path(registered(&again), UID);
    assert_eq!(
        fake.writes()[writes..],
        [
            format!("create file {}", installed(&again)),
            format!("create profile {}", registered(&again)),
            format!("default {path}"),
        ]
    );
    assert_eq!(b.restore_profile(&again), Ok(change.original.clone()));
    assert_eq!(fake.state(), before);
    assert_eq!(fake.mapping(), cleared(&again, 1));
}

#[test]
fn registered_profile_in_use_elsewhere_is_kept() {
    let fake = Fake::with(2);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    let path = profile_path(registered(&change), UID);
    fake.associate(2, &path, Relation::Hard);
    b.restore_profile(&change).unwrap();
    let w = fake.0.borrow();
    assert!(w.profiles.contains_key(&path));
    assert!(w.files.contains_key(installed(&change)));
    assert!(!w.writes.iter().any(|w| w.starts_with("delete")));
}

#[test]
fn malformed_records_are_rejected_without_writes() {
    let fake = Fake::with(1);
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    let mut bad = Vec::new();

    let mut c = change.clone();
    c.version += 1;
    bad.push(c);
    let mut c = change.clone();
    c.resources.pop();
    bad.push(c);
    let mut c = change.clone();
    c.resources[2].kind = ResourceKind::InstalledProfile;
    bad.push(c);
    let mut c = change.clone();
    c.resources[0].native_id = format!("{DIR}/user.icc");
    bad.push(c);
    let mut c = change.clone();
    c.resources[0].native_id = format!("{}.icc", registered(&change));
    bad.push(c);
    let mut c = change.clone();
    c.resources[1].native_id = "icc-user".into();
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

/// The other devices keep their profiles.
#[test]
fn other_devices_are_independent() {
    let fake = Fake::with(2);
    let p = fake.put_profile("icc-user", 5, UID);
    fake.associate(2, &p, Relation::Hard);
    let before = fake.state();
    let mut b = fake.backend();
    let change = b.prepare_icc_profile(&target(1), &icc(1)).unwrap();
    b.set_icc_profile(&change).unwrap();
    assert_eq!(fake.state().0[1], before.0[1]);
    b.restore_profile(&change).unwrap();
    assert_eq!(fake.state(), before);
}
