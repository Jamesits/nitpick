use super::*;

const SOURCE: &str = "fake report";

fn id(key: &str) -> DeviceId {
    DeviceId {
        backend: "fake".into(),
        key: key.into(),
    }
}

fn info(key: &str, name: &str, edid: &str) -> DeviceInfo {
    DeviceInfo {
        id: id(key),
        name: name.into(),
        evidence: vec![("edid".into(), edid.into())],
    }
}

fn user_scope() -> ProfileScope {
    ProfileScope {
        user: UserScope::Current,
        host: None,
    }
}

fn target(key: &str, role: ProfileRole) -> ProfileTarget {
    ProfileTarget {
        device: id(key),
        role,
        scope: user_scope(),
    }
}

fn no_override(target: ProfileTarget) -> ProfileState {
    ProfileState {
        target,
        associations: Report::known(vec![], SOURCE),
        selection: ProfileSelection::NoOverride,
        revision: 0,
    }
}

/// Minimal header-only profile; `tag` varies the content hash.
fn icc(tag: u8) -> IccProfile {
    let mut bytes = vec![0; 132];
    bytes[..4].copy_from_slice(&132u32.to_be_bytes());
    bytes[36..40].copy_from_slice(b"acsp");
    bytes[128] = tag;
    IccProfile::new(bytes).unwrap()
}

/// HDR display with HDR off and every field readable.
fn hdr_capable_query(device: DeviceInfo) -> DeviceQuery {
    DeviceQuery {
        device,
        capabilities: DeviceCapabilities {
            hdr: Report::known(true, SOURCE),
            sdr_wcg: Report::known(true, SOURCE),
            wire_depths: Report::known(vec![8, 10], SOURCE),
            icc_profiles: Report::known(true, SOURCE),
        },
        mode: DeviceMode {
            hdr_on: Report::known(false, SOURCE),
            gamut: Report::known(Gamut::Srgb, SOURCE),
            wire_depth: Report::known(8, SOURCE),
            peak_nits: Report::known(600.0, SOURCE),
            min_nits: Report::known(0.05, SOURCE),
            full_frame_nits: Report::known(400.0, SOURCE),
            sdr_white_nits: Report::known(80.0, SOURCE),
            native_size: Report::known((3840, 2160), SOURCE),
            edr: Report::unknown("not macOS"),
        },
        topology_revision: 0,
    }
}

struct FakeDevice {
    query: DeviceQuery,
    profiles: Vec<ProfileState>,
}

/// In-memory backend that follows the trait contract. `writes` records each
/// mutation so tests can prove that a call made none.
#[derive(Default)]
struct Fake {
    devices: Vec<FakeDevice>,
    topology_revision: u64,
    writes: Vec<String>,
}

impl Fake {
    fn add(&mut self, query: DeviceQuery, roles: &[ProfileRole]) {
        let key = query.device.id.key.clone();
        self.devices.push(FakeDevice {
            query,
            profiles: roles
                .iter()
                .map(|&r| no_override(target(&key, r)))
                .collect(),
        });
    }

    fn device(&self, id: &DeviceId) -> Result<&FakeDevice, DeviceError> {
        let mut matches = self.devices.iter().filter(|d| d.query.device.id == *id);
        let found = matches
            .next()
            .ok_or_else(|| DeviceError::Disconnected(id.clone()))?;
        if matches.next().is_some() {
            return Err(DeviceError::AmbiguousIdentity(id.to_string()));
        }
        Ok(found)
    }

    fn state_mut(&mut self, target: &ProfileTarget) -> Result<&mut ProfileState, DeviceError> {
        self.device(&target.device)?;
        self.devices
            .iter_mut()
            .flat_map(|d| d.profiles.iter_mut())
            .find(|p| p.target == *target)
            .ok_or_else(|| unsupported_target(target))
    }

    /// Simulates a topology change: the device at `key` is replaced.
    fn replace(&mut self, key: &str, new: DeviceInfo) {
        self.topology_revision += 1;
        let d = self
            .devices
            .iter_mut()
            .find(|d| d.query.device.id.key == key)
            .unwrap();
        for p in &mut d.profiles {
            p.target.device = new.id.clone();
        }
        d.query.device = new;
    }

    fn recheck(&self, change: &PreparedProfileChange) -> Result<(), DeviceError> {
        change.check_version()?;
        let current = self.device(&change.device.id)?;
        if current.query.device != change.device
            || self.topology_revision != change.topology_revision
        {
            return Err(DeviceError::StateConflict("device identity changed".into()));
        }
        Ok(())
    }
}

fn unsupported_target(target: &ProfileTarget) -> DeviceError {
    DeviceError::Unsupported {
        operation: "profile target".into(),
        reason: format!("{:?} {:?} not listed", target.role, target.scope),
    }
}

