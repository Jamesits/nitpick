//! Reader for the output fixtures in `tests/fixtures/output`.
//!
//! The fixtures and their expected values come from the independent
//! reference generator in `tests/reference`. This crate only reads them; it
//! needs no Python. It does not depend on `nitpick-core`, so that crate can
//! use it as a dev-dependency, and formats stay strings
//! (`"pq-full"`, depth `10`) for the consumer to map.

use std::fmt;
use std::io;
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};

/// Group files with patch and image cases; the file is `<group>.json`.
pub const CASE_GROUPS: [&str; 6] = ["codes", "luminance", "gamut", "mixed", "geometry", "state"];

#[must_use]
pub fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/output")
}

/// Reads and parses one fixture file.
///
/// # Errors
/// If the file is missing or does not match `T`.
pub fn load<T: DeserializeOwned>(file: &str) -> io::Result<T> {
    let bytes = std::fs::read(fixture_dir().join(file))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// # Errors
/// See [`load`].
pub fn group(name: &str) -> io::Result<GroupFile> {
    load(&format!("{name}.json"))
}

/// # Errors
/// See [`load`].
pub fn scalar() -> io::Result<ScalarFile> {
    load("scalar.json")
}

/// # Errors
/// See [`load`].
pub fn invalid() -> io::Result<InvalidFile> {
    load("invalid.json")
}

#[derive(Debug, Deserialize)]
pub struct GroupFile {
    pub cases: Vec<Case>,
    #[serde(default)]
    pub sequences: Vec<Sequence>,
}

/// Case input. Invalid cases use the same shape, so the numeric fields
/// accept values that [`validate`] rejects.
#[derive(Debug, Clone, Deserialize)]
pub struct CaseInput {
    pub id: String,
    pub format: FormatSpec,
    /// Width and height in pixels.
    pub size: [usize; 2],
    pub palette: Vec<PaletteEntry>,
    pub layout: Layout,
    pub metadata: Option<MetadataRequest>,
}

/// A valid case with its expected results.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    #[serde(flatten)]
    pub input: CaseInput,
    pub requires: Requires,
    pub expected: Expected,
    /// Frame marker of state frames.
    #[serde(default)]
    pub marker: Option<Marker>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FormatSpec {
    /// `pq-full`, `pq-narrow` or `hlg-narrow`.
    pub encoding: String,
    /// Bits per component.
    pub depth: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaletteEntry {
    /// R', G', B' codes. Signed so that invalid cases can hold negative codes.
    pub codes: [i64; 3],
    #[serde(default)]
    pub intended: Option<Intended>,
}

/// The colour a code triple was quantized from, where the case defines one.
#[derive(Debug, Clone, Deserialize)]
pub struct Intended {
    /// Neutral luminance in cd/m².
    #[serde(default)]
    pub nits: Option<f64>,
    /// Source colour space of a gamut patch: `bt709`, `p3-d65` or `bt2020`.
    #[serde(default)]
    pub space: Option<String>,
    /// Linear RGB direction of a gamut patch in its source space.
    #[serde(default)]
    pub rgb: Option<[u8; 3]>,
    /// Luminance Y of a gamut patch in cd/m².
    #[serde(default, rename = "Y")]
    pub big_y: Option<f64>,
    #[serde(default)]
    pub xyz: Option<[f64; 3]>,
    /// Linear BT.2020 RGB in cd/m² before quantization.
    #[serde(default)]
    pub nits_2020: Option<[f64; 3]>,
    #[serde(default)]
    pub scrgb: Option<[f64; 3]>,
    /// A negative BT.2020 component was clipped to 0 before encoding.
    #[serde(default)]
    pub container_clip: Option<bool>,
    /// The quantized codes decoded with their own encoding; `None` for HLG.
    #[serde(default)]
    pub after_quantization: Option<Nominal>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Nominal {
    pub nits_2020: [f64; 3],
    pub xyz: [f64; 3],
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Layout {
    /// Background index, then `[x, y, width, height, index]` painted in order.
    Rects {
        background: usize,
        rects: Vec<[i64; 5]>,
    },
    /// Palette index of each pixel, row-major.
    Rows { rows: Vec<Vec<usize>> },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MetadataRequest {
    /// CIE xy of red, green and blue.
    pub primaries: [[f64; 2]; 3],
    pub white: [f64; 2],
    pub max_mastering_nits: f64,
    pub min_mastering_nits: f64,
    pub light_level: LightLevelRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LightLevelRequest {
    FromFrame,
    Explicit { max_cll: f64, max_fall: f64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Requires {
    pub depth: u32,
    pub encoding: String,
    pub hdr_metadata: bool,
    pub auto_light_level: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Expected {
    /// Pixels of each palette entry after layout.
    pub pixel_counts: Vec<usize>,
    /// Indexed like [`CaseInput::palette`].
    pub palette: Vec<PaletteExpected>,
    pub light_level: PerDriver<Resolved<LightLevel>>,
    /// `None` if the case has no metadata request.
    pub metadata: Option<PerDriver<Resolved<ResolvedMetadata>>>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PerDriver<T> {
    pub qrhi: T,
    pub browser: T,
}

/// A value, or the error kind a driver must return instead.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Resolved<T> {
    Value(T),
    Error { error: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct LightLevel {
    /// cd/m².
    pub max_cll: f64,
    pub max_fall: f64,
}

/// `Hdr10Metadata` in natural units.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResolvedMetadata {
    pub primaries: [[f64; 2]; 3],
    pub white: [f64; 2],
    pub max_mastering_nits: f64,
    pub min_mastering_nits: f64,
    pub max_cll: f64,
    pub max_fall: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaletteExpected {
    pub qrhi: QrhiExpected,
    pub browser: BrowserExpected,
}

/// `QRhi` CPU output: the codes as full-range PQ for every encoding.
#[derive(Debug, Clone, Deserialize)]
pub struct QrhiExpected {
    /// $D / (2^n - 1)$.
    pub signal: [f64; 3],
    /// Linear BT.2020 in cd/m².
    pub nits_2020: [f64; 3],
    /// Ideal scRGB before f16 rounding and the neighbour search.
    pub scrgb: [f64; 3],
    /// Packed word at 10 bits; `None` at 12 bits.
    pub r10g10b10a2: Option<u32>,
}

/// Browser CPU output.
#[derive(Debug, Clone, Deserialize)]
pub struct BrowserExpected {
    /// Full-range 16-bit PNG samples.
    pub sample: [u16; 3],
    pub clip: [Clip; 3],
    /// PQ luminance of the stored samples; `None` for HLG.
    pub nits_2020: Option<[f64; 3]>,
    pub scrgb: Option<[f64; 3]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Clip {
    Below,
    None,
    Above,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Marker {
    pub value: u32,
    pub bits: u32,
    /// Cell edge in pixels.
    pub cell: usize,
    /// Top-left corner; bits run left to right, most significant first.
    pub origin: [usize; 2],
    /// Palette index of a set bit and a clear bit.
    pub one: usize,
    pub zero: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Sequence {
    pub id: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Step {
    Open,
    Show(Box<ShowStep>),
    /// Host close.
    Close,
    /// External disconnect or window closure.
    Disconnect,
    Resize {
        size: [usize; 2],
    },
    DeviceLoss,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShowStep {
    /// Case ID of a state frame.
    pub frame: String,
    /// `false`: the next event happens before acknowledgement.
    pub wait: bool,
    pub metadata: Option<MetadataRequest>,
    pub expect: ShowExpected,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ShowExpected {
    pub outcome: Outcome,
    /// `OutputError` variant name, or `any` where the design fixes no kind.
    #[serde(default)]
    pub error: Option<String>,
    /// Marker value of the current frame after this step.
    #[serde(default)]
    pub marker: Option<u32>,
    /// Absent: not checked. `Some(None)`: no metadata.
    #[serde(default, deserialize_with = "present")]
    pub resolved: Option<Option<PerDriver<Resolved<ResolvedMetadata>>>>,
}

fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ready,
    Cancelled,
    Error,
}

#[derive(Debug, Deserialize)]
pub struct ScalarFile {
    pub qrhi: Vec<QrhiScalar>,
    pub browser: Vec<BrowserScalar>,
}

/// Every code at one depth, indexed by code. Valid for all encodings.
#[derive(Debug, Deserialize)]
pub struct QrhiScalar {
    pub depth: u32,
    pub count: usize,
    pub signal: Vec<f64>,
    pub nits_2020: Vec<f64>,
    /// scRGB of the grey triple of each code.
    pub scrgb_grey: Vec<[f64; 3]>,
    /// Words with the code in one channel and 0 in the others; 10 bits only.
    pub r10g10b10a2: Option<ChannelWords>,
}

#[derive(Debug, Deserialize)]
pub struct ChannelWords {
    pub r: Vec<u32>,
    pub g: Vec<u32>,
    pub b: Vec<u32>,
}

#[derive(Debug, Deserialize)]
pub struct BrowserScalar {
    pub encoding: String,
    pub depth: u32,
    pub count: usize,
    pub sample: Vec<u16>,
    pub clip: Vec<Clip>,
    pub nits_2020: Option<Vec<f64>>,
}

#[derive(Debug, Deserialize)]
pub struct InvalidFile {
    pub cases: Vec<InvalidCase>,
}

#[derive(Debug, Deserialize)]
pub struct InvalidCase {
    pub id: String,
    /// [`FixtureError::id`].
    pub error: String,
    pub input: CaseInput,
}

/// Reasons to reject a case input. [`Self::id`] matches the generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureError {
    UnknownEncoding,
    UnsupportedDepth,
    EmptyImage,
    CodeOutOfRange,
    LayoutMismatch,
    RectOutOfBounds,
    InvalidMetadata,
}

impl FixtureError {
    pub const ALL: [Self; 7] = [
        Self::UnknownEncoding,
        Self::UnsupportedDepth,
        Self::EmptyImage,
        Self::CodeOutOfRange,
        Self::LayoutMismatch,
        Self::RectOutOfBounds,
        Self::InvalidMetadata,
    ];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::UnknownEncoding => "unknown_encoding",
            Self::UnsupportedDepth => "unsupported_depth",
            Self::EmptyImage => "empty_image",
            Self::CodeOutOfRange => "code_out_of_range",
            Self::LayoutMismatch => "layout_mismatch",
            Self::RectOutOfBounds => "rect_out_of_bounds",
            Self::InvalidMetadata => "invalid_metadata",
        }
    }
}

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Checks a case input with the same rules, in the same order, as the
/// generator.
///
/// # Errors
/// The first rule that fails.
pub fn validate(input: &CaseInput) -> Result<(), FixtureError> {
    if !matches!(
        input.format.encoding.as_str(),
        "pq-full" | "pq-narrow" | "hlg-narrow"
    ) {
        return Err(FixtureError::UnknownEncoding);
    }
    if !matches!(input.format.depth, 10 | 12) {
        return Err(FixtureError::UnsupportedDepth);
    }
    let [w, h] = input.size;
    if w == 0 || h == 0 {
        return Err(FixtureError::EmptyImage);
    }
    let max = (1i64 << input.format.depth) - 1;
    if input
        .palette
        .iter()
        .flat_map(|e| e.codes)
        .any(|c| !(0..=max).contains(&c))
    {
        return Err(FixtureError::CodeOutOfRange);
    }
    let n = input.palette.len();
    match &input.layout {
        Layout::Rects { background, rects } => {
            if *background >= n {
                return Err(FixtureError::LayoutMismatch);
            }
            let (w, h) = (to_i64(w), to_i64(h));
            for &[x, y, rw, rh, idx] in rects {
                if rw < 1 || rh < 1 || x < 0 || y < 0 || x + rw > w || y + rh > h {
                    return Err(FixtureError::RectOutOfBounds);
                }
                if usize::try_from(idx).map_or(true, |i| i >= n) {
                    return Err(FixtureError::LayoutMismatch);
                }
            }
        }
        Layout::Rows { rows } => {
            if rows.len() != h || rows.iter().any(|r| r.len() != w) {
                return Err(FixtureError::LayoutMismatch);
            }
            if rows.iter().flatten().any(|&i| i >= n) {
                return Err(FixtureError::LayoutMismatch);
            }
        }
    }
    if let Some(md) = &input.metadata {
        validate_metadata(md)?;
    }
    Ok(())
}

fn validate_metadata(md: &MetadataRequest) -> Result<(), FixtureError> {
    let coords = md.primaries.iter().flatten().chain(&md.white);
    if !coords
        .into_iter()
        .all(|v| v.is_finite() && *v > 0.0 && *v < 1.0)
    {
        return Err(FixtureError::InvalidMetadata);
    }
    let (lo, hi) = (md.min_mastering_nits, md.max_mastering_nits);
    if !(lo.is_finite() && hi.is_finite() && 0.0 <= lo && lo < hi) {
        return Err(FixtureError::InvalidMetadata);
    }
    if let LightLevelRequest::Explicit { max_cll, max_fall } = md.light_level
        && !(max_cll.is_finite() && max_fall.is_finite() && 0.0 <= max_fall && max_fall <= max_cll)
    {
        return Err(FixtureError::InvalidMetadata);
    }
    Ok(())
}

fn to_i64(v: usize) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Palette index of every pixel, row-major. The input must be valid.
#[must_use]
pub fn raster(input: &CaseInput) -> Vec<usize> {
    let [w, h] = input.size;
    match &input.layout {
        Layout::Rows { rows } => rows.iter().flatten().copied().collect(),
        Layout::Rects { background, rects } => {
            let mut px = vec![*background; w * h];
            for r in rects {
                // validate() guarantees non-negative, in-bounds values.
                let [x, y, rw, rh, idx] = r.map(|v| usize::try_from(v).unwrap_or(0));
                for row in px[y * w..(y + rh) * w].chunks_mut(w) {
                    row[x..x + rw].fill(idx);
                }
            }
            px
        }
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
