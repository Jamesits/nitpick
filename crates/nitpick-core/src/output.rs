//! Conversion of a pattern image to swap chain pixels.
//!
//! No swap chain API has a 12-bit integer format. 10-bit patterns use the
//! HDR10 swap chain (`R10G10B10A2_UNORM`, PQ, BT.2020): the code values go
//! in unchanged. 12-bit patterns use the FP16 scRGB swap chain (linear,
//! BT.709 primaries, 1.0 = 80 cd/m²): the compositor or the driver encodes
//! the values back to PQ / BT.2020 codes at the wire bit depth.
//!
//! The 80 cd/m² scale is the Windows scRGB convention. The Vulkan
//! extended-linear sRGB colour space uses the same convention; no
//! compositor test confirms it. It does not apply to other linear HDR
//! formats: macOS EDR has 1.0 = the current SDR white, so the `QRhi`
//! driver rejects 12-bit output there and this conversion is not used.
//!
//! In both cases a code value is a full-range PQ code on the wire, also for
//! the narrow-range encodings (see [`crate::signal::Encoding`]).

use std::collections::HashMap;
use std::fmt;
use std::sync::OnceLock;

use half::f16;

use crate::image::Image;
use crate::signal::{
    BT709_PRIMARIES, BT2020_PRIMARIES, BitDepth, D65, Format, Rgb, pq_eotf, pq_inverse_eotf,
};

/// Swap chain pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SwapChainFormat {
    /// `R10G10B10A2_UNORM` with PQ / BT.2020.
    Hdr10,
    /// `R16G16B16A16_FLOAT` with linear scRGB, 1.0 = 80 cd/m². Only for a
    /// platform with this scale; see the module documentation.
    ScRgb,
}

impl SwapChainFormat {
    #[must_use]
    pub fn for_depth(depth: BitDepth) -> Self {
        match depth {
            BitDepth::Ten => Self::Hdr10,
            BitDepth::Twelve => Self::ScRgb,
        }
    }

    #[must_use]
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Hdr10 => 4,
            Self::ScRgb => 8,
        }
    }
}

/// A pixel code above the maximum code of the swap chain depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeRangeError {
    pub x: usize,
    pub y: usize,
    pub code: Rgb,
    pub depth: BitDepth,
}

impl fmt::Display for CodeRangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Rgb { r, g, b } = self.code;
        write!(
            f,
            "pixel ({}, {}) code ({r}, {g}, {b}) exceeds {} for {}",
            self.x,
            self.y,
            self.depth.max_code(),
            self.depth
        )
    }
}

impl std::error::Error for CodeRangeError {}

/// Rejects the first pixel (in row-major order) with a code above the
/// maximum of `depth`. Masking or clamping would put a different code on
/// the wire without notice.
fn check_codes(img: &Image, depth: BitDepth) -> Result<(), CodeRangeError> {
    let max = depth.max_code();
    match img.pixels().iter().position(|c| c.max_component() > max) {
        None => Ok(()),
        Some(i) => Err(CodeRangeError {
            x: i % img.width(),
            y: i / img.width(),
            code: img.pixels()[i],
            depth,
        }),
    }
}

/// Swap chain pixels of an image in `format`, for the swap chain of
/// [`SwapChainFormat::for_depth`]. The encoding does not change the pixels:
/// every code is a full-range PQ code on the wire.
///
/// # Errors
/// If a code exceeds the maximum code of `format.depth`.
pub fn swap_chain_bytes(img: &Image, format: Format) -> Result<Vec<u8>, CodeRangeError> {
    match SwapChainFormat::for_depth(format.depth) {
        SwapChainFormat::Hdr10 => hdr10_bytes(img),
        SwapChainFormat::ScRgb => scrgb_bytes(img, format.depth),
    }
}

/// Packs 10-bit codes to `R10G10B10A2_UNORM` (R in the low bits), alpha
/// opaque. `None` if a code exceeds 1023.
#[must_use]
pub const fn pack_r10g10b10a2(c: Rgb) -> Option<u32> {
    if c.r > 0x3ff || c.g > 0x3ff || c.b > 0x3ff {
        return None;
    }
    Some((0b11 << 30) | ((c.b as u32) << 20) | ((c.g as u32) << 10) | c.r as u32)
}