fn same_state(a: &ProfileState, b: &ProfileState) -> bool {
    a.selection == b.selection && a.associations == b.associations
}

impl DisplayDeviceControl for Fake {
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, DeviceError> {
        Ok(self
            .devices
            .iter()
            .map(|d| d.query.device.clone())
            .collect())
    }

    fn query_device(&mut self, device: &DeviceId) -> Result<DeviceQuery, DeviceError> {
        let mut query = self.device(device)?.query.clone();
        query.topology_revision = self.topology_revision;
        Ok(query)
    }

    fn profile_targets(&mut self, device: &DeviceId) -> Result<Vec<ProfileTarget>, DeviceError> {
        Ok(self
            .device(device)?
            .profiles
            .iter()
            .map(|p| p.target.clone())
            .collect())
    }

    fn profile_state(&mut self, target: &ProfileTarget) -> Result<ProfileState, DeviceError> {
        self.state_mut(target).map(|s| s.clone())
    }

    fn prepare_icc_profile(
        &mut self,
        target: &ProfileTarget,
        profile: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError> {
        let original = self.profile_state(target)?;
        original.check_restorable()?;
        Ok(PreparedProfileChange {
            version: PREPARED_CHANGE_VERSION,
            device: self.device(&target.device)?.query.device.clone(),
            topology_revision: self.topology_revision,
            target: target.clone(),
            original,
            profile: profile.clone(),
            resources: vec![PlannedResource {
                kind: ResourceKind::InstalledProfile,
                native_id: format!("nitpick-{}.icc", profile.hash()),
            }],
        })
    }

    fn set_icc_profile(
        &mut self,
        change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError> {
        self.recheck(change)?;
        let state = self.state_mut(&change.target)?;
        if state.revision != change.original.revision {
            return Err(DeviceError::StateConflict("profile state changed".into()));
        }
        let profile = ProfileRef {
            native_id: change.resources[0].native_id.clone(),
            hash: Some(change.profile.hash()),
        };
        state.associations = Report::known(vec![profile.clone()], SOURCE);
        state.selection = ProfileSelection::Override(profile);
        state.revision += 1;
        let state = state.clone();
        self.writes.push(format!("set {}", change.target.device));
        Ok(state)
    }

    fn restore_profile(
        &mut self,
        change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError> {
        self.recheck(change)?;
        let state = self.state_mut(&change.target)?;
        if same_state(state, &change.original) {
            return Ok(state.clone());
        }
        let ours = matches!(&state.selection,
            ProfileSelection::Override(p) if p.hash == Some(change.profile.hash()));
        if !ours {
            return Err(DeviceError::StateConflict(
                "changed by another actor".into(),
            ));
        }
        state.associations = change.original.associations.clone();
        state.selection = change.original.selection.clone();
        state.revision += 1;
        let state = state.clone();
        self.writes
            .push(format!("restore {}", change.target.device));
        Ok(state)
    }
}

/// Two HDR-capable monitors with the same name, and one device without
/// readable reports or profile targets.
fn fake() -> Fake {
    let mut f = Fake::default();
    f.add(
        hdr_capable_query(info("a", "Monitor", "edid-a")),
        &[ProfileRole::Standard, ProfileRole::AdvancedColor],
    );
    f.add(
        hdr_capable_query(info("b", "Monitor", "edid-b")),
        &[ProfileRole::Standard],
    );
    let mut partial = hdr_capable_query(info("c", "Projector", "edid-c"));
    partial.capabilities.hdr = Report::unknown("no native report");
    partial.capabilities.wire_depths = Report::unknown("no native report");
    partial.mode.hdr_on = Report::unknown("no native report");
    partial.mode.wire_depth = Report::unknown("no native report");
    partial.mode.peak_nits = Report::unknown("no native report");
    partial.mode.full_frame_nits = Report::unknown("no native report");
    f.add(partial, &[]);
    f
}

#[test]
fn supported_and_active_are_separate() {
    let q = fake().query_device(&id("a")).unwrap();
    assert_eq!(q.capabilities.hdr.support(), Support::Yes);
    assert_eq!(q.mode.hdr_on.value(), Some(&false));
    assert_eq!(q.capabilities.sdr_wcg.support(), Support::Yes);
    assert_eq!(q.mode.gamut.value(), Some(&Gamut::Srgb));
}

#[test]
fn report_to_support() {
    assert_eq!(Report::known(true, "x").support(), Support::Yes);
    assert_eq!(
        Report::known(false, "DXGI").support(),
        Support::No("reported by DXGI".into())
    );
    assert_eq!(
        Report::<bool>::unknown("no API").support(),
        Support::Unknown("no API".into())
    );
}

#[test]
fn partial_mode_keeps_unknown_fields() {
    let q = fake().query_device(&id("c")).unwrap();
    assert_eq!(
        q.capabilities.hdr.support(),
        Support::Unknown("no native report".into())
    );
    assert_eq!(q.capabilities.wire_depths.value(), None);
    assert_eq!(q.mode.hdr_on.value(), None);
    assert_eq!(q.mode.peak_nits.value(), None);
    assert_eq!(q.mode.min_nits.value(), Some(&0.05));
    assert_eq!(q.mode.native_size.value(), Some(&(3840, 2160)));
}

#[test]
fn unknown_list_is_not_an_empty_list() {
    let empty: Report<Vec<u8>> = Report::known(vec![], SOURCE);
    let unknown: Report<Vec<u8>> = Report::unknown("no report");
    assert_eq!(empty.value(), Some(&vec![]));
    assert_eq!(unknown.value(), None);
    assert_ne!(empty, unknown);
}

#[test]
fn duplicate_names_have_distinct_identity() {
    let mut f = fake();
    let monitors: Vec<_> = f
        .devices()
        .unwrap()
        .into_iter()
        .filter(|d| d.name == "Monitor")
        .collect();
    assert_eq!(monitors.len(), 2);
    assert_ne!(monitors[0].id, monitors[1].id);
    for d in &monitors {
        assert_eq!(f.query_device(&d.id).unwrap().device, *d);
    }
    assert_eq!(f.profile_targets(&id("a")).unwrap().len(), 2);
    assert_eq!(f.profile_targets(&id("b")).unwrap().len(), 1);
}

#[test]
fn replaced_device_invalidates_old_identity() {
    let mut f = fake();
    let t = target("a", ProfileRole::Standard);
    let change = f.prepare_icc_profile(&t, &icc(1)).unwrap();
    let before = f.query_device(&id("a")).unwrap().topology_revision;

    f.replace("a", info("a2", "Monitor", "edid-new"));
    assert_eq!(
        f.query_device(&id("a")),
        Err(DeviceError::Disconnected(id("a")))
    );
    assert_ne!(f.query_device(&id("a2")).unwrap().topology_revision, before);
    assert_eq!(
        f.set_icc_profile(&change),
        Err(DeviceError::Disconnected(id("a")))
    );
    assert_eq!(f.writes, Vec::<String>::new());
}

#[test]
fn changed_evidence_conflicts_without_write() {
    let mut f = fake();
    let t = target("a", ProfileRole::Standard);
    let change = f.prepare_icc_profile(&t, &icc(1)).unwrap();
    f.replace("a", info("a", "Monitor", "edid-other"));
    assert!(matches!(
        f.set_icc_profile(&change),
        Err(DeviceError::StateConflict(_))
    ));
    assert_eq!(f.writes, Vec::<String>::new());
}

#[test]
fn ambiguous_identity_fails() {
    let mut f = fake();
    f.add(hdr_capable_query(info("a", "Monitor", "edid-a")), &[]);
    assert!(matches!(
        f.query_device(&id("a")),
        Err(DeviceError::AmbiguousIdentity(_))
    ));
    assert!(matches!(
        f.prepare_icc_profile(&target("a", ProfileRole::Standard), &icc(1)),
        Err(DeviceError::AmbiguousIdentity(_))
    ));
}

#[test]
fn queries_and_prepare_do_not_write() {
    let mut f = fake();
    let t = target("a", ProfileRole::AdvancedColor);
    let before = f.profile_state(&t).unwrap();
    f.devices().unwrap();
    f.query_device(&id("a")).unwrap();
    f.profile_targets(&id("a")).unwrap();
    let change = f.prepare_icc_profile(&t, &icc(1)).unwrap();
    assert_eq!(f.writes, Vec::<String>::new());
    assert_eq!(f.profile_state(&t).unwrap(), before);
    assert_eq!(change.original, before);
    assert_eq!(change.version, PREPARED_CHANGE_VERSION);
}

#[test]
fn unsupported_targets_are_rejected() {
    let mut f = fake();
    assert_eq!(f.profile_targets(&id("c")).unwrap(), vec![]);
    let role = target("b", ProfileRole::AdvancedColor);
    let mut scope = target("a", ProfileRole::Standard);
    scope.scope.user = UserScope::All;
    for t in [role, scope, target("c", ProfileRole::Standard)] {
        assert!(matches!(
            f.profile_state(&t),
            Err(DeviceError::Unsupported { .. })
        ));
        assert!(matches!(
            f.prepare_icc_profile(&t, &icc(1)),
            Err(DeviceError::Unsupported { .. })
        ));
    }
    assert_eq!(f.writes, Vec::<String>::new());
}

#[test]
fn unreadable_original_is_not_restorable() {
    let t = target("a", ProfileRole::Standard);
    let mut s = no_override(t.clone());
    s.selection = ProfileSelection::Unreadable("access denied".into());
    assert_eq!(
        s.check_restorable(),
        Err(DeviceError::Unsupported {
            operation: "profile restoration".into(),
            reason: "selection unreadable: access denied".into()
        })
    );
    let mut s = no_override(t.clone());
    s.associations = Report::unknown("no list API");
    assert!(s.check_restorable().is_err());
    assert_eq!(no_override(t.clone()).check_restorable(), Ok(()));

    let inherited = ProfileState {
        selection: ProfileSelection::Inherited {
            from: ProfileScope {
                user: UserScope::All,
                host: None,
            },
            profile: ProfileRef {
                native_id: "system.icc".into(),
                hash: None,
            },
        },
        ..no_override(t)
    };
    assert_eq!(inherited.check_restorable(), Ok(()));
}

#[test]
fn external_change_conflicts_on_set_and_restore() {
    let mut f = fake();
    let t = target("a", ProfileRole::Standard);
    let change = f.prepare_icc_profile(&t, &icc(1)).unwrap();
    f.state_mut(&t).unwrap().revision += 1;
    assert!(matches!(
        f.set_icc_profile(&change),
        Err(DeviceError::StateConflict(_))
    ));

    let mut f = fake();
    let change = f.prepare_icc_profile(&t, &icc(1)).unwrap();
    f.set_icc_profile(&change).unwrap();
    let other = ProfileRef {
        native_id: "user.icc".into(),
        hash: Some(icc(2).hash()),
    };
    f.state_mut(&t).unwrap().selection = ProfileSelection::Override(other.clone());
    assert!(matches!(
        f.restore_profile(&change),
        Err(DeviceError::StateConflict(_))
    ));
    assert_eq!(
        f.profile_state(&t).unwrap().selection,
        ProfileSelection::Override(other)
    );
}

#[test]
fn old_record_version_is_rejected() {
    let mut f = fake();
    let t = target("a", ProfileRole::Standard);
    let mut change = f.prepare_icc_profile(&t, &icc(1)).unwrap();
    change.version = PREPARED_CHANGE_VERSION + 1;
    assert!(matches!(
        f.set_icc_profile(&change),
        Err(DeviceError::Unsupported { .. })
    ));
    assert_eq!(f.writes, Vec::<String>::new());
}

/// The host owns the backend as a trait object on its device worker.
#[test]
fn trait_object_on_worker() {
    let backend: Box<dyn DisplayDeviceControl + Send> = Box::new(fake());
    let worker = std::thread::spawn(move || {
        let mut backend = backend;
        let device = backend.devices().unwrap()[0].id.clone();
        let t = backend.profile_targets(&device).unwrap()[0].clone();
        let profile = icc(7);
        let change = backend.prepare_icc_profile(&t, &profile).unwrap();
        let set = backend.set_icc_profile(&change).unwrap();
        let restored = backend.restore_profile(&change).unwrap();
        let again = backend.restore_profile(&change).unwrap();
        (profile, change, set, restored, again)
    });
    let (profile, change, set, restored, again) = worker.join().unwrap();
    assert!(matches!(&set.selection,
        ProfileSelection::Override(p) if p.hash == Some(profile.hash())));
    assert_eq!(restored.selection, change.original.selection);
    assert_eq!(restored.associations, change.original.associations);
    assert_eq!(again, restored);
}

#[test]
fn icc_header_checks() {
    let good = icc(0);
    assert_eq!(good.bytes().len(), 132);
    assert_eq!(good.hash(), ContentHash::of(good.bytes()));
    assert_ne!(icc(0).hash(), icc(1).hash());

    assert!(matches!(
        IccProfile::new(vec![0; 64]),
        Err(DeviceError::InvalidProfile(_))
    ));
    let mut wrong_size = good.bytes().to_vec();
    wrong_size[3] = 200;
    assert!(matches!(
        IccProfile::new(wrong_size),
        Err(DeviceError::InvalidProfile(_))
    ));
    let mut no_sig = good.bytes().to_vec();
    no_sig[36] = b'x';
    assert!(matches!(
        IccProfile::new(no_sig),
        Err(DeviceError::InvalidProfile(_))
    ));
}

#[test]
fn content_hash_is_sha256() {
    assert_eq!(
        ContentHash::of(b"abc").to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn error_text_keeps_native_reason() {
    let e = DeviceError::Native {
        api: "ColorProfileAddDisplayAssociation".into(),
        code: Some(0x8007_0005),
        message: "Access is denied.".into(),
    };
    assert_eq!(
        e.to_string(),
        "ColorProfileAddDisplayAssociation failed (0x80070005): Access is denied."
    );
}
