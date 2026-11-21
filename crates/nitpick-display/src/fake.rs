//! In-memory [`DisplayDeviceControl`] for host and restoration tests. It
//! follows the backend contract: identity and state rechecks before a
//! write, one native step per write, restoration from any state that a
//! prefix of the steps gives, and removal of planned resources only.
//!
//! Faults make a step fail, time out after its write, or time out and
//! write later. The fake does not touch the OS.

use std::collections::BTreeMap;

use nitpick_core::device::{
    ContentHash, DeviceCapabilities, DeviceError, DeviceId, DeviceInfo, DeviceMode, DeviceQuery,
    DisplayDeviceControl, DisplaySetup, GammaRamp, IccProfile, PREPARED_CHANGE_VERSION,
    PlannedResource, PreparedProfileChange, ProfileRef, ProfileRole, ProfileScope,
    ProfileSelection, ProfileState, ProfileTarget, Report, ResourceKind, UserScope,
};

use crate::common::{fnv1a64, unsupported};

pub const BACKEND: &str = "fake";

const SOURCE: &str = "fake";
const UNREAD: &str = "the fake has no such report";

/// One native write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Install,
    Associate,
    Select,
    /// Restoration: select the original profile, or none.
    Reselect,
    Unassociate,
    Uninstall,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Fault {
    /// The step fails without its write.
    Fail(DeviceError),
    /// The step writes, then the call returns [`DeviceError::Timeout`].
    TimeoutAfterWrite,
    /// The call returns [`DeviceError::Timeout`] without the write. The
    /// write happens at the start of the `calls`-th later trait call, as a
    /// native service that finishes a request after the caller stopped
    /// waiting.
    LateWrite { calls: u32 },
}

#[derive(Debug, Clone, PartialEq)]
enum Write {
    Install {
        name: String,
        hash: ContentHash,
    },
    Uninstall {
        name: String,
    },
    Associate {
        target: ProfileTarget,
        name: String,
    },
    Unassociate {
        target: ProfileTarget,
        name: String,
    },
    Select {
        target: ProfileTarget,
        name: String,
    },
    Reselect {
        target: ProfileTarget,
        name: Option<String>,
    },
}

impl Write {
    fn step(&self) -> Step {
        match self {
            Self::Install { .. } => Step::Install,
            Self::Uninstall { .. } => Step::Uninstall,
            Self::Associate { .. } => Step::Associate,
            Self::Unassociate { .. } => Step::Unassociate,
            Self::Select { .. } => Step::Select,
            Self::Reselect { .. } => Step::Reselect,
        }
    }
}

#[derive(Debug, Clone)]
struct TargetState {
    target: ProfileTarget,
    associations: Vec<String>,
    selection: Option<String>,
    revision: u64,
}

#[derive(Debug, Clone)]
struct Device {
    info: DeviceInfo,
    connected: bool,
    targets: Vec<TargetState>,
    setup: DisplaySetup,
}

#[derive(Debug, Default)]
pub struct FakeDisplay {
    devices: Vec<Device>,
    /// The profile store: file name and content.
    files: BTreeMap<String, ContentHash>,
    topology: u64,
    faults: Vec<(Step, Fault)>,
    late: Option<(u32, Write)>,
    writes: Vec<String>,
}

fn conflict(msg: impl Into<String>) -> DeviceError {
    DeviceError::StateConflict(msg.into())
}

/// The one target of each fake device.
#[must_use]
pub fn standard_target(device: &DeviceId) -> ProfileTarget {
    ProfileTarget {
        device: device.clone(),
        role: ProfileRole::Standard,
        scope: ProfileScope {
            user: UserScope::Current,
            host: None,
        },
    }
}

/// A setup without competing transforms.
#[must_use]
pub fn clean_setup(device: &DeviceId) -> DisplaySetup {
    DisplaySetup {
        device: device.clone(),
        gamma_ramp: Report::known(GammaRamp::Identity, SOURCE),
        os_calibration_loader: Report::known(false, SOURCE),
        external_calibration_loader: Report::known(false, SOURCE),
        gpu_gamut_clamp: Report::known(false, SOURCE),
        dithering: Report::known(false, SOURCE),
    }
}

