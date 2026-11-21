//! Records of the console IDD callbacks (`IddCx` 1.10) and their replay.
//!
//! An instrumented IDD logs each callback and each acquired frame in the
//! order of its own processing. [`replay`] runs these records through an
//! explicit state machine and gives the state of each frame: one transform
//! generation, the committed wire mode, the surface colour space and the
//! selected HDR10 metadata. [`FrameState::model`] turns that state into a
//! [`Model`]. No installed driver is necessary.
//!
//! The records keep the values as received, also those of disabled stages.

use std::fmt;
use std::sync::Arc;

use nitpick_compare::colour;

use crate::{
    Affine, Calibration, Lut, Model, ModelError, Primaries, Sdr, Source, SourceEncoding, Wire,
    WireEncoding, finite, scale,
};

/// Entries of the 3×4 transform LUT.
pub const LUT_LEN: usize = 4096;
/// Entries of each channel of the legacy gamma ramp.
pub const LEGACY_LUT_LEN: usize = 256;

/// `IDDCX_GAMMARAMP_3X4_COLORSPACE_TRANSFORM`.
#[derive(Debug, Clone, PartialEq)]
pub struct Transform3x4 {
    /// `MatrixEnabled`: the matrix and the scalar apply.
    pub matrix_enabled: bool,
    /// `ColorMatrix3x4`: three rows of four, in CIE XYZ.
    pub matrix: [[f32; 4]; 3],
    /// `ScalarMultiplier`.
    pub scalar: f32,
    /// `LutEnabled`.
    pub lut_enabled: bool,
    /// `LookupTable1D`: [`LUT_LEN`] entries of red, green and blue, in the
    /// wire colour space.
    pub lut: Vec<[f32; 3]>,
}

/// `IDARG_IN_SET_GAMMARAMP`, by `IDDCX_GAMMARAMP_TYPE`.
#[derive(Debug, Clone, PartialEq)]
pub enum GammaRamp {
    /// `IDDCX_GAMMARAMP_TYPE_UNINITIALIZED`.
    Uninitialized,
    /// `IDDCX_GAMMARAMP_TYPE_DEFAULT`.
    Default,
    /// `IDDCX_GAMMARAMP_TYPE_RGB256x3x16`: red, green and blue.
    Rgb256x3x16(Box<[[u16; LEGACY_LUT_LEN]; 3]>),
    /// `IDDCX_GAMMARAMP_TYPE_3x4_COLORSPACE_TRANSFORM`.
    Transform3x4(Transform3x4),
}

/// `IDDCX_HDR10_METADATA`, as stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hdr10Metadata {
    /// CIE x and y of red, green, blue and the white point; units of
    /// 0.00002.
    pub primaries: [[u16; 2]; 3],
    pub white_point: [u16; 2],
    /// cd/m².
    pub max_mastering_nits: u16,
    /// Units of 0.0001 cd/m².
    pub min_mastering: u16,
    /// cd/m².
    pub max_content_light_level: u16,
    /// cd/m².
    pub max_frame_average_light_level: u16,
}

/// `IDARG_IN_MONITOR_SET_DEFAULT_HDR_METADATA`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultHdrMetadata {
    /// `IDDCX_HDRMETADATA_TYPE_UNINITIALIZED`.
    Uninitialized,
    /// `IDDCX_HDRMETADATA_TYPE_HDR10`.
    Hdr10(Hdr10Metadata),
}

/// `IDDCX_METADATA2::Hdr10FrameMetaData`, with its valid flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameHdrMetadata {
    /// `IDDCX_METADATA2_VALID_FLAGS_HDR10METADATA` is clear.
    Absent,
    /// `IDDCX_HDR10_FRAME_METADATA_TYPE_UNINITIALIZED`.
    Uninitialized,
    /// `IDDCX_HDR10_FRAME_METADATA_TYPE_DEFAULT`.
    Default,
    /// `IDDCX_HDR10_FRAME_METADATA_TYPE_UNCHANGED`.
    Unchanged,
    /// `IDDCX_HDR10_FRAME_METADATA_TYPE_NEW`.
    New(Hdr10Metadata),
}

/// `IDDCX_COLOR_SPACE` of `IDDCX_WIRE_FORMAT_INFO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireColourSpace {
    Uninitialized,
    G22P709,
    G2084P2020,
    G22P709Wcg,
}

/// The committed wire format of the path (`IDDCX_PATH2::WireFormatInfo`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireMode {
    pub colour_space: WireColourSpace,
    /// RGB bits per component. YCbCr wire encodings are not modelled.
    pub depth: u8,
}

