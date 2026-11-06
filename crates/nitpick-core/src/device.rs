//! Display device API: device enumeration, capability and mode queries, and
//! ICC profile assignment with restoration. See `doc/design/display_device.md`.
//!
//! The module has no OS, GUI, executor or colour-library dependency. Native
//! backends live in `nitpick-display`.

use std::fmt;

use sha2::{Digest, Sha256};

use crate::driver::{Gamut, Support};

/// Format version of [`PreparedProfileChange`]. Increase it when a field
/// changes meaning, so that an old recovery record fails instead of
/// restoring the wrong state.
pub const PREPARED_CHANGE_VERSION: u32 = 1;

/// Backend and native device identity. Distinct from
/// [`crate::driver::DisplayId`]: a browser client is not a local device.
///
/// `backend` is owned, unlike `DisplayId::driver`, because recovery records
/// are read back from storage.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeviceId {
    pub backend: String,
    /// Native identity, for example a `DisplayConfig` target path, a
    /// `ColorSync` device UUID or a DRM connector with its EDID identity.
    /// Not screen order or a display name.
    pub key: String,
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.backend, self.key)
    }
}

/// A native value with its origin, or the reason it is not available.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub enum Report<T> {
    Known {
        value: T,
        /// Native report that supplied the value, for example
        /// `"DXGI_OUTPUT_DESC1"`.
        source: String,
    },
    Unknown(String),
}

impl<T> Report<T> {
    pub fn known(value: T, source: impl Into<String>) -> Self {
        Self::Known {
            value,
            source: source.into(),
        }
    }

    pub fn unknown(reason: impl Into<String>) -> Self {
        Self::Unknown(reason.into())
    }

    /// `None` if unknown.
    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Known { value, .. } => Some(value),
            Self::Unknown(_) => None,
        }
    }
}

