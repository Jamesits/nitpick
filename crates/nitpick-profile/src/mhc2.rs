//! Checks and decodes the stored bytes of an `MHC2` tag.
//!
//! Little CMS `Type_MHC2_Read` ignores the tag size and the `sf32`
//! signatures, adds offsets to the tag start in 32-bit arithmetic and fails
//! for LUT count 0. These checks run on the raw tag first; the decoded values
//! must then agree with the Little CMS read.

use crate::{Defect, ProfileError, Tag, Unsupported};

const HEADER_LEN: usize = 36;
/// 12 signed 15.16 values; no type header.
const MATRIX_LEN: usize = 48;
/// `sf32` and four reserved bytes.
const LUT_HEADER_LEN: usize = 8;
pub const MAX_LUT_LEN: u32 = 4096;

const IDENTITY: [[f64; 4]; 3] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Red,
    Green,
    Blue,
}

impl Channel {
    pub const ALL: [Self; 3] = [Self::Red, Self::Green, Self::Blue];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Element {
    Matrix,
    Lut(Channel),
}

/// Calibration data of a Microsoft `MHC2` tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Mhc2 {
    /// cd/m².
    pub min_nits: f64,
    /// cd/m².
    pub peak_nits: f64,
    /// Three rows of four, as stored. `None`: matrix offset 0, which selects
    /// identity.
    pub matrix: Option<[[f64; 4]; 3]>,
    /// Red, green and blue samples of equal length, 2 to 4096. `None`: LUT
    /// count 0, which selects identity.
    pub lut: Option<[Vec<f64>; 3]>,
}

impl Mhc2 {
    /// The XYZ adjustment: the left 3×3 of [`Self::matrix`], or identity.
    /// Windows ignores the stored fourth column; it is not an offset.
    #[must_use]
    pub fn xyz_matrix(&self) -> [[f64; 3]; 3] {
        let m = self.matrix.unwrap_or(IDENTITY);
        m.map(|r| [r[0], r[1], r[2]])
    }
}

fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn s15f16(b: &[u8], at: usize) -> f64 {
    f64::from(be_u32(b, at).cast_signed()) / 65536.0
}

/// The element bytes, if the element starts after the header and ends
/// within the tag. Microsoft places elements after the header; one that
/// overlaps it would decode header fields as samples.
fn element(tag: &[u8], offset: u32, len: usize) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    if start < HEADER_LEN {
        return None;
    }
    tag.get(start..start.checked_add(len)?)
}

/// Checks and decodes a raw `MHC2` tag. Structural defects are reported
/// before unsupported values, so a malformed tag is never `Unsupported`.
///
/// Reserved fields and the luminance values are not checked: no rule for
/// them is decided, so they pass unchanged.
pub(crate) fn parse(tag: &[u8]) -> Result<Mhc2, ProfileError> {
    let malformed = |d| ProfileError::MalformedTag(Tag::Mhc2, d);
    let unsupported = |u| ProfileError::Unsupported(Tag::Mhc2, u);
    if tag.len() < HEADER_LEN {
        return Err(malformed(Defect::Truncated));
    }
    if tag[..4] != *b"MHC2" {
        return Err(malformed(Defect::TypeSignature));
    }
    let count = be_u32(tag, 8);
    if count > MAX_LUT_LEN {
        return Err(malformed(Defect::LutCount));
    }
    let matrix_offset = be_u32(tag, 20);
    let matrix = if matrix_offset == 0 {
        None
    } else {
        let b = element(tag, matrix_offset, MATRIX_LEN)
            .ok_or(malformed(Defect::OutOfBounds(Element::Matrix)))?;
        Some([0, 1, 2].map(|r| [0, 1, 2, 3].map(|c| s15f16(b, 16 * r + 4 * c))))
    };

    // Count 0 selects identity; Microsoft gives no meaning to its offsets.
    if count == 0 {
        return Ok(Mhc2 {
            min_nits: s15f16(tag, 12),
            peak_nits: s15f16(tag, 16),
            matrix,
            lut: None,
        });
    }
    let n = count as usize;
    let lut_len = LUT_HEADER_LEN + 4 * n;
    let mut luts: [Option<&[u8]>; 3] = [None; 3];
    for (i, ch) in Channel::ALL.into_iter().enumerate() {
        let offset = be_u32(tag, 24 + 4 * i);
        if offset == 0 {
            continue;
        }
        let b = element(tag, offset, lut_len)
            .ok_or(malformed(Defect::OutOfBounds(Element::Lut(ch))))?;
        if b[..4] != *b"sf32" {
            return Err(malformed(Defect::LutSignature(ch)));
        }
        luts[i] = Some(b);
    }
    if count == 1 {
        return Err(unsupported(Unsupported::SingleEntryLut));
    }
    let mut lut: [Vec<f64>; 3] = Default::default();
    for (i, ch) in Channel::ALL.into_iter().enumerate() {
        // Microsoft does not say whether offset 0 with a nonzero count
        // selects identity for that channel; reject it until decided.
        let b = luts[i].ok_or(unsupported(Unsupported::NullLutOffset(ch)))?;
        lut[i] = (0..n).map(|k| s15f16(b, LUT_HEADER_LEN + 4 * k)).collect();
    }
    Ok(Mhc2 {
        min_nits: s15f16(tag, 12),
        peak_nits: s15f16(tag, 16),
        matrix,
        lut: Some(lut),
    })
}

/// Whether the Little CMS read gives exactly the decoded raw values.
pub(crate) fn agrees(raw: &Mhc2, lib: &crate::native::Mhc2Values) -> bool {
    let same = |a: &[f64], b: &[f64]| {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
    };
    let raw_matrix = raw.matrix.unwrap_or(IDENTITY);
    let lut_ok = match &raw.lut {
        Some(l) => (0..3).all(|i| same(&l[i], &lib.lut[i])),
        None => lib.lut.iter().all(Vec::is_empty),
    };
    same(
        &[raw.min_nits, raw.peak_nits],
        &[lib.min_nits, lib.peak_nits],
    ) && same(raw_matrix.as_flattened(), lib.matrix.as_flattened())
        && lut_ok
}

#[cfg(test)]
#[path = "mhc2_tests.rs"]
mod tests;
