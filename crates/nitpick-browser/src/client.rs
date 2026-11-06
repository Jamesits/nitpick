//! Client state from `hello` and the browser capability rules. See
//! "Graphics path", "Pixel mapping" and "Mapping to `ModeSettings`" in
//! `doc/design/output_driver/browser.md`.
//!
//! The rules use only the cached `hello`, so `query` and `open` never wait
//! for the client.

use std::net::SocketAddr;

use nitpick_core::driver::{
    Capabilities, Conversion, DepthSupport, DisplayId, EncodingSupport, Gamut, ModeSettings, Query,
    Support,
};
use nitpick_core::signal::{BitDepth, Encoding};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Largest surface side in display pixels. An 8K screen is 7680 wide; the
/// limit stops a client from making the host render a very large frame.
pub(crate) const MAX_SURFACE_SIDE: u16 = 16_384;

/// Path name shared by both depths: the PNG has 16-bit samples for each.
const PNG_PATH: &str = "16-bit PNG, cICP";

/// Fields of the `hello` message. Sizes are in CSS pixels.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Hello {
    pub(crate) client: Agent,
    pub(crate) screen: CssSize,
    pub(crate) viewport: Viewport,
    /// `devicePixelRatio`.
    pub(crate) pixel_ratio: f64,
    pub(crate) dynamic_range: DynamicRange,
    pub(crate) color_gamut: ColorGamut,
    pub(crate) fullscreen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Agent {
    pub(crate) user_agent: String,
    /// The page runs in the `nitpick-webview` shell.
    pub(crate) webview: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct CssSize {
    pub(crate) width: f64,
    pub(crate) height: f64,
}

/// `visualViewport`: `scale` is the pinch zoom.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub(crate) struct Viewport {
    pub(crate) width: f64,
    pub(crate) height: f64,
    pub(crate) scale: f64,
}

/// CSS `(dynamic-range: …)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DynamicRange {
    High,
    Standard,
}

/// The widest matching CSS `(color-gamut: …)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ColorGamut {
    Rec2020,
    P3,
    Srgb,
}

impl From<ColorGamut> for Gamut {
    fn from(g: ColorGamut) -> Self {
        match g {
            ColorGamut::Rec2020 => Self::Rec2020,
            ColorGamut::P3 => Self::P3,
            ColorGamut::Srgb => Self::Srgb,
        }
    }
}

/// Values that select the surface. A change of any value invalidates an
/// open surface: the size changes, or the browser maps image pixels to
/// display pixels in a different way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Geometry {
    size: (usize, usize),
    /// Bit patterns: an exact comparison of the reported values.
    pixel_ratio: u64,
    scale: u64,
}

impl Hello {
    /// `fields` are the message fields without `type`.
    ///
    /// # Errors
    /// A missing or wrong field, or a size outside the limits.
    pub(crate) fn parse(fields: Map<String, Value>) -> Result<Self, String> {
        let hello: Self =
            serde_json::from_value(Value::Object(fields)).map_err(|e| e.to_string())?;
        hello.validate()?;
        Ok(hello)
    }

    fn validate(&self) -> Result<(), String> {
        let positive = [
            ("screen.width", self.screen.width),
            ("screen.height", self.screen.height),
            ("viewport.width", self.viewport.width),
            ("viewport.height", self.viewport.height),
            ("viewport.scale", self.viewport.scale),
            ("pixelRatio", self.pixel_ratio),
        ];
        for (name, value) in positive {
            if !(value.is_finite() && value > 0.0) {
                return Err(format!("{name} must be a positive number"));
            }
        }
        let (w, h) = self.surface_size_f64();
        let max = f64::from(MAX_SURFACE_SIDE);
        if !(w >= 1.0 && h >= 1.0 && w <= max && h <= max) {
            return Err(format!(
                "viewport of {w}×{h} display px is outside 1..={MAX_SURFACE_SIDE}"
            ));
        }
        Ok(())
    }

    fn surface_size_f64(&self) -> (f64, f64) {
        (
            (self.viewport.width * self.pixel_ratio).round(),
            (self.viewport.height * self.pixel_ratio).round(),
        )
    }

