//! Output driver API: display reports, capability rules and asynchronous
//! presentation. See `doc/design/output_driver/overview.md`.
//!
//! The module has no GUI, server or executor dependency. Drivers and hosts
//! live in other crates.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use crate::image::Image;
use crate::metadata::{Hdr10Metadata, LightLevel};
use crate::signal::{BitDepth, Encoding, Format};

/// Tri-state feature report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Yes,
    /// Not supported; the reason is shown by the host.
    No(String),
    /// The driver cannot detect the state; the reason is shown by the host.
    Unknown(String),
}

impl Support {
    /// Warning text for a feature that does not reject the request.
    /// `None` for `Yes`.
    #[must_use]
    pub fn warning(&self, feature: &str) -> Option<String> {
        match self {
            Self::Yes => None,
            Self::No(reason) => Some(format!("{feature}: no ({reason})")),
            Self::Unknown(reason) => Some(format!("{feature}: unknown ({reason})")),
        }
    }
}

/// Display identity within one driver.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DisplayId {
    pub driver: &'static str,
    pub key: String,
}

impl fmt::Display for DisplayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.driver, self.key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayInfo {
    pub id: DisplayId,
    /// Screen name, or client name and address.
    pub name: String,
    /// Native pixels; the size of the reference table.
    pub size: (usize, usize),
}

/// Features available with the current display and OS settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// A depth absent from the list is not available.
    pub depths: Vec<(BitDepth, DepthSupport)>,
    /// An encoding absent from the list is not accepted.
    pub encodings: Vec<(Encoding, EncodingSupport)>,
    /// ST 2086 and CTA-861.3 metadata reach the display.
    pub hdr_metadata: Support,
    /// One pattern pixel is one display pixel.
    pub pixel_exact: Support,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthSupport {
    /// The driver can open this output path.
    pub available: Support,
    /// For example "HDR10 swap chain" or "16-bit PNG, cICP PQ".
    pub path: String,
    /// How the path changes codes; it selects the reference table marks.
    pub conversion: Conversion,
    /// The codes reach the wire or the panel without change.
    pub exact: Support,
}

/// Code conversion of an output path: only the known per-level limits of
/// the driver's own conversion. OS and display changes are in `exact`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Conversion {
    /// Codes go to the output unchanged, for example the HDR10 swap chain.
    Direct,
    /// FP16 scRGB with 1.0 = 80 cd/m². Some levels do not decode back to
    /// their codes; see [`crate::output::scrgb_is_exact`].
    ScRgb80,
    /// Full-range 16-bit PNG samples. Narrow-range components outside the
    /// nominal range clip; see [`Format::is_nominal`].
    Png16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodingSupport {
    /// The driver accepts frames in this encoding.
    pub input: Support,
    /// The output preserves the intended transfer and range.
    pub interpretation: Support,
    /// The driver can calculate absolute content light levels.
    pub auto_light_level: Support,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gamut {
    Srgb,
    P3,
    Rec2020,
}

/// Current settings of the OS, the driver and the browser. Read-only.
/// `None`: the driver cannot read the value.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeSettings {
    pub hdr_on: Option<bool>,
    pub gamut: Option<Gamut>,
    /// Bits per component.
    pub wire_depth: Option<u8>,
    /// cd/m².
    pub peak_nits: Option<f64>,
    pub min_nits: Option<f64>,
    pub sdr_white_nits: Option<f64>,
    /// Display pixels per layout pixel.
    pub pixel_ratio: f64,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    pub capabilities: Capabilities,
    pub mode: ModeSettings,
}

/// An opened output path. The host renders the pattern at `size`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    pub display: DisplayId,
    pub size: (usize, usize),
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContentLightLevel {
    /// The driver calculates `MaxCLL` and `MaxFALL` from the signal it sends.
    FromFrame,
    /// cd/m².
    Explicit { max_cll: f64, max_fall: f64 },
}

