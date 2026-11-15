//! OS capture of the pattern window for the test host: Windows Graphics
//! Capture on Windows, none elsewhere. See "Platform capture adapters" in
//! `doc/design/output_driver/testing.md`.

use nitpick_test_host::CaptureAdapter;

use crate::display::ffi;

/// The native handle of the open pattern window: the `HWND` on Windows.
///
/// # Errors
/// No pattern window is open.
pub fn pattern_window_handle() -> Result<isize, String> {
    let handle = ffi::pattern_window_handle().map_err(|e| e.what().to_owned())?;
    isize::try_from(handle).map_err(|_| format!("invalid window handle {handle:#x}"))
}

/// The capture adapter of the pattern window. It captures the open window,
/// which shows the acknowledged frame of the open surface; the host checks
/// that the target is that frame.
#[cfg(windows)]
#[must_use]
pub fn adapter() -> Box<dyn CaptureAdapter> {
    Box::new(nitpick_capture::wgc::WgcAdapter::new(Box::new(|_| {
        pattern_window_handle()
    })))
}

#[cfg(not(windows))]
#[must_use]
pub fn adapter() -> Box<dyn CaptureAdapter> {
    Box::new(nitpick_test_host::NoCapture(
        "no OS capture adapter is implemented for the QRhi host on this platform".into(),
    ))
}
