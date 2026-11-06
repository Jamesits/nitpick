//! Linux backend. The DRM connectors in sysfs give the devices and their
//! EDID; KMS connector properties give the signalled HDR state and
//! colorimetry; colord gives the profile-service device.
//!
//! The backend reads the kernel state, not a Wayland or X11 client
//! report, so it works with any compositor and without a window system.
//!
//! The mapping is portable. Native calls are in `system`, which builds on
//! Linux only.

use std::collections::BTreeMap;

use nitpick_core::device::{
    DeviceCapabilities, DeviceError, DeviceId, DeviceInfo, DeviceMode, DeviceQuery,
    DisplayDeviceControl, IccProfile, PreparedProfileChange, ProfileState, ProfileTarget, Report,
};
use nitpick_core::driver::Gamut;

use crate::common::{Field, NativeFailure, resolve, topology_revision, unsupported};
use crate::edid::{self, Edid, HdrBlock, HdrStaticMetadata};

#[cfg(target_os = "linux")]
mod system;
#[cfg(all(target_os = "linux", feature = "x11"))]
pub mod x11;

pub const BACKEND: &str = "linux";

const EDID: &str = "EDID";
const EDID_HDR: &str = "EDID HDR static metadata";
const KMS_PROPERTIES: &str = "KMS connector properties";
const HDR_OUTPUT_METADATA: &str = "KMS HDR_OUTPUT_METADATA";
const COLORSPACE: &str = "KMS Colorspace";
const CRTC_MODE: &str = "KMS CRTC mode";
const COLORD: &str = "colord GetDevicesByKind";

const NO_PROFILE_CONTROL: &str = "Linux ICC profile assignment is not implemented";
const NO_SDR_WCG: &str = "Linux has no report of an SDR wide-colour mode";
const NO_WIRE_DEPTH: &str = "KMS does not report the link depth; max bpc is only a limit";
const NO_SDR_WHITE: &str = "KMS has no SDR white level; compositor settings are not read";

/// colord metadata key of the EDID MD5.
pub const COLORD_EDID_MD5: &str = "OutputEdidMd5";

/// Names in [`DeviceInfo::evidence`] that [`screen_device`] reads.
const EVIDENCE_CONNECTOR: &str = "connector";
const EVIDENCE_EDID_MD5: &str = "edid_md5";

/// One connector in `/sys/class/drm`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectorReport {
    /// DRM card, for example `card1`.
    pub card: String,
    /// Kernel connector name, for example `HDMI-A-2`. A compositor can use
    /// another name: GNOME calls this connector `HDMI-2`.
    pub connector: String,
    /// Bus ID of the card's device, for example `0000:01:00.0`. Stable
    /// across restarts, unlike the card number.
    pub bus: Field<String>,
    /// KMS object ID.
    pub connector_id: Field<u32>,
    /// `connected`, `disconnected` or `unknown`.
    pub status: String,
    /// Empty if the connector has no EDID.
    pub edid: Field<Vec<u8>>,
}

/// A KMS connector property.
#[derive(Debug, Clone, PartialEq)]
pub enum Property<T> {
    /// The driver does not expose it.
    Absent,
    Value(T),
    Failed(NativeFailure),
}

/// KMS state of one connector.
#[derive(Debug, Clone, PartialEq)]
pub struct KmsConnector {
    pub connector_id: u32,
    /// Blob of `struct hdr_output_metadata`; `None` for blob ID 0.
    pub hdr_output_metadata: Property<Option<Vec<u8>>>,
    /// Name of the `Colorspace` enum value.
    pub colorspace: Property<String>,
    /// Mode size of the connector's CRTC; `None` without a CRTC.
    pub crtc_mode: Field<Option<(u32, u32)>>,
}

