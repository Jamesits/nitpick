//! Requests and events of the test-host protocol: one JSON object per line.
//! See "Host boundary" in `doc/design/output_driver/testing.md`.

use nitpick_core::driver::{
    Capabilities, Conversion, Gamut, ModeSettings, OutputError, Query, Support,
};
use nitpick_fixtures::MetadataRequest;
use serde::{Deserialize, Deserializer, Serialize};

/// Version in the first event, [`Event::Hello`].
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case", deny_unknown_fields)]
// Empty braces, not unit variants: serde ignores unknown fields of a unit
// variant in an internally tagged enum.
pub enum Request {
    Displays {},
    /// Reads the display state again; [`Event::Displays`] follows the read.
    Refresh {},
    Query {
        display: String,
    },
    Open {
        display: String,
        /// Bits per component.
        depth: u32,
    },
    Show {
        /// Greater than the frame of every earlier `show`.
        frame: u64,
        /// Case ID of an output fixture.
        fixture: String,
        /// Measurement delay; absent: the host default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delay_ms: Option<u32>,
        /// Absent: the request of the fixture. `null`: no metadata.
        #[allow(clippy::option_option)]
        #[serde(
            default,
            deserialize_with = "present",
            skip_serializing_if = "Option::is_none"
        )]
        metadata: Option<Option<MetadataRequest>>,
    },
    Capture {
        frame: u64,
    },
    Close {},
    Quit {},
}

/// Separates an explicit `null` from an absent field.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

