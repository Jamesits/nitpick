//! HDR10 static metadata of the `QRhi` pattern window: resolution of the
//! request, packing into the native structure of each backend, and the
//! sink that sends it to the swap chain. See "HDR10 static metadata" in
//! `doc/design/output_driver/qt.md`.

use nitpick_core::driver::Hdr10Request;
use nitpick_core::image::Image;
use nitpick_core::metadata::{self, Hdr10Metadata};
use nitpick_core::signal::BitDepth;

use crate::display::{Backend, ffi};

/// cd/m² of linear 1.0 for `CAEDRMetadata`, as in Chromium. Its effect on
/// a PQ layer is not documented.
pub const METAL_OPTICAL_OUTPUT_SCALE: f32 = 100.0;

/// Resolves `request` for `image` at `depth`. Automatic light levels read
/// every code as full-range PQ, for every encoding including HLG scope
/// input: the window sends the codes as full-range PQ.
#[must_use]
pub fn resolve(
    request: Option<&Hdr10Request>,
    image: &Image,
    depth: BitDepth,
) -> Option<Hdr10Metadata> {
    let max = f64::from(depth.max_code());
    request.map(|r| {
        r.resolve(|| {
            metadata::light_level(
                image
                    .pixels()
                    .iter()
                    .map(|p| [p.r, p.g, p.b].map(|c| f64::from(c) / max)),
            )
        })
    })
}

/// Native metadata structure of one backend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NativeMetadata {
    Dxgi(ffi::DxgiHdrMetadata),
    Vulkan(ffi::VulkanHdrMetadata),
    Metal(ffi::MetalHdrMetadata),
}

/// Packs `md` for `backend`. All backends get the values after rounding to
/// the ST 2086 and CTA-861.3 fields, so they receive the same metadata.
#[must_use]
pub fn pack(backend: Backend, md: &Hdr10Metadata) -> NativeMetadata {
    let [r, g, b] = md.primaries;
    let xy = |[x, y]: [f64; 2]| {
        [
            Hdr10Metadata::chromaticity_code(x),
            Hdr10Metadata::chromaticity_code(y),
        ]
    };
    let max_mastering = Hdr10Metadata::luminance_code(md.max_mastering_nits);
    let min_mastering = Hdr10Metadata::luminance_code(md.min_mastering_nits);
    let max_cll = Hdr10Metadata::light_level_code(md.max_cll);
    let max_fall = Hdr10Metadata::light_level_code(md.max_fall);
    match backend {
        Backend::D3d11 => NativeMetadata::Dxgi(ffi::DxgiHdrMetadata {
            red_primary: xy(r),
            green_primary: xy(g),
            blue_primary: xy(b),
            white_point: xy(md.white),
            // Rounded to whole cd/m², half up.
            max_mastering_luminance: max_mastering / 10_000
                + u32::from(max_mastering % 10_000 >= 5_000),
            min_mastering_luminance: min_mastering,
            max_content_light_level: max_cll,
            max_frame_average_light_level: max_fall,
        }),
        Backend::Vulkan => {
            // The narrowing to f32 is the precision of the Vulkan fields.
            #[allow(clippy::cast_possible_truncation)]
            let f = |code: f64, unit: f64| (code / unit) as f32;
            let xy = |c: [f64; 2]| xy(c).map(|v| f(f64::from(v), 50_000.0));
            NativeMetadata::Vulkan(ffi::VulkanHdrMetadata {
                display_primary_red: xy(r),
                display_primary_green: xy(g),
                display_primary_blue: xy(b),
                white_point: xy(md.white),
                max_luminance: f(f64::from(max_mastering), 10_000.0),
                min_luminance: f(f64::from(min_mastering), 10_000.0),
                max_content_light_level: f32::from(max_cll),
                max_frame_average_light_level: f32::from(max_fall),
            })
        }
        Backend::Metal => NativeMetadata::Metal(ffi::MetalHdrMetadata {
            display_info: md.mastering_display_bytes(),
            content_info: md.content_light_level_bytes(),
            optical_output_scale: METAL_OPTICAL_OUTPUT_SCALE,
        }),
    }
}

/// Receiver of native metadata for the swap chain of the open window.
/// Tests inject a recording sink.
pub trait MetadataSink {
    /// # Errors
    /// The reason the native call failed.
    fn set(&mut self, metadata: &NativeMetadata) -> Result<(), String>;

    /// Removes the metadata from the swap chain.
    ///
    /// # Errors
    /// The reason the native call failed or does not exist.
    fn clear(&mut self) -> Result<(), String>;
}

/// The C++ pattern window.
pub struct BridgeSink;

impl MetadataSink for BridgeSink {
    fn set(&mut self, metadata: &NativeMetadata) -> Result<(), String> {
        match metadata {
            NativeMetadata::Dxgi(m) => ffi::set_dxgi_hdr_metadata(m),
            NativeMetadata::Vulkan(m) => ffi::set_vulkan_hdr_metadata(m),
            NativeMetadata::Metal(m) => ffi::set_metal_hdr_metadata(m),
        }
        .map_err(|e| e.to_string())
    }

    fn clear(&mut self) -> Result<(), String> {
        ffi::clear_hdr_metadata().map_err(|e| e.to_string())
    }
}

/// Metadata state of one swap chain. A new swap chain has no metadata.
#[derive(Debug, Default)]
pub struct SwapChainMetadata {
    /// The swap chain can hold metadata from an earlier frame.
    sent: bool,
}

impl SwapChainMetadata {
    /// Sends `metadata` for the next frame, or removes earlier metadata if
    /// it is `None`. Call before the frame is presented. A failed call does
    /// not stop the frame; the status line reports it.
    pub fn apply(
        &mut self,
        sink: &mut impl MetadataSink,
        backend: Backend,
        metadata: Option<&Hdr10Metadata>,
    ) -> String {
        let Some(md) = metadata else {
            if !self.sent {
                return "No HDR metadata.".into();
            }
            return match sink.clear() {
                Ok(()) => {
                    self.sent = false;
                    "HDR metadata removed.".into()
                }
                Err(e) => format!("HDR metadata NOT removed: {e}."),
            };
        };
        // On failure the earlier metadata, if any, stays on the swap chain.
        match sink.set(&pack(backend, md)) {
            Ok(()) => {
                self.sent = true;
                let kind = if backend == Backend::Metal {
                    "EDR HDR10"
                } else {
                    "HDR10"
                };
                format!("{kind} metadata: {}.", describe(md))
            }
            Err(e) => format!("HDR metadata NOT sent: {e}."),
        }
    }
}

/// Status line values after rounding to the ST 2086 and CTA-861.3 fields.
fn describe(md: &Hdr10Metadata) -> String {
    format!(
        "MaxCLL {}, MaxFALL {}, mastering {:.4} to {:.0} cd/m2",
        Hdr10Metadata::light_level_code(md.max_cll),
        Hdr10Metadata::light_level_code(md.max_fall),
        f64::from(Hdr10Metadata::luminance_code(md.min_mastering_nits)) / 10_000.0,
        f64::from(Hdr10Metadata::luminance_code(md.max_mastering_nits)) / 10_000.0,
    )
}

#[cfg(test)]
#[path = "hdr_metadata_tests.rs"]
mod tests;
