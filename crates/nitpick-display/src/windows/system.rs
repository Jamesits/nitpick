//! `DisplayConfig` and DXGI calls. Neither needs COM initialization or a
//! particular thread.

use std::mem::size_of;

use ::windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_TARGET, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SDR_WHITE_LEVEL,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use ::windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LUID};
use ::windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1, IDXGIOutput6,
};
use ::windows::core::Interface;
use nitpick_core::device::DeviceError;

use super::{
    AdvancedColorInfo, AdvancedColorInfo2, DxgiOutput, OutputDesc1, PathReport, TargetName,
    WindowsNative,
};
use crate::common::{Field, NativeFailure};

/// `DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2`. Not in the
/// `windows` crate metadata yet.
const GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(15);

/// `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` from `wingdi.h` (SDK
/// 10.0.26100). The union of bit fields is one `UINT32`.
#[repr(C)]
#[derive(Default)]
struct AdvancedColorInfo2Packet {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
    _color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

/// `edidIdsValid` in `DISPLAYCONFIG_TARGET_DEVICE_NAME_FLAGS`.
const EDID_IDS_VALID: u32 = 1 << 2;

/// Native `QueryDisplayConfig` path index for "no mode".
const MODE_IDX_INVALID: u32 = 0xffff_ffff;

/// Retries when the topology changes between the size and query calls.
const QUERY_ATTEMPTS: usize = 4;

pub struct System;

fn luid(l: LUID) -> i64 {
    (i64::from(l.HighPart) << 32) | i64::from(l.LowPart)
}

fn wide(s: &[u16]) -> String {
    let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..end])
}

/// Packet types whose first field is the header.
///
/// # Safety
/// `Self` is `repr(C)` and starts with `DISPLAYCONFIG_DEVICE_INFO_HEADER`.
unsafe trait Packet: Default {
    fn header(&mut self) -> &mut DISPLAYCONFIG_DEVICE_INFO_HEADER;
}

macro_rules! packet {
    ($t:ty) => {
        // SAFETY: `wingdi.h` packets start with the header.
        unsafe impl Packet for $t {
            fn header(&mut self) -> &mut DISPLAYCONFIG_DEVICE_INFO_HEADER {
                &mut self.header
            }
        }
    };
}

packet!(DISPLAYCONFIG_SOURCE_DEVICE_NAME);
packet!(DISPLAYCONFIG_TARGET_DEVICE_NAME);
packet!(DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO);
packet!(DISPLAYCONFIG_SDR_WHITE_LEVEL);
packet!(AdvancedColorInfo2Packet);

fn device_info<P: Packet>(
    api: &'static str,
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    adapter: LUID,
    id: u32,
) -> Field<P> {
    let mut packet = P::default();
    let header = packet.header();
    header.r#type = kind;
    header.size = u32::try_from(size_of::<P>()).expect("packet size");
    header.adapterId = adapter;
    header.id = id;
    // SAFETY: the header starts a packet of `header.size` bytes; the
    // pointer covers the whole packet.
    let code = unsafe {
        DisplayConfigGetDeviceInfo(
            std::ptr::from_mut(&mut packet).cast::<DISPLAYCONFIG_DEVICE_INFO_HEADER>(),
        )
    };
    if code == 0 {
        Ok(packet)
    } else {
        Err(NativeFailure {
            api,
            code: i64::from(code),
        })
    }
}

fn query_paths() -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>), DeviceError>
{
    let fail = |api: &'static str, code: u32| -> DeviceError {
        NativeFailure {
            api,
            code: i64::from(code),
        }
        .into()
    };
    for _ in 0..QUERY_ATTEMPTS {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        // SAFETY: both pointers are valid for writes.
        let r = unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &raw mut n_paths, &raw mut n_modes)
        };
        if r != ERROR_SUCCESS {
            return Err(fail("GetDisplayConfigBufferSizes", r.0));
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        // SAFETY: the arrays have the lengths passed in the counts.
        let r = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &raw mut n_paths,
                paths.as_mut_ptr(),
                &raw mut n_modes,
                modes.as_mut_ptr(),
                None,
            )
        };
        if r == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if r != ERROR_SUCCESS {
            return Err(fail("QueryDisplayConfig", r.0));
        }
        paths.truncate(n_paths as usize);
        modes.truncate(n_modes as usize);
        return Ok((paths, modes));
    }
    Err(fail("QueryDisplayConfig", ERROR_INSUFFICIENT_BUFFER.0))
}

