//! Bridge to the C++ full-screen HDR pattern window (`cpp/pattern_window.cpp`)
//! and the `QRhi` output report: native device state from
//! `DisplayDeviceControl`, combined with the swap chain path rules. See
//! `doc/design/output_driver/qt.md`.

#[cxx_qt::bridge]
pub mod ffi {
    /// HDR10 static metadata in SMPTE ST 2086 and CTA-861.3 units
    /// (see `nitpick_core::metadata::Hdr10Metadata`).
    #[namespace = "nitpick"]
    struct HdrMetadata {
        /// If false, the window clears the metadata of the swap chain.
        enabled: bool,
        /// CIE xy of red, green, blue and white in units of 0.00002.
        chromaticity: [u16; 8],
        /// Mastering display luminance in units of 0.0001 cd/m².
        max_mastering: u32,
        min_mastering: u32,
        /// Content light levels in cd/m².
        max_cll: u16,
        max_fall: u16,
        /// The same values as big-endian SEI payloads, for `CAEDRMetadata` (macOS).
        mastering_display: [u8; 24],
        content_light_level: [u8; 4],
    }

    /// The open pattern window: pattern area size in pixels and a description
    /// of the swap chain. The pattern area is the swap chain without the
    /// safe-area margins of the window.
    #[namespace = "nitpick"]
    struct PatternSurface {
        /// False if QRhi rejected the swap chain format for this display.
        /// No window is then open, the size is 0 and `description` gives
        /// the reason. The window does not fall back to SDR.
        format_supported: bool,
        width: i32,
        height: i32,
        description: String,
    }

    /// Native identity of a Qt screen. Fields of other platforms are empty
    /// or 0.
    #[namespace = "nitpick"]
    struct ScreenIdentity {
        /// `QScreen::name()`.
        name: String,
        /// Windows: `MONITORINFOEXW::szDevice`, for example `\\.\DISPLAY1`.
        gdi_name: String,
        /// macOS: `CGDirectDisplayID` (`NSScreenNumber`).
        display_id: u32,
    }

    unsafe extern "C++" {
        include!("nitpick-app/cpp/pattern_window.h");

        /// Pixel size of the pattern area of a full-screen window on screen
        /// `screen` (index into `QGuiApplication::screens()`): the native size
        /// without the safe-area insets.
        #[namespace = "nitpick"]
        #[cxx_name = "screenPixelWidth"]
        fn screen_pixel_width(screen: i32) -> Result<i32>;
        #[namespace = "nitpick"]
        #[cxx_name = "screenPixelHeight"]
        fn screen_pixel_height(screen: i32) -> Result<i32>;

        #[namespace = "nitpick"]
        #[cxx_name = "screenIdentity"]
        fn screen_identity(screen: i32) -> Result<ScreenIdentity>;

        /// Opens a full-screen window on `screen` with an HDR10 swap chain, or
        /// an FP16 scRGB swap chain if `fp16`, and replaces any open pattern
        /// window. Blocks until the swap chain exists.
        #[namespace = "nitpick"]
        #[cxx_name = "openPatternWindow"]
        fn open_pattern_window(screen: i32, fp16: bool) -> Result<PatternSurface>;

        /// Shows `pixels` in the open window: tightly packed rows in the swap
        /// chain format, `width × height` equal to the pattern area size. Sets
        /// the HDR metadata first. Returns a status line.
        #[namespace = "nitpick"]
        #[cxx_name = "presentPattern"]
        fn present_pattern(
            pixels: &[u8],
            width: i32,
            height: i32,
            metadata: &HdrMetadata,
        ) -> Result<String>;
    }
}

use nitpick_core::device::{DeviceQuery, Report};
use nitpick_core::driver::{
    Capabilities, Conversion, DepthSupport, DisplayId, EncodingSupport, ModeSettings, OutputError,
    Query, Support, Surface,
};
use nitpick_core::output::SwapChainFormat;
use nitpick_core::signal::{BitDepth, Encoding};

pub const DRIVER: &str = "qrhi";

/// `QRhi` backend of the pattern window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    D3d11,
    Metal,
    Vulkan,
}

impl Backend {
    /// The backend that `pattern_window.cpp` selects for the target OS.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::D3d11
        } else if cfg!(target_os = "macos") {
            Self::Metal
        } else {
            Self::Vulkan
        }
    }
}

/// Native device report of a screen, or the reason it is unavailable.
pub type DeviceReport = Result<DeviceQuery, String>;