/// One colord display device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColordDevice {
    /// `DeviceId`, for example `xrandr-Dell Inc.-DELL U2417H-<serial>`.
    pub id: String,
    /// Owner UID. The session's colour daemon registers devices per user.
    pub owner: u32,
    /// `Metadata`.
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColordReport {
    /// UID of this process.
    pub uid: u32,
    pub devices: Vec<ColordDevice>,
}

/// Native calls. [`LinuxDisplay::new`] uses the OS; tests inject reports.
pub trait LinuxNative {
    /// Every connector of every card.
    ///
    /// # Errors
    /// Unreadable `/sys/class/drm`; [`DeviceError::Unsupported`] if it
    /// does not exist.
    fn connectors(&mut self) -> Result<Vec<ConnectorReport>, DeviceError>;
    /// Connectors of one card.
    ///
    /// # Errors
    /// The card cannot be opened, for example without seat access. The
    /// KMS fields of its devices are then unknown.
    fn kms(&mut self, card: &str) -> Field<Vec<KmsConnector>>;
    /// Display devices without starting colord.
    ///
    /// # Errors
    /// The reason, if colord is not running or does not answer.
    fn colord(&mut self) -> Result<ColordReport, String>;
}

/// How the window system names a screen. See "Linux reports" in
/// `doc/design/display_device.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenSource {
    /// A Wayland output.
    Wayland,
    /// An Xwayland RandR output: it has the property `RANDR Emulation`.
    Xwayland,
    /// A RandR output of another X server.
    X11 {
        /// The RandR `EDID` output property; `None` without it.
        edid: Option<Vec<u8>>,
    },
}

/// A window-system screen, for example a Qt `QScreen`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxScreen {
    /// Output name: `QScreen::name()` or the RandR output name.
    pub name: String,
    pub source: ScreenSource,
}