/// `IDDCX_METADATA2::SurfaceColorSpace`, a `DXGI_COLOR_SPACE_TYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceColourSpace {
    /// `DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709`.
    RgbFullG22P709,
    /// `DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709`: scRGB.
    RgbFullG10P709,
    /// `DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020`.
    RgbFullG2084P2020,
    /// Another `DXGI_COLOR_SPACE_TYPE` value.
    Other(u32),
}

/// A frame from `IddCxSwapChainReleaseAndAcquireBuffer2`, logged where the
/// driver takes its transform for the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRecord {
    /// `PresentationFrameNumber`.
    pub frame_number: u32,
    pub surface: SurfaceColourSpace,
    /// `SdrWhiteLevel`, cd/m².
    pub sdr_white_nits: u32,
    pub hdr: FrameHdrMetadata,
    /// The transform generation that the driver used for the frame.
    pub generation: u64,
}

/// One record, in the order of the driver's processing.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// `EVT_IDD_CX_MONITOR_SET_GAMMA_RAMP`.
    SetGammaRamp(GammaRamp),
    /// `EVT_IDD_CX_MONITOR_SET_DEFAULT_HDR_METADATA`.
    SetDefaultHdrMetadata(DefaultHdrMetadata),
    /// The path of the monitor in `EVT_IDD_CX_ADAPTER_COMMIT_MODES2`.
    CommitMode(WireMode),
    Frame(FrameRecord),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RampKind {
    Default,
    Rgb256x3x16,
    Transform3x4,
}

/// The validated state of one gamma callback. Each callback is a new
/// generation, also with the same values.
#[derive(Debug, Clone, PartialEq)]
pub struct TransformState {
    generation: u64,
    kind: RampKind,
    /// The enabled 3×4 matrix and its scalar.
    matrix: Option<([[f64; 4]; 3], f64)>,
    lut: Option<Lut>,
}

impl TransformState {
    fn new(generation: u64, ramp: &GammaRamp) -> Result<Self, IddError> {
        let (kind, matrix, lut) = match ramp {
            GammaRamp::Uninitialized => return Err(IddError::Uninitialized(Field::GammaRamp)),
            // The default ramp is taken as identity, as the default GDI
            // ramp is. Microsoft does not state it for the 3×4 transform.
            GammaRamp::Default => (RampKind::Default, None, None),
            // Taken as a LUT in the encoded wire domain, as the 3×4 LUT is.
            GammaRamp::Rgb256x3x16(ramp) => {
                let lut = Lut::new(ramp.map(|c| {
                    c.iter()
                        .map(|&v| f64::from(v) / f64::from(u16::MAX))
                        .collect()
                }))?;
                (RampKind::Rgb256x3x16, None, Some(lut))
            }
            GammaRamp::Transform3x4(t) => {
                // The C array has a fixed length; another length is a
                // defective record, also with the LUT disabled.
                if t.lut.len() != LUT_LEN {
                    return Err(IddError::LutLength(t.lut.len()));
                }
                // A disabled stage can hold any values; they are not read.
                let matrix = if t.matrix_enabled {
                    let m = t.matrix.map(|r| r.map(f64::from));
                    let s = f64::from(t.scalar);
                    finite(m.as_flattened())?;
                    finite(&[s])?;
                    Some((m, s))
                } else {
                    None
                };
                let lut = if t.lut_enabled {
                    let channel = |k: usize| t.lut.iter().map(|e| f64::from(e[k])).collect();
                    Some(Lut::new([channel(0), channel(1), channel(2)])?)
                } else {
                    None
                };
                (RampKind::Transform3x4, matrix, lut)
            }
        };
        Ok(Self {
            generation,
            kind,
            matrix,
            lut,
        })
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn kind(&self) -> RampKind {
        self.kind
    }

    /// The enabled 3×4 matrix, rows of four.
    #[must_use]
    pub fn matrix(&self) -> Option<&[[f64; 4]; 3]> {
        self.matrix.as_ref().map(|(m, _)| m)
    }

    /// The scalar of the enabled matrix.
    #[must_use]
    pub fn scalar(&self) -> Option<f64> {
        self.matrix.map(|(_, s)| s)
    }

    #[must_use]
    pub fn lut(&self) -> Option<&Lut> {
        self.lut.as_ref()
    }

    /// # Errors
    /// An offset unit that is not finite and above 0.
    pub fn calibration(&self, offset_nits_per_unit: f64) -> Result<Calibration, ModelError> {
        scale(offset_nits_per_unit)?;
        let affine = self
            .matrix
            .map(|(m, s)| Affine::new(m, s, offset_nits_per_unit))
            .transpose()?;
        Ok(Calibration::idd(affine, self.lut.clone()))
    }
}

/// The state that applies to one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameState {
    pub frame_number: u32,
    /// `None` before the first gamma callback.
    pub transform: Option<Arc<TransformState>>,
    pub wire: WireMode,
    pub surface: SurfaceColourSpace,
    /// Recorded only: the model does not apply it to the surface.
    pub sdr_white_nits: u32,
    /// The selected HDR10 metadata; `None` without valid metadata.
    pub hdr: Option<Hdr10Metadata>,
}