/// Reason for the macOS 12-bit rejection. Qt's linear HDR path on Metal
/// has 1.0 = the current SDR white, whose luminance is not reported.
pub const NO_FP16_SCALE: &str = "absolute FP16 luminance scale unavailable";

/// Device state of `screen` through `DisplayDeviceControl`. Does native
/// I/O on the calling thread.
pub fn device_report(screen: i32) -> DeviceReport {
    let identity = ffi::screen_identity(screen).map_err(|e| e.to_string())?;
    native_report(&identity)
}

#[cfg(windows)]
fn native_report(identity: &ffi::ScreenIdentity) -> DeviceReport {
    use nitpick_core::device::DisplayDeviceControl;
    let mut backend = nitpick_display::windows::WindowsDisplay::new();
    backend
        .device_for_gdi_name(&identity.gdi_name)
        .and_then(|id| backend.query_device(&id))
        .map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn native_report(identity: &ffi::ScreenIdentity) -> DeviceReport {
    use nitpick_core::device::DisplayDeviceControl;
    let mut backend = nitpick_display::macos::MacDisplay::new();
    backend
        .device_for_display_id(identity.display_id)
        .and_then(|id| backend.query_device(&id))
        .map_err(|e| e.to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn native_report(identity: &ffi::ScreenIdentity) -> DeviceReport {
    Err(format!(
        "no device mapping for screen {} on this platform",
        identity.name
    ))
}

/// Output query of a screen with the device report `device`. The device
/// gives the physical fields; the backend gives the path rules.
#[must_use]
pub fn query(backend: Backend, device: &DeviceReport) -> Query {
    Query {
        capabilities: capabilities(backend, device),
        mode: mode(device),
    }
}

/// `QRhi` path rules of the feature matrix in the output driver design.
/// The window checks swap chain support itself when it opens.
#[must_use]
pub fn capabilities(backend: Backend, device: &DeviceReport) -> Capabilities {
    let depth = |d: BitDepth| {
        let support = match SwapChainFormat::for_depth(d) {
            SwapChainFormat::Hdr10 => DepthSupport {
                available: available(backend, d, device),
                path: "HDR10 swap chain".into(),
                conversion: Conversion::Direct,
                exact: exact(d, device, None),
            },
            SwapChainFormat::ScRgb => DepthSupport {
                available: available(backend, d, device),
                path: "FP16 scRGB swap chain, 1.0 = 80 cd/m2".into(),
                conversion: Conversion::ScRgb80,
                exact: exact(d, device, Some("FP16 cannot carry the levels marked *")),
            },
        };
        (d, support)
    };
    let encoding = |e: Encoding| {
        let interpretation = match e {
            Encoding::PqFull => Support::Yes,
            Encoding::PqNarrow => Support::No("codes are sent as full-range PQ".into()),
            Encoding::HlgNarrow => {
                Support::No("scope use only: codes are sent as full-range PQ".into())
            }
        };
        let support = EncodingSupport {
            input: Support::Yes,
            interpretation,
            auto_light_level: Support::Yes,
        };
        (e, support)
    };
    let hdr_metadata = match backend {
        Backend::D3d11 | Backend::Metal => Support::Yes,
        Backend::Vulkan => Support::Unknown(
            "needs VK_EXT_hdr_metadata; the status line reports it after the pattern is shown"
                .into(),
        ),
    };
    Capabilities {
        depths: BitDepth::ALL.into_iter().map(depth).collect(),
        encodings: Encoding::ALL.into_iter().map(encoding).collect(),
        hdr_metadata,
        pixel_exact: Support::Yes,
    }
}

fn available(backend: Backend, depth: BitDepth, device: &DeviceReport) -> Support {
    let fp16 = SwapChainFormat::for_depth(depth) == SwapChainFormat::ScRgb;
    if fp16 && backend == Backend::Metal {
        return Support::No(NO_FP16_SCALE.into());
    }
    // QRhi D3D11 offers both HDR formats only for an output in the DXGI PQ
    // colour space (Qt 6.12 `QDxgiHdrInfo::isHdrCapable`). SDR WCG is not
    // that colour space.
    if backend == Backend::D3d11
        && let Ok(q) = device
        && let Report::Known {
            value: false,
            source,
        } = &q.mode.hdr_on
    {
        return Support::No(format!(
            "Windows HDR is off ({source}); D3D11 has HDR swap chains only with HDR on"
        ));
    }
    if fp16 && backend == Backend::Vulkan {
        return Support::Unknown(
            "the compositor scale of the extended-linear sRGB swap chain is unverified".into(),
        );
    }
    Support::Yes
}

/// Never `Yes`: no backend reports calibration, dithering or compositor
/// changes. A known cause of changed codes gives `No`.
fn exact(depth: BitDepth, device: &DeviceReport, path_limit: Option<&str>) -> Support {
    let mut no: Vec<String> = path_limit.map(String::from).into_iter().collect();
    let unknown = match device {
        Ok(q) => {
            if let Report::Known {
                value: false,
                source,
            } = &q.capabilities.hdr
            {
                no.push(format!("the display reports no HDR support ({source})"));
            }
            if let Report::Known {
                value: false,
                source,
            } = &q.mode.hdr_on
            {
                no.push(format!("HDR is off ({source}): the OS converts the signal"));
            }
            if let Report::Known { value, source } = &q.mode.wire_depth
                && u32::from(*value) < depth.bits()
            {
                no.push(format!(
                    "{value}-bit wire ({source}): the GPU dithers or truncates the codes"
                ));
            }
            "calibration, dithering and compositor changes are not reported".to_owned()
        }
        Err(reason) => format!("display state unknown: {reason}"),
    };
    if no.is_empty() {
        Support::Unknown(unknown)
    } else {
        Support::No(no.join("; "))
    }
}

/// Physical fields come only from the device mode. Advanced Color, SDR
/// WCG, a wide gamut or a GPU colour conversion do not set `hdr_on`.
#[must_use]
pub fn mode(device: &DeviceReport) -> ModeSettings {
    let description = describe(device);
    // The swap chain has one pixel per display pixel.
    let pixel_ratio = 1.0;
    let Ok(q) = device else {
        return ModeSettings {
            hdr_on: None,
            gamut: None,
            wire_depth: None,
            peak_nits: None,
            min_nits: None,
            sdr_white_nits: None,
            pixel_ratio,
            description,
        };
    };
    let m = &q.mode;
    ModeSettings {
        hdr_on: m.hdr_on.value().copied(),
        gamut: m.gamut.value().copied(),
        wire_depth: m.wire_depth.value().copied(),
        peak_nits: m.peak_nits.value().copied(),
        min_nits: m.min_nits.value().copied(),
        sdr_white_nits: m.sdr_white_nits.value().copied(),
        pixel_ratio,
        description,
    }
}

/// Status line text: the HDR state and the known fields.
fn describe(device: &DeviceReport) -> String {
    let q = match device {
        Ok(q) => q,
        Err(reason) => return format!("display state unknown: {reason}"),
    };
    let m = &q.mode;
    let mut parts = vec![
        match m.hdr_on.value() {
            Some(true) => "HDR on",
            Some(false) => "HDR off",
            None => "HDR state unknown",
        }
        .to_owned(),
    ];
    if let Some(bits) = m.wire_depth.value() {
        parts.push(format!("{bits} bpc"));
    }
    if let Some(v) = m.peak_nits.value() {
        parts.push(format!("peak {v:.0} cd/m2"));
    }
    if let Some(v) = m.min_nits.value() {
        parts.push(format!("min {v:.4} cd/m2"));
    }
    if let Some(v) = m.sdr_white_nits.value() {
        parts.push(format!("SDR white {v:.0} cd/m2"));
    }
    if let Some(e) = m.edr.value() {
        parts.push(format!(
            "EDR headroom {:.2} (potential {:.2})",
            e.current, e.potential
        ));
    }
    format!("{}: {}", q.device.name, parts.join(", "))
}

/// The key is the Qt screen index; it changes when screens are added or
/// removed.
#[must_use]
pub fn display_id(screen: i32) -> DisplayId {
    DisplayId {
        driver: DRIVER,
        key: screen.to_string(),
    }
}

/// Surface of an opened pattern window.
///
/// # Errors
/// [`OutputError::Unsupported`] if `QRhi` rejected the swap chain format;
/// [`OutputError::Platform`] for a negative size.
pub fn surface(
    display: DisplayId,
    depth: BitDepth,
    s: &ffi::PatternSurface,
) -> Result<Surface, OutputError> {
    if !s.format_supported {
        return Err(OutputError::Unsupported {
            feature: depth.label().into(),
            reason: s.description.clone(),
        });
    }
    let size = |v: i32| {
        usize::try_from(v).map_err(|_| OutputError::Platform(format!("invalid pattern size {v}")))
    };
    Ok(Surface {
        display,
        size: (size(s.width)?, size(s.height)?),
        description: s.description.clone(),
    })
}

#[cfg(test)]
#[path = "display_tests.rs"]
mod tests;
