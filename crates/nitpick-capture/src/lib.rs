//! OS capture adapters of the nitpick test hosts, for the `CaptureAdapter`
//! interface of `nitpick-test-host`. See "Platform capture adapters" in
//! `doc/design/output_driver/testing.md`.
//!
//! Windows: Windows Graphics Capture of one window into an FP16 frame pool
//! ([`wgc`]). Other platforms have no adapter yet.

pub mod frame;
#[cfg(windows)]
pub mod wgc;