/// HDR10 static metadata settings from the host. The driver resolves it to
/// [`Hdr10Metadata`] with [`Hdr10Request::resolve`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hdr10Request {
    /// CIE xy of red, green and blue.
    pub primaries: [[f64; 2]; 3],
    pub white: [f64; 2],
    /// cd/m².
    pub max_mastering_nits: f64,
    pub min_mastering_nits: f64,
    pub light_level: ContentLightLevel,
}

impl Hdr10Request {
    /// Calls `from_frame` only for [`ContentLightLevel::FromFrame`]; explicit
    /// values bypass the calculation. `from_frame` gives the result of
    /// [`crate::metadata::light_level`] for the driver's effective PQ samples.
    #[must_use]
    pub fn resolve(&self, from_frame: impl FnOnce() -> LightLevel) -> Hdr10Metadata {
        let (max_cll, max_fall) = match self.light_level {
            ContentLightLevel::FromFrame => {
                let l = from_frame();
                (l.max_cll, l.max_fall)
            }
            ContentLightLevel::Explicit { max_cll, max_fall } => (max_cll, max_fall),
        };
        Hdr10Metadata {
            primaries: self.primaries,
            white: self.white,
            max_mastering_nits: self.max_mastering_nits,
            min_mastering_nits: self.min_mastering_nits,
            max_cll,
            max_fall,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Frame<'a> {
    /// Code values; the size is the surface size.
    pub image: &'a Image,
    pub format: Format,
    /// `None`: no metadata.
    pub metadata: Option<&'a Hdr10Request>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputError {
    Unsupported { feature: String, reason: String },
    Disconnected,
    Timeout,
    Platform(String),
}

impl fmt::Display for OutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { feature, reason } => write!(f, "{feature} not supported: {reason}"),
            Self::Disconnected => f.write_str("display disconnected"),
            Self::Timeout => f.write_str("driver acknowledgement timed out"),
            Self::Platform(msg) => write!(f, "platform error: {msg}"),
        }
    }
}

impl std::error::Error for OutputError {}

/// Default measurement delay of each host, in ms. The host waits this long
/// after `present` succeeds; the driver timeout excludes it.
/// **Unverified:** sufficient for a particular display or probe.
pub const DEFAULT_DELAY_MS: u32 = 1000;

/// Resolves to a status line after driver acknowledgement.
///
/// No `Send` bound: `QRhi` must stay on the Qt GUI thread. Dropping the future
/// cancels its wait and callbacks.
pub type PresentFuture<'a> = Pin<Box<dyn Future<Output = Result<String, OutputError>> + 'a>>;

pub trait OutputDriver {
    fn id(&self) -> &'static str;
    fn displays(&mut self) -> Vec<DisplayInfo>;
    /// Must not wait for network replies or user actions.
    ///
    /// # Errors
    /// Unknown or disconnected display, or a platform failure.
    fn query(&mut self, display: &DisplayId) -> Result<Query, OutputError>;
    /// Opens an available surface for `depth` and gives its pixel size.
    /// The host renders the pattern at this size. Use [`check_open`].
    ///
    /// # Errors
    /// Unavailable depth, unknown or disconnected display, or a platform failure.
    fn open(&mut self, display: &DisplayId, depth: BitDepth) -> Result<Surface, OutputError>;
    /// Resolves after driver acknowledgement, or an error. Yields while
    /// waiting. Physical presentation is not confirmed. Use [`check_frame`].
    fn present<'a>(&'a mut self, surface: &'a Surface, frame: &'a Frame<'a>) -> PresentFuture<'a>;
    fn close(&mut self, display: &DisplayId);
}

/// Limits of the selected output path for one frame format. The reference
/// table shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLimits {
    /// `None`: the driver cannot open or present the format. The table then
    /// makes no claim of exact output.
    pub path: Option<OutputPath>,
    /// Warnings of [`check_open`] and [`check_frame`], or the rejection reason.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPath {
    pub description: String,
    pub conversion: Conversion,
}

