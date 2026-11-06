//! Signal encodings, bit depths, transfer functions and colour matrices (BT.2100).

use std::fmt;
use std::str::FromStr;

/// Transfer function and range of the R'G'B' code values that go to the display.
///
/// The display side always signals PQ / BT.2020 full range. The narrow-range
/// encodings write their code values into it unchanged, so the wire carries
/// them only if the GPU output is set to RGB full range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// PQ, narrow range (10-bit black 64, nominal peak 940).
    PqNarrow,
    /// PQ, full range (10-bit black 0, peak 1023).
    PqFull,
    /// HLG, narrow range. The display still signals PQ; for scope use only.
    HlgNarrow,
}

impl Encoding {
    pub const ALL: [Self; 3] = [Self::PqNarrow, Self::PqFull, Self::HlgNarrow];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::PqNarrow => "pq-narrow",
            Self::PqFull => "pq-full",
            Self::HlgNarrow => "hlg-narrow",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PqNarrow => "PQ narrow range",
            Self::PqFull => "PQ full range",
            Self::HlgNarrow => "HLG narrow range",
        }
    }

    #[must_use]
    pub fn is_narrow(self) -> bool {
        matches!(self, Self::PqNarrow | Self::HlgNarrow)
    }

    #[must_use]
    pub fn is_pq(self) -> bool {
        matches!(self, Self::PqNarrow | Self::PqFull)
    }
}

impl fmt::Display for Encoding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for Encoding {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|e| e.id() == s)
            .ok_or_else(|| format!("unknown encoding `{s}`"))
    }
}

/// Bits per R'G'B' component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitDepth {
    Ten,
    Twelve,
}

impl BitDepth {
    pub const ALL: [Self; 2] = [Self::Ten, Self::Twelve];

    #[must_use]
    pub fn bits(self) -> u32 {
        match self {
            Self::Ten => 10,
            Self::Twelve => 12,
        }
    }

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Ten => "10",
            Self::Twelve => "12",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Ten => "10-bit",
            Self::Twelve => "12-bit",
        }
    }

    /// Largest code value: 1023 or 4095.
    #[must_use]
    pub fn max_code(self) -> u16 {
        (1 << self.bits()) - 1
    }

    /// Left shift from a 10-bit code to the same narrow-range level at this depth.
    #[must_use]
    pub fn shift(self) -> u32 {
        self.bits() - 10
    }
}

impl fmt::Display for BitDepth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for BitDepth {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|d| d.id() == s)
            .ok_or_else(|| format!("unknown bit depth `{s}`"))
    }
}

/// Signal format of the pattern code values: encoding and bit depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Format {
    pub encoding: Encoding,
    pub depth: BitDepth,
}

impl Format {
    #[must_use]
    pub const fn new(encoding: Encoding, depth: BitDepth) -> Self {
        Self { encoding, depth }
    }

    #[must_use]
    pub fn is_narrow(self) -> bool {
        self.encoding.is_narrow()
    }

    #[must_use]
    pub fn max_code(self) -> u16 {
        self.depth.max_code()
    }

    /// Code value of 0 % signal.
    #[must_use]
    pub fn black(self) -> u16 {
        if self.is_narrow() {
            64 << self.depth.shift()
        } else {
            0
        }
    }

    /// Code value of 100 % signal.
    #[must_use]
    pub fn white(self) -> u16 {
        if self.is_narrow() {
            940 << self.depth.shift()
        } else {
            self.max_code()
        }
    }

    /// The code is between 0 % and 100 % signal. Full-range codes always are.
    #[must_use]
    pub fn is_nominal(self, code: u16) -> bool {
        (self.black()..=self.white()).contains(&code)
    }

    /// Lowest and highest permitted code values (BT.2100 Table 9):
    /// narrow 4 to 1019 (10-bit) and 16 to 4079 (12-bit).
    #[must_use]
    pub fn code_limits(self) -> (u16, u16) {
        if self.is_narrow() {
            let s = self.depth.shift();
            (4 << s, (1020 << s) - 1)
        } else {
            (0, self.max_code())
        }
    }

    /// Quantises a non-linear signal value E' to a code (BT.2100 Table 9:
    /// narrow $(219E' + 16) \cdot 2^{n-8}$, full $(2^n - 1)E'$), clamped to
    /// [`Self::code_limits`].
    #[must_use]
    pub fn code_from_signal(self, signal: f64) -> u16 {
        let scale = f64::from(1u32 << (self.depth.bits() - 8));
        let code = if self.is_narrow() {
            (219.0 * signal + 16.0) * scale
        } else {
            f64::from(self.max_code()) * signal
        };
        let (lo, hi) = self.code_limits();
        // Clamped to the code range first, so the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let code = code.round().clamp(0.0, f64::from(self.max_code())) as u16;
        code.clamp(lo, hi)
    }