impl FakeDisplay {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A connected device with no profile, one [`standard_target`] and a
    /// [`clean_setup`].
    pub fn add_device(&mut self, key: &str) -> DeviceId {
        let id = DeviceId {
            backend: BACKEND.into(),
            key: key.into(),
        };
        self.devices.push(Device {
            info: DeviceInfo {
                id: id.clone(),
                name: format!("Fake {key}"),
                evidence: vec![("serial".into(), key.into())],
            },
            connected: true,
            targets: vec![TargetState {
                target: standard_target(&id),
                associations: Vec::new(),
                selection: None,
                revision: 0,
            }],
            setup: clean_setup(&id),
        });
        self.topology += 1;
        id
    }

    /// Another actor installs, associates and selects a profile. Used for
    /// a prior override and for an external change.
    ///
    /// # Panics
    /// For a target that is not listed or a disconnected device.
    pub fn select_external(&mut self, target: &ProfileTarget, name: &str, bytes: &[u8]) {
        self.files.insert(name.into(), ContentHash::of(bytes));
        let t = self.target_mut(target).expect("listed target");
        if !t.associations.iter().any(|a| a == name) {
            t.associations.push(name.into());
        }
        t.selection = Some(name.into());
        t.revision += 1;
    }

    /// The next write of `step` gets `fault`. Each fault applies once.
    pub fn inject(&mut self, step: Step, fault: Fault) {
        self.faults.push((step, fault));
    }

    /// # Panics
    /// For a device that was not added.
    pub fn set_connected(&mut self, device: &DeviceId, connected: bool) {
        let d = self
            .devices
            .iter_mut()
            .find(|d| d.info.id == *device)
            .expect("added device");
        d.connected = connected;
        self.topology += 1;
    }

    /// For example a loader that applies a ramp again.
    ///
    /// # Panics
    /// For a device that was not added.
    pub fn setup_mut(&mut self, device: &DeviceId) -> &mut DisplaySetup {
        &mut self
            .devices
            .iter_mut()
            .find(|d| d.info.id == *device)
            .expect("added device")
            .setup
    }

    /// Each applied write, in order.
    #[must_use]
    pub fn writes(&self) -> &[String] {
        &self.writes
    }

    /// Installed profile file names.
    #[must_use]
    pub fn files(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }

    /// The state without a trait call, so a pending late write stays
    /// pending.
    ///
    /// # Panics
    /// For a target that was not added.
    #[must_use]
    pub fn peek(&self, target: &ProfileTarget) -> ProfileState {
        let t = self
            .devices
            .iter()
            .flat_map(|d| &d.targets)
            .find(|t| t.target == *target)
            .expect("listed target");
        self.state_of(t)
    }

    /// Start of each trait call: a late write whose time has come.
    fn tick(&mut self) {
        let Some((calls, _)) = &mut self.late else {
            return;
        };
        *calls = calls.saturating_sub(1);
        if *calls == 0
            && let Some((_, w)) = self.late.take()
        {
            self.apply(&w);
        }
    }

    fn reference(&self, name: &str) -> ProfileRef {
        ProfileRef {
            native_id: name.into(),
            hash: self.files.get(name).copied(),
        }
    }

    fn state_of(&self, t: &TargetState) -> ProfileState {
        ProfileState {
            target: t.target.clone(),
            associations: Report::known(
                t.associations.iter().map(|n| self.reference(n)).collect(),
                SOURCE,
            ),
            selection: t
                .selection
                .as_ref()
                .map_or(ProfileSelection::NoOverride, |n| {
                    ProfileSelection::Override(self.reference(n))
                }),
            applied_scope: None,
            revision: t.revision,
        }
    }

    fn device(&self, id: &DeviceId) -> Result<&Device, DeviceError> {
        self.devices
            .iter()
            .find(|d| d.info.id == *id && d.connected)
            .ok_or_else(|| DeviceError::Disconnected(id.clone()))
    }

    fn target_mut(&mut self, target: &ProfileTarget) -> Result<&mut TargetState, DeviceError> {
        self.device(&target.device)?;
        self.devices
            .iter_mut()
            .flat_map(|d| d.targets.iter_mut())
            .find(|t| t.target == *target)
            .ok_or_else(|| unsupported("profile target", format!("{target:?} not listed")))
    }

