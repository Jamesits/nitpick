//! macOS backend. Core Graphics gives the active displays and their modes;
//! `ColorSync` gives the device identity; `AppKit` gives the name, EDR
//! headroom and P3 support.
//!
//! The mapping is portable. Native calls are in `system`, which builds on
//! macOS only.

use nitpick_core::device::{
    DeviceCapabilities, DeviceError, DeviceId, DeviceInfo, DeviceMode, DeviceQuery,
    DisplayDeviceControl, EdrHeadroom, IccProfile, PreparedProfileChange, ProfileState,
    ProfileTarget, Report,
};

use crate::common::{Field, resolve, topology_revision, unsupported};

#[cfg(target_os = "macos")]
mod system;

pub const BACKEND: &str = "macos";

const COLORSYNC_INFO: &str = "ColorSyncDeviceCopyDeviceInfo";
const MODES: &str = "CGDisplayCopyAllDisplayModes";
const EDR: &str = "NSScreen EDR component values";
const GAMUT: &str = "NSScreen.canRepresentDisplayGamut";

const NO_PROFILE_CONTROL: &str = "macOS ICC profile assignment is not implemented";
const NO_HDR_REPORT: &str = "macOS has no public HDR report; EDR headroom does not establish HDR";
const NO_WIRE_DEPTH: &str = "macOS does not report the wire depth";
const NO_LUMINANCE: &str = "macOS reports EDR headroom relative to SDR white, not luminance";
const NO_GAMUT: &str = "macOS converts to the display profile and does not report the signal gamut";

/// `kDisplayModeNativeFlag` (`IOGraphicsTypes.h`).
pub const MODE_NATIVE_FLAG: u32 = 0x0200_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeReport {
    pub pixel_width: usize,
    pub pixel_height: usize,
    /// `CGDisplayModeGetIOFlags`.
    pub io_flags: u32,
}

/// `ColorSyncDeviceCopyDeviceInfo` of a registered display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorSyncDevice {
    /// `kColorSyncDeviceDescription`.
    pub description: Option<String>,
    /// `kColorSyncDeviceDefaultProfileID`.
    pub default_profile_id: Option<String>,
}

/// One active Core Graphics display.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayReport {
    /// `CGDirectDisplayID`. Changes after a reconnect or restart.
    pub display_id: u32,
    /// `CGDisplayCreateUUIDFromDisplayID`: the `ColorSync` device ID.
    pub uuid: Field<String>,
    pub vendor: u32,
    pub model: u32,
    /// 0 if the EDID has no serial number.
    pub serial: u32,
    pub builtin: bool,
    /// Includes duplicate low-resolution modes.
    pub modes: Field<Vec<ModeReport>>,
    /// `None` if `ColorSync` has no device for the UUID or the UUID failed.
    pub colorsync: Option<ColorSyncDevice>,
}

/// One `NSScreen`.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenReport {
    /// `NSScreenNumber`.
    pub display_id: u32,
    pub name: String,
    /// `maximumExtendedDynamicRangeColorComponentValue`.
    pub edr_current: f64,
    /// `maximumPotentialExtendedDynamicRangeColorComponentValue`.
    pub edr_potential: f64,
    /// `maximumReferenceExtendedDynamicRangeColorComponentValue`; 0 without
    /// a reference mode.
    pub edr_reference: f64,
    /// `canRepresentDisplayGamut(P3)`.
    pub p3: bool,
}

/// Native calls. [`MacDisplay::new`] uses the OS; tests inject reports.
pub trait MacNative {
    /// Active displays. Core Graphics and `ColorSync` only; callable on any
    /// thread.
    ///
    /// # Errors
    /// `CGGetActiveDisplayList` failure.
    fn displays(&mut self) -> Result<Vec<DisplayReport>, DeviceError>;
    /// `AppKit` screens. Runs on the main thread.
    ///
    /// # Errors
    /// The reason, if the main thread did not run the query. The `AppKit`
    /// fields of every device are then unknown.
    fn screens(&mut self) -> Result<Vec<ScreenReport>, String>;
}

pub struct MacDisplay<N> {
    native: N,
}

#[cfg(target_os = "macos")]
impl MacDisplay<system::System> {
    #[must_use]
    pub fn new() -> Self {
        Self::with_native(system::System)
    }
}

#[cfg(target_os = "macos")]
impl Default for MacDisplay<system::System> {
    fn default() -> Self {
        Self::new()
    }
}

struct Entry {
    info: DeviceInfo,
    display: DisplayReport,
    screen: Result<ScreenReport, String>,
}

struct Snapshot {
    entries: Vec<Entry>,
    revision: u64,
}

impl<N: MacNative> MacDisplay<N> {
    pub fn with_native(native: N) -> Self {
        Self { native }
    }

    /// The device of the Core Graphics display `display_id`, for example
    /// the `NSScreenNumber` of a window-system screen.
    ///
    /// # Errors
    /// [`DeviceError::Unsupported`] if no active display has this ID; a
    /// `CGGetActiveDisplayList` failure.
    pub fn device_for_display_id(&mut self, display_id: u32) -> Result<DeviceId, DeviceError> {
        self.snapshot()?
            .entries
            .into_iter()
            .find(|e| e.display.display_id == display_id)
            .map(|e| e.info.id)
            .ok_or_else(|| {
                unsupported(
                    "screen lookup",
                    format!("no active display with ID {display_id}"),
                )
            })
    }

