//! ITU-R BT.2124 `ΔE_ITP`: `ICtCp` of BT.2100 Table 7 from
//! display-referred linear BT.2020 RGB, with `T = C_T / 2`.

use crate::colour::{Matrix, Nits2020, apply, pq_inverse_eotf};
use crate::{Invalid, finite};

/// BT.2100 linear RGB to LMS; each row has the sum 1.
const RGB_TO_LMS: Matrix = [
    [1688.0 / 4096.0, 2146.0 / 4096.0, 262.0 / 4096.0],
    [683.0 / 4096.0, 2951.0 / 4096.0, 462.0 / 4096.0],
    [99.0 / 4096.0, 309.0 / 4096.0, 3688.0 / 4096.0],
];

/// `ΔE_ITP` per unit of ITP distance.
const SCALE: f64 = 720.0;

/// BT.2124 ITP coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Itp {
    pub i: f64,
    pub t: f64,
    pub p: f64,
}

impl Itp {
    /// Negative RGB components stay negative through the LMS matrix. The
    /// result exists only if each LMS component is at least 0; nothing is
    /// clipped to get one.
    ///
    /// # Errors
    /// [`Invalid::NegativeLms`], or a non-finite value.
    pub fn new(rgb: Nits2020) -> Result<Self, Invalid> {
        let lms = apply(&RGB_TO_LMS, finite(rgb.0)?);
        if lms.iter().any(|&c| c < 0.0) {
            return Err(Invalid::NegativeLms(lms));
        }
        let [l, m, s] = lms;
        let (l, m, s) = (
            pq_inverse_eotf(l)?,
            pq_inverse_eotf(m)?,
            pq_inverse_eotf(s)?,
        );
        let ct = (6610.0 * l - 13613.0 * m + 7003.0 * s) / 4096.0;
        let cp = (17933.0 * l - 17390.0 * m - 543.0 * s) / 4096.0;
        Ok(Self {
            i: 0.5 * l + 0.5 * m,
            t: 0.5 * ct,
            p: cp,
        })
    }

    /// `ΔE_ITP` between two ITP values.
    #[must_use]
    pub fn delta_e(self, other: Self) -> f64 {
        let (di, dt, dp) = (self.i - other.i, self.t - other.t, self.p - other.p);
        SCALE * (di * di + dt * dt + dp * dp).sqrt()
    }
}

/// `ΔE_ITP` between two colours.
///
/// # Errors
/// See [`Itp::new`].
pub fn delta_e_itp(a: Nits2020, b: Nits2020) -> Result<f64, Invalid> {
    Ok(Itp::new(a)?.delta_e(Itp::new(b)?))
}

#[cfg(test)]
#[path = "itp_tests.rs"]
mod tests;