/// Parses one input line. `Ok(None)` for a blank line.
///
/// # Errors
/// The line is not a valid request; the text is for [`ErrorKind::Malformed`].
pub fn parse_request(line: &str) -> Result<Option<Request>, String> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    serde_json::from_str(line)
        .map(Some)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Always the first event.
    Hello {
        protocol: u32,
        driver: String,
    },
    /// Browser host: the service URL.
    Listening {
        url: String,
    },
    /// Browser host: the code for the client.
    PairingCode {
        code: String,
    },
    /// A change reported by the driver side, outside the requests.
    Display {
        state: DisplayState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    Displays {
        displays: Vec<DisplayEntry>,
    },
    Query {
        display: String,
        capabilities: CapabilitiesReport,
        mode: ModeReport,
    },
    /// Each `open` starts a new generation; `stale` and `closed` end it.
    Surface {
        generation: u64,
        state: SurfaceState,
        display: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<[usize; 2]>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause: Option<Cause>,
    },
    /// The driver acknowledged the frame; the measurement delay starts.
    Submitted {
        frame: u64,
        generation: u64,
        delay_ms: u32,
        /// The status line of the driver.
        status: String,
    },
    /// The measurement delay of the frame ended.
    Ready {
        frame: u64,
        delay_ms: u32,
    },
    /// The host ended the presentation or the delay of the frame.
    Cancelled {
        frame: u64,
        cause: Cause,
    },
    Error {
        /// The frame of a `show` or `capture` request, or of a presentation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame: Option<u64>,
        kind: ErrorKind,
        message: String,
    },
    Captured {
        frame: u64,
        result: CaptureOutcome,
        reason: String,
    },
    /// Always the last event.
    Stopped {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl Event {
    /// One output line without the line end.
    ///
    /// # Panics
    /// Not for these types: they have no map keys and no failing
    /// `Serialize` implementation.
    #[must_use]
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("events have only serializable fields")
    }

    #[must_use]
    pub fn error(frame: Option<u64>, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::Error {
            frame,
            kind,
            message: message.into(),
        }
    }

    /// The `error` event of a driver error.
    #[must_use]
    pub fn output_error(frame: Option<u64>, e: &OutputError) -> Self {
        Self::error(frame, ErrorKind::of(e), e.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayState {
    Connected,
    Authenticated,
    Ready,
    /// The surface geometry of the display changed.
    Changed,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceState {
    Open,
    /// The display changed; `show` fails until the next `open`.
    Stale,
    Closed,
}

/// Why the host ended a frame or a surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// A new `show`.
    Replaced,
    /// A new `open`.
    Opened,
    /// The `close` request.
    Closed,
    /// The display changed.
    Changed,
    /// The display disconnected or the surface closed outside the host.
    Disconnected,
    /// `open` failed; the earlier surface is closed.
    OpenFailed,
    /// `quit`, the end of the input or a service stop.
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Not a valid request line.
    Malformed,
    UnknownDisplay,
    UnknownFixture,
    InvalidMetadata,
    /// The frame ID is not greater than the earlier ones.
    FrameOrder,
    NoSurface,
    StaleSurface,
    FixtureTooLarge,
    /// `capture` of a frame that is not the current acknowledged frame.
    StaleFrame,
    /// The measurement delay timer stopped.
    Timer,
    /// [`OutputError::Unsupported`].
    Unsupported,
    /// [`OutputError::Disconnected`].
    Disconnected,
    /// [`OutputError::Timeout`].
    Timeout,
    /// [`OutputError::Platform`].
    Platform,
}

impl ErrorKind {
    #[must_use]
    pub fn of(e: &OutputError) -> Self {
        match e {
            OutputError::Unsupported { .. } => Self::Unsupported,
            OutputError::Disconnected => Self::Disconnected,
            OutputError::Timeout => Self::Timeout,
            OutputError::Platform(_) => Self::Platform,
        }
    }
}

/// Result of a capture check. Raw captures are not implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureOutcome {
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayEntry {
    /// `DisplayId` text, for example `browser:client-1`.
    pub id: String,
    pub name: String,
    pub size: [usize; 2],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportState {
    Yes,
    No,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportReport {
    pub state: SupportState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl From<&Support> for SupportReport {
    fn from(s: &Support) -> Self {
        let (state, reason) = match s {
            Support::Yes => (SupportState::Yes, None),
            Support::No(r) => (SupportState::No, Some(r.clone())),
            Support::Unknown(r) => (SupportState::Unknown, Some(r.clone())),
        };
        Self { state, reason }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepthReport {
    pub depth: u32,
    pub available: SupportReport,
    pub path: String,
    pub conversion: ConversionReport,
    pub exact: SupportReport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversionReport {
    Direct,
    ScRgb80,
    Png16,
}

impl From<Conversion> for ConversionReport {
    fn from(c: Conversion) -> Self {
        match c {
            Conversion::Direct => Self::Direct,
            Conversion::ScRgb80 => Self::ScRgb80,
            Conversion::Png16 => Self::Png16,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncodingReport {
    /// For example `pq-narrow`.
    pub encoding: String,
    pub input: SupportReport,
    pub interpretation: SupportReport,
    pub auto_light_level: SupportReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesReport {
    pub depths: Vec<DepthReport>,
    pub encodings: Vec<EncodingReport>,
    pub hdr_metadata: SupportReport,
    pub pixel_exact: SupportReport,
}

impl From<&Capabilities> for CapabilitiesReport {
    fn from(c: &Capabilities) -> Self {
        Self {
            depths: c
                .depths
                .iter()
                .map(|(depth, d)| DepthReport {
                    depth: depth.bits(),
                    available: (&d.available).into(),
                    path: d.path.clone(),
                    conversion: d.conversion.into(),
                    exact: (&d.exact).into(),
                })
                .collect(),
            encodings: c
                .encodings
                .iter()
                .map(|(encoding, e)| EncodingReport {
                    encoding: encoding.id().into(),
                    input: (&e.input).into(),
                    interpretation: (&e.interpretation).into(),
                    auto_light_level: (&e.auto_light_level).into(),
                })
                .collect(),
            hdr_metadata: (&c.hdr_metadata).into(),
            pixel_exact: (&c.pixel_exact).into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GamutReport {
    Srgb,
    P3,
    Rec2020,
}

impl From<Gamut> for GamutReport {
    fn from(g: Gamut) -> Self {
        match g {
            Gamut::Srgb => Self::Srgb,
            Gamut::P3 => Self::P3,
            Gamut::Rec2020 => Self::Rec2020,
        }
    }
}

/// [`ModeSettings`]; `null` where the driver cannot read the value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModeReport {
    pub hdr_on: Option<bool>,
    pub gamut: Option<GamutReport>,
    pub wire_depth: Option<u8>,
    pub peak_nits: Option<f64>,
    pub min_nits: Option<f64>,
    pub sdr_white_nits: Option<f64>,
    pub pixel_ratio: f64,
    pub description: String,
}

impl From<&ModeSettings> for ModeReport {
    fn from(m: &ModeSettings) -> Self {
        Self {
            hdr_on: m.hdr_on,
            gamut: m.gamut.map(Into::into),
            wire_depth: m.wire_depth,
            peak_nits: m.peak_nits,
            min_nits: m.min_nits,
            sdr_white_nits: m.sdr_white_nits,
            pixel_ratio: m.pixel_ratio,
            description: m.description.clone(),
        }
    }
}

impl Event {
    #[must_use]
    pub fn query(display: String, query: &Query) -> Self {
        Self::Query {
            display,
            capabilities: (&query.capabilities).into(),
            mode: (&query.mode).into(),
        }
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