    /// Viewport in display pixels: $\mathrm{round}(w_{css} \cdot r)$.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(crate) fn surface_size(&self) -> (usize, usize) {
        // `validate` limits both values to 1..=MAX_SURFACE_SIDE.
        let (w, h) = self.surface_size_f64();
        (w as usize, h as usize)
    }

    pub(crate) fn geometry(&self) -> Geometry {
        Geometry {
            size: self.surface_size(),
            pixel_ratio: self.pixel_ratio.to_bits(),
            scale: self.viewport.scale.to_bits(),
        }
    }

    /// Reason why the browser shows HDR images as SDR, or `None`.
    pub(crate) fn sdr_reason(&self) -> Option<String> {
        if let Some(limit) = sdr_engine(&self.client.user_agent) {
            return Some(format!("{limit} shows HDR images as SDR"));
        }
        (self.dynamic_range == DynamicRange::Standard)
            .then(|| "the browser reports dynamic-range: standard".into())
    }

    /// Warning for a layout that does not cover the screen.
    pub(crate) fn layout_warning(&self) -> Option<&'static str> {
        (!self.fullscreen)
            .then_some("not full screen: the pattern covers the browser viewport only")
    }

    pub(crate) fn query(&self) -> Query {
        let sdr = self.sdr_reason();
        let interpretation = match &sdr {
            Some(reason) => Support::No(format!(
                "SDR preview: {reason}; the browser tone maps the image"
            )),
            None => Support::Unknown(
                "the browser and the OS do not report how they apply the cICP transfer".into(),
            ),
        };
        let encodings = Encoding::ALL
            .into_iter()
            .map(|encoding| {
                let auto_light_level = if encoding.is_pq() {
                    Support::Yes
                } else {
                    Support::No(
                        "HLG output has no absolute light level; enter explicit values".into(),
                    )
                };
                (
                    encoding,
                    EncodingSupport {
                        input: Support::Yes,
                        interpretation: interpretation.clone(),
                        auto_light_level,
                    },
                )
            })
            .collect();
        let depths = BitDepth::ALL
            .into_iter()
            .map(|depth| {
                (
                    depth,
                    DepthSupport {
                        available: Support::Yes,
                        path: PNG_PATH.into(),
                        conversion: Conversion::Png16,
                        exact: Support::Unknown(
                            "the browser and the OS do not report the displayed codes".into(),
                        ),
                    },
                )
            })
            .collect();
        let capabilities = Capabilities {
            depths,
            encodings,
            hdr_metadata: Support::Unknown(
                "written to PNG mDCV and cLLI; browser use is not reported".into(),
            ),
            pixel_exact: self.pixel_exact(),
        };
        let mode = ModeSettings {
            hdr_on: Some(sdr.is_none()),
            gamut: Some(self.color_gamut.into()),
            wire_depth: None,
            peak_nits: None,
            min_nits: None,
            sdr_white_nits: None,
            pixel_ratio: self.pixel_ratio,
            description: self.description(sdr.as_deref()),
        };
        Query { capabilities, mode }
    }

    /// `Yes` needs the layout size from `applied`; before it, the best
    /// state is `Unknown`.
    fn pixel_exact(&self) -> Support {
        let r = self.pixel_ratio;
        if r.fract() != 0.0 {
            return Support::No(format!(
                "pixel ratio {r} is not an integer; the browser resamples the image"
            ));
        }
        #[allow(clippy::float_cmp)] // The client reports exactly 1 without zoom.
        if self.viewport.scale != 1.0 {
            return Support::No(format!(
                "zoom {}: the browser resamples the image",
                self.viewport.scale
            ));
        }
        Support::Unknown("the client reports the layout size after the first frame".into())
    }

    fn description(&self, sdr: Option<&str>) -> String {
        let range = match self.dynamic_range {
            DynamicRange::High => "high",
            DynamicRange::Standard => "standard",
        };
        let gamut = match self.color_gamut {
            ColorGamut::Rec2020 => "rec2020",
            ColorGamut::P3 => "p3",
            ColorGamut::Srgb => "srgb",
        };
        let mut text = format!(
            "{}, dynamic range {range}, gamut {gamut}, pixel ratio {}, zoom {}",
            self.engine_label(),
            self.pixel_ratio,
            self.viewport.scale
        );
        if let Some(reason) = sdr {
            text.push_str("; SDR preview: ");
            text.push_str(reason);
        }
        if let Some(warning) = self.layout_warning() {
            text.push_str("; ");
            text.push_str(warning);
        }
        text
    }

    fn engine_label(&self) -> String {
        let engine = engine(&self.client.user_agent);
        if self.client.webview {
            format!("{engine} webview")
        } else {
            engine.into()
        }
    }

    /// Client name and address, for the display list.
    pub(crate) fn display_name(&self, peer: SocketAddr) -> String {
        format!("{} at {}", self.engine_label(), peer.ip())
    }
}

fn is_linux(ua: &str) -> bool {
    (ua.contains("Linux") || ua.contains("X11")) && !ua.contains("Android")
}

fn is_chromium(ua: &str) -> bool {
    ua.contains("Chrome/") || ua.contains("Chromium/")
}

fn is_webkitgtk(ua: &str) -> bool {
    ua.contains("AppleWebKit/") && is_linux(ua) && !is_chromium(ua)
}

/// Engines that show HDR images as SDR also on an HDR display: Firefox on
/// Windows and Linux, and WebKitGTK (Tauri on Linux). See "Graphics path"
/// in the design. **Unverified** on devices; from documentation and bug
/// reports.
pub(crate) fn sdr_engine(ua: &str) -> Option<&'static str> {
    if ua.contains("Firefox/") && (ua.contains("Windows") || is_linux(ua)) {
        Some("Firefox on Windows and Linux")
    } else if is_webkitgtk(ua) {
        Some("WebKitGTK")
    } else {
        None
    }
}

/// Short engine name for the status line. Edge and Chromium-based browsers
/// also contain "Safari/" and "Chrome/", so the order matters.
pub(crate) fn engine(ua: &str) -> &'static str {
    if ua.contains("Edg/") {
        "Edge"
    } else if is_chromium(ua) {
        "Chrome"
    } else if ua.contains("Firefox/") {
        "Firefox"
    } else if is_webkitgtk(ua) {
        "WebKitGTK"
    } else if ua.contains("AppleWebKit/") {
        "Safari"
    } else {
        "browser"
    }
}

/// Cached state of the authenticated client after a valid `hello`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClientRecord {
    pub(crate) display: DisplayId,
    pub(crate) peer: SocketAddr,
    pub(crate) hello: Hello,
    /// Changes with each session and each change of [`Geometry`]. An open
    /// surface is valid while its generation is current.
    pub(crate) generation: u64,
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
