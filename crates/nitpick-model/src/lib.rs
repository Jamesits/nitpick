//! Explicit HDR display model, as test support.
//!
//! Predicts the wire code of a colour for a stated source space, an MHC2
//! calibration (an XYZ matrix and channel LUTs) and a wire mode. Each result
//! is an ideal f64 calculation under the rules below. It does not describe
//! GPU precision, clipping order or dithering, and it does not show that an
//! OS loaded or applied the calibration.
//!
//! Stage order, with column vectors:
//! 1. Decode the source to linear RGB in cd/m².
//! 2. `XYZ = M_src · RGB`, with the SMPTE RP 177 matrix of the primaries
//!    and their white. There is no D50 adaptation.
//! 3. `XYZ' = A · XYZ`, with `A` the XYZ matrix of the calibration.
//! 4. `RGB_wire = M_wire⁻¹ · XYZ'`.
//! 5. Encode the wire transfer function; clip the signal to 0..1.
//! 6. Apply the LUT of each channel; clamp the result to 0..1.
//! 7. Quantize to a full-range code; exact halves go up.
//!
//! There is no ICC TRC, `vcgt`, rendering-intent or D50 stage. An MHC2 LUT
//! already holds the calibration of the wire curve, so another curve stage
//! would apply it two times.

use std::fmt;

use nitpick_compare::colour::{self, Matrix};

mod lut;
mod transfer;

pub use lut::{Lut, MAX_LUT_LEN, MIN_LUT_LEN};
pub use transfer::SdrTransfer;

/// CIE xy chromaticities of red, green and blue, and of the white.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Primaries {
    pub rgb: [[f64; 2]; 3],
    pub white: [f64; 2],
}

impl Primaries {
    pub const BT709: Self = Self {
        rgb: colour::BT709,
        white: colour::D65,
    };
    pub const BT2020: Self = Self {
        rgb: colour::BT2020,
        white: colour::D65,
    };
}

