//! Native implementations of [`DisplayDeviceControl`] for Windows, macOS
//! and Linux. See `doc/design/display_device.md`.
//!
//! Each platform module maps native reports to the core types through an
//! injected native-call trait. The mapping compiles on every OS, so its
//! tests run without the platform or a display. Only the module that calls
//! the OS is limited to its target.

use nitpick_core::device::DisplayDeviceControl;

pub mod common;
pub mod macos;
pub mod recovery;
pub mod windows;

/// The backend for the target OS.
///
/// Linux has no backend yet; on it, and on other targets, every call fails
/// with [`nitpick_core::device::DeviceError::Unsupported`].
#[must_use]
pub fn native() -> Box<dyn DisplayDeviceControl + Send> {
    #[cfg(windows)]
    return Box::new(windows::WindowsDisplay::new());
    #[cfg(target_os = "macos")]
    return Box::new(macos::MacDisplay::new());
    #[cfg(not(any(windows, target_os = "macos")))]
    return Box::new(common::Unavailable::new(format!(
        "no display backend for {}",
        std::env::consts::OS
    )));
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
