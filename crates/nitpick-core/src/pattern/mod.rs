//! Test patterns: the catalogue, the resolution-independent model and the
//! generators that build it.

pub mod bt2111;
mod catalogue;
pub mod grayscale;
pub mod layout;
pub mod model;

pub use catalogue::{MIN_SIZE, PatternError, PatternKind};