/// An SDR transfer function and the luminance of signal 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sdr {
    pub transfer: SdrTransfer,
    pub white_nits: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SourceEncoding {
    /// Linear values; 1.0 is `nits_per_unit` cd/m². scRGB is 80.
    Linear {
        nits_per_unit: f64,
    },
    /// SMPTE ST 2084, absolute.
    Pq,
    Sdr(Sdr),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WireEncoding {
    /// SMPTE ST 2084, absolute.
    Pq,
    Sdr(Sdr),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Source {
    pub primaries: Primaries,
    pub encoding: SourceEncoding,
}

/// The wire mode. The primaries describe the wire encoding, not the
/// measured panel. Codes are full range; range conversion, chroma
/// subsampling and dithering are outside the model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wire {
    pub primaries: Primaries,
    pub encoding: WireEncoding,
    /// Bits per code, 1 to 16.
    pub depth: u8,
}

/// An XYZ matrix and channel LUTs in the encoded wire domain.
#[derive(Debug, Clone, PartialEq)]
pub struct Calibration {
    matrix: Matrix,
    lut: Option<Lut>,
}

impl Calibration {
    /// `matrix` is the 3×3 XYZ adjustment, rows. For MHC2 it is the left
    /// 3×3 of the stored matrix; the stored fourth column is not an offset.
    /// `lut: None` selects identity.
    ///
    /// # Errors
    /// A non-finite matrix element.
    pub fn new(matrix: Matrix, lut: Option<Lut>) -> Result<Self, ModelError> {
        finite(matrix.as_flattened())?;
        Ok(Self { matrix, lut })
    }

    #[must_use]
    pub fn identity() -> Self {
        Self {
            matrix: IDENTITY,
            lut: None,
        }
    }

    #[must_use]
    pub fn matrix(&self) -> &Matrix {
        &self.matrix
    }

    #[must_use]
    pub fn lut(&self) -> Option<&Lut> {
        self.lut.as_ref()
    }
}

const IDENTITY: Matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Where a stage limited a channel to its range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clip {
    None,
    Low,
    High,
}

/// The stages of one pixel. The floats before quantization stay available,
/// so a comparison can report the error before rounding.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    /// Linear source RGB, cd/m².
    pub source_nits: [f64; 3],
    /// Linear wire RGB, cd/m², before clipping.
    pub wire_nits: [f64; 3],
    /// Encoded wire signal after clipping, before the LUT.
    pub signal: [f64; 3],
    /// LUT result before the clamp; equal to `signal` without a LUT.
    pub lut: [f64; 3],
    /// Clamped LUT result.
    pub output: [f64; 3],
    pub code: [u32; 3],
    /// Clipping of the wire encoding: below 0 cd/m², or above the PQ peak
    /// or the SDR white.
    pub encode_clip: [Clip; 3],
    /// Clamp of the LUT result to 0..1.
    pub output_clip: [Clip; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    source: Source,
    wire: Wire,
    calibration: Calibration,
    src_to_xyz: Matrix,
    xyz_to_wire: Matrix,
}

impl Model {
    /// # Errors
    /// A luminance scale that is not finite and above 0, a depth outside 1
    /// to 16, or primaries without an invertible matrix.
    pub fn new(source: Source, calibration: Calibration, wire: Wire) -> Result<Self, ModelError> {
        match source.encoding {
            SourceEncoding::Linear { nits_per_unit } => scale(nits_per_unit)?,
            SourceEncoding::Pq => {}
            SourceEncoding::Sdr(s) => scale(s.white_nits)?,
        }
        if let WireEncoding::Sdr(s) = wire.encoding {
            scale(s.white_nits)?;
        }
        if !(1..=16).contains(&wire.depth) {
            return Err(ModelError::Depth(wire.depth));
        }
        let (src_to_xyz, _) = npm(source.primaries)?;
        let (_, xyz_to_wire) = npm(wire.primaries)?;
        Ok(Self {
            source,
            wire,
            calibration,
            src_to_xyz,
            xyz_to_wire,
        })
    }

    /// # Errors
    /// A non-finite input or intermediate value, or a PQ or SDR source
    /// signal outside 0..1.
    pub fn predict(&self, rgb: [f64; 3]) -> Result<Prediction, ModelError> {
        finite(&rgb)?;
        let source_nits = match self.source.encoding {
            SourceEncoding::Linear { nits_per_unit } => rgb.map(|v| v * nits_per_unit),
            SourceEncoding::Pq => each(rgb, |s| {
                colour::pq_eotf(s).map_err(|_| ModelError::Signal(s))
            })?,
            SourceEncoding::Sdr(sdr) => each(rgb, |s| {
                if (0.0..=1.0).contains(&s) {
                    Ok(sdr.transfer.decode(s) * sdr.white_nits)
                } else {
                    Err(ModelError::Signal(s))
                }
            })?,
        };
        let xyz = colour::apply(&self.src_to_xyz, source_nits);
        let calibrated = colour::apply(&self.calibration.matrix, xyz);
        let wire_nits = colour::apply(&self.xyz_to_wire, calibrated);
        // A large finite input can overflow in the matrices.
        finite(&wire_nits)?;
        let encoded = wire_nits.map(|n| match self.wire.encoding {
            WireEncoding::Pq => transfer::encode_pq(n),
            WireEncoding::Sdr(s) => transfer::encode_sdr(s, n),
        });
        let signal = encoded.map(|(s, _)| s);
        let lut = self
            .calibration
            .lut
            .as_ref()
            .map_or(signal, |l| l.apply(signal));
        let clamped = lut.map(clamp);
        let output = clamped.map(|(v, _)| v);
        Ok(Prediction {
            source_nits,
            wire_nits,
            signal,
            lut,
            output,
            code: output.map(|v| quantize(v, self.wire.depth)),
            encode_clip: encoded.map(|(_, c)| c),
            output_clip: clamped.map(|(_, c)| c),
        })
    }
}

fn each(v: [f64; 3], f: impl Fn(f64) -> Result<f64, ModelError>) -> Result<[f64; 3], ModelError> {
    Ok([f(v[0])?, f(v[1])?, f(v[2])?])
}

fn finite(v: &[f64]) -> Result<(), ModelError> {
    match v.iter().find(|c| !c.is_finite()) {
        Some(&c) => Err(ModelError::NonFinite(c)),
        None => Ok(()),
    }
}

fn scale(nits: f64) -> Result<(), ModelError> {
    if nits.is_finite() && nits > 0.0 {
        Ok(())
    } else {
        Err(ModelError::Scale(nits))
    }
}

/// The RP 177 matrix of `p` and its inverse.
fn npm(p: Primaries) -> Result<(Matrix, Matrix), ModelError> {
    let m = colour::rgb_to_xyz(p.rgb, p.white);
    let inv = colour::invert(&m);
    // A singular matrix can give finite values from roundoff, so check the
    // product too. Real primaries are far inside this limit.
    let product = colour::mul(&inv, &m);
    let ok = m
        .as_flattened()
        .iter()
        .chain(inv.as_flattened())
        .all(|v| v.is_finite())
        && (0..3).all(|i| (0..3).all(|j| (product[i][j] - IDENTITY[i][j]).abs() <= 1e-9));
    if ok {
        Ok((m, inv))
    } else {
        Err(ModelError::Primaries)
    }
}

fn clamp(v: f64) -> (f64, Clip) {
    if v < 0.0 {
        (0.0, Clip::Low)
    } else if v > 1.0 {
        (1.0, Clip::High)
    } else {
        (v, Clip::None)
    }
}

/// `⌊(2^depth − 1) · v + 1/2⌋` for `v` in 0..1.
fn quantize(v: f64, depth: u8) -> u32 {
    let max = f64::from((1u32 << depth) - 1);
    // `v` is in 0..1 and `depth` at most 16, so the result fits.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let code = (v * max + 0.5).floor() as u32;
    code
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelError {
    /// NaN or an infinity in an input or a calibration value.
    NonFinite(f64),
    /// A PQ or SDR source signal outside 0..1.
    Signal(f64),
    /// A luminance scale that is not finite and above 0.
    Scale(f64),
    /// A wire depth outside 1 to 16.
    Depth(u8),
    /// Primaries without a finite, invertible matrix.
    Primaries,
    /// LUT channels of this length; see [`MIN_LUT_LEN`] and [`MAX_LUT_LEN`].
    LutLength(usize),
    /// LUT channels of different lengths.
    LutChannels,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinite(v) => write!(f, "the value {v} is not finite"),
            Self::Signal(v) => write!(f, "the source signal {v} is outside 0 to 1"),
            Self::Scale(v) => write!(f, "the luminance scale {v} is not above 0"),
            Self::Depth(d) => write!(f, "the wire depth {d} is outside 1 to 16"),
            Self::Primaries => f.write_str("the primaries have no invertible matrix"),
            Self::LutLength(n) => write!(
                f,
                "a LUT of {n} entries is outside {MIN_LUT_LEN} to {MAX_LUT_LEN}"
            ),
            Self::LutChannels => f.write_str("the LUT channels have different lengths"),
        }
    }
}

impl std::error::Error for ModelError {}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
