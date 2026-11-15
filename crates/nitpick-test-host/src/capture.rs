//! Raw captures: the planes of an OS capture adapter with a descriptor of
//! their memory layout and colour. See "Capture interface and artifacts"
//! in `doc/design/output_driver/testing.md`.
//!
//! A descriptor field that the API does not give stays `None`. Nothing in
//! this module derives a colour value from the sample format: FP16 storage
//! does not imply linear scRGB or 80 cd/m².

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;
use std::pin::Pin;

use futures_core::Stream;
use nitpick_core::driver::DisplayId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The acknowledged frame to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTarget<'a> {
    pub display: &'a DisplayId,
    pub frame: u64,
    pub generation: u64,
}

/// The frames of one capture session in arrival order. `Err`: the session
/// failed, with the reason. Dropping the stream ends the session.
pub type CaptureFrames = Pin<Box<dyn Stream<Item = Result<Capture, String>>>>;

/// An OS capture adapter. It is not part of `OutputDriver`.
pub trait CaptureAdapter {
    /// The current time in the time base of the frame timestamps. The host
    /// reads it before each presentation. `None`: the adapter has no clock.
    fn now(&self) -> Option<Timestamp>;

    /// Starts a capture session of `target`. The adapter must not block
    /// the host thread: it delivers frames through the stream.
    ///
    /// # Errors
    /// No capture is available, for example without permission or after an
    /// API failure; the reason.
    fn start(&mut self, target: &CaptureTarget<'_>) -> Result<CaptureFrames, String>;
}

/// A host without a capture adapter.
#[derive(Debug, Clone)]
pub struct NoCapture(pub String);

impl CaptureAdapter for NoCapture {
    fn now(&self) -> Option<Timestamp> {
        None
    }

    fn start(&mut self, _: &CaptureTarget<'_>) -> Result<CaptureFrames, String> {
        Err(self.0.clone())
    }
}

/// Pixels `[x, x + width)` × `[y, y + height)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    #[must_use]
    pub const fn new(x: usize, y: usize, width: usize, height: usize) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The end coordinates `(x + width, y + height)`; `None` on overflow.
    #[must_use]
    pub fn end(&self) -> Option<(usize, usize)> {
        Some((
            self.x.checked_add(self.width)?,
            self.y.checked_add(self.height)?,
        ))
    }

    #[must_use]
    pub fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && y >= self.y && x - self.x < self.width && y - self.y < self.height
    }

    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        let overlap = |a: usize, aw: usize, b: usize, bw: usize| {
            aw > 0 && bw > 0 && (a <= b && b - a < aw || b < a && a - b < bw)
        };
        overlap(self.x, self.width, other.x, other.width)
            && overlap(self.y, self.height, other.y, other.height)
    }

    /// Each edge moved `by` pixels inward; `None` if nothing remains.
    #[must_use]
    pub fn inset(&self, by: usize) -> Option<Self> {
        let width = self.width.checked_sub(by.checked_mul(2)?)?;
        let height = self.height.checked_sub(by.checked_mul(2)?)?;
        (width > 0 && height > 0).then(|| Self::new(self.x + by, self.y + by, width, height))
    }

    /// The part of `self` that the rectangle at `origin` covers, in
    /// coordinates relative to `origin`.
    #[must_use]
    pub fn relative_to(&self, origin: (usize, usize)) -> Option<Self> {
        let (end_x, end_y) = self.end()?;
        let (x, y) = (self.x.max(origin.0), self.y.max(origin.1));
        (end_x > x && end_y > y)
            .then(|| Self::new(x - origin.0, y - origin.1, end_x - x, end_y - y))
    }
}

/// The output of an adapter, unchanged.
#[derive(Debug, Clone, PartialEq)]
pub struct Capture {
    pub descriptor: CaptureDescriptor,
    /// The bytes of each plane of [`CaptureDescriptor::planes`], with the
    /// row padding of the API.
    pub planes: Vec<Vec<u8>>,
    /// API metadata as the adapter received it, for example pixel-buffer
    /// attachments.
    pub raw: Map<String, Value>,
    /// Binary API metadata by name; see [`valid_blob_name`].
    pub raw_blobs: BTreeMap<String, Vec<u8>>,
    /// The settings of the adapter for this capture, for example a preset.
    pub config: Map<String, Value>,
    /// PNG made by the adapter or the OS, tone mapped. Never a sample
    /// source.
    pub preview: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureDescriptor {
    /// Pixels.
    pub width: usize,
    pub height: usize,
    pub planes: Vec<PlaneLayout>,
    /// The part of the image that shows the source content, for example
    /// without window chrome, a capture border or unused texture area.
    /// `None`: the whole image.
    pub content: Option<Rect>,
    pub cursor: Cursor,
    /// The meaning of the `a` field. `None`: unknown, or no `a` field.
    pub alpha: Option<Alpha>,
    pub colour: Colour,
    pub timestamps: Timestamps,
    pub source: Source,
}

/// The mouse cursor in the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Cursor {
    /// The adapter told the API not to draw it.
    Hidden,
    /// It can cover `rect` of the image.
    Shown { rect: Rect },
    /// The API can draw it, at an unknown position.
    Unknown,
}

