//! Offscreen draw and readback through the draw path of the pattern window
//! (`cpp/pattern_renderer.cpp`): the same texture upload, copy shader and
//! pipeline, into a texture of the swap chain format. See "`QRhi` readback"
//! in `doc/design/output_driver/testing.md`.
//!
//! An offscreen pass checks the draw path only. It does not show the format
//! or colour space of a real swap chain.

#[cxx_qt::bridge]
mod ffi {
    #[namespace = "nitpick"]
    #[repr(u8)]
    enum OffscreenBackend {
        D3d11,
        Vulkan,
        Metal,
    }

    /// The pattern of `width × height` at `origin` in a target of
    /// `target_width × target_height`, all in pixels.
    #[namespace = "nitpick"]
    struct OffscreenFrame {
        fp16: bool,
        width: i32,
        height: i32,
        target_width: i32,
        target_height: i32,
        origin_x: i32,
        origin_y: i32,
    }

    /// `QRhiReadbackResult` of the target, unchanged.
    #[namespace = "nitpick"]
    struct OffscreenReadback {
        data: Vec<u8>,
        width: i32,
        height: i32,
        /// `QRhi::isYUpInFramebuffer`.
        y_up: bool,
    }

    unsafe extern "C++" {
        include!("nitpick-app/cpp/readback.h");

        #[namespace = "nitpick"]
        type OffscreenRenderer;

        #[namespace = "nitpick"]
        #[cxx_name = "createOffscreenRenderer"]
        fn create_offscreen_renderer(
            backend: OffscreenBackend,
            software: bool,
        ) -> Result<UniquePtr<OffscreenRenderer>>;

        #[cxx_name = "backendName"]
        fn backend_name(self: &OffscreenRenderer) -> String;
        #[cxx_name = "deviceName"]
        fn device_name(self: &OffscreenRenderer) -> String;
        #[cxx_name = "isSoftwareDevice"]
        fn is_software_device(self: &OffscreenRenderer) -> bool;

        fn draw(
            self: Pin<&mut OffscreenRenderer>,
            frame: &OffscreenFrame,
            pixels: &[u8],
        ) -> Result<OffscreenReadback>;
    }
}

use cxx::UniquePtr;
use nitpick_core::output::SwapChainFormat;

pub use crate::display::Backend;

/// The device that `QRhi` selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// `QRhi::backendName`, for example `D3D11`.
    pub backend: String,
    /// `QRhiDriverInfo::deviceName`.
    pub name: String,
    /// `QRhiDriverInfo::CpuDevice`, for example WARP or lavapipe.
    pub software: bool,
}

/// Position of the pattern in the render target, as the pattern area in
/// the swap chain of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Pattern size in pixels.
    pub size: (usize, usize),
    /// Render target size in pixels.
    pub target: (usize, usize),
    /// Top-left pixel of the pattern in the target.
    pub origin: (usize, usize),
}

impl Placement {
    /// The pattern fills the target.
    #[must_use]
    pub const fn whole(size: (usize, usize)) -> Self {
        Self {
            size,
            target: size,
            origin: (0, 0),
        }
    }

    fn check(&self) -> Result<(), String> {
        let (w, h) = self.size;
        let (tw, th) = self.target;
        let (x, y) = self.origin;
        let outside = |start: usize, len: usize, limit: usize| {
            start.checked_add(len).is_none_or(|end| end > limit)
        };
        if w == 0 || h == 0 || outside(x, w, tw) || outside(y, h, th) {
            return Err(format!(
                "a {w}×{h} pattern at ({x}, {y}) is not inside a {tw}×{th} target"
            ));
        }
        Ok(())
    }
}

/// A `QRhi` device without a window. Use it on the thread that created it.
pub struct OffscreenDevice {
    renderer: UniquePtr<ffi::OffscreenRenderer>,
}

impl OffscreenDevice {
    /// Creates the device of `backend`. With `software`, `QRhi` prefers a
    /// software device; it uses another device if it finds none, so check
    /// [`DeviceInfo::software`]. Vulkan needs a `QGuiApplication`.
    ///
    /// # Errors
    /// The backend is not built in, or `QRhi` creates no device.
    pub fn new(backend: Backend, software: bool) -> Result<Self, String> {
        let backend = match backend {
            Backend::D3d11 => ffi::OffscreenBackend::D3d11,
            Backend::Vulkan => ffi::OffscreenBackend::Vulkan,
            Backend::Metal => ffi::OffscreenBackend::Metal,
        };
        let renderer =
            ffi::create_offscreen_renderer(backend, software).map_err(|e| e.what().to_owned())?;
        Ok(Self { renderer })
    }

