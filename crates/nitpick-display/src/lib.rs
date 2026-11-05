//! Native implementations of [`DisplayDeviceControl`] for Windows, macOS
//! and Linux. See `doc/design/display_device.md`.
//!
//! Each platform module maps native reports to the core types through an
//! injected native-call trait. The mapping compiles on every OS, so its
//! tests run without the platform or a display. Only the module that calls
//! the OS is limited to its target.

use nitpick_core::device::DisplayDeviceControl;

pub mod common;
pub mod recovery;
pub mod windows;

/// The backend for the target OS.
///
/// macOS and Linux have no backend yet; on them, and on other targets,
/// every call fails with [`nitpick_core::device::DeviceError::Unsupported`].
#[must_use]
pub fn native() -> Box<dyn DisplayDeviceControl + Send> {
    #[cfg(windows)]
    return Box::new(windows::WindowsDisplay::new());
    #[cfg(not(windows))]
    return Box::new(common::Unavailable::new(format!(
        "no display backend for {}",
        std::env::consts::OS
    )));
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
