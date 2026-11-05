//! Core library of nitpick: signal encodings, HDR/WCG test pattern
//! generators, swap chain pixel conversion and HDR10 metadata.
//!
//! Patterns are rendered as 10-bit or 12-bit R'G'B' code values.
//! [`output`] converts them to swap chain pixels.

pub mod driver;
pub mod image;
pub mod metadata;
pub mod output;
pub mod pattern;
pub mod signal;
pub mod text;

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
