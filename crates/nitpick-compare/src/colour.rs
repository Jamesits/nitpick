//! Transfer functions and primary matrices of the reference.
//!
//! Sources: SMPTE ST 2084 (PQ), ITU-R BT.2100 Table 5 (HLG reference
//! EOTF), SMPTE RP 177 (normalized primary matrix) with the BT.709 and
//! BT.2020 primaries and D65. The scRGB scale is the Windows convention,
//! 1.0 = 80 cd/m², BT.709 primaries.

use std::sync::LazyLock;

use crate::{Invalid, finite};

/// 3×3 matrix as rows; it multiplies column vectors.
pub type Matrix = [[f64; 3]; 3];

/// CIE xy chromaticities of red, green and blue.
pub const BT709: [[f64; 2]; 3] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]];
pub const BT2020: [[f64; 2]; 3] = [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]];
pub const D65: [f64; 2] = [0.3127, 0.3290];

/// cd/m² of scRGB 1.0.
pub const SCRGB_WHITE_NITS: f64 = 80.0;
/// cd/m² of PQ signal 1.0.
pub const PQ_PEAK_NITS: f64 = 10000.0;

// ST 2084 constants; each is exact in binary64.
const M1: f64 = 2610.0 / 16384.0;
const M2: f64 = 2523.0 / 4096.0 * 128.0;
const C1: f64 = 3424.0 / 4096.0;
const C2: f64 = 2413.0 / 4096.0 * 32.0;
const C3: f64 = 2392.0 / 4096.0 * 32.0;