fn path_report(path: &DISPLAYCONFIG_PATH_INFO, modes: &[DISPLAYCONFIG_MODE_INFO]) -> PathReport {
    let source = path.sourceInfo;
    let target = path.targetInfo;
    let source_name = device_info::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
        "DisplayConfigGetDeviceInfo(GET_SOURCE_NAME)",
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
        source.adapterId,
        source.id,
    )
    .map(|p| wide(&p.viewGdiDeviceName));
    let target_name = device_info::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
        "DisplayConfigGetDeviceInfo(GET_TARGET_NAME)",
        DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        target.adapterId,
        target.id,
    )
    .map(|p| {
        // SAFETY: every bit pattern is a valid `u32`.
        let flags = unsafe { p.flags.Anonymous.value };
        TargetName {
            friendly_name: wide(&p.monitorFriendlyDeviceName),
            device_path: wide(&p.monitorDevicePath),
            edid_ids: (flags & EDID_IDS_VALID != 0)
                .then_some((p.edidManufactureId, p.edidProductCodeId)),
            connector_instance: p.connectorInstance,
            output_technology: p.outputTechnology.0,
        }
    });
    let color_info = device_info::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
        "DisplayConfigGetDeviceInfo(GET_ADVANCED_COLOR_INFO)",
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
        target.adapterId,
        target.id,
    )
    .map(|p| AdvancedColorInfo {
        // SAFETY: every bit pattern is a valid `u32`.
        flags: unsafe { p.Anonymous.value },
        bits_per_color_channel: p.bitsPerColorChannel,
    });
    let color_info_2 = device_info::<AdvancedColorInfo2Packet>(
        "DisplayConfigGetDeviceInfo(GET_ADVANCED_COLOR_INFO_2)",
        GET_ADVANCED_COLOR_INFO_2,
        target.adapterId,
        target.id,
    )
    .map(|p| AdvancedColorInfo2 {
        flags: p.value,
        bits_per_color_channel: p.bits_per_color_channel,
        active_color_mode: p.active_color_mode,
    });
    let sdr_white_level = device_info::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(
        "DisplayConfigGetDeviceInfo(GET_SDR_WHITE_LEVEL)",
        DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
        target.adapterId,
        target.id,
    )
    .map(|p| p.SDRWhiteLevel);
    // SAFETY: without `QDC_VIRTUAL_MODE_AWARE` the union holds `modeInfoIdx`.
    let mode_idx = unsafe { target.Anonymous.modeInfoIdx };
    let target_active_size = (mode_idx != MODE_IDX_INVALID)
        .then(|| modes.get(mode_idx as usize))
        .flatten()
        .filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_TARGET)
        .map(|m| {
            // SAFETY: `infoType` selects the target mode member.
            let size = unsafe { m.Anonymous.targetMode.targetVideoSignalInfo.activeSize };
            (size.cx, size.cy)
        });
    PathReport {
        adapter_luid: luid(source.adapterId),
        source_id: source.id,
        target_id: target.id,
        source_name,
        target_name,
        color_info,
        color_info_2,
        sdr_white_level,
        target_active_size,
    }
}

fn hresult(api: &'static str, e: &::windows::core::Error) -> NativeFailure {
    NativeFailure {
        api,
        code: i64::from(e.code().0),
    }
}

impl WindowsNative for System {
    fn display_paths(&mut self) -> Result<Vec<PathReport>, DeviceError> {
        let (paths, modes) = query_paths()?;
        Ok(paths.iter().map(|p| path_report(p, &modes)).collect())
    }

    fn dxgi_outputs(&mut self) -> Field<Vec<DxgiOutput>> {
        // SAFETY: no preconditions.
        let factory: IDXGIFactory1 =
            unsafe { CreateDXGIFactory1() }.map_err(|e| hresult("CreateDXGIFactory1", &e))?;
        let mut outputs = Vec::new();
        for a in 0.. {
            // SAFETY: COM call on a valid factory.
            let adapter = match unsafe { factory.EnumAdapters1(a) } {
                Ok(adapter) => adapter,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(hresult("IDXGIFactory1::EnumAdapters1", &e)),
            };
            for o in 0.. {
                // SAFETY: COM call on a valid adapter.
                let output = match unsafe { adapter.EnumOutputs(o) } {
                    Ok(output) => output,
                    Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                    Err(e) => return Err(hresult("IDXGIAdapter1::EnumOutputs", &e)),
                };
                // SAFETY: COM call on a valid output.
                let desc =
                    unsafe { output.GetDesc() }.map_err(|e| hresult("IDXGIOutput::GetDesc", &e))?;
                let desc1 = output
                    .cast::<IDXGIOutput6>()
                    .map_err(|e| hresult("QueryInterface(IDXGIOutput6)", &e))
                    .and_then(|o6| {
                        // SAFETY: COM call on a valid output.
                        unsafe { o6.GetDesc1() }.map_err(|e| hresult("IDXGIOutput6::GetDesc1", &e))
                    })
                    .map(|d| OutputDesc1 {
                        color_space: d.ColorSpace.0,
                        bits_per_color: d.BitsPerColor,
                        min_luminance: d.MinLuminance,
                        max_luminance: d.MaxLuminance,
                        max_full_frame_luminance: d.MaxFullFrameLuminance,
                    });
                outputs.push(DxgiOutput {
                    device_name: wide(&desc.DeviceName),
                    desc1,
                });
            }
        }
        Ok(outputs)
    }
}
