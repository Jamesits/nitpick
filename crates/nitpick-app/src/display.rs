//! Bridge to the C++ full-screen HDR10 pattern window (`cpp/pattern_window.cpp`).

#[cxx_qt::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("nitpick-app/cpp/pattern_window.h");

        /// Native pixel size of screen `screen` (index into `QGuiApplication::screens()`).
        #[namespace = "nitpick"]
        #[cxx_name = "screenPixelWidth"]
        fn screen_pixel_width(screen: i32) -> Result<i32>;
        #[namespace = "nitpick"]
        #[cxx_name = "screenPixelHeight"]
        fn screen_pixel_height(screen: i32) -> Result<i32>;

        /// Shows packed `R10G10B10A2` pixels full screen on `screen` through an
        /// HDR10 swap chain, replacing any open pattern window. Returns a
        /// description of the display output.
        #[namespace = "nitpick"]
        #[cxx_name = "showPattern"]
        fn show_pattern(pixels: &[u32], width: i32, height: i32, screen: i32) -> Result<String>;
    }
}
