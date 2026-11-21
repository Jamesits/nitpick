//! Wire and SDR transfer functions with the clipping rules of the model.

use nitpick_compare::colour::{self, PQ_PEAK_NITS};

use crate::{Clip, Sdr};

/// SDR transfer functions. The model does not select one for a runner: the
/// SDR wire curve of an OS and display mode is a measured or stated fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdrTransfer {
    /// IEC 61966-2-1 piecewise curve.
    Srgb,
    /// Pure power 2.2.
    Gamma22,
}

// IEC 61966-2-1. The two knees are the published values; they do not map
// exactly to each other, so the curves differ by up to about 1e-8 near them.
const SRGB_DECODE_KNEE: f64 = 0.040_45;
const SRGB_ENCODE_KNEE: f64 = 0.003_130_8;
const GAMMA: f64 = 2.2;

impl SdrTransfer {
    /// Signal in 0..1 to linear value relative to white.
    #[must_use]
    pub fn decode(self, s: f64) -> f64 {
        match self {
            Self::Srgb if s <= SRGB_DECODE_KNEE => s / 12.92,
            Self::Srgb => ((s + 0.055) / 1.055).powf(2.4),
            Self::Gamma22 => s.powf(GAMMA),
        }
    }

    /// Linear value relative to white, in 0..1, to signal.
    #[must_use]
    pub fn encode(self, v: f64) -> f64 {
        match self {
            Self::Srgb if v <= SRGB_ENCODE_KNEE => 12.92 * v,
            Self::Srgb => 1.055 * v.powf(1.0 / 2.4) - 0.055,
            Self::Gamma22 => v.powf(1.0 / GAMMA),
        }
    }
}

/// PQ signal of a linear wire value in cd/m².
///
/// **Deviation from ST 2084:** a value at or below 0 gives signal 0. The
/// inverse EOTF of 0 is `c1^m2`, about 7.3e-7, so the model is not
/// continuous at 0. The step is below 0.05 of a 16-bit code, so it changes
/// no code.
pub(crate) fn encode_pq(nits: f64) -> (f64, Clip) {
    if nits <= 0.0 {
        let clip = if nits < 0.0 { Clip::Low } else { Clip::None };
        (0.0, clip)
    } else if nits > PQ_PEAK_NITS {
        (1.0, Clip::High)
    } else {
        let s = colour::pq_inverse_eotf(nits).expect("PQ inverse EOTF of a value in 0..=10000");
        (s, Clip::None)
    }
}

/// SDR signal of a linear wire value in cd/m², relative to the SDR white.
pub(crate) fn encode_sdr(sdr: Sdr, nits: f64) -> (f64, Clip) {
    let v = nits / sdr.white_nits;
    if v < 0.0 {
        (0.0, Clip::Low)
    } else if v > 1.0 {
        (1.0, Clip::High)
    } else {
        (sdr.transfer.encode(v), Clip::None)
    }
}

#[cfg(test)]
#[path = "transfer_tests.rs"]
mod tests;