/// The one device in `devices` that shows `screen`.
///
/// Wayland compositors and Xwayland name an output after its kernel
/// connector, but GNOME writes the type `HDMI-A` as `HDMI`. The names do
/// not include the card, so one name on two cards is ambiguous. Other X
/// servers can use other names, so their outputs match by EDID MD5.
///
/// # Errors
/// [`DeviceError::Unsupported`] if no device matches;
/// [`DeviceError::AmbiguousIdentity`] if more than one matches.
pub fn screen_device(
    screen: &LinuxScreen,
    devices: &[DeviceInfo],
) -> Result<DeviceId, DeviceError> {
    let evidence = |d: &DeviceInfo, name: &str| {
        d.evidence
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    let (rule, matches): (String, Vec<&DeviceInfo>) = match &screen.source {
        ScreenSource::Wayland | ScreenSource::Xwayland => (
            format!("output name {}", screen.name),
            devices
                .iter()
                .filter(|d| {
                    evidence(d, EVIDENCE_CONNECTOR)
                        .is_some_and(|c| connector_has_name(&c, &screen.name))
                })
                .collect(),
        ),
        ScreenSource::X11 { edid: None } => {
            return Err(unsupported(
                "screen lookup",
                format!("RandR output {} has no EDID property", screen.name),
            ));
        }
        ScreenSource::X11 { edid: Some(edid) } => {
            let md5 = edid::md5_hex(edid);
            (
                format!("EDID MD5 {md5} of RandR output {}", screen.name),
                devices
                    .iter()
                    .filter(|d| evidence(d, EVIDENCE_EDID_MD5).as_ref() == Some(&md5))
                    .collect(),
            )
        }
    };
    match matches.as_slice() {
        [d] => Ok(d.id.clone()),
        [] => Err(unsupported(
            "screen lookup",
            format!("no connected DRM connector matches the {rule}"),
        )),
        many => Err(DeviceError::AmbiguousIdentity(format!(
            "{} DRM connectors match the {rule}",
            many.len()
        ))),
    }
}

/// The kernel has no connector type `HDMI`, so the GNOME form `HDMI-n`
/// names only `HDMI-A-n`.
fn connector_has_name(connector: &str, name: &str) -> bool {
    connector == name
        || name
            .strip_prefix("HDMI-")
            .is_some_and(|n| connector.strip_prefix("HDMI-A-") == Some(n))
}

pub struct LinuxDisplay<N> {
    native: N,
}

#[cfg(target_os = "linux")]
impl LinuxDisplay<system::System> {
    #[must_use]
    pub fn new() -> Self {
        Self::with_native(system::System)
    }
}

#[cfg(target_os = "linux")]
impl Default for LinuxDisplay<system::System> {
    fn default() -> Self {
        Self::new()
    }
}

struct Entry {
    info: DeviceInfo,
    connector: ConnectorReport,
    edid: Result<Edid, String>,
    /// MD5 of a non-empty EDID.
    md5: Option<String>,
}

struct Snapshot {
    entries: Vec<Entry>,
    revision: u64,
}

impl<N: LinuxNative> LinuxDisplay<N> {
    pub fn with_native(native: N) -> Self {
        Self { native }
    }

    /// The device of a window-system screen; see [`screen_device`]. Reads
    /// sysfs only.
    ///
    /// # Errors
    /// As [`screen_device`]; an unreadable `/sys/class/drm`.
    pub fn device_for_screen(&mut self, screen: &LinuxScreen) -> Result<DeviceId, DeviceError> {
        screen_device(screen, &self.devices()?)
    }

    /// Sysfs only: listing devices opens no device node and calls no
    /// service.
    fn snapshot(&mut self) -> Result<Snapshot, DeviceError> {
        let entries: Vec<Entry> = self
            .native
            .connectors()?
            .into_iter()
            .filter(|c| c.status == "connected")
            .map(entry)
            .collect();
        let infos: Vec<DeviceInfo> = entries.iter().map(|e| e.info.clone()).collect();
        Ok(Snapshot {
            revision: topology_revision(&infos),
            entries,
        })
    }
}

fn entry(connector: ConnectorReport) -> Entry {
    let (edid, md5) = match &connector.edid {
        Ok(bytes) if bytes.is_empty() => (Err("connector has no EDID".into()), None),
        Ok(bytes) => (Edid::parse(bytes), Some(edid::md5_hex(bytes))),
        Err(e) => (Err(e.to_string()), None),
    };
    Entry {
        info: device_info(&connector, edid.as_ref().ok(), md5.as_deref()),
        connector,
        edid,
        md5,
    }
}

/// The key holds the card's bus ID, the connector and the EDID identity.
/// A display on another port, or another unit on the same port, gets a
/// new key, so an old recovery record fails as disconnected instead of
/// matching it. Without a readable bus ID the key holds the card number,
/// which can change after a restart.
///
/// The serial text is preferred to the numeric serial: many displays put
/// a fixed numeric value in the EDID.
fn device_info(c: &ConnectorReport, edid: Option<&Edid>, md5: Option<&str>) -> DeviceInfo {
    let bus = c.bus.clone().unwrap_or_else(|_| c.card.clone());
    let unit = edid.map_or_else(
        || "no-edid".to_owned(),
        |e| {
            let serial = e
                .serial_text
                .clone()
                .unwrap_or_else(|| format!("{:08X}", e.serial));
            format!("{}{:04X}-{serial}", e.pnp_id(), e.product)
        },
    );
    let name = edid
        .and_then(|e| e.name.clone())
        .unwrap_or_else(|| c.connector.clone());
    // The card number is left out: it can change after a restart.
    let mut evidence = vec![(EVIDENCE_CONNECTOR.into(), c.connector.clone())];
    if let Some(e) = edid {
        evidence.push((
            "edid_ids".into(),
            format!("{:04X}:{:04X}", e.manufacturer, e.product),
        ));
        evidence.push(("serial".into(), format!("{:08X}", e.serial)));
        if let Some(s) = &e.serial_text {
            evidence.push(("serial_text".into(), s.clone()));
        }
    }
    if let Some(md5) = md5 {
        evidence.push((EVIDENCE_EDID_MD5.into(), md5.to_owned()));
    }
    DeviceInfo {
        id: DeviceId {
            backend: BACKEND.into(),
            key: format!("{bus}/{}/{unit}", c.connector),
        },
        name,
        evidence,
    }
}

fn known<T>(value: T, source: &str) -> Report<T> {
    Report::known(value, source)
}

fn unknown<T>(reason: impl Into<String>) -> Report<T> {
    Report::unknown(reason)
}

fn hdr_metadata(e: &Entry) -> Result<&HdrStaticMetadata, String> {
    match &e.edid {
        Ok(Edid {
            hdr: HdrBlock::Found(h),
            ..
        }) => Ok(h),
        Ok(_) => Err(format!("{EDID} has no HDR static metadata")),
        Err(reason) => Err(reason.clone()),
    }
}

/// HDR support needs the display (EDID) and the driver (the property that
/// carries the HDR infoframe). It does not show that the compositor can
/// enable HDR.
fn hdr_support(e: &Entry, kms: Result<&KmsConnector, &str>) -> Report<bool> {
    let display = match &e.edid {
        Ok(Edid {
            hdr: HdrBlock::Found(h),
            ..
        }) => known(h.pq(), EDID_HDR),
        Ok(Edid {
            hdr: HdrBlock::Absent { displayid: true },
            ..
        }) => unknown(format!(
            "{EDID} has a DisplayID extension, which is not read"
        )),
        Ok(_) => known(false, EDID),
        Err(reason) => unknown(reason.clone()),
    };
    let driver = match kms {
        Ok(k) => known(
            !matches!(k.hdr_output_metadata, Property::Absent),
            KMS_PROPERTIES,
        ),
        Err(reason) => unknown(reason),
    };
    match (display, driver) {
        (no @ Report::Known { value: false, .. }, _)
        | (_, no @ Report::Known { value: false, .. }) => no,
        (Report::Known { .. }, Report::Known { .. }) => {
            known(true, &format!("{EDID_HDR}; {KMS_PROPERTIES}"))
        }
        (Report::Unknown(reason), _) | (_, Report::Unknown(reason)) => unknown(reason),
    }
}

/// `HDMI_STATIC_METADATA_TYPE1` and the `hdmi_eotf` values
/// (`linux/hdmi.h`).
mod hdmi {
    pub const STATIC_METADATA_TYPE1: u32 = 0;
    pub const EOTF_TRADITIONAL_SDR: u8 = 0;
    pub const EOTF_ST2084: u8 = 2;
    pub const EOTF_HLG: u8 = 3;
}

/// The blob is `struct hdr_output_metadata` (`drm_mode.h`): a native-endian
/// `u32` type, then the infoframe with the EOTF in its first byte.
fn hdr_on(kms: Result<&KmsConnector, &str>) -> Report<bool> {
    let k = match kms {
        Ok(k) => k,
        Err(reason) => return unknown(reason),
    };
    let blob = match &k.hdr_output_metadata {
        Property::Absent => return known(false, KMS_PROPERTIES),
        Property::Failed(e) => return unknown(e.to_string()),
        Property::Value(None) => return known(false, HDR_OUTPUT_METADATA),
        Property::Value(Some(blob)) => blob,
    };
    let (Some(kind), Some(&eotf)) = (blob.get(..4), blob.get(4)) else {
        return unknown(format!("{HDR_OUTPUT_METADATA}: {} bytes", blob.len()));
    };
    let kind = u32::from_ne_bytes([kind[0], kind[1], kind[2], kind[3]]);
    if kind != hdmi::STATIC_METADATA_TYPE1 {
        return unknown(format!("{HDR_OUTPUT_METADATA}: metadata type {kind}"));
    }
    match eotf {
        hdmi::EOTF_ST2084 | hdmi::EOTF_HLG => known(true, HDR_OUTPUT_METADATA),
        hdmi::EOTF_TRADITIONAL_SDR => known(false, HDR_OUTPUT_METADATA),
        other => unknown(format!("{HDR_OUTPUT_METADATA}: EOTF {other}")),
    }
}

/// Only an explicit colorimetry gives a gamut. With `Default` the driver
/// signals no colorimetry, and a colour-managed compositor can send the
/// panel's native gamut.
fn gamut(kms: Result<&KmsConnector, &str>) -> Report<Gamut> {
    let k = match kms {
        Ok(k) => k,
        Err(reason) => return unknown(reason),
    };
    match &k.colorspace {
        Property::Absent => unknown("driver has no Colorspace property"),
        Property::Failed(e) => unknown(e.to_string()),
        Property::Value(name) => match name.as_str() {
            "BT2020_RGB" | "BT2020_YCC" | "BT2020_CYCC" => known(Gamut::Rec2020, COLORSPACE),
            "DCI-P3_RGB_D65" => known(Gamut::P3, COLORSPACE),
            "Default" => unknown("Colorspace Default signals no colorimetry"),
            other => unknown(format!("{COLORSPACE} {other} is not a known gamut")),
        },
    }
}

/// From the EDID, as Windows takes them through DXGI. Code 0 of a maximum
/// is "not given": 50 cd/m² is below any HDR display. A minimum is known
/// only with a known peak, because its code scales the peak.
fn luminance(e: &Entry) -> (Report<f64>, Report<f64>, Report<f64>) {
    let h = match hdr_metadata(e) {
        Ok(h) => h,
        Err(reason) => return (unknown(&reason), unknown(&reason), unknown(reason)),
    };
    let max = |cv: Option<u8>, field: &str| match cv {
        Some(cv) if cv > 0 => known(edid::max_luminance(cv), EDID_HDR),
        Some(_) => unknown(format!("{EDID_HDR}: {field} code 0")),
        None => unknown(format!("{EDID_HDR}: no {field}")),
    };
    let peak = max(h.max_luminance, "max luminance");
    let full_frame = max(h.max_frame_average, "max frame-average luminance");
    let min = match (&peak, h.min_luminance) {
        (Report::Known { value, .. }, Some(cv)) => known(edid::min_luminance(cv, *value), EDID_HDR),
        (Report::Known { .. }, None) => unknown(format!("{EDID_HDR}: no min luminance")),
        (Report::Unknown(reason), _) => unknown(reason.clone()),
    };
    (peak, min, full_frame)
}

/// The CRTC mode is the composed frame size. Unverified: on an internal
/// panel with a GPU scaler, the wire size can differ from it.
fn native_size(kms: Result<&KmsConnector, &str>) -> Report<(usize, usize)> {
    match kms.map(|k| &k.crtc_mode) {
        Err(reason) => unknown(reason),
        Ok(Err(e)) => unknown(e.to_string()),
        Ok(Ok(None)) => unknown("connector has no CRTC"),
        Ok(Ok(Some((w, h)))) if *w > 0 && *h > 0 => known((*w as usize, *h as usize), CRTC_MODE),
        Ok(Ok(Some((w, h)))) => unknown(format!("{CRTC_MODE} is {w}x{h}")),
    }
}

/// colord matches a display device to an output by the EDID MD5 in its
/// metadata. Devices of other users are not this process's devices.
fn icc_profiles(e: &Entry, all: &[Entry], colord: &Result<ColordReport, String>) -> Report<bool> {
    let colord = match colord {
        Ok(c) => c,
        Err(reason) => return unknown(format!("colord: {reason}")),
    };
    let Some(md5) = &e.md5 else {
        return unknown("no EDID to match a colord device");
    };
    let outputs = all.iter().filter(|o| o.md5.as_ref() == Some(md5)).count();
    if outputs > 1 {
        return unknown(format!("{outputs} outputs have the same EDID"));
    }
    let (own, other): (Vec<&ColordDevice>, Vec<&ColordDevice>) = colord
        .devices
        .iter()
        .filter(|d| d.metadata.get(COLORD_EDID_MD5) == Some(md5))
        .partition(|d| d.owner == colord.uid);
    match (own.as_slice(), other.as_slice()) {
        ([d], _) => unknown(format!("{NO_PROFILE_CONTROL}; colord device {}", d.id)),
        ([], []) => known(false, COLORD),
        ([], [d, ..]) => unknown(format!("colord device {} has owner {}", d.id, d.owner)),
        (many, _) => unknown(format!("{} colord devices match the EDID", many.len())),
    }
}

impl<N: LinuxNative> DisplayDeviceControl for LinuxDisplay<N> {
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, DeviceError> {
        Ok(self
            .snapshot()?
            .entries
            .into_iter()
            .map(|e| e.info)
            .collect())
    }

    fn query_device(&mut self, device: &DeviceId) -> Result<DeviceQuery, DeviceError> {
        let snapshot = self.snapshot()?;
        let e = resolve(&snapshot.entries, device, |e| &e.info)?;
        let card = &e.connector.card;
        let kms_list = self.native.kms(card);
        let kms: Result<&KmsConnector, String> = match (&kms_list, &e.connector.connector_id) {
            (Err(err), _) => Err(format!("{card}: {err}")),
            (_, Err(err)) => Err(err.to_string()),
            (Ok(list), Ok(id)) => list
                .iter()
                .find(|k| k.connector_id == *id)
                .ok_or_else(|| format!("{card} has no KMS connector {id}")),
        };
        let kms = kms.as_ref().copied().map_err(String::as_str);
        let colord = self.native.colord();
        let (peak_nits, min_nits, full_frame_nits) = luminance(e);
        Ok(DeviceQuery {
            device: e.info.clone(),
            capabilities: DeviceCapabilities {
                hdr: hdr_support(e, kms),
                sdr_wcg: unknown(NO_SDR_WCG),
                wire_depths: unknown(NO_WIRE_DEPTH),
                icc_profiles: icc_profiles(e, &snapshot.entries, &colord),
            },
            mode: DeviceMode {
                hdr_on: hdr_on(kms),
                gamut: gamut(kms),
                wire_depth: unknown(NO_WIRE_DEPTH),
                peak_nits,
                min_nits,
                full_frame_nits,
                sdr_white_nits: unknown(NO_SDR_WHITE),
                native_size: native_size(kms),
                edr: unknown("EDR headroom is a macOS report"),
            },
            topology_revision: snapshot.revision,
        })
    }

    fn profile_targets(&mut self, device: &DeviceId) -> Result<Vec<ProfileTarget>, DeviceError> {
        let snapshot = self.snapshot()?;
        resolve(&snapshot.entries, device, |e| &e.info)?;
        Ok(Vec::new())
    }

    fn profile_state(&mut self, _: &ProfileTarget) -> Result<ProfileState, DeviceError> {
        Err(unsupported("profile state", NO_PROFILE_CONTROL))
    }

    fn prepare_icc_profile(
        &mut self,
        _: &ProfileTarget,
        _: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError> {
        Err(unsupported("profile preparation", NO_PROFILE_CONTROL))
    }

    fn set_icc_profile(&mut self, _: &PreparedProfileChange) -> Result<ProfileState, DeviceError> {
        Err(unsupported("profile assignment", NO_PROFILE_CONTROL))
    }

    fn restore_profile(&mut self, _: &PreparedProfileChange) -> Result<ProfileState, DeviceError> {
        Err(unsupported("profile restoration", NO_PROFILE_CONTROL))
    }
}

#[cfg(test)]
#[path = "linux_tests.rs"]
mod tests;