impl OutputLimits {
    /// Applies [`check_open`] and [`check_frame`] to the reported path.
    #[must_use]
    pub fn for_frame(caps: &Capabilities, format: Format, metadata: Option<&Hdr10Request>) -> Self {
        let checked = check_open(caps, format.depth).and_then(|mut warnings| {
            warnings.extend(check_frame(caps, format, metadata)?);
            Ok(warnings)
        });
        match checked {
            Ok(warnings) => Self {
                // Always found: check_open rejects an unreported depth.
                path: caps
                    .depths
                    .iter()
                    .find(|(k, _)| *k == format.depth)
                    .map(|(_, d)| OutputPath {
                        description: d.path.clone(),
                        conversion: d.conversion,
                    }),
                warnings,
            },
            Err(e) => Self::unavailable(e.to_string()),
        }
    }

    /// No usable path, for example when `query` fails.
    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            path: None,
            warnings: vec![reason.into()],
        }
    }
}

fn unsupported(feature: impl Into<String>, reason: impl Into<String>) -> OutputError {
    OutputError::Unsupported {
        feature: feature.into(),
        reason: reason.into(),
    }
}

/// Applies the `open` rules. Returns the warnings for the status line.
///
/// # Errors
/// [`OutputError::Unsupported`] if `available` is `No` or the depth is not
/// reported.
pub fn check_open(caps: &Capabilities, depth: BitDepth) -> Result<Vec<String>, OutputError> {
    let feature = depth.label();
    let Some((_, d)) = caps.depths.iter().find(|(k, _)| *k == depth) else {
        return Err(unsupported(feature, "not reported by the driver"));
    };
    if let Support::No(reason) = &d.available {
        return Err(unsupported(feature, reason.clone()));
    }
    let path = format!("{feature} ({})", d.path);
    Ok([
        d.available.warning(&path),
        d.exact.warning(&format!("{path} exact codes")),
        caps.pixel_exact.warning("pixel-exact layout"),
    ]
    .into_iter()
    .flatten()
    .collect())
}

/// Applies the `present` rules for the encoding and metadata of a frame.
/// Returns the warnings for the status line and the reference table.
///
/// `interpretation = No` only warns: `QRhi` scope use sends HLG codes with PQ
/// signalling.
///
/// # Errors
/// [`OutputError::Unsupported`] if the encoding is not reported or its
/// `input` is `No`, if metadata is requested and `hdr_metadata` is `No`, or
/// if [`ContentLightLevel::FromFrame`] is requested and `auto_light_level`
/// is `No`.
pub fn check_frame(
    caps: &Capabilities,
    format: Format,
    metadata: Option<&Hdr10Request>,
) -> Result<Vec<String>, OutputError> {
    let feature = format.encoding.label();
    let Some((_, e)) = caps.encodings.iter().find(|(k, _)| *k == format.encoding) else {
        return Err(unsupported(feature, "not reported by the driver"));
    };
    if let Support::No(reason) = &e.input {
        return Err(unsupported(feature, reason.clone()));
    }
    let mut warnings: Vec<String> = [
        e.input.warning(&format!("{feature} input")),
        e.interpretation
            .warning(&format!("{feature} interpretation")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if let Some(request) = metadata {
        if let Support::No(reason) = &caps.hdr_metadata {
            return Err(unsupported("HDR10 metadata", reason.clone()));
        }
        warnings.extend(caps.hdr_metadata.warning("HDR10 metadata"));
        if request.light_level == ContentLightLevel::FromFrame {
            let light = format!("{feature} automatic light level");
            if let Support::No(reason) = &e.auto_light_level {
                return Err(unsupported(light, reason.clone()));
            }
            warnings.extend(e.auto_light_level.warning(&light));
        }
    }
    Ok(warnings)
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
