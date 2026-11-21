//! nitpick: HDR/WCG test pattern launcher and the `QRhi` output driver.
//! `main.rs` starts the QML launcher or the test host.
//!
//! A library, so that integration tests link the Rust side of the cxx-qt
//! bridges: the build script links the generated C++ into every target.

mod browser;
mod delay;
mod display;
mod gui_executor;
mod hdr_metadata;
// The cxx-qt generated constructor boxes the Rust struct, and the
// generated `showPattern` wrapper has one argument per QML parameter;
// cxx-qt rejects attributes on the bridge module itself.
#[allow(clippy::unnecessary_box_returns, clippy::too_many_arguments)]
mod launcher;
pub mod pattern_capture;
pub mod qrhi_driver;
pub mod readback;
mod session;
mod settings;
mod submission;
pub mod swap_chain_probe;
pub mod test_host;
mod webview;

// For the swap-chain tests, which drive the real window through
// `qrhi_driver`.
pub use display::{Backend, DeviceStates, device_reports};