impl Report<bool> {
    /// Tri-state for capability rules. The source of a `false` report is
    /// the `No` reason.
    #[must_use]
    pub fn support(&self) -> Support {
        match self {
            Self::Known { value: true, .. } => Support::Yes,
            Self::Known {
                value: false,
                source,
            } => Support::No(format!("reported by {source}")),
            Self::Unknown(reason) => Support::Unknown(reason.clone()),
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: DeviceId,
    /// Display name for the host. Names can repeat.
    pub name: String,
    /// Native fields that identify the physical device, as (name, value),
    /// for example an EDID hash, a connector or an adapter LUID. A backend
    /// compares them before a write to detect replacement or ambiguous
    /// matches.
    pub evidence: Vec<(String, String)>,
}

/// Detected support. Not the active settings; see [`DeviceMode`].
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceCapabilities {
    pub hdr: Report<bool>,
    /// Wide colour gamut with HDR off, for example Windows SDR Advanced
    /// Color.
    pub sdr_wcg: Report<bool>,
    /// Supported bits per component. A known empty list means none; an
    /// unknown list is [`Report::Unknown`].
    pub wire_depths: Report<Vec<u8>>,
    /// The OS can assign an ICC profile to this device.
    pub icc_profiles: Report<bool>,
}

/// Current settings. Read-only.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceMode {
    /// HDR signalling is active. Advanced Color support or an FP16
    /// composition format alone does not set it.
    pub hdr_on: Report<bool>,
    pub gamut: Report<Gamut>,
    /// Bits per component on the wire.
    pub wire_depth: Report<u8>,
    /// cd/m². macOS EDR headroom is a ratio and does not fill these fields;
    /// see [`Self::edr`].
    pub peak_nits: Report<f64>,
    pub min_nits: Report<f64>,
    pub full_frame_nits: Report<f64>,
    pub sdr_white_nits: Report<f64>,
    /// Native pixels.
    pub native_size: Report<(usize, usize)>,
    /// macOS only.
    pub edr: Report<EdrHeadroom>,
}

/// macOS extended dynamic range: the largest colour component value that
/// the display can show, relative to SDR white at 1.0.
///
/// A ratio, not a luminance. The SDR white luminance changes with the
/// brightness setting and is not reported, so these values give no
/// absolute scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EdrHeadroom {
    /// Available now. Changes with brightness and other EDR content.
    pub current: f64,
    /// Available at the current settings when EDR content is shown.
    pub potential: f64,
    /// Headroom of a reference preset; `None` without a reference mode.
    pub reference: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceQuery {
    pub device: DeviceInfo,
    pub capabilities: DeviceCapabilities,
    pub mode: DeviceMode,
    /// Changes when the backend detects a display topology or identity
    /// change. Opaque; compare for equality only.
    pub topology_revision: u64,
}

/// Native profile role.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProfileRole {
    /// Ordinary display profile.
    Standard,
    /// Windows Advanced Color profile; used with HDR or SDR WCG active.
    AdvancedColor,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserScope {
    Current,
    All,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HostScope {
    Current,
    Any,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProfileScope {
    pub user: UserScope,
    /// `None` on an OS without a host scope. macOS `ColorSync` has one.
    pub host: Option<HostScope>,
}

/// One addressable profile assignment. Listed by
/// [`DisplayDeviceControl::profile_targets`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProfileTarget {
    pub device: DeviceId,
    pub role: ProfileRole,
    pub scope: ProfileScope,
}

/// SHA-256 of profile bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash(pub [u8; 32]);

impl ContentHash {
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ContentHash({self})")
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.iter().try_for_each(|b| write!(f, "{b:02x}"))
    }
}

/// An installed or registered profile as the OS names it: a file name on
/// Windows, a profile URL on macOS or a colord object ID.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProfileRef {
    pub native_id: String,
    /// `None` if the backend cannot read the profile bytes.
    pub hash: Option<ContentHash>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileSelection {
    /// No selection at this target or a wider scope; the OS default applies.
    NoOverride,
    /// No selection at this target; a selection at `from` applies.
    Inherited {
        from: ProfileScope,
        profile: ProfileRef,
    },
    /// Selected at this target.
    Override(ProfileRef),
    Unreadable(String),
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileState {
    pub target: ProfileTarget,
    /// Profiles associated with the device at this target.
    pub associations: Report<Vec<ProfileRef>>,
    pub selection: ProfileSelection,
    /// Changes with each observed state change. Opaque; compare for
    /// equality only.
    pub revision: u64,
}

impl ProfileState {
    /// `prepare_icc_profile` rejects a target that this check rejects.
    ///
    /// # Errors
    /// [`DeviceError::Unsupported`] if the selection or associations are
    /// unreadable, because restoration cannot reproduce them.
    pub fn check_restorable(&self) -> Result<(), DeviceError> {
        let reason = match (&self.selection, &self.associations) {
            (ProfileSelection::Unreadable(reason), _) => format!("selection unreadable: {reason}"),
            (_, Report::Unknown(reason)) => format!("associations unreadable: {reason}"),
            _ => return Ok(()),
        };
        Err(DeviceError::Unsupported {
            operation: "profile restoration".into(),
            reason,
        })
    }
}

/// Owned ICC profile. The content hash is the identity; a file path is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IccProfile {
    bytes: Vec<u8>,
    hash: ContentHash,
}

/// ICC.1:2022 7.2: the header is 128 bytes; bytes 0..4 hold the profile
/// size and bytes 36..40 hold the signature `acsp`.
const ICC_HEADER_LEN: usize = 128;

impl IccProfile {
    /// Checks the header size and signature only. The profile contents are
    /// validated by the colour adapter, not here.
    ///
    /// # Errors
    /// [`DeviceError::InvalidProfile`] for a short header, a size field
    /// that differs from the byte count, or a missing signature.
    pub fn new(bytes: Vec<u8>) -> Result<Self, DeviceError> {
        if bytes.len() < ICC_HEADER_LEN {
            return Err(DeviceError::InvalidProfile(format!(
                "{} bytes, shorter than the {ICC_HEADER_LEN}-byte header",
                bytes.len()
            )));
        }
        let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if usize::try_from(size).ok() != Some(bytes.len()) {
            return Err(DeviceError::InvalidProfile(format!(
                "header size {size} differs from {} bytes",
                bytes.len()
            )));
        }
        if &bytes[36..40] != b"acsp" {
            return Err(DeviceError::InvalidProfile("no acsp signature".into()));
        }
        let hash = ContentHash::of(&bytes);
        Ok(Self { bytes, hash })
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn hash(&self) -> ContentHash {
        self.hash
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    /// A profile file copied into the OS profile store.
    InstalledProfile,
    /// A profile registered with a profile service, for example colord.
    RegisteredProfile,
    /// A device-to-profile association.
    Association,
}

/// A native resource that the change creates. Restoration removes only
/// these, and only if no longer used.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PlannedResource {
    pub kind: ResourceKind,
    /// Unique per change, so restoration cannot remove a user's resource.
    pub native_id: String,
}

/// A validated profile change. The host saves it as the recovery record
/// before [`DisplayDeviceControl::set_icc_profile`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedProfileChange {
    /// [`PREPARED_CHANGE_VERSION`] when prepared.
    pub version: u32,
    /// Identity when prepared. The backend compares it before each write.
    pub device: DeviceInfo,
    pub topology_revision: u64,
    pub target: ProfileTarget,
    /// State to restore. Passed [`ProfileState::check_restorable`].
    pub original: ProfileState,
    pub profile: IccProfile,
    /// Empty if an identical profile and association already exist.
    pub resources: Vec<PlannedResource>,
}

impl PreparedProfileChange {
    /// # Errors
    /// [`DeviceError::Unsupported`] for a record of another version.
    pub fn check_version(&self) -> Result<(), DeviceError> {
        if self.version == PREPARED_CHANGE_VERSION {
            return Ok(());
        }
        Err(DeviceError::Unsupported {
            operation: "recovery record".into(),
            reason: format!(
                "version {}, expected {PREPARED_CHANGE_VERSION}",
                self.version
            ),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceError {
    /// Unsupported operation, role, scope or record.
    Unsupported {
        operation: String,
        reason: String,
    },
    PermissionDenied(String),
    InvalidProfile(String),
    /// The device is not in the current topology. A replaced device has a
    /// new [`DeviceId`], so its old ID gives this error.
    Disconnected(DeviceId),
    /// More than one current device matches the identity evidence.
    AmbiguousIdentity(String),
    /// The device or profile state changed after the query or preparation.
    StateConflict(String),
    Timeout,
    Native {
        /// For example `"ColorProfileAddDisplayAssociation"`.
        api: String,
        /// HRESULT, `OSStatus` or errno, if available.
        code: Option<i64>,
        message: String,
    },
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { operation, reason } => {
                write!(f, "{operation} not supported: {reason}")
            }
            Self::PermissionDenied(msg) => write!(f, "permission denied: {msg}"),
            Self::InvalidProfile(msg) => write!(f, "invalid ICC profile: {msg}"),
            Self::Disconnected(id) => write!(f, "device {id} disconnected"),
            Self::AmbiguousIdentity(msg) => write!(f, "ambiguous device identity: {msg}"),
            Self::StateConflict(msg) => write!(f, "device state conflict: {msg}"),
            Self::Timeout => f.write_str("device operation timed out"),
            Self::Native { api, code, message } => match code {
                Some(code) => write!(f, "{api} failed ({code:#x}): {message}"),
                None => write!(f, "{api} failed: {message}"),
            },
        }
    }
}

impl std::error::Error for DeviceError {}

/// Local display devices and their OS profile assignments.
///
/// Calls can block on native I/O. Hosts call them on a worker and serialize
/// changes for a device. Dropping a host wait does not undo a write.
pub trait DisplayDeviceControl {
    /// # Errors
    /// Native or permission failure.
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, DeviceError>;
    /// Read-only. Unavailable fields are [`Report::Unknown`].
    ///
    /// # Errors
    /// Disconnected or ambiguous device, or a native failure.
    fn query_device(&mut self, device: &DeviceId) -> Result<DeviceQuery, DeviceError>;
    /// Targets this backend can address for `device`; can be empty.
    ///
    /// # Errors
    /// Disconnected device or a native failure.
    fn profile_targets(&mut self, device: &DeviceId) -> Result<Vec<ProfileTarget>, DeviceError>;
    /// Read-only.
    ///
    /// # Errors
    /// Unlisted target, disconnected device or a native failure.
    fn profile_state(&mut self, target: &ProfileTarget) -> Result<ProfileState, DeviceError>;
    /// Validates without a change. Rejects a target that fails
    /// [`ProfileState::check_restorable`].
    ///
    /// # Errors
    /// Unlisted target, unrestorable state, invalid profile, disconnected
    /// device or a native failure.
    fn prepare_icc_profile(
        &mut self,
        target: &ProfileTarget,
        profile: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError>;
    /// Installs, associates and selects the profile, then reads it back.
    /// Rechecks identity and state before the first write.
    ///
    /// # Errors
    /// [`DeviceError::StateConflict`] after an external change; native,
    /// permission or timeout failure. Restore after any failure.
    fn set_icc_profile(
        &mut self,
        change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError>;
    /// Restores `change.original` and removes unused planned resources.
    /// Safe to repeat.
    ///
    /// # Errors
    /// [`DeviceError::StateConflict`] if another actor changed the state;
    /// keep the recovery record.
    fn restore_profile(
        &mut self,
        change: &PreparedProfileChange,
    ) -> Result<ProfileState, DeviceError>;
}

#[cfg(feature = "serde")]
#[path = "device_serde.rs"]
mod serde_impl;

#[cfg(test)]
#[path = "device_tests.rs"]
mod tests;
