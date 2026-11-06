//! Windows backend. `DisplayConfig` paths give identity, Advanced Color
//! state, wire depth and SDR white; DXGI outputs give luminance and the
//! composition colour space.
//!
//! The mapping is portable. Native calls are in `system`, which builds on
//! Windows only.

use nitpick_core::device::{
    DeviceCapabilities, DeviceError, DeviceId, DeviceInfo, DeviceMode, DeviceQuery,
    DisplayDeviceControl, IccProfile, PreparedProfileChange, ProfileState, ProfileTarget, Report,
};
use nitpick_core::driver::Gamut;

use crate::common::{Field, NativeFailure, resolve, topology_revision, unsupported};

#[cfg(windows)]
mod system;

pub const BACKEND: &str = "windows";

const INFO: &str = "DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO";
const INFO_2: &str = "DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2";
const DXGI_DESC1: &str = "DXGI_OUTPUT_DESC1";
const SDR_WHITE: &str = "DISPLAYCONFIG_SDR_WHITE_LEVEL";
const TARGET_MODE: &str = "DISPLAYCONFIG_TARGET_MODE";

const NO_PROFILE_CONTROL: &str = "Windows ICC profile assignment is not implemented";

/// `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO` bit fields (`wingdi.h`).
pub mod info_flags {
    pub const SUPPORTED: u32 = 1 << 0;
    pub const ENABLED: u32 = 1 << 1;
    pub const WIDE_COLOR_ENFORCED: u32 = 1 << 2;
}

/// `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` bit fields (`wingdi.h`,
/// SDK 10.0.22000 and later).
pub mod info_2_flags {
    pub const SUPPORTED: u32 = 1 << 0;
    pub const ACTIVE: u32 = 1 << 1;
    pub const HDR_SUPPORTED: u32 = 1 << 4;
    pub const WCG_SUPPORTED: u32 = 1 << 6;
}

/// `DISPLAYCONFIG_ADVANCED_COLOR_MODE`.
pub mod color_mode {
    pub const SDR: i32 = 0;
    pub const WCG: i32 = 1;
    pub const HDR: i32 = 2;
}

/// `DXGI_COLOR_SPACE_TYPE` values that the mapping reads.
pub mod dxgi_color_space {
    pub const RGB_FULL_G22_NONE_P709: i32 = 0;
    pub const RGB_FULL_G2084_NONE_P2020: i32 = 12;
}

