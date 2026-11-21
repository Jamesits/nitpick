//! GDI gamma ramp and `mscms.dll` calibration state reads.

use std::sync::OnceLock;

use ::windows::Win32::Graphics::Gdi::{CreateDCW, DeleteDC};
use ::windows::Win32::System::LibraryLoader::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW};
use ::windows::Win32::UI::ColorSystem::GetDeviceGammaRamp;
use ::windows::core::{BOOL, HSTRING, PCWSTR, s, w};
use nitpick_core::device::DeviceError;

use super::System;
use super::profile::{check_bool, proc};
use crate::common::unsupported;
use crate::windows::{RAMP_LEN, WindowsSetupNative};

type CalibrationStateFn = unsafe extern "system" fn(*mut BOOL) -> BOOL;

static CALIBRATION_STATE: OnceLock<Result<CalibrationStateFn, String>> = OnceLock::new();

/// Resolved at run time, as every `mscms.dll` function of this backend.
fn calibration_state() -> Result<CalibrationStateFn, DeviceError> {
    let load = || {
        // SAFETY: loads a system DLL by name from System32 only. The module
        // is never freed, so the function pointer stays valid.
        let module = unsafe { LoadLibraryExW(w!("mscms.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) }
            .map_err(|e| format!("LoadLibraryExW(mscms.dll): {e}"))?;
        // SAFETY: the type is the `icm.h` signature of the export.
        unsafe { proc::<CalibrationStateFn>(module, s!("WcsGetCalibrationManagementState")) }
    };
    CALIBRATION_STATE
        .get_or_init(load)
        .clone()
        .map_err(|reason| unsupported("calibration state", reason))
}

impl WindowsSetupNative for System {
    fn gamma_ramp(&mut self, gdi_name: &str) -> Result<Vec<u16>, DeviceError> {
        let name = HSTRING::from(gdi_name);
        // SAFETY: `name` outlives the call; no port or mode.
        let dc = unsafe { CreateDCW(w!("DISPLAY"), &name, PCWSTR::null(), None) };
        if dc.is_invalid() {
            return Err(DeviceError::Native {
                api: "CreateDCW".into(),
                code: None,
                message: format!("no device context for {gdi_name}"),
            });
        }
        let mut ramp = vec![0u16; RAMP_LEN];
        // SAFETY: `ramp` holds the 3 x 256 entries that the function writes.
        let ok = unsafe { GetDeviceGammaRamp(dc, ramp.as_mut_ptr().cast()) };
        // SAFETY: `dc` came from `CreateDCW` and is not used after this.
        unsafe {
            let _ = DeleteDC(dc);
        }
        // The reference states no `GetLastError` code for a failure.
        if !ok.as_bool() {
            return Err(DeviceError::Native {
                api: "GetDeviceGammaRamp".into(),
                code: None,
                message: format!("no gamma ramp for {gdi_name}"),
            });
        }
        Ok(ramp)
    }

    fn calibration_management(&mut self) -> Result<bool, DeviceError> {
        let f = calibration_state()?;
        let mut on = BOOL(0);
        // SAFETY: `on` is valid for a write.
        let ok = unsafe { f(&raw mut on) };
        check_bool("WcsGetCalibrationManagementState", ok)?;
        Ok(on.as_bool())
    }
}