/// `m · v`.
#[must_use]
pub fn apply(m: &Matrix, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// `a · b`.
#[must_use]
pub fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

/// Inverse by cofactors. The callers invert only primary matrices, which
/// are far from singular.
#[must_use]
pub fn invert(m: &Matrix) -> Matrix {
    let c = |i: usize, j: usize| {
        let (r0, r1) = ((i + 1) % 3, (i + 2) % 3);
        let (c0, c1) = ((j + 1) % 3, (j + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let det: f64 = (0..3).map(|j| m[0][j] * c(0, j)).sum();
    std::array::from_fn(|i| std::array::from_fn(|j| c(j, i) / det))
}

/// Normalized primary matrix: linear RGB to XYZ, with RGB (1, 1, 1) at the
/// white point and `Y = 1`.
#[must_use]
pub fn rgb_to_xyz(primaries: [[f64; 2]; 3], white: [f64; 2]) -> Matrix {
    let xyz = |[x, y]: [f64; 2]| [x / y, 1.0, (1.0 - x - y) / y];
    let p = primaries.map(xyz);
    let columns: Matrix = std::array::from_fn(|i| std::array::from_fn(|j| p[j][i]));
    let s = apply(&invert(&columns), xyz(white));
    std::array::from_fn(|i| std::array::from_fn(|j| columns[i][j] * s[j]))
}

/// XYZ to linear BT.2020 RGB, both in cd/m².
pub static XYZ_TO_BT2020: LazyLock<Matrix> = LazyLock::new(|| invert(&rgb_to_xyz(BT2020, D65)));

/// Linear BT.709 RGB to linear BT.2020 RGB.
pub static BT709_TO_BT2020: LazyLock<Matrix> =
    LazyLock::new(|| mul(&XYZ_TO_BT2020, &rgb_to_xyz(BT709, D65)));

/// ST 2084 EOTF: signal to cd/m².
///
/// # Errors
/// A signal outside 0 to 1 or not finite.
pub fn pq_eotf(signal: f64) -> Result<f64, Invalid> {
    unit_signal(signal)?;
    let p = signal.powf(1.0 / M2);
    let num = (p - C1).max(0.0);
    Ok(PQ_PEAK_NITS * (num / (C2 - C3 * p)).powf(1.0 / M1))
}

/// ST 2084 inverse EOTF: cd/m² to signal.
///
/// **Deviation from ST 2084:** a value above 10000 cd/m² is not clipped.
/// The formula continues above signal 1, so an overshoot of a capture
/// keeps its size.
///
/// # Errors
/// A negative or non-finite value.
pub fn pq_inverse_eotf(nits: f64) -> Result<f64, Invalid> {
    if !nits.is_finite() {
        return Err(Invalid::NonFinite(nits));
    }
    if nits < 0.0 {
        return Err(Invalid::Negative(nits));
    }
    let yp = (nits / PQ_PEAK_NITS).powf(M1);
    Ok(((C1 + C2 * yp) / (1.0 + C3 * yp)).powf(M2))
}

fn unit_signal(v: f64) -> Result<f64, Invalid> {
    if !v.is_finite() {
        Err(Invalid::NonFinite(v))
    } else if !(0.0..=1.0).contains(&v) {
        Err(Invalid::Signal(v))
    } else {
        Ok(v)
    }
}

/// Display-referred linear BT.2020 RGB in cd/m². Components can be
/// negative: a colour outside BT.2020 keeps its sign.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Nits2020(pub [f64; 3]);

impl Nits2020 {
    /// # Errors
    /// A component is not finite.
    pub fn new(v: [f64; 3]) -> Result<Self, Invalid> {
        finite(v).map(Self)
    }

    /// Full-range PQ signals (normalized codes).
    ///
    /// # Errors
    /// A signal outside 0 to 1 or not finite.
    pub fn from_pq(signal: [f64; 3]) -> Result<Self, Invalid> {
        let [r, g, b] = signal;
        Ok(Self([pq_eotf(r)?, pq_eotf(g)?, pq_eotf(b)?]))
    }

    /// HLG signals on the display `d`. There is no default display: an
    /// HLG signal has no absolute light without one.
    ///
    /// # Errors
    /// A signal outside 0 to 1 or not finite.
    pub fn from_hlg(signal: [f64; 3], d: &HlgDisplay) -> Result<Self, Invalid> {
        let [r, g, b] = signal;
        let lift = |e: f64| {
            unit_signal(e).map(|e| hlg_inverse_oetf(((1.0 - d.beta) * e + d.beta).max(0.0)))
        };
        let scene = [lift(r)?, lift(g)?, lift(b)?];
        let ys = 0.2627 * scene[0] + 0.6780 * scene[1] + 0.0593 * scene[2];
        if ys == 0.0 {
            return Ok(Self([0.0; 3]));
        }
        let gain = d.peak_nits * ys.powf(d.system_gamma - 1.0);
        Ok(Self(scene.map(|e| gain * e)))
    }

    /// scRGB: linear BT.709, 1.0 = 80 cd/m².
    ///
    /// # Errors
    /// A component is not finite.
    pub fn from_scrgb(v: [f64; 3]) -> Result<Self, Invalid> {
        let lin = apply(&BT709_TO_BT2020, finite(v)?);
        Ok(Self(lin.map(|c| c * SCRGB_WHITE_NITS)))
    }

    /// CIE XYZ in cd/m².
    ///
    /// # Errors
    /// A component is not finite.
    pub fn from_xyz(v: [f64; 3]) -> Result<Self, Invalid> {
        Ok(Self(apply(&XYZ_TO_BT2020, finite(v)?)))
    }
}

/// The linear space of compared samples. Component errors and bounds are
/// in the units of this space; the metric converts to [`Nits2020`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    /// Linear BT.709, 1.0 = 80 cd/m².
    ScRgb,
    /// Linear BT.2020 in cd/m².
    Bt2020Nits,
}

impl Space {
    /// # Errors
    /// A component is not finite.
    pub fn to_nits_2020(self, v: [f64; 3]) -> Result<Nits2020, Invalid> {
        match self {
            Self::ScRgb => Nits2020::from_scrgb(v),
            Self::Bt2020Nits => Nits2020::new(v),
        }
    }
}

// BT.2100 Table 5 HLG constants.
const HLG_A: f64 = 0.178_832_77;
static HLG_B: LazyLock<f64> = LazyLock::new(|| 1.0 - 4.0 * HLG_A);
static HLG_C: LazyLock<f64> = LazyLock::new(|| 0.5 - HLG_A * (4.0 * HLG_A).ln());

/// HLG inverse OETF: signal to normalized scene light.
fn hlg_inverse_oetf(e: f64) -> f64 {
    if e <= 0.5 {
        e * e / 3.0
    } else {
        (((e - *HLG_C) / HLG_A).exp() + *HLG_B) / 12.0
    }
}

/// The BT.2100 HLG reference display. Every value is explicit: the
/// nominal system gamma depends on the peak, and a capture or a browser
/// gives neither.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HlgDisplay {
    /// Peak `L_W`, cd/m².
    peak_nits: f64,
    /// Black level `L_B`, cd/m².
    black_nits: f64,
    /// System gamma.
    system_gamma: f64,
    /// Black lift `beta = sqrt(3 (L_B / L_W)^(1 / gamma))`.
    beta: f64,
}

impl HlgDisplay {
    /// # Errors
    /// A non-finite value, `L_W <= 0`, `L_B < 0`, `L_B >= L_W` or
    /// `gamma <= 0`.
    pub fn new(peak_nits: f64, black_nits: f64, system_gamma: f64) -> Result<Self, Invalid> {
        finite([peak_nits, black_nits, system_gamma])?;
        if peak_nits <= 0.0 {
            return Err(Invalid::HlgDisplay("the peak must be above 0"));
        }
        if !(0.0..peak_nits).contains(&black_nits) {
            return Err(Invalid::HlgDisplay(
                "the black level must be at least 0 and below the peak",
            ));
        }
        if system_gamma <= 0.0 {
            return Err(Invalid::HlgDisplay("the system gamma must be above 0"));
        }
        let beta = (3.0 * (black_nits / peak_nits).powf(1.0 / system_gamma)).sqrt();
        Ok(Self {
            peak_nits,
            black_nits,
            system_gamma,
            beta,
        })
    }

    #[must_use]
    pub fn peak_nits(&self) -> f64 {
        self.peak_nits
    }

    #[must_use]
    pub fn black_nits(&self) -> f64 {
        self.black_nits
    }

    #[must_use]
    pub fn system_gamma(&self) -> f64 {
        self.system_gamma
    }
}

#[cfg(test)]
#[path = "colour_tests.rs"]
mod tests;