/// Values that the IDD records do not state. Qualification of the runner
/// must give each one; there are no defaults.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IddContract {
    /// Transfer and white of a [`SurfaceColourSpace::RgbFullG22P709`]
    /// surface.
    pub surface_sdr: Option<Sdr>,
    /// Transfer and white of a G22 wire.
    pub wire_sdr: Option<Sdr>,
    /// Primaries of a [`WireColourSpace::G22P709Wcg`] wire.
    pub wcg_wire_primaries: Option<Primaries>,
    /// cd/m² of one unit of the 3×4 offset column.
    pub offset_nits_per_unit: f64,
}

impl FrameState {
    /// The display model of this frame: the surface colour space for the
    /// source, the transform of the frame's generation and the committed
    /// wire colour space.
    ///
    /// # Errors
    /// No transform, an unsupported surface, a value missing from the
    /// contract or an invalid model value.
    pub fn model(&self, contract: &IddContract) -> Result<Model, IddError> {
        let transform = self.transform.as_ref().ok_or(IddError::NoTransform)?;
        let calibration = transform.calibration(contract.offset_nits_per_unit)?;
        Ok(Model::new(
            self.source(contract)?,
            calibration,
            self.wire_model(contract)?,
        )?)
    }

    fn source(&self, contract: &IddContract) -> Result<Source, IddError> {
        let (primaries, encoding) = match self.surface {
            // scRGB is absolute: 1.0 is 80 cd/m² for any SDR white. The
            // SDR white is for SDR content such as a separate cursor; a
            // second scale here would apply it two times.
            SurfaceColourSpace::RgbFullG10P709 => (
                Primaries::BT709,
                SourceEncoding::Linear {
                    nits_per_unit: colour::SCRGB_WHITE_NITS,
                },
            ),
            SurfaceColourSpace::RgbFullG2084P2020 => (Primaries::BT2020, SourceEncoding::Pq),
            SurfaceColourSpace::RgbFullG22P709 => (
                Primaries::BT709,
                SourceEncoding::Sdr(
                    contract
                        .surface_sdr
                        .ok_or(IddError::Missing(Field::SurfaceSdr))?,
                ),
            ),
            SurfaceColourSpace::Other(v) => return Err(IddError::UnsupportedSurface(v)),
        };
        Ok(Source {
            primaries,
            encoding,
        })
    }

    fn wire_model(&self, contract: &IddContract) -> Result<Wire, IddError> {
        let sdr = || contract.wire_sdr.ok_or(IddError::Missing(Field::WireSdr));
        let (primaries, encoding) = match self.wire.colour_space {
            WireColourSpace::Uninitialized => {
                return Err(IddError::Uninitialized(Field::WireColourSpace));
            }
            WireColourSpace::G2084P2020 => (Primaries::BT2020, WireEncoding::Pq),
            WireColourSpace::G22P709 => (Primaries::BT709, WireEncoding::Sdr(sdr()?)),
            // The name states P709, but the SDR WCG wire primaries are
            // not verified, so the contract gives them.
            WireColourSpace::G22P709Wcg => (
                contract
                    .wcg_wire_primaries
                    .ok_or(IddError::Missing(Field::WcgWirePrimaries))?,
                WireEncoding::Sdr(sdr()?),
            ),
        };
        Ok(Wire {
            primaries,
            encoding,
            depth: self.wire.depth,
        })
    }
}

/// The replay state machine. Each event either changes the state as a
/// whole or gives an error and leaves it unchanged.
#[derive(Debug, Clone, Default)]
pub struct Replay {
    /// The count of gamma callbacks; 0 before the first.
    generation: u64,
    transform: Option<Arc<TransformState>>,
    mode: Option<WireMode>,
    default_hdr: Option<Hdr10Metadata>,
    /// The metadata of the last frame since the last mode commit.
    last_hdr: Option<Hdr10Metadata>,
}

