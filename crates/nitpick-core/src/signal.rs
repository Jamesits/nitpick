//! Signal encodings, transfer functions and colour matrices (BT.2100).

use std::fmt;
use std::str::FromStr;

/// Encoding of the 10-bit R'G'B' code values that go to the display.
///
/// The swap chain is always 10-bit full-range RGB with the PQ transfer and
/// BT.2020 primaries. The narrow-range encodings write their code values
/// into it unchanged, so the wire carries them only if the GPU output is
/// set to RGB full range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// PQ, narrow range (black 64, nominal peak 940).
    PqNarrow,
    /// PQ, full range (black 0, peak 1023).
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

    /// Code value of 0 % signal.
    #[must_use]
    pub fn black(self) -> u16 {
        if self.is_narrow() { 64 } else { 0 }
    }

    /// Code value of 100 % signal.
    #[must_use]
    pub fn white(self) -> u16 {
        if self.is_narrow() { 940 } else { 1023 }
    }

    /// Lowest and highest permitted code values (BT.2100 Table 9).
    #[must_use]
    pub fn code_limits(self) -> (u16, u16) {
        if self.is_narrow() {
            (4, 1019)
        } else {
            (0, 1023)
        }
    }

    /// Quantises a non-linear signal value E' to a 10-bit code (BT.2100 Table 9),
    /// clamped to [`Self::code_limits`].
    #[must_use]
    pub fn code_from_signal(self, signal: f64) -> u16 {
        let code = if self.is_narrow() {
            876.0 * signal + 64.0
        } else {
            1023.0 * signal
        };
        let (lo, hi) = self.code_limits();
        // Clamped to 0..=1023 first, so the cast cannot truncate.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let code = code.round().clamp(0.0, 1023.0) as u16;
        code.clamp(lo, hi)
    }

    /// Inverse of [`Self::code_from_signal`] without clamping.
    #[must_use]
    pub fn signal_from_code(self, code: u16) -> f64 {
        if self.is_narrow() {
            (f64::from(code) - 64.0) / 876.0
        } else {
            f64::from(code) / 1023.0
        }
    }

    /// Code value for an absolute luminance in cd/m². PQ encodings only.
    #[must_use]
    pub fn pq_code_from_nits(self, nits: f64) -> Option<u16> {
        match self {
            Self::PqNarrow | Self::PqFull => Some(self.code_from_signal(pq_inverse_eotf(nits))),
            Self::HlgNarrow => None,
        }
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

/// One pixel of 10-bit R'G'B' code values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb10 {
    pub r: u16,
    pub g: u16,
    pub b: u16,
}

impl Rgb10 {
    #[must_use]
    pub const fn new(r: u16, g: u16, b: u16) -> Self {
        Self { r, g, b }
    }

    #[must_use]
    pub const fn grey(v: u16) -> Self {
        Self { r: v, g: v, b: v }
    }

    /// Packs to `DXGI_FORMAT_R10G10B10A2_UNORM` (R in the low bits), alpha opaque.
    #[must_use]
    pub const fn pack(self) -> u32 {
        (0b11 << 30)
            | ((self.b as u32 & 0x3ff) << 20)
            | ((self.g as u32 & 0x3ff) << 10)
            | (self.r as u32 & 0x3ff)
    }

    #[must_use]
    pub const fn unpack(v: u32) -> Self {
        // Each channel is masked to 10 bits, so the casts cannot truncate.
        #[allow(clippy::cast_possible_truncation)]
        Self {
            r: (v & 0x3ff) as u16,
            g: ((v >> 10) & 0x3ff) as u16,
            b: ((v >> 20) & 0x3ff) as u16,
        }
    }
}

/// 10-bit narrow-range Y'Cb'Cr' code values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YCbCr10 {
    pub y: u16,
    pub cb: u16,
    pub cr: u16,
}

/// BT.2020 / BT.2100 non-constant-luminance coefficients.
const KR: f64 = 0.2627;
const KB: f64 = 0.0593;

/// Converts R'G'B' codes of `encoding` to the narrow-range Y'Cb'Cr' codes a
/// vectorscope reads, with the BT.2020 non-constant-luminance matrix.
///
/// For full-range input this is the result of a GPU that outputs YCbCr
/// narrow range. Rounding inside the GPU can differ by one code.
#[must_use]
pub fn ycbcr_narrow(rgb: Rgb10, encoding: Encoding) -> YCbCr10 {
    let red = encoding.signal_from_code(rgb.r);
    let green = encoding.signal_from_code(rgb.g);
    let blue = encoding.signal_from_code(rgb.b);
    let luma = KR * red + (1.0 - KR - KB) * green + KB * blue;
    let cb = (blue - luma) / (2.0 * (1.0 - KB));
    let cr = (red - luma) / (2.0 * (1.0 - KR));
    // Clamped to 4..=1019, so the casts cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let q = |v: f64| v.round().clamp(4.0, 1019.0) as u16;
    YCbCr10 {
        y: q(876.0 * luma + 64.0),
        cb: q(896.0 * cb + 512.0),
        cr: q(896.0 * cr + 512.0),
    }
}

#[cfg(test)]
#[path = "signal_tests.rs"]
mod tests;
