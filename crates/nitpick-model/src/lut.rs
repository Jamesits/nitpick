//! Channel LUTs in the encoded wire domain.

use crate::{ModelError, finite};

/// Fewest entries: one entry has no defined interpolation in this model.
pub const MIN_LUT_LEN: usize = 2;
/// Most entries, as for an MHC2 tag and the IDD gamma ramp.
pub const MAX_LUT_LEN: usize = 4096;

/// Red, green and blue LUTs of equal length. Entry `k` of `n` is the
/// output for input `k / (n − 1)`. Entries can be outside 0..1; the model
/// clamps the result, not the entries.
#[derive(Debug, Clone, PartialEq)]
pub struct Lut {
    channels: [Vec<f64>; 3],
    /// `n − 1`.
    last: f64,
}

impl Lut {
    /// # Errors
    /// Channels of different lengths, a length outside [`MIN_LUT_LEN`] to
    /// [`MAX_LUT_LEN`], or a non-finite entry.
    pub fn new(channels: [Vec<f64>; 3]) -> Result<Self, ModelError> {
        let n = channels[0].len();
        if channels.iter().any(|c| c.len() != n) {
            return Err(ModelError::LutChannels);
        }
        if !(MIN_LUT_LEN..=MAX_LUT_LEN).contains(&n) {
            return Err(ModelError::LutLength(n));
        }
        for c in &channels {
            finite(c)?;
        }
        let last = u16::try_from(n - 1).map_err(|_| ModelError::LutLength(n))?;
        Ok(Self {
            channels,
            last: f64::from(last),
        })
    }

    #[must_use]
    pub fn channels(&self) -> &[Vec<f64>; 3] {
        &self.channels
    }

    /// Linear interpolation between the two entries next to each input.
    /// An input outside 0..1 is clamped to it.
    #[must_use]
    pub fn apply(&self, x: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|k| self.eval(&self.channels[k], x[k]))
    }

    fn eval(&self, entries: &[f64], x: f64) -> f64 {
        let pos = x.clamp(0.0, 1.0) * self.last;
        // `pos` is in 0..=4095, so the index and its float are exact.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let i = (pos.floor() as usize).min(entries.len() - 2);
        #[allow(clippy::cast_precision_loss)]
        let frac = pos - i as f64;
        entries[i] + frac * (entries[i + 1] - entries[i])
    }
}

#[cfg(test)]
#[path = "lut_tests.rs"]
mod tests;
