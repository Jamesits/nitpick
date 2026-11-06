//! Browser output of nitpick: an HTTP and WebSocket service on its own
//! Tokio thread and a static web client. See
//! `doc/design/output_driver/browser.md`.
//!
//! This crate must not depend on Qt, Tauri or `nitpick-app`; the
//! command-line host builds it without a window system.

pub mod encode;
pub mod service;

pub use encode::{Clipping, EncodeError, EncodedFrame, PngSamples, encode_frame};
pub use service::{
    BindHost, BrowserService, ClientState, ServiceConfig, ServiceEvent, ServiceEvents, StartError,
};