/// `DISPLAYCONFIG_TARGET_DEVICE_NAME`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetName {
    pub friendly_name: String,
    /// Monitor device interface path. Includes the connection instance,
    /// so two identical monitors have different paths.
    pub device_path: String,
    /// EDID manufacturer and product code, if `edidIdsValid` is set.
    pub edid_ids: Option<(u16, u16)>,
    pub connector_instance: u32,
    /// `DISPLAYCONFIG_VIDEO_OUTPUT_TECHNOLOGY`.
    pub output_technology: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvancedColorInfo {
    /// [`info_flags`].
    pub flags: u32,
    pub bits_per_color_channel: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvancedColorInfo2 {
    /// [`info_2_flags`].
    pub flags: u32,
    pub bits_per_color_channel: u32,
    /// [`color_mode`].
    pub active_color_mode: i32,
}

/// One active `QueryDisplayConfig` path and its device information.
#[derive(Debug, Clone, PartialEq)]
pub struct PathReport {
    /// `(HighPart << 32) | LowPart`. Changes after a restart.
    pub adapter_luid: i64,
    pub source_id: u32,
    pub target_id: u32,
    /// GDI name, for example `\\.\DISPLAY1`.
    pub source_name: Field<String>,
    pub target_name: Field<TargetName>,
    pub color_info: Field<AdvancedColorInfo>,
    /// Fails before Windows 11 24H2.
    pub color_info_2: Field<AdvancedColorInfo2>,
    /// Raw `SDRWhiteLevel`: 1000 is 80 cd/m².
    pub sdr_white_level: Field<u32>,
    /// Active signal size of the target mode. `None` if the path has no
    /// target mode.
    pub target_active_size: Option<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputDesc1 {
    /// `DXGI_COLOR_SPACE_TYPE`.
    pub color_space: i32,
    pub bits_per_color: u32,
    /// cd/m².
    pub min_luminance: f32,
    pub max_luminance: f32,
    pub max_full_frame_luminance: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DxgiOutput {
    /// GDI name; equals [`PathReport::source_name`] of its paths.
    pub device_name: String,
    /// Fails without `IDXGIOutput6`.
    pub desc1: Field<OutputDesc1>,
}

/// Native calls. [`WindowsDisplay::new`] uses the OS; tests inject reports.
pub trait WindowsNative {
    /// Active paths.
    ///
    /// # Errors
    /// `QueryDisplayConfig` failure. Per-path device information failures
    /// are in the report fields.
    fn display_paths(&mut self) -> Result<Vec<PathReport>, DeviceError>;
    /// Outputs of every adapter.
    ///
    /// # Errors
    /// DXGI factory or enumeration failure. The DXGI fields of every
    /// device are then unknown.
    fn dxgi_outputs(&mut self) -> Field<Vec<DxgiOutput>>;
}

pub struct WindowsDisplay<N> {
    native: N,
}

#[cfg(windows)]
impl WindowsDisplay<system::System> {
    #[must_use]
    pub fn new() -> Self {
        Self::with_native(system::System)
    }
}

#[cfg(windows)]
impl Default for WindowsDisplay<system::System> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: WindowsNative> WindowsDisplay<N> {
    pub fn with_native(native: N) -> Self {
        Self { native }
    }

    /// The device shown by the GDI display `name`, for example the
    /// `MONITORINFOEXW::szDevice` of a window-system screen.
    ///
    /// # Errors
    /// [`DeviceError::AmbiguousIdentity`] if the source drives more than
    /// one target (clone mode); [`DeviceError::Unsupported`] if no active
    /// path has this source; a `QueryDisplayConfig` failure.
    pub fn device_for_gdi_name(&mut self, name: &str) -> Result<DeviceId, DeviceError> {
        let snapshot = self.snapshot()?;
        let mut matches = snapshot.entries.iter().filter(|e| {
            e.path
                .source_name
                .as_ref()
                .is_ok_and(|n| n.eq_ignore_ascii_case(name))
        });
        let first = matches
            .next()
            .ok_or_else(|| unsupported("screen lookup", format!("no display path for {name}")))?;
        let others = matches.count();
        if others > 0 {
            return Err(DeviceError::AmbiguousIdentity(format!(
                "{name} drives {} display targets",
                others + 1
            )));
        }
        Ok(first.info.id.clone())
    }

    fn snapshot(&mut self) -> Result<Snapshot, DeviceError> {
        let entries: Vec<Entry> = self
            .native
            .display_paths()?
            .into_iter()
            .map(|path| Entry {
                info: device_info(&path),
                path,
            })
            .collect();
        let infos: Vec<DeviceInfo> = entries.iter().map(|e| e.info.clone()).collect();
        Ok(Snapshot {
            revision: topology_revision(&infos),
            entries,
            dxgi: self.native.dxgi_outputs(),
        })
    }
}

struct Entry {
    info: DeviceInfo,
    path: PathReport,
}

struct Snapshot {
    entries: Vec<Entry>,
    dxgi: Field<Vec<DxgiOutput>>,
    revision: u64,
}

/// The monitor device path is the key: it is stable across restarts and
/// distinguishes identical monitors. The fallback key contains the adapter
/// LUID, so it does not survive a restart; a recovery record for it then
/// fails as disconnected instead of matching another display.
fn device_info(path: &PathReport) -> DeviceInfo {
    let target = path.target_name.as_ref().ok();
    let key = match target {
        Some(t) if !t.device_path.is_empty() => t.device_path.clone(),
        _ => format!("target:{:016x}:{}", path.adapter_luid, path.target_id),
    };
    let name = target
        .map(|t| t.friendly_name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| path.source_name.clone().ok())
        .unwrap_or_else(|| format!("Display target {}", path.target_id));
    // Only fields that survive a restart. The adapter LUID, source and
    // target IDs are re-resolved from the key before each native call.
    let mut evidence = Vec::new();
    if let Some(t) = target {
        if let Some((manufacturer, product)) = t.edid_ids {
            evidence.push((
                "edid_ids".into(),
                format!("{manufacturer:04X}:{product:04X}"),
            ));
        }
        evidence.push((
            "connector_instance".into(),
            t.connector_instance.to_string(),
        ));
        evidence.push(("output_technology".into(), t.output_technology.to_string()));
    }
    DeviceInfo {
        id: DeviceId {
            backend: BACKEND.into(),
            key,
        },
        name,
        evidence,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColorKind {
    Sdr,
    /// SDR with Advanced Color (FP16 composition, display-referred).
    Wcg,
    Hdr,
}

struct Active {
    hdr_on: Report<bool>,
    kind: Report<ColorKind>,
}

fn known<T>(value: T, source: &str) -> Report<T> {
    Report::known(value, source)
}

fn unknown<T>(reason: impl Into<String>) -> Report<T> {
    Report::unknown(reason)
}

fn failed<T>(e: &NativeFailure) -> Report<T> {
    unknown(e.to_string())
}

/// `advancedColorEnabled` alone does not set HDR: it is also set for SDR
/// WCG. Without `INFO_2`, HDR comes from the DXGI colour space.
fn active(path: &PathReport, desc1: Result<&OutputDesc1, &str>) -> Active {
    if let Ok(info) = &path.color_info_2 {
        let kind = match info.active_color_mode {
            color_mode::SDR => known(ColorKind::Sdr, INFO_2),
            color_mode::WCG => known(ColorKind::Wcg, INFO_2),
            color_mode::HDR => known(ColorKind::Hdr, INFO_2),
            other => unknown(format!("{INFO_2}: unknown activeColorMode {other}")),
        };
        let hdr_on = match &kind {
            Report::Known { value, .. } => known(*value == ColorKind::Hdr, INFO_2),
            Report::Unknown(reason) => unknown(reason.clone()),
        };
        return Active { hdr_on, kind };
    }
    let legacy = path.color_info.as_ref().ok().map(|i| i.flags);
    if legacy.is_some_and(|f| f & info_flags::ENABLED == 0) {
        return Active {
            hdr_on: known(false, INFO),
            kind: known(ColorKind::Sdr, INFO),
        };
    }
    if let Ok(d) = desc1
        && d.color_space == dxgi_color_space::RGB_FULL_G2084_NONE_P2020
    {
        return Active {
            hdr_on: known(true, DXGI_DESC1),
            kind: known(ColorKind::Hdr, DXGI_DESC1),
        };
    }
    // Unverified: WIDE_COLOR_ENFORCED is set only for SDR WCG.
    if legacy.is_some_and(|f| f & info_flags::WIDE_COLOR_ENFORCED != 0) {
        return Active {
            hdr_on: known(false, INFO),
            kind: known(ColorKind::Wcg, INFO),
        };
    }
    let hdr_on = match (desc1, &path.color_info) {
        (Ok(_), _) => known(false, DXGI_DESC1),
        (Err(d), Ok(_)) => unknown(d),
        (Err(d), Err(e)) => unknown(format!("{e}; {d}")),
    };
    let kind = match &path.color_info {
        Ok(_) => unknown(format!("{INFO} does not distinguish SDR from SDR WCG")),
        Err(e) => failed(e),
    };
    Active { hdr_on, kind }
}

fn capabilities(path: &PathReport, active: &Active) -> DeviceCapabilities {
    let (hdr, sdr_wcg) = match (&path.color_info_2, &path.color_info) {
        (Ok(i), _) => (
            known(i.flags & info_2_flags::HDR_SUPPORTED != 0, INFO_2),
            known(i.flags & info_2_flags::WCG_SUPPORTED != 0, INFO_2),
        ),
        (Err(_), Ok(i)) if i.flags & info_flags::SUPPORTED == 0 => {
            (known(false, INFO), known(false, INFO))
        }
        // The legacy support bit covers both kinds of Advanced Color. Only
        // the active kind proves support for that kind.
        (Err(_), Ok(_)) => {
            let reason = format!("{INFO} does not distinguish HDR from SDR WCG support");
            let proven = |kind| match &active.kind {
                Report::Known { value, source } if *value == kind => known(true, source),
                _ => unknown(reason.clone()),
            };
            (proven(ColorKind::Hdr), proven(ColorKind::Wcg))
        }
        (Err(e2), Err(e)) => {
            let reason = format!("{e2}; {e}");
            (unknown(reason.clone()), unknown(reason))
        }
    };
    DeviceCapabilities {
        hdr,
        sdr_wcg,
        wire_depths: unknown("Windows reports the active wire depth only"),
        icc_profiles: unknown(NO_PROFILE_CONTROL),
    }
}

fn mode(path: &PathReport, desc1: Result<&OutputDesc1, &str>, active: Active) -> DeviceMode {
    let gamut = match &active.kind {
        Report::Known { value, source } => match value {
            ColorKind::Hdr => known(Gamut::Rec2020, source),
            // Plain SDR is not colour managed: the signal is nominal sRGB.
            ColorKind::Sdr => known(Gamut::Srgb, source),
            // Unverified: the compositor targets the panel gamut from the
            // EDID or profile, which is not one of the named gamuts.
            ColorKind::Wcg => unknown("SDR WCG sends the panel's native gamut"),
        },
        Report::Unknown(reason) => unknown(reason.clone()),
    };
    let wire_depth = match (&path.color_info_2, &path.color_info) {
        (Ok(i), _) => depth(i.bits_per_color_channel, INFO_2),
        (Err(_), Ok(i)) => depth(i.bits_per_color_channel, INFO),
        (Err(e2), Err(e)) => unknown(format!("{e2}; {e}")),
    };
    let (peak_nits, min_nits, full_frame_nits) = luminance(desc1);
    let sdr_white_nits = match (&active.kind, &path.sdr_white_level) {
        (Report::Known { value, .. }, Ok(level)) if *value == ColorKind::Hdr && *level > 0 => {
            known(f64::from(*level) / 1000.0 * 80.0, SDR_WHITE)
        }
        (Report::Known { value, .. }, Ok(_)) if *value == ColorKind::Hdr => {
            unknown(format!("{SDR_WHITE} reported 0"))
        }
        (Report::Known { value, .. }, Err(e)) if *value == ColorKind::Hdr => failed(e),
        (Report::Known { .. }, _) => unknown("SDR white level applies only with HDR active"),
        (Report::Unknown(reason), _) => unknown(reason.clone()),
    };
    let native_size = match path.target_active_size {
        Some((w, h)) if w > 0 && h > 0 => known((w as usize, h as usize), TARGET_MODE),
        _ => unknown("path has no target mode"),
    };
    DeviceMode {
        hdr_on: active.hdr_on,
        gamut,
        wire_depth,
        peak_nits,
        min_nits,
        full_frame_nits,
        sdr_white_nits,
        native_size,
        edr: unknown("EDR headroom is a macOS report"),
    }
}

fn depth(bits: u32, source: &str) -> Report<u8> {
    match u8::try_from(bits) {
        Ok(b) if b > 0 => known(b, source),
        _ => unknown(format!("{source} reported {bits} bits per channel")),
    }
}

/// DXGI reports 0 for an absent luminance. A minimum of 0 is valid, so it
/// is known only with a known peak. Unverified: whether DXGI substitutes
/// OS defaults for an EDID without HDR static metadata.
fn luminance(desc1: Result<&OutputDesc1, &str>) -> (Report<f64>, Report<f64>, Report<f64>) {
    let d = match desc1 {
        Ok(d) => d,
        Err(reason) => return (unknown(reason), unknown(reason), unknown(reason)),
    };
    let positive = |v: f32, field: &str| {
        if v > 0.0 {
            known(f64::from(v), DXGI_DESC1)
        } else {
            unknown(format!("{DXGI_DESC1}.{field} is {v}"))
        }
    };
    let peak = positive(d.max_luminance, "MaxLuminance");
    let min = match &peak {
        Report::Known { .. } if d.min_luminance >= 0.0 => {
            known(f64::from(d.min_luminance), DXGI_DESC1)
        }
        Report::Known { .. } => {
            unknown(format!("{DXGI_DESC1}.MinLuminance is {}", d.min_luminance))
        }
        Report::Unknown(reason) => unknown(reason.clone()),
    };
    let full_frame = positive(d.max_full_frame_luminance, "MaxFullFrameLuminance");
    (peak, min, full_frame)
}

/// The DXGI description of the output that scans out `path`, matched by
/// GDI name, or the reason it is unavailable.
fn find_desc1<'a>(
    path: &PathReport,
    dxgi: &'a Field<Vec<DxgiOutput>>,
) -> Result<&'a OutputDesc1, String> {
    let outputs = dxgi.as_ref().map_err(ToString::to_string)?;
    let name = path
        .source_name
        .as_ref()
        .map_err(|e| format!("source name: {e}"))?;
    let output = outputs
        .iter()
        .find(|o| o.device_name.eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no DXGI output for {name}"))?;
    output.desc1.as_ref().map_err(ToString::to_string)
}

impl<N: WindowsNative> DisplayDeviceControl for WindowsDisplay<N> {
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
        let entry = resolve(&snapshot.entries, device, |e| &e.info)?;
        let desc1 = find_desc1(&entry.path, &snapshot.dxgi);
        let desc1 = desc1.as_ref().copied().map_err(String::as_str);
        let active = active(&entry.path, desc1);
        Ok(DeviceQuery {
            device: entry.info.clone(),
            capabilities: capabilities(&entry.path, &active),
            mode: mode(&entry.path, desc1, active),
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
#[path = "windows_tests.rs"]
mod tests;
