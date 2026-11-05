//! Core library of nitpick: signal encodings and HDR/WCG test pattern generators.
//!
//! All patterns are rendered as 10-bit R'G'B' code values packed for an
//! `R10G10B10A2_UNORM` swap chain. The display side copies them without
//! any conversion.

pub mod image;
pub mod pattern;
pub mod signal;
pub mod text;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
