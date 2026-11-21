//! Reader for the ICC and MHC2 profile fixtures in `tests/fixtures/profile`.
//!
//! The manifest holds the values defined before serialization (`intended`)
//! and the signed 15.16 integers in the profile bytes (`encoded`). Compare
//! extracted profile data with `encoded`; [`decode_s15f16`] gives the exact
//! value of a stored integer.

use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{MetadataRequest, Resolved};

#[must_use]
pub fn profile_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/profile")
}

/// # Errors
/// If the manifest is missing or does not match [`ProfileManifest`].
pub fn manifest() -> io::Result<ProfileManifest> {
    let bytes = std::fs::read(profile_dir().join("manifest.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Deserialize)]
pub struct ProfileManifest {
    /// HDR10 metadata of frames shown during calibration checks.
    pub frame_metadata: MetadataRequest,
    pub profiles: Vec<ProfileEntry>,
    pub transforms: Vec<TransformCase>,
    pub model: Vec<ModelCase>,
}

/// A case of the explicit HDR display model. The generator computes
/// `expected` from the stored calibration values and the stored `input`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelCase {
    pub id: String,
    pub calibration: ModelCalibration,
    pub source: ModelSpace,
    pub wire: ModelWire,
    /// Source signals, or linear values for a linear source.
    pub input: Vec<[f64; 3]>,
    pub expected: Vec<ModelExpected>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ModelCalibration {
    /// The MHC2 values of a profile fixture, read through the adapter.
    Profile { profile: String },
    /// An XYZ matrix and channel LUTs; `None` selects identity LUTs.
    Inline {
        matrix: [[f64; 3]; 3],
        lut: Option<[Vec<f64>; 3]>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelSpace {
    /// `bt709` or `bt2020`, with the D65 white.
    pub primaries: String,
    pub encoding: ModelEncoding,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelWire {
    #[serde(flatten)]
    pub space: ModelSpace,
    /// Bits per full-range code.
    pub depth: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ModelEncoding {
    Linear {
        nits_per_unit: f64,
    },
    Pq,
    Sdr {
        transfer: SdrTransfer,
        white_nits: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SdrTransfer {
    /// IEC 61966-2-1.
    Srgb,
    /// Pure power 2.2.
    Gamma22,
}

/// Expected stages of one pixel; see the display model for each stage.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelExpected {
    pub source_nits: [f64; 3],
    pub wire_nits: [f64; 3],
    pub signal: [f64; 3],
    pub lut: [f64; 3],
    pub output: [f64; 3],
    pub code: [u32; 3],
    pub clip: ModelClip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct ModelClip {
    pub encode: [ClipState; 3],
    pub output: [ClipState; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipState {
    None,
    Low,
    High,
}

/// A conventional ICC transform with its expected output. The generator
/// computes `expected` from the stored profile values and the stored
/// `input`, without a colour library.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct TransformCase {
    pub id: String,
    /// Profile ID; `None` for the D50 PCS XYZ endpoint (XYZ, Y = 1 for the
    /// PCS white).
    pub source: Option<String>,
    pub target: Option<String>,
    pub intent: RenderingIntent,
    pub black_point_compensation: bool,
    /// Observer adaptation for the absolute colorimetric intent: 1 keeps
    /// the `chad` adaptation, 0 undoes it.
    pub adaptation_state: f64,
    /// RGB device values or PCS XYZ, as the source requires.
    pub input: Vec<[f64; 3]>,
    pub expected: Vec<[f64; 3]>,
}

/// ICC.1:2010 rendering intents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RenderingIntent {
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProfileEntry {
    pub id: String,
    /// File name in [`profile_dir`].
    pub file: String,
    #[serde(rename = "use")]
    pub usage: ProfileUse,
    pub size: usize,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
    /// `None` if the profile itself is invalid.
    pub display: Option<DisplayTags>,
    /// MHC2 values, or the error kind the profile adapter must return:
    /// `missing_tag`, `malformed_tag`, `unsupported` or `invalid_profile`.
    pub mhc2: Resolved<Mhc2Expected>,
}

impl ProfileEntry {
    /// Reads the profile file and checks it against the manifest size and
    /// hash, so a stale file fails here instead of in a later comparison.
    ///
    /// # Errors
    /// If the file is missing or differs from the manifest.
    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        let bytes = std::fs::read(profile_dir().join(&self.file))?;
        let hash = Sha256::digest(&bytes)
            .iter()
            .fold(String::with_capacity(64), |mut s, b| {
                let _ = write!(s, "{b:02x}");
                s
            });
        if bytes.len() != self.size || hash != self.sha256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: size or SHA-256 differs from the manifest", self.file),
            ));
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileUse {
    /// No MHC2 tag; for profile assignment on every OS.
    Ordinary,
    /// Windows calibration set; only `desc` and `MHC2` differ between them.
    Mhc2,
    /// Profile adapter cases only; never install.
    Adapter,
}

/// A value before serialization and its stored signed 15.16 form.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Serialized<I, E> {
    pub intended: I,
    pub encoded: E,
}

pub type Fixed = Serialized<f64, i32>;
/// Three rows of four.
pub type Matrix3x4 = Serialized<[[f64; 4]; 3], [[i32; 4]; 3]>;
/// Red, green and blue LUTs of equal length.
pub type Luts = Serialized<[Vec<f64>; 3], [Vec<i32>; 3]>;

/// ICC colour tags. Chromaticities are intended values; they are not
/// stored in the profile.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DisplayTags {
    /// CIE xy of red, green and blue.
    pub primaries: [[f64; 2]; 3],
    /// CIE xy of the display white.
    pub white: [f64; 2],
    /// `wtpt`: the D50 PCS illuminant, as ICC.1:2010 requires for a
    /// display profile.
    pub media_white: Serialized<[f64; 3], [i32; 3]>,
    /// `chad`: Bradford from `white` to D50, rows.
    pub chad: Serialized<[[f64; 3]; 3], [[i32; 3]; 3]>,
    pub colorants: Colorants,
    /// `rTRC`, `gTRC` and `bTRC` (equal).
    pub trc: ParametricCurve,
    /// `lumi` Y in cd/m²: full-frame luminance.
    pub lumi: Option<Fixed>,
}

/// `rXYZ`, `gXYZ` and `bXYZ`, one XYZ triple per primary.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Colorants {
    /// Unadapted XYZ with white Y = 1.
    pub native: [[f64; 3]; 3],
    /// After `chad`.
    pub intended: [[f64; 3]; 3],
    pub encoded: [[i32; 3]; 3],
}

/// ICC `parametricCurveType`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ParametricCurve {
    pub function: u16,
    pub intended: Vec<f64>,
    pub encoded: Vec<i32>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Mhc2Expected {
    /// cd/m².
    pub min_nits: Fixed,
    pub peak_nits: Fixed,
    /// The fourth column is stored but ignored. `None`: matrix offset 0,
    /// which selects identity.
    pub matrix: Option<Matrix3x4>,
    /// `None`: LUT count 0, which selects identity.
    pub lut: Option<Luts>,
    /// The RGB action the matrix was derived from, where defined.
    #[serde(default)]
    pub rgb_action: Option<RgbAction>,
}

/// Matrix $A = M P M^{-1}$ with $M$ the RGB-to-XYZ matrix of `space` at
/// D65, without D50 adaptation.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RgbAction {
    /// `bt709`.
    pub space: String,
    /// $P$, rows.
    pub matrix: [[f64; 3]; 3],
}

/// Exact value of a signed 15.16 integer.
#[must_use]
pub fn decode_s15f16(v: i32) -> f64 {
    f64::from(v) / 65536.0
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