    fn state(&mut self, target: &ProfileTarget) -> Result<ProfileState, DeviceError> {
        let t = self.target_mut(target)?.clone();
        Ok(self.state_of(&t))
    }

    fn apply(&mut self, w: &Write) {
        self.writes.push(format!("{w:?}"));
        let target = match w {
            Write::Install { name, hash } => {
                self.files.insert(name.clone(), *hash);
                return;
            }
            Write::Uninstall { name } => {
                self.files.remove(name);
                return;
            }
            Write::Associate { target, .. }
            | Write::Unassociate { target, .. }
            | Write::Select { target, .. }
            | Write::Reselect { target, .. } => target,
        };
        // A late write lands also if the device is gone now.
        let Some(t) = self
            .devices
            .iter_mut()
            .flat_map(|d| d.targets.iter_mut())
            .find(|t| t.target == *target)
        else {
            return;
        };
        match w {
            Write::Associate { name, .. } if !t.associations.contains(name) => {
                t.associations.push(name.clone());
            }
            Write::Unassociate { name, .. } => t.associations.retain(|a| a != name),
            Write::Select { name, .. } => t.selection = Some(name.clone()),
            Write::Reselect { name, .. } => t.selection.clone_from(name),
            _ => return,
        }
        t.revision += 1;
    }

    fn write(&mut self, w: Write) -> Result<(), DeviceError> {
        let step = w.step();
        let fault = self
            .faults
            .iter()
            .position(|(s, _)| *s == step)
            .map(|i| self.faults.remove(i).1);
        match fault {
            None => {
                self.apply(&w);
                Ok(())
            }
            Some(Fault::Fail(e)) => Err(e),
            Some(Fault::TimeoutAfterWrite) => {
                self.apply(&w);
                Err(DeviceError::Timeout)
            }
            Some(Fault::LateWrite { calls }) => {
                self.late = Some((calls, w));
                Err(DeviceError::Timeout)
            }
        }
    }

    fn check_identity(&self, change: &PreparedProfileChange) -> Result<(), DeviceError> {
        change.check_version()?;
        if self.device(&change.device.id)?.info != change.device {
            return Err(conflict("device identity changed"));
        }
        Ok(())
    }

    /// The planned file, which is also the planned association.
    fn planned(change: &PreparedProfileChange) -> Option<&str> {
        change
            .resources
            .iter()
            .find(|r| r.kind == ResourceKind::InstalledProfile)
            .map(|r| r.native_id.as_str())
    }

    fn used(&self, name: &str) -> bool {
        self.devices.iter().flat_map(|d| &d.targets).any(|t| {
            t.selection.as_deref() == Some(name) || t.associations.iter().any(|a| a == name)
        })
    }
}

fn names(state: &ProfileState) -> Vec<String> {
    state
        .associations
        .value()
        .map(|l| l.iter().map(|r| r.native_id.clone()).collect())
        .unwrap_or_default()
}

fn selected(state: &ProfileState) -> Option<String> {
    match &state.selection {
        ProfileSelection::Override(p) => Some(p.native_id.clone()),
        _ => None,
    }
}

impl DisplayDeviceControl for FakeDisplay {
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, DeviceError> {
        self.tick();
        Ok(self
            .devices
            .iter()
            .filter(|d| d.connected)
            .map(|d| d.info.clone())
            .collect())
    }

    fn query_device(&mut self, device: &DeviceId) -> Result<DeviceQuery, DeviceError> {
        self.tick();
        let info = self.device(device)?.info.clone();

        Ok(DeviceQuery {
            device: info,
            capabilities: DeviceCapabilities {
                hdr: Report::unknown(UNREAD),
                sdr_wcg: Report::unknown(UNREAD),
                wire_depths: Report::unknown(UNREAD),
                icc_profiles: Report::known(true, SOURCE),
            },
            mode: DeviceMode {
                hdr_on: Report::unknown(UNREAD),
                gamut: Report::unknown(UNREAD),
                wire_depth: Report::unknown(UNREAD),
                peak_nits: Report::unknown(UNREAD),
                min_nits: Report::unknown(UNREAD),
                full_frame_nits: Report::unknown(UNREAD),
                sdr_white_nits: Report::unknown(UNREAD),
                native_size: Report::unknown(UNREAD),
                edr: Report::unknown(UNREAD),
            },
            topology_revision: self.topology,
        })
    }