    /// Inverse of [`Self::code_from_signal`] without clamping.
    #[must_use]
    pub fn signal_from_code(self, code: u16) -> f64 {
        if self.is_narrow() {
            let scale = f64::from(1u32 << (self.depth.bits() - 8));
            (f64::from(code) / scale - 16.0) / 219.0
        } else {
            f64::from(code) / f64::from(self.max_code())
        }
    }

    /// Code value for an absolute luminance in cd/m². PQ encodings only.
    #[must_use]
    pub fn pq_code_from_nits(self, nits: f64) -> Option<u16> {
        self.encoding
            .is_pq()
            .then(|| self.code_from_signal(pq_inverse_eotf(nits)))
    }

    #[must_use]
    pub fn label(self) -> String {
        format!("{}, {}", self.encoding.label(), self.depth.label())
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// SMPTE ST 2084 constants.
const PQ_M1: f64 = 2610.0 / 16384.0;
const PQ_M2: f64 = 2523.0 / 4096.0 * 128.0;
const PQ_C1: f64 = 3424.0 / 4096.0;
const PQ_C2: f64 = 2413.0 / 4096.0 * 32.0;
const PQ_C3: f64 = 2392.0 / 4096.0 * 32.0;

/// PQ inverse EOTF: luminance in cd/m² (0..=10000) to signal E' (0..=1).
#[must_use]
pub fn pq_inverse_eotf(nits: f64) -> f64 {
    let y = (nits / 10000.0).clamp(0.0, 1.0).powf(PQ_M1);
    ((PQ_C1 + PQ_C2 * y) / (1.0 + PQ_C3 * y)).powf(PQ_M2)
}

/// PQ EOTF: signal E' (0..=1) to luminance in cd/m².
#[must_use]
pub fn pq_eotf(signal: f64) -> f64 {
    let e = signal.clamp(0.0, 1.0).powf(1.0 / PQ_M2);
    let y = ((e - PQ_C1).max(0.0) / (PQ_C2 - PQ_C3 * e)).powf(1.0 / PQ_M1);
    y * 10000.0
}

/// One pixel of R'G'B' code values. The bit depth is a property of the
/// pattern model, not of the pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u16,
    pub g: u16,
    pub b: u16,
}

impl Rgb {
    #[must_use]
    pub const fn new(r: u16, g: u16, b: u16) -> Self {
        Self { r, g, b }
    }

    #[must_use]
    pub const fn grey(v: u16) -> Self {
        Self { r: v, g: v, b: v }
    }

    /// The same level `shift` bits deeper, as BT.2111 narrow-range tables
    /// derive 12-bit from 10-bit values.
    #[must_use]
    pub const fn shl(self, shift: u32) -> Self {
        Self::new(self.r << shift, self.g << shift, self.b << shift)
    }

    #[must_use]
    pub fn max_component(self) -> u16 {
        self.r.max(self.g).max(self.b)
    }
}

/// Narrow-range Y'Cb'Cr' code values at the bit depth of the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YCbCr {
    pub y: u16,
    pub cb: u16,
    pub cr: u16,
}

/// D65 white point, CIE xy.
pub const D65: [f64; 2] = [0.3127, 0.3290];
/// CIE xy of the red, green and blue primaries.
pub const BT709_PRIMARIES: [[f64; 2]; 3] = [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]];
pub const BT2020_PRIMARIES: [[f64; 2]; 3] = [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]];

/// BT.2020 / BT.2100 non-constant-luminance coefficients.
const KR: f64 = 0.2627;
const KB: f64 = 0.0593;

/// Converts R'G'B' codes of `format` to the narrow-range Y'Cb'Cr' codes a
/// vectorscope reads, with the BT.2020 non-constant-luminance matrix, at
/// the same bit depth.
///
/// For full-range input this is the result of a GPU that outputs YCbCr
/// narrow range. Rounding inside the GPU can differ by one code.
#[must_use]
pub fn ycbcr_narrow(rgb: Rgb, format: Format) -> YCbCr {
    let red = format.signal_from_code(rgb.r);
    let green = format.signal_from_code(rgb.g);
    let blue = format.signal_from_code(rgb.b);
    let luma = KR * red + (1.0 - KR - KB) * green + KB * blue;
    let cb = (blue - luma) / (2.0 * (1.0 - KB));
    let cr = (red - luma) / (2.0 * (1.0 - KR));
    let narrow = Format::new(Encoding::PqNarrow, format.depth);
    let (lo, hi) = narrow.code_limits();
    let scale = f64::from(1u32 << (format.depth.bits() - 8));
    // Clamped to the narrow code limits, so the casts cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let q = |v: f64| (v * scale).round().clamp(f64::from(lo), f64::from(hi)) as u16;
    YCbCr {
        y: q(219.0 * luma + 16.0),
        cb: q(224.0 * cb + 128.0),
        cr: q(224.0 * cr + 128.0),
    }
}

#[cfg(test)]
#[path = "signal_tests.rs"]
mod tests;