    fn snapshot(&mut self) -> Result<Snapshot, DeviceError> {
        let displays = self.native.displays()?;
        let screens = self.native.screens();
        let entries: Vec<Entry> = displays
            .into_iter()
            .map(|display| {
                let screen = match &screens {
                    Ok(list) => list
                        .iter()
                        .find(|s| s.display_id == display.display_id)
                        .cloned()
                        .ok_or_else(|| format!("no NSScreen for display {}", display.display_id)),
                    Err(reason) => Err(reason.clone()),
                };
                Entry {
                    info: device_info(&display, screen.as_ref().ok()),
                    display,
                    screen,
                }
            })
            .collect();
        let infos: Vec<DeviceInfo> = entries.iter().map(|e| e.info.clone()).collect();
        Ok(Snapshot {
            revision: topology_revision(&infos),
            entries,
        })
    }
}

/// The `ColorSync` UUID is the key: profile assignment needs it.
/// Unverified: that the UUID survives a restart, and whether two identical
/// monitors without EDID serial numbers get the same UUID; `resolve` then
/// reports them as ambiguous. The fallback key is the display ID, which
/// does not survive a reconnect.
fn device_info(display: &DisplayReport, screen: Option<&ScreenReport>) -> DeviceInfo {
    let key = match &display.uuid {
        Ok(uuid) if !uuid.is_empty() => uuid.clone(),
        _ => format!("display:{}", display.display_id),
    };
    let name = screen
        .map(|s| s.name.clone())
        .filter(|n| !n.is_empty())
        .or_else(|| {
            display
                .colorsync
                .as_ref()
                .and_then(|c| c.description.clone())
                .filter(|n| !n.is_empty())
        })
        .unwrap_or_else(|| format!("Display {}", display.display_id));
    // The display ID is left out: it changes after a reconnect.
    let evidence = vec![
        (
            "vendor_model".into(),
            format!("{:08X}:{:08X}", display.vendor, display.model),
        ),
        ("serial".into(), format!("{:08X}", display.serial)),
        ("builtin".into(), display.builtin.to_string()),
    ];
    DeviceInfo {
        id: DeviceId {
            backend: BACKEND.into(),
            key,
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

fn capabilities(entry: &Entry) -> DeviceCapabilities {
    // Unverified: whether the P3 report follows the panel or the assigned
    // display profile.
    let sdr_wcg = match &entry.screen {
        Ok(s) => known(s.p3, GAMUT),
        Err(reason) => unknown(reason.clone()),
    };
    let icc_profiles = match (&entry.display.uuid, &entry.display.colorsync) {
        (Err(e), _) => unknown(format!("no ColorSync device ID: {e}")),
        (Ok(_), None) => known(false, COLORSYNC_INFO),
        (Ok(_), Some(_)) => unknown(NO_PROFILE_CONTROL),
    };
    DeviceCapabilities {
        hdr: unknown(NO_HDR_REPORT),
        sdr_wcg,
        wire_depths: unknown(NO_WIRE_DEPTH),
        icc_profiles,
    }
}

fn mode(entry: &Entry) -> DeviceMode {
    DeviceMode {
        hdr_on: unknown(NO_HDR_REPORT),
        gamut: unknown(NO_GAMUT),
        wire_depth: unknown(NO_WIRE_DEPTH),
        peak_nits: unknown(NO_LUMINANCE),
        min_nits: unknown(NO_LUMINANCE),
        full_frame_nits: unknown(NO_LUMINANCE),
        sdr_white_nits: unknown(NO_LUMINANCE),
        native_size: native_size(&entry.display.modes),
        edr: match &entry.screen {
            Ok(s) => edr(s),
            Err(reason) => unknown(reason.clone()),
        },
    }
}

/// The size of the mode with the native flag. The current mode is not
/// used: in a scaled mode the framebuffer differs from the panel, so a
/// swap chain of the framebuffer size is resampled. Unverified: whether
/// every panel flags its native mode.
fn native_size(modes: &Field<Vec<ModeReport>>) -> Report<(usize, usize)> {
    let modes = match modes {
        Ok(modes) => modes,
        Err(e) => return unknown(e.to_string()),
    };
    let mut sizes: Vec<(usize, usize)> = modes
        .iter()
        .filter(|m| m.io_flags & MODE_NATIVE_FLAG != 0)
        .map(|m| (m.pixel_width, m.pixel_height))
        .filter(|&(w, h)| w > 0 && h > 0)
        .collect();
    sizes.sort_unstable();
    sizes.dedup();
    match sizes.as_slice() {
        [size] => known(*size, MODES),
        [] => unknown(format!("{MODES}: no mode has the native flag")),
        _ => unknown(format!("{MODES}: {} native mode sizes", sizes.len())),
    }
}

/// SDR white is 1.0, so a finite headroom below 1 is not a valid report.
fn edr(s: &ScreenReport) -> Report<EdrHeadroom> {
    let valid = |v: f64| v.is_finite() && v >= 1.0;
    if !valid(s.edr_current) || !valid(s.edr_potential) {
        return unknown(format!(
            "{EDR}: current {}, potential {}",
            s.edr_current, s.edr_potential
        ));
    }
    let reference = if valid(s.edr_reference) {
        Some(s.edr_reference)
    } else {
        None
    };
    known(
        EdrHeadroom {
            current: s.edr_current,
            potential: s.edr_potential,
            reference,
        },
        EDR,
    )
}

impl<N: MacNative> DisplayDeviceControl for MacDisplay<N> {
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
        Ok(DeviceQuery {
            device: entry.info.clone(),
            capabilities: capabilities(entry),
            mode: mode(entry),
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
#[path = "macos_tests.rs"]
mod tests;
