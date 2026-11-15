//! nitpick: HDR/WCG test pattern launcher and the `QRhi` output driver.
//! `main.rs` starts the QML launcher or the test host.
//!
//! A library, so that integration tests link the Rust side of the cxx-qt
//! bridges: the build script links the generated C++ into every target.

mod delay;
mod display;
mod gui_executor;
mod hdr_metadata;
// The cxx-qt generated constructor boxes the Rust struct; cxx-qt rejects
// attributes on the bridge module itself.
#[allow(clippy::unnecessary_box_returns)]
mod launcher;
mod qrhi_driver;
pub mod readback;
mod session;
mod settings;
mod submission;
pub mod test_host;
