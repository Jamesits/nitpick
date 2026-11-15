//! Comparison of output samples with expected values: signed component
//! errors, the ITU-R BT.2124 colour difference `ΔE_ITP` and fixed
//! per-case limits.
//!
//! The crate has its own transfer functions and primary matrices. It does
//! not use the conversion code of the output drivers, so one error cannot
//! appear on both sides of a comparison.
//!
//! Nothing here clips a value. A value outside the domain of a step is an
//! [`Invalid`] result.

use std::fmt;

pub mod check;
pub mod colour;
pub mod itp;
pub mod stats;

pub use check::{Expectation, Failure, PERCEPTUAL_TARGET, PatchReport, Verdict, check_patch};
pub use colour::{HlgDisplay, Nits2020, Space};
pub use itp::{Itp, delta_e_itp};
pub use stats::ErrorStats;

/// A value that a conversion or the metric cannot use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Invalid {
    /// NaN or an infinity.
    NonFinite(f64),
    /// A non-linear signal outside 0 to 1.
    Signal(f64),
    /// A negative linear value where the step needs at least 0.
    Negative(f64),
    /// A linear LMS component below 0. The PQ inverse EOTF of the metric
    /// has no value for it.
    NegativeLms([f64; 3]),
    /// A negative or non-finite component bound.
    Bound(f64),
    /// HLG display parameters outside their domain; the text names the rule.
    HlgDisplay(&'static str),
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite(v) => write!(f, "the value {v} is not finite"),
            Self::Signal(v) => write!(f, "the signal {v} is outside 0 to 1"),
            Self::Negative(v) => write!(f, "the linear value {v} is negative"),
            Self::NegativeLms([l, m, s]) => write!(
                f,
                "LMS ({l}, {m}, {s}) cd/m² has a negative component, outside the metric domain"
            ),
            Self::Bound(v) => write!(f, "the bound {v} is negative or not finite"),
            Self::HlgDisplay(rule) => write!(f, "HLG display: {rule}"),
        }
    }
}

impl std::error::Error for Invalid {}

/// Every component of `v` is finite.
///
/// # Errors
/// The first non-finite component.
pub fn finite(v: [f64; 3]) -> Result<[f64; 3], Invalid> {
    match v.iter().find(|c| !c.is_finite()) {
        Some(&c) => Err(Invalid::NonFinite(c)),
        None => Ok(v),
    }
}
