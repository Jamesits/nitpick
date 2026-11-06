//! Bridge to the C++ full-screen HDR pattern window (`cpp/pattern_window.cpp`).

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
    /// of the display output. The pattern area is the swap chain without the
    /// safe-area margins of the window.
    #[namespace = "nitpick"]
    struct PatternSurface {
        width: i32,
        height: i32,
        description: String,
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