    fn display_setup(&mut self, device: &DeviceId) -> Result<DisplaySetup, DeviceError> {
        self.tick();
        Ok(self.device(device)?.setup.clone())
    }

    fn profile_targets(&mut self, device: &DeviceId) -> Result<Vec<ProfileTarget>, DeviceError> {
        self.tick();
        Ok(self
            .device(device)?
            .targets
            .iter()
            .map(|t| t.target.clone())
            .collect())
    }

    fn profile_state(&mut self, target: &ProfileTarget) -> Result<ProfileState, DeviceError> {
        self.tick();
        self.state(target)
    }

    fn prepare_icc_profile(
        &mut self,
        target: &ProfileTarget,
        profile: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError> {
        self.tick();
        let original = self.state(target)?;
        original.check_restorable()?;
        let selected_same = matches!(&original.selection,
            ProfileSelection::Override(p) if p.hash == Some(profile.hash()));
        let resources = if selected_same {
            Vec::new()
        } else {
            let target_text = format!("{target:?}");
            let name = format!(
                "nitpick-{}-{:016x}.icc",
                &profile.hash().to_string()[..16],
                fnv1a64(&[target_text])
            );
            if self.files.contains_key(&name) {
                return Err(conflict(format!(
                    "{name} exists: an earlier change did not finish"
                )));
            }
            vec![
                PlannedResource {
                    kind: ResourceKind::InstalledProfile,
                    native_id: name.clone(),
                },
                PlannedResource {
                    kind: ResourceKind::Association,
                    native_id: name,
                },
            ]
        };
        Ok(PreparedProfileChange {
            version: PREPARED_CHANGE_VERSION,
            device: self.device(&target.device)?.info.clone(),
            topology_revision: self.topology,
            target: target.clone(),
            original,
            profile: profile.clone(),
            resources,
        })
    }

    fn set_icc_profile(
        &mut self,
        change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError> {
        self.tick();
        self.check_identity(change)?;
        if self.topology != change.topology_revision {
            return Err(conflict("display topology changed"));
        }
        let target = change.target.clone();
        if self.state(&target)?.revision != change.original.revision {
            return Err(conflict("profile state changed after preparation"));
        }
        if let Some(name) = Self::planned(change) {
            if self.files.contains_key(name) {
                return Err(conflict(format!("{name} exists")));
            }
            self.write(Write::Install {
                name: name.into(),
                hash: change.profile.hash(),
            })?;
            self.write(Write::Associate {
                target: target.clone(),
                name: name.into(),
            })?;
            self.write(Write::Select {
                target: target.clone(),
                name: name.into(),
            })?;
        }
        let state = self.state(&target)?;
        match &state.selection {
            ProfileSelection::Override(p) if p.hash == Some(change.profile.hash()) => Ok(state),
            _ => Err(conflict("readback does not select the profile")),
        }
    }

    fn restore_profile(
        &mut self,
        change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError> {
        self.tick();
        self.check_identity(change)?;
        let target = change.target.clone();
        let original = &change.original;
        let Some(name) = Self::planned(change) else {
            return self.state(&target);
        };
        let now = self.state(&target)?;
        let mut others = names(&now);
        let has_planned = others.iter().any(|n| n == name);
        others.retain(|n| n != name);
        let selection = selected(&now);
        let prefix = others == names(original)
            && (selection == selected(original)
                || (selection.as_deref() == Some(name) && has_planned));
        if !prefix {
            return Err(conflict("profile state changed by another actor"));
        }
        if selection != selected(original) {
            self.write(Write::Reselect {
                target: target.clone(),
                name: selected(original),
            })?;
        }
        if has_planned {
            self.write(Write::Unassociate {
                target: target.clone(),
                name: name.into(),
            })?;
        }
        if self.files.contains_key(name) && !self.used(name) {
            self.write(Write::Uninstall { name: name.into() })?;
        }
        let state = self.state(&target)?;
        if state.associations != original.associations || state.selection != original.selection {
            return Err(conflict("readback differs from the original state"));
        }
        Ok(state)
    }
}