impl Replay {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Applies one event. Gives the state of a frame event.
    ///
    /// # Errors
    /// See [`IddError`].
    pub fn apply(&mut self, event: &Event) -> Result<Option<FrameState>, IddError> {
        match event {
            Event::SetGammaRamp(ramp) => {
                let generation = self.generation + 1;
                self.transform = Some(Arc::new(TransformState::new(generation, ramp)?));
                self.generation = generation;
                Ok(None)
            }
            Event::SetDefaultHdrMetadata(DefaultHdrMetadata::Uninitialized) => {
                Err(IddError::Uninitialized(Field::DefaultHdrMetadata))
            }
            Event::SetDefaultHdrMetadata(DefaultHdrMetadata::Hdr10(m)) => {
                self.default_hdr = Some(*m);
                Ok(None)
            }
            Event::CommitMode(mode) => {
                if mode.colour_space == WireColourSpace::Uninitialized {
                    return Err(IddError::Uninitialized(Field::WireColourSpace));
                }
                self.mode = Some(*mode);
                // Microsoft does not state whether "unchanged" refers to a
                // frame before the commit; the replay does not allow it.
                self.last_hdr = None;
                Ok(None)
            }
            Event::Frame(f) => self.frame(f).map(Some),
        }
    }

    fn frame(&mut self, f: &FrameRecord) -> Result<FrameState, IddError> {
        let wire = self.mode.ok_or(IddError::NoMode)?;
        if f.generation != self.generation {
            return Err(IddError::Generation {
                recorded: f.generation,
                replayed: self.generation,
            });
        }
        let hdr = match f.hdr {
            FrameHdrMetadata::Absent => None,
            FrameHdrMetadata::Uninitialized => {
                return Err(IddError::Uninitialized(Field::FrameHdrMetadata));
            }
            FrameHdrMetadata::Default => Some(self.default_hdr.ok_or(IddError::NoDefaultMetadata)?),
            // The value used with the last frame, also if the default
            // changed since then.
            FrameHdrMetadata::Unchanged => Some(self.last_hdr.ok_or(IddError::NoPreviousMetadata)?),
            // New metadata does not replace the default.
            FrameHdrMetadata::New(m) => Some(m),
        };
        // A frame without metadata leaves nothing for "unchanged".
        self.last_hdr = hdr;
        Ok(FrameState {
            frame_number: f.frame_number,
            transform: self.transform.clone(),
            wire,
            surface: f.surface,
            sdr_white_nits: f.sdr_white_nits,
            hdr,
        })
    }
}

/// Replays `events` from the initial state and gives the state of each
/// frame, in order.
///
/// # Errors
/// The first event that fails, with its index.
pub fn replay(events: &[Event]) -> Result<Vec<FrameState>, ReplayError> {
    let mut state = Replay::new();
    let mut frames = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if let Some(f) = state
            .apply(event)
            .map_err(|error| ReplayError { index, error })?
        {
            frames.push(f);
        }
    }
    Ok(frames)
}

/// A record field or a contract value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    GammaRamp,
    DefaultHdrMetadata,
    FrameHdrMetadata,
    WireColourSpace,
    SurfaceSdr,
    WireSdr,
    WcgWirePrimaries,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IddError {
    /// An `UNINITIALIZED` type or colour space.
    Uninitialized(Field),
    /// A 3×4 LUT without [`LUT_LEN`] entries.
    LutLength(usize),
    /// A frame before the first mode commit.
    NoMode,
    /// The driver used another transform generation than the replay.
    Generation { recorded: u64, replayed: u64 },
    /// Default metadata selected before any was set.
    NoDefaultMetadata,
    /// Unchanged metadata selected without metadata on the last frame
    /// since the last mode commit.
    NoPreviousMetadata,
    /// A frame before the first gamma callback.
    NoTransform,
    /// A surface colour space outside the model.
    UnsupportedSurface(u32),
    /// A contract value that this frame needs.
    Missing(Field),
    /// An invalid value in an enabled stage or the model.
    Model(ModelError),
}

impl From<ModelError> for IddError {
    fn from(e: ModelError) -> Self {
        Self::Model(e)
    }
}

impl fmt::Display for IddError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uninitialized(field) => write!(f, "{field:?} is uninitialized"),
            Self::LutLength(n) => write!(f, "a 3x4 LUT of {n} entries, not {LUT_LEN}"),
            Self::NoMode => f.write_str("a frame before the first mode commit"),
            Self::Generation { recorded, replayed } => write!(
                f,
                "the frame used transform generation {recorded}, the replay has {replayed}"
            ),
            Self::NoDefaultMetadata => f.write_str("default HDR metadata before any was set"),
            Self::NoPreviousMetadata => {
                f.write_str("unchanged HDR metadata without metadata on the last frame")
            }
            Self::NoTransform => f.write_str("a frame before the first gamma callback"),
            Self::UnsupportedSurface(v) => {
                write!(f, "the surface colour space {v} is outside the model")
            }
            Self::Missing(field) => write!(f, "the contract has no {field:?}"),
            Self::Model(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for IddError {}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayError {
    /// Index of the failed event.
    pub index: usize,
    pub error: IddError,
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event {}: {}", self.index, self.error)
    }
}

impl std::error::Error for ReplayError {}

#[cfg(test)]
#[path = "idd_tests.rs"]
mod tests;
