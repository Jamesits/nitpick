//! Test-host protocol of nitpick: a test process controls a host through
//! one JSON object per line on standard input and output. The command-line
//! host runs it with the browser driver, the Qt launcher with the `QRhi`
//! driver. See "Host boundary" in `doc/design/output_driver/testing.md`.
//!
//! The crate has no executor, GUI or server dependency.

pub mod artifact;
pub mod capture;
#[cfg(any(test, feature = "contract"))]
pub mod contract;
pub mod controller;
pub mod fixture;
pub mod gate;
pub mod marker;
pub mod protocol;
pub mod region;
#[cfg(any(test, feature = "contract"))]
pub mod replay;

pub use artifact::{ArtifactSink, ArtifactWriter, Artifacts, Environment};
pub use capture::{Capture, CaptureAdapter, CaptureFrames, CaptureTarget, NoCapture};
pub use controller::{Clock, DriverEvent, Exit, Hooks, LocalFuture, TestHost};
pub use fixture::{FixtureFrame, Fixtures};
pub use protocol::{Event, Request};