/// One plane: rows of blocks; each block holds one bit field per channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaneLayout {
    /// Bytes from the start of a row to the start of the next row.
    pub row_pitch: usize,
    /// Pixels per block, horizontally and vertically: 1 or 2. The plane
    /// has `ceil(width / x)` × `ceil(height / y)` blocks.
    pub subsampling: [usize; 2],
    /// 1 to 16.
    pub block_bytes: usize,
    /// Of the block as one integer. Thus a big-endian block of 16-bit
    /// components has its first component in the highest bits.
    pub byte_order: ByteOrder,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteOrder {
    Little,
    Big,
}

/// A bit field of the block integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub channel: Channel,
    /// Position of the lowest bit.
    pub shift: u32,
    pub bits: u32,
    pub kind: SampleKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    R,
    G,
    B,
    A,
    Y,
    Cb,
    Cr,
}

impl Channel {
    const ALL: [Self; 7] = [
        Self::R,
        Self::G,
        Self::B,
        Self::A,
        Self::Y,
        Self::Cb,
        Self::Cr,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleKind {
    /// An unsigned integer code, at most 32 bits.
    Uint,
    /// IEEE 754 binary16 or binary32.
    Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alpha {
    Straight,
    Premultiplied,
    /// The API states that the field has no meaning.
    Ignored,
}

/// Each field: `None` if the API does not state it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colour {
    pub transfer: Option<Transfer>,
    pub primaries: Option<Primaries>,
    pub white: Option<WhitePoint>,
    /// Of integer codes only.
    pub range: Option<Range>,
    /// Of `Y`, `Cb` and `Cr` channels only.
    pub matrix: Option<YcbcrMatrix>,
    pub luminance: Option<LuminanceScale>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    Linear,
    Srgb,
    Gamma22,
    /// The ITU-R BT.709 OETF.
    Bt709,
    Pq,
    Hlg,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primaries {
    Bt709,
    Bt2020,
    /// SMPTE EG 432-1 (P3) primaries; the white point is separate.
    P3,
    /// CIE xy.
    Custom {
        red: [f64; 2],
        green: [f64; 2],
        blue: [f64; 2],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhitePoint {
    D65,
    D50,
    /// CIE xy.
    Custom([f64; 2]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    Full,
    Narrow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum YcbcrMatrix {
    Bt601,
    Bt709,
    Bt2020Ncl,
}

/// The luminance of a linear value 1.0 after the transfer decode.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LuminanceScale {
    /// For example 80 for scRGB, 10000 for PQ.
    Absolute { nits_per_unit: f64 },
    /// Relative to a display state that the capture does not give.
    Relative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timestamps {
    /// The API time of the frame content.
    pub frame: Option<Timestamp>,
    /// When the adapter received the frame.
    pub arrival: Option<Timestamp>,
}

/// Raw ticks; compare only values of the same [`TimeBase`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timestamp {
    pub base: TimeBase,
    pub ticks: u64,
    pub ticks_per_second: u64,
}

impl Timestamp {
    /// Compares the times exactly, also at different tick rates. `None`:
    /// another time base, or a tick rate of 0.
    #[must_use]
    pub fn cmp_time(&self, other: &Self) -> Option<Ordering> {
        if self.base != other.base || self.ticks_per_second == 0 || other.ticks_per_second == 0 {
            return None;
        }
        let a = u128::from(self.ticks) * u128::from(other.ticks_per_second);
        let b = u128::from(other.ticks) * u128::from(self.ticks_per_second);
        Some(a.cmp(&b))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBase {
    /// Windows `QueryPerformanceCounter`.
    Qpc,
    /// macOS `mach_absolute_time`.
    MachAbsolute,
    /// POSIX `CLOCK_MONOTONIC`.
    Monotonic,
}

/// What the adapter captured.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub kind: SourceKind,
    /// The native handle as text, for example a window handle.
    pub native: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Window,
    Output,
}

/// One stored sample, unchanged except binary16 widened to binary32.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sample {
    Uint(u32),
    Float(f32),
}

impl Sample {
    /// Exact: both kinds fit in binary64.
    #[must_use]
    pub fn value(self) -> f64 {
        match self {
            Self::Uint(v) => f64::from(v),
            Self::Float(v) => f64::from(v),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldProblem {
    ZeroWidth,
    /// Above 32 bits for an integer; not 16 or 32 bits for a float.
    Width,
    OutsideBlock,
    Overlap,
}

/// Why a descriptor does not describe its planes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorError {
    EmptyImage,
    PlaneCount {
        descriptor: usize,
        data: usize,
    },
    BlockBytes {
        plane: usize,
    },
    /// Not 1 or 2, or a channel other than `Cb` and `Cr` subsampled.
    Subsampling {
        plane: usize,
    },
    Field {
        plane: usize,
        channel: Channel,
        problem: FieldProblem,
    },
    RowPitch {
        plane: usize,
        pitch: usize,
        row: usize,
    },
    DataLength {
        plane: usize,
        len: usize,
        min: usize,
        max: usize,
    },
    /// The plane size does not fit in `usize`.
    Overflow {
        plane: usize,
    },
    DuplicateChannel(Channel),
    /// Not R, G, B or Y, Cb, Cr, each with an optional A.
    Channels,
    /// The colour channels differ in kind or width.
    MixedSamples,
    AlphaWithoutField,
    RangeOfFloat,
    MatrixOfRgb,
    Chromaticity,
    Luminance,
    Timestamp,
    BlobName(String),
    /// The content rectangle is empty or not inside the image.
    Content,
    /// The cursor rectangle is empty.
    Cursor,
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyImage => write!(f, "zero width or height"),
            Self::PlaneCount { descriptor, data } => write!(
                f,
                "the descriptor has {descriptor} planes; the capture has {data}"
            ),
            Self::BlockBytes { plane } => write!(f, "plane {plane}: block size not 1 to 16 bytes"),
            Self::Subsampling { plane } => write!(f, "plane {plane}: invalid subsampling"),
            Self::Field {
                plane,
                channel,
                problem,
            } => write!(f, "plane {plane}: field {channel:?}: {problem:?}"),
            Self::RowPitch { plane, pitch, row } => write!(
                f,
                "plane {plane}: row pitch {pitch} B is less than the row of {row} B"
            ),
            Self::DataLength {
                plane,
                len,
                min,
                max,
            } => write!(f, "plane {plane}: {len} B, expected {min} to {max} B"),
            Self::Overflow { plane } => write!(f, "plane {plane}: size overflows"),
            Self::DuplicateChannel(c) => write!(f, "channel {c:?} in more than one field"),
            Self::Channels => write!(f, "the channels are not RGB or YCbCr with optional alpha"),
            Self::MixedSamples => write!(f, "the colour channels differ in kind or width"),
            Self::AlphaWithoutField => write!(f, "an alpha meaning without an alpha field"),
            Self::RangeOfFloat => write!(f, "a range for float samples"),
            Self::MatrixOfRgb => write!(f, "a YCbCr matrix for RGB samples"),
            Self::Chromaticity => write!(f, "invalid chromaticity"),
            Self::Luminance => write!(f, "invalid luminance scale"),
            Self::Timestamp => write!(f, "zero ticks per second"),
            Self::BlobName(name) => write!(f, "invalid raw blob name {name:?}"),
            Self::Content => write!(f, "the content rectangle is empty or outside the image"),
            Self::Cursor => write!(f, "the cursor rectangle is empty"),
        }
    }
}

impl std::error::Error for DescriptorError {}

/// A blob name is also a part of a file name: 1 to 64 of `a`–`z`, `0`–`9`,
/// `_` and `-`.
#[must_use]
pub fn valid_blob_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// A validated capture with sample access.
#[derive(Debug, Clone, Copy)]
pub struct CaptureView<'a> {
    capture: &'a Capture,
    /// Plane and field of each [`Channel`].
    fields: [Option<(usize, Field)>; 7],
}

impl Capture {
    /// Checks that the descriptor describes the planes and is consistent.
    /// It does not fill unknown fields.
    ///
    /// # Errors
    /// The first defect found.
    pub fn validate(&self) -> Result<CaptureView<'_>, DescriptorError> {
        let d = &self.descriptor;
        if d.width == 0 || d.height == 0 {
            return Err(DescriptorError::EmptyImage);
        }
        if let Some(c) = d.content
            && (c.width == 0
                || c.height == 0
                || c.end().is_none_or(|(x, y)| x > d.width || y > d.height))
        {
            return Err(DescriptorError::Content);
        }
        if let Cursor::Shown { rect } = d.cursor
            && (rect.width == 0 || rect.height == 0 || rect.end().is_none())
        {
            return Err(DescriptorError::Cursor);
        }
        if d.planes.is_empty() || d.planes.len() != self.planes.len() {
            return Err(DescriptorError::PlaneCount {
                descriptor: d.planes.len(),
                data: self.planes.len(),
            });
        }
        let mut fields = [None; 7];
        for (i, (layout, data)) in d.planes.iter().zip(&self.planes).enumerate() {
            check_plane(i, layout, data.len(), d.width, d.height)?;
            for f in &layout.fields {
                let slot = &mut fields[f.channel.index()];
                if slot.is_some() {
                    return Err(DescriptorError::DuplicateChannel(f.channel));
                }
                *slot = Some((i, *f));
            }
        }
        let view = CaptureView {
            capture: self,
            fields,
        };
        view.check_channels()?;
        check_colour(&d.colour, view.colour_kind(), view.is_ycbcr())?;
        if d.alpha.is_some() && view.field(Channel::A).is_none() {
            return Err(DescriptorError::AlphaWithoutField);
        }
        for t in [&d.timestamps.frame, &d.timestamps.arrival]
            .into_iter()
            .flatten()
        {
            if t.ticks_per_second == 0 {
                return Err(DescriptorError::Timestamp);
            }
        }
        if let Some(name) = self.raw_blobs.keys().find(|n| !valid_blob_name(n)) {
            return Err(DescriptorError::BlobName(name.clone()));
        }
        Ok(view)
    }
}

fn check_plane(
    plane: usize,
    layout: &PlaneLayout,
    len: usize,
    width: usize,
    height: usize,
) -> Result<(), DescriptorError> {
    if !(1..=16).contains(&layout.block_bytes) {
        return Err(DescriptorError::BlockBytes { plane });
    }
    let [sx, sy] = layout.subsampling;
    let chroma_only = layout
        .fields
        .iter()
        .all(|f| matches!(f.channel, Channel::Cb | Channel::Cr));
    if !(1..=2).contains(&sx) || !(1..=2).contains(&sy) || (!chroma_only && (sx, sy) != (1, 1)) {
        return Err(DescriptorError::Subsampling { plane });
    }
    let block_bits = 8 * layout.block_bytes;
    let mut used = 0u128;
    for f in &layout.fields {
        let problem = if f.bits == 0 {
            Some(FieldProblem::ZeroWidth)
        } else if match f.kind {
            SampleKind::Uint => f.bits > 32,
            SampleKind::Float => f.bits != 16 && f.bits != 32,
        } {
            Some(FieldProblem::Width)
        } else if (f.shift as usize).saturating_add(f.bits as usize) > block_bits {
            Some(FieldProblem::OutsideBlock)
        } else if used & (mask(f.bits) << f.shift) != 0 {
            Some(FieldProblem::Overlap)
        } else {
            used |= mask(f.bits) << f.shift;
            None
        };
        if let Some(problem) = problem {
            return Err(DescriptorError::Field {
                plane,
                channel: f.channel,
                problem,
            });
        }
    }
    let (w, h) = (width.div_ceil(sx), height.div_ceil(sy));
    let row = w
        .checked_mul(layout.block_bytes)
        .ok_or(DescriptorError::Overflow { plane })?;
    if layout.row_pitch < row {
        return Err(DescriptorError::RowPitch {
            plane,
            pitch: layout.row_pitch,
            row,
        });
    }
    let max = layout
        .row_pitch
        .checked_mul(h)
        .ok_or(DescriptorError::Overflow { plane })?;
    // The last row can end without its padding.
    let min = max - (layout.row_pitch - row);
    if !(min..=max).contains(&len) {
        return Err(DescriptorError::DataLength {
            plane,
            len,
            min,
            max,
        });
    }
    Ok(())
}

fn check_colour(c: &Colour, kind: SampleKind, ycbcr: bool) -> Result<(), DescriptorError> {
    if kind == SampleKind::Float && c.range.is_some() {
        return Err(DescriptorError::RangeOfFloat);
    }
    if !ycbcr && c.matrix.is_some() {
        return Err(DescriptorError::MatrixOfRgb);
    }
    let mut xy = Vec::new();
    if let Some(Primaries::Custom { red, green, blue }) = c.primaries {
        xy.extend([red, green, blue]);
    }
    if let Some(WhitePoint::Custom(w)) = c.white {
        xy.push(w);
    }
    if !xy
        .iter()
        .all(|&[x, y]| x.is_finite() && y.is_finite() && x >= 0.0 && y > 0.0 && x + y <= 1.0)
    {
        return Err(DescriptorError::Chromaticity);
    }
    match (c.luminance, c.transfer) {
        (Some(LuminanceScale::Absolute { nits_per_unit: n }), _) if !(n.is_finite() && n > 0.0) => {
            Err(DescriptorError::Luminance)
        }
        // ST 2084 fixes the scale; another value contradicts the transfer.
        (Some(LuminanceScale::Relative), Some(Transfer::Pq)) => Err(DescriptorError::Luminance),
        (Some(LuminanceScale::Absolute { nits_per_unit }), Some(Transfer::Pq))
            if nits_per_unit.to_bits() != 10_000f64.to_bits() =>
        {
            Err(DescriptorError::Luminance)
        }
        _ => Ok(()),
    }
}

fn mask(bits: u32) -> u128 {
    if bits >= 128 {
        u128::MAX
    } else {
        (1u128 << bits) - 1
    }
}

impl<'a> CaptureView<'a> {
    #[must_use]
    pub fn capture(&self) -> &'a Capture {
        self.capture
    }

    fn field(&self, channel: Channel) -> Option<(usize, Field)> {
        self.fields[channel.index()]
    }

    fn is_ycbcr(&self) -> bool {
        self.field(Channel::Y).is_some()
    }

    /// R, G, B, or Y, Cb, Cr.
    #[must_use]
    pub fn colour_channels(&self) -> [Channel; 3] {
        if self.is_ycbcr() {
            [Channel::Y, Channel::Cb, Channel::Cr]
        } else {
            [Channel::R, Channel::G, Channel::B]
        }
    }

    fn check_channels(&self) -> Result<(), DescriptorError> {
        let has = |c| self.field(c).is_some();
        let rgb = [Channel::R, Channel::G, Channel::B];
        let ycc = [Channel::Y, Channel::Cb, Channel::Cr];
        let valid = (rgb.iter().all(|&c| has(c)) && !ycc.iter().any(|&c| has(c)))
            || (ycc.iter().all(|&c| has(c)) && !rgb.iter().any(|&c| has(c)));
        if !valid {
            return Err(DescriptorError::Channels);
        }
        let mut colour = Channel::ALL
            .into_iter()
            .filter(|&c| c != Channel::A)
            .filter_map(|c| self.field(c))
            .map(|(_, f)| (f.kind, f.bits));
        let first = colour.next();
        if colour.any(|s| Some(s) != first) {
            return Err(DescriptorError::MixedSamples);
        }
        Ok(())
    }

    /// Call after [`Self::check_channels`].
    fn colour_kind(&self) -> SampleKind {
        let c = if self.is_ycbcr() {
            Channel::Y
        } else {
            Channel::R
        };
        self.field(c).expect("checked channels").1.kind
    }

    /// The stored sample of `channel` at pixel `(x, y)`. A subsampled
    /// channel gives the block that holds the pixel. `None`: no such
    /// channel, or a position outside the image.
    #[must_use]
    pub fn sample(&self, channel: Channel, x: usize, y: usize) -> Option<Sample> {
        let d = &self.capture.descriptor;
        if x >= d.width || y >= d.height {
            return None;
        }
        let (plane, field) = self.field(channel)?;
        let layout = &d.planes[plane];
        let [sx, sy] = layout.subsampling;
        let start = (y / sy) * layout.row_pitch + (x / sx) * layout.block_bytes;
        let bytes = &self.capture.planes[plane][start..start + layout.block_bytes];
        let block = match layout.byte_order {
            ByteOrder::Little => bytes
                .iter()
                .rev()
                .fold(0u128, |v, &b| v << 8 | u128::from(b)),
            ByteOrder::Big => bytes.iter().fold(0u128, |v, &b| v << 8 | u128::from(b)),
        };
        // Validation limits a field to 32 bits, so the casts keep every bit.
        #[allow(clippy::cast_possible_truncation)]
        let bits = ((block >> field.shift) & mask(field.bits)) as u32;
        Some(match (field.kind, field.bits) {
            (SampleKind::Uint, _) => Sample::Uint(bits),
            #[allow(clippy::cast_possible_truncation)]
            (SampleKind::Float, 16) => Sample::Float(half::f16::from_bits(bits as u16).to_f32()),
            (SampleKind::Float, _) => Sample::Float(f32::from_bits(bits)),
        })
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