    #[must_use]
    pub fn info(&self) -> DeviceInfo {
        DeviceInfo {
            backend: self.renderer.backend_name(),
            name: self.renderer.device_name(),
            software: self.renderer.is_software_device(),
        }
    }

    /// Draws `pixels` (tightly packed rows of `format`, as the pattern
    /// window gets them) at `placement` and reads the target back. Returns
    /// the target in tightly packed rows from the top-left pixel.
    ///
    /// # Errors
    /// An invalid placement or pixel buffer, a `QRhi` failure, or a
    /// readback of another layout.
    pub fn draw(
        &mut self,
        format: SwapChainFormat,
        pixels: &[u8],
        placement: Placement,
    ) -> Result<Vec<u8>, String> {
        placement.check()?;
        let bpp = format.bytes_per_pixel();
        let (w, h) = placement.size;
        if pixels.len() != w * h * bpp {
            return Err(format!(
                "{} bytes for a {w}×{h} pattern of {bpp} bytes per pixel",
                pixels.len()
            ));
        }
        let int = |v: usize| i32::try_from(v).map_err(|_| format!("size {v} too large"));
        let frame = ffi::OffscreenFrame {
            fp16: format == SwapChainFormat::ScRgb,
            width: int(w)?,
            height: int(h)?,
            target_width: int(placement.target.0)?,
            target_height: int(placement.target.1)?,
            origin_x: int(placement.origin.0)?,
            origin_y: int(placement.origin.1)?,
        };
        let raw = self
            .renderer
            .pin_mut()
            .draw(&frame, pixels)
            .map_err(|e| e.what().to_owned())?;
        if (raw.width, raw.height) != (frame.target_width, frame.target_height) {
            return Err(format!(
                "readback of {}×{} for a {}×{} target",
                raw.width, raw.height, frame.target_width, frame.target_height
            ));
        }
        normalize(raw.data, placement.target, bpp, raw.y_up)
    }
}

/// Target pixel of the copy shader outside the pattern: opaque black.
#[must_use]
pub fn outside_pixel(format: SwapChainFormat) -> &'static [u8] {
    match format {
        // Alpha 3 in the top two bits of the little-endian word.
        SwapChainFormat::Hdr10 => &[0, 0, 0, 0xc0],
        // Binary16 0, 0, 0, 1.0 (0x3c00), little-endian.
        SwapChainFormat::ScRgb => &[0, 0, 0, 0, 0, 0, 0, 0x3c],
    }
}

/// The expected target: `pixels` at the placement, [`outside_pixel`]
/// elsewhere.
///
/// # Errors
/// An invalid placement or pixel buffer.
pub fn expected_target(
    format: SwapChainFormat,
    pixels: &[u8],
    placement: Placement,
) -> Result<Vec<u8>, String> {
    placement.check()?;
    let bpp = format.bytes_per_pixel();
    let (w, h) = placement.size;
    if pixels.len() != w * h * bpp {
        return Err(format!("{} bytes for a {w}×{h} pattern", pixels.len()));
    }
    let (tw, th) = placement.target;
    let (x, y) = placement.origin;
    let mut out = outside_pixel(format).repeat(tw * th);
    for (row, src) in pixels.chunks_exact(w * bpp).enumerate() {
        let start = ((y + row) * tw + x) * bpp;
        out[start..start + w * bpp].copy_from_slice(src);
    }
    Ok(out)
}

/// Tightly packed rows from the top-left pixel.
///
/// **Deviation from a documented layout:** `QRhiReadbackResult` gives no
/// row pitch, and the Qt documentation states none. The pitch is the byte
/// count divided by the height; it must hold a row. Rows run bottom to top
/// only if `y_up` (`QRhi::isYUpInFramebuffer`, true for OpenGL only).
///
/// # Errors
/// The byte count does not divide into `height` rows of at least one
/// tightly packed row each.
pub fn normalize(
    data: Vec<u8>,
    size: (usize, usize),
    bytes_per_pixel: usize,
    y_up: bool,
) -> Result<Vec<u8>, String> {
    let (w, h) = size;
    let row = w * bytes_per_pixel;
    if row == 0 || h == 0 || !data.len().is_multiple_of(h) || data.len() / h < row {
        return Err(format!(
            "{} readback bytes do not form {h} rows of {row} bytes",
            data.len()
        ));
    }
    let pitch = data.len() / h;
    if pitch == row && !y_up {
        return Ok(data);
    }
    let mut out = Vec::with_capacity(row * h);
    let mut push = |r: &[u8]| out.extend_from_slice(&r[..row]);
    if y_up {
        data.chunks_exact(pitch).rev().for_each(&mut push);
    } else {
        data.chunks_exact(pitch).for_each(&mut push);
    }
    Ok(out)
}

#[cfg(test)]
#[path = "readback_tests.rs"]
mod tests;
