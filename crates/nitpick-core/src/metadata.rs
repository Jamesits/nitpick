//! HDR10 static metadata: SMPTE ST 2086 mastering display colour volume and
//! CTA-861.3 content light level.

use std::fmt;
use std::str::FromStr;

use crate::signal::{BT2020_PRIMARIES, pq_eotf};

/// Mastering display primaries preset. Both use the D65 white point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MasteringPrimaries {
    Bt2020,
    P3D65,
}

impl MasteringPrimaries {
    pub const ALL: [Self; 2] = [Self::Bt2020, Self::P3D65];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Bt2020 => "bt2020",
            Self::P3D65 => "p3-d65",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Bt2020 => "BT.2020",
            Self::P3D65 => "P3-D65",
        }
    }

    /// CIE xy of red, green and blue.
    #[must_use]
    pub fn primaries(self) -> [[f64; 2]; 3] {
        match self {
            Self::Bt2020 => BT2020_PRIMARIES,
            Self::P3D65 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
        }
    }
}

impl fmt::Display for MasteringPrimaries {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

impl FromStr for MasteringPrimaries {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.id() == s)
            .ok_or_else(|| format!("unknown mastering primaries `{s}`"))
    }
}

/// HDR10 static metadata in natural units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hdr10Metadata {
    /// CIE xy of the mastering display red, green and blue.
    pub primaries: [[f64; 2]; 3],
    /// CIE xy of the mastering display white point.
    pub white: [f64; 2],
    /// Mastering display luminance range in cd/m².
    pub max_mastering_nits: f64,
    pub min_mastering_nits: f64,
    /// Maximum content light level and maximum frame-average light level in cd/m².
    pub max_cll: f64,
    pub max_fall: f64,
}

/// Rounds and clamps to an unsigned integer field.
fn field(v: f64, max: u32) -> u32 {
    // Clamped to 0..=max, so the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = v.round().clamp(0.0, f64::from(max)) as u32;
    v
}

fn field16(v: f64) -> u16 {
    // field() clamps to u16::MAX.
    #[allow(clippy::cast_possible_truncation)]
    let v = field(v, u32::from(u16::MAX)) as u16;
    v
}

impl Hdr10Metadata {
    /// Chromaticity in ST 2086 units of 0.00002, clamped to 0..=50000.
    #[must_use]
    pub fn chromaticity_code(v: f64) -> u16 {
        field16((v * 50_000.0).min(50_000.0))
    }

    /// Luminance in ST 2086 units of 0.0001 cd/m².
    #[must_use]
    pub fn luminance_code(nits: f64) -> u32 {
        field(nits * 10_000.0, u32::MAX)
    }

    /// Content light level in whole cd/m², clamped to 0..=65535.
    #[must_use]
    pub fn light_level_code(nits: f64) -> u16 {
        field16(nits)
    }

    /// The 24-byte mastering display colour volume in big-endian order, as
    /// in the HEVC SEI message and `CAEDRMetadata`: primaries in the order
    /// green, blue, red, then the white point, then maximum and minimum
    /// luminance.
    #[must_use]
    pub fn mastering_display_bytes(&self) -> [u8; 24] {
        let [r, g, b] = self.primaries;
        let mut out = Vec::with_capacity(24);
        for [x, y] in [g, b, r, self.white] {
            out.extend(Self::chromaticity_code(x).to_be_bytes());
            out.extend(Self::chromaticity_code(y).to_be_bytes());
        }
        out.extend(Self::luminance_code(self.max_mastering_nits).to_be_bytes());
        out.extend(Self::luminance_code(self.min_mastering_nits).to_be_bytes());
        out.try_into().unwrap_or([0; 24])
    }

    /// The 4-byte content light level information in big-endian order:
    /// `MaxCLL`, then `MaxFALL`.
    #[must_use]
    pub fn content_light_level_bytes(&self) -> [u8; 4] {
        let [a, b] = Self::light_level_code(self.max_cll).to_be_bytes();
        let [c, d] = Self::light_level_code(self.max_fall).to_be_bytes();
        [a, b, c, d]
    }
}

/// Content light level of a frame in cd/m².
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LightLevel {
    pub max_cll: f64,
    pub max_fall: f64,
}

/// `MaxCLL` and `MaxFALL` (CTA-861.3): the maximum and the frame average of
/// the per-pixel maximum of R, G and B in cd/m².
///
/// `samples` holds the normalized PQ signal E' of R, G and B for each output
/// pixel. The driver supplies the transfer and range interpretation of its
/// output signal. [`pq_eotf`] clamps values outside 0..=1. An empty frame
/// gives 0 for both values.
#[must_use]
pub fn light_level(samples: impl IntoIterator<Item = [f64; 3]>) -> LightLevel {
    let mut max_cll = 0.0f64;
    let mut sum = 0.0;
    let mut n = 0usize;
    // Patterns have large flat regions, so the previous EOTF result usually
    // applies. Keyed by bits: exact matches only.
    let mut last: Option<(u64, f64)> = None;
    for [r, g, b] in samples {
        // The PQ EOTF is monotonic, so the EOTF of the maximum signal is the
        // maximum of the per-channel luminances.
        let e = r.max(g).max(b);
        let l = match last {
            Some((bits, l)) if bits == e.to_bits() => l,
            _ => {
                let l = pq_eotf(e);
                last = Some((e.to_bits(), l));
                l
            }
        };
        max_cll = max_cll.max(l);
        sum += l;
        n += 1;
    }
    // Pixel counts are far below 2^52, so the conversion is exact.
    #[allow(clippy::cast_precision_loss)]
    let max_fall = sum / n.max(1) as f64;
    LightLevel { max_cll, max_fall }
}

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
