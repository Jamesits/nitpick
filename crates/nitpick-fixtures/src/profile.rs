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
    /// CIE xy of the display white (D65).
    pub white: [f64; 2],
    /// `wtpt`: the D50 PCS illuminant, as ICC.1:2010 requires for a
    /// display profile.
    pub media_white: Serialized<[f64; 3], [i32; 3]>,
    /// `chad`: Bradford D65 to D50, rows.
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
    /// Unadapted D65 XYZ with white Y = 1.
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