/// Swap chain pixels of a 10-bit image in little-endian byte order.
///
/// # Errors
/// If a code exceeds 1023.
pub fn hdr10_bytes(img: &Image) -> Result<Vec<u8>, CodeRangeError> {
    check_codes(img, BitDepth::Ten)?;
    Ok(img
        .pixels()
        .iter()
        // `check_codes` passed, so every pixel packs.
        .filter_map(|&c| pack_r10g10b10a2(c))
        .flat_map(u32::to_le_bytes)
        .collect())
}

/// Swap chain pixels of an image at `depth` as `R16G16B16A16_FLOAT` scRGB,
/// in little-endian byte order.
///
/// # Errors
/// If a code exceeds the maximum code of `depth`.
pub fn scrgb_bytes(img: &Image, depth: BitDepth) -> Result<Vec<u8>, CodeRangeError> {
    check_codes(img, depth)?;
    let mut cache: HashMap<Rgb, [u8; 8]> = HashMap::new();
    let mut out = Vec::with_capacity(img.pixels().len() * 8);
    for &c in img.pixels() {
        let px = *cache.entry(c).or_insert_with(|| {
            let [r, g, b] = scrgb_from_codes(c, depth);
            let a = f16::ONE.to_bits();
            let mut px = [0; 8];
            for (i, v) in [r, g, b, a].into_iter().enumerate() {
                px[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
            }
            px
        });
        out.extend_from_slice(&px);
    }
    Ok(out)
}

type Matrix = [[f64; 3]; 3];

/// RGB to XYZ matrix of a colour space with primaries `rgb` and white `w`
/// (CIE xy chromaticities).
fn rgb_to_xyz(rgb: [[f64; 2]; 3], w: [f64; 2]) -> Matrix {
    let xyz = |[x, y]: [f64; 2]| [x / y, 1.0, (1.0 - x - y) / y];
    let p = rgb.map(xyz);
    let primaries = [
        [p[0][0], p[1][0], p[2][0]],
        [p[0][1], p[1][1], p[2][1]],
        [p[0][2], p[1][2], p[2][2]],
    ];
    let s = mul_vec(&invert(&primaries), xyz(w));
    std::array::from_fn(|i| std::array::from_fn(|j| primaries[i][j] * s[j]))
}

fn mul(a: &Matrix, b: &Matrix) -> Matrix {
    std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum()))
}

fn mul_vec(a: &Matrix, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| (0..3).map(|k| a[i][k] * v[k]).sum())
}

fn invert(m: &Matrix) -> Matrix {
    let c = |i: usize, j: usize| {
        let (r0, r1) = ((i + 1) % 3, (i + 2) % 3);
        let (c0, c1) = ((j + 1) % 3, (j + 2) % 3);
        m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0]
    };
    let det: f64 = (0..3).map(|j| m[0][j] * c(0, j)).sum();
    std::array::from_fn(|i| std::array::from_fn(|j| c(j, i) / det))
}

/// Linear BT.2020 to linear BT.709 (first) and its inverse (second).
fn matrices() -> &'static (Matrix, Matrix) {
    static M: OnceLock<(Matrix, Matrix)> = OnceLock::new();
    M.get_or_init(|| {
        let to_709 = mul(
            &invert(&rgb_to_xyz(BT709_PRIMARIES, D65)),
            &rgb_to_xyz(BT2020_PRIMARIES, D65),
        );
        (to_709, invert(&to_709))
    })
}

/// scRGB value 1.0 in cd/m² (Windows scRGB). Not the macOS EDR scale.
const SCRGB_WHITE_NITS: f64 = 80.0;

