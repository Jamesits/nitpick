//! Read of the display settings outside the profile assignment: the GDI
//! gamma ramp and the Windows calibration loader. GPU vendor controls
//! (gamut clamp, dithering) are not read.

use nitpick_core::device::{DeviceError, DeviceId, DisplaySetup, GammaRamp, Report};

use super::{WindowsDisplay, WindowsNative};
use crate::common::{fnv1a64_bytes, resolve};

/// Entries of a GDI gamma ramp: 256 each of red, green and blue.
pub const RAMP_LEN: usize = 3 * 256;

const GAMMA_RAMP: &str = "GetDeviceGammaRamp";
const CALIBRATION: &str = "WcsGetCalibrationManagementState";

const NO_EXTERNAL_LOADER: &str = "external calibration loaders are not detected; a ramp that \
    they load shows in the gamma ramp, and a later load shows as a gamma ramp change";
const NO_VENDOR_CONTROL: &str = "GPU vendor controls (NVAPI, AMD ADL, Intel IGCL) are not read";

/// Native calls for [`DisplaySetup`]. Read-only.
pub trait WindowsSetupNative {
    /// `GetDeviceGammaRamp` of the GDI display `gdi_name`, red then green
    /// then blue.
    ///
    /// # Errors
    /// No device context for the display, or a native failure.
    fn gamma_ramp(&mut self, gdi_name: &str) -> Result<Vec<u16>, DeviceError>;
    /// `WcsGetCalibrationManagementState`: Windows loads the `vcgt` of the
    /// default profile into the gamma ramp. One setting for all displays.
    ///
    /// # Errors
    /// Native failure.
    fn calibration_management(&mut self) -> Result<bool, DeviceError>;
}

/// The two identity forms: `i * 257` (full scale) and `i * 256` (the high
/// byte only). **Unverified:** which form each driver gives without a
/// loaded ramp; another form reads as a custom ramp.
fn is_identity(channel: &[u16]) -> bool {
    let full = channel.iter().zip(0u16..).all(|(&e, i)| e == i * 257);
    let high_byte = channel.iter().zip(0u16..).all(|(&e, i)| e == i << 8);
    full || high_byte
}

/// The digest is over the little-endian entries, so it does not depend on
/// the host byte order.
pub(super) fn ramp(entries: &[u16]) -> Result<GammaRamp, String> {
    if entries.len() != RAMP_LEN {
        return Err(format!(
            "{GAMMA_RAMP} gave {} entries, expected {RAMP_LEN}",
            entries.len()
        ));
    }
    if entries.chunks(RAMP_LEN / 3).all(is_identity) {
        return Ok(GammaRamp::Identity);
    }
    let bytes: Vec<u8> = entries.iter().flat_map(|e| e.to_le_bytes()).collect();
    Ok(GammaRamp::Custom {
        digest: fnv1a64_bytes(&bytes),
    })
}

impl<N: WindowsNative + WindowsSetupNative> WindowsDisplay<N> {
    pub(super) fn setup_of(&mut self, device: &DeviceId) -> Result<DisplaySetup, DeviceError> {
        let snapshot = self.snapshot()?;
        let entry = resolve(&snapshot.entries, device, |e| &e.info)?;
        // **Unverified:** whether the GDI ramp applies while Advanced Color
        // is active. The field records the read value in every mode.
        let gamma_ramp = match &entry.path.source_name {
            Ok(name) => match self.native.gamma_ramp(name).map_err(|e| e.to_string()) {
                Ok(entries) => {
                    ramp(&entries).map_or_else(Report::unknown, |r| Report::known(r, GAMMA_RAMP))
                }
                Err(reason) => Report::unknown(reason),
            },
            Err(e) => Report::unknown(format!("source name: {e}")),
        };
        let os_calibration_loader = match self.native.calibration_management() {
            Ok(on) => Report::known(on, CALIBRATION),
            Err(e) => Report::unknown(e.to_string()),
        };
        Ok(DisplaySetup {
            device: device.clone(),
            gamma_ramp,
            os_calibration_loader,
            external_calibration_loader: Report::unknown(NO_EXTERNAL_LOADER),
            gpu_gamut_clamp: Report::unknown(NO_VENDOR_CONTROL),
            dithering: Report::unknown(NO_VENDOR_CONTROL),
        })
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