/// Decodes scRGB components to codes at `depth` with exact arithmetic: the
/// reference for what the compositor should send. Negative BT.2020
/// components clamp to code 0.
#[must_use]
pub fn codes_from_scrgb(px: [u16; 3], depth: BitDepth) -> Rgb {
    let (_, to_2020) = matrices();
    let lin = px.map(|b| f64::from(f16::from_bits(b)) * SCRGB_WHITE_NITS);
    let max = f64::from(depth.max_code());
    // Clamped to the code range, so the cast cannot truncate.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let code = |nits: f64| {
        (pq_inverse_eotf(nits.max(0.0)) * max)
            .round()
            .clamp(0.0, max) as u16
    };
    let [r, g, b] = mul_vec(to_2020, lin).map(code);
    Rgb::new(r, g, b)
}

/// Whether [`scrgb_from_codes`] carries `c` exactly through the reference
/// decoder [`codes_from_scrgb`].
#[must_use]
pub fn scrgb_is_exact(c: Rgb, depth: BitDepth) -> bool {
    codes_from_scrgb(scrgb_from_codes(c, depth), depth) == c
}

/// f16 bits in value order: `ordered(a) < ordered(b)` if and only if `a < b`.
fn ordered(bits: u16) -> i32 {
    let magnitude = i32::from(bits & 0x7fff);
    if bits & 0x8000 == 0 {
        magnitude
    } else {
        -magnitude
    }
}

fn from_ordered(v: i32) -> u16 {
    // |v| <= 0x7bff (largest finite f16) in every caller.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let magnitude = v.unsigned_abs() as u16;
    if v < 0 { 0x8000 | magnitude } else { magnitude }
}

/// Search distance in f16 steps per component. Measured on the BT.2111
/// levels: ±2 steps reduce the levels that do not decode exactly from 58 to
/// 36; ±8 steps only to 31. Greys need no search.
const SEARCH: i32 = 2;

/// scRGB f16 components (as bits) for the codes `c` at `depth`.
///
/// Rounding each component to f16 independently is not enough: after the
/// BT.709 to BT.2020 matrix, the rounding errors of the large components
/// leak into the small ones. A component that must be code 0 then becomes
/// a small positive luminance, which PQ maps to a high code. The function
/// therefore searches the f16 neighbours of the rounded value for the set
/// that [`codes_from_scrgb`] decodes closest to `c`. Negative errors on a
/// zero component are harmless, because the decoder clamps them to 0.
///
/// "Closest" is the smallest largest-component code error, then the
/// smallest sum of step offsets. On an equal score the offset
/// (dR, dG, dB) that is first in lexicographic order wins. An unchanged -0
/// in a searched set becomes +0.
///
/// Some levels have no exact set (see [`scrgb_is_exact`]): a component
/// near black next to bright components.
#[must_use]
pub fn scrgb_from_codes(c: Rgb, depth: BitDepth) -> [u16; 3] {
    let (to_709, _) = matrices();
    let max = f64::from(depth.max_code());
    let nits = [c.r, c.g, c.b].map(|v| pq_eotf(f64::from(v) / max));
    let base = mul_vec(to_709, nits).map(|v| f16::from_f64(v / SCRGB_WHITE_NITS).to_bits());
    if codes_from_scrgb(base, depth) == c {
        return base;
    }
    let mut best = (u32::MAX, base);
    let range = -SEARCH..=SEARCH;
    for dr in range.clone() {
        for dg in range.clone() {
            for db in range.clone() {
                let px = [dr, dg, db]
                    .iter()
                    .zip(base)
                    .map(|(d, b)| from_ordered(ordered(b) + d))
                    .collect::<Vec<_>>();
                let px = [px[0], px[1], px[2]];
                let got = codes_from_scrgb(px, depth);
                let err = [(got.r, c.r), (got.g, c.g), (got.b, c.b)]
                    .iter()
                    .map(|&(a, b)| u32::from(a.abs_diff(b)))
                    .max()
                    .unwrap_or(0);
                let dist = dr.unsigned_abs() + dg.unsigned_abs() + db.unsigned_abs();
                let score = err * 1000 + dist;
                if score < best.0 {
                    best = (score, px);
                }
            }
        }
    }
    best.1
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
