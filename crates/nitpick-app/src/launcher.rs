//! QML-facing launcher object: pattern catalogue, reference tables and
//! the call that renders a pattern and opens the HDR window.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(QString, status)]
        type Launcher = super::LauncherRust;

        /// JSON object: `{patterns: [{id, label, description, encodings: [{id, label}]}],
        /// depths: [{id, label}], primaries: [{id, label}]}`.
        #[qinvokable]
        fn catalogue(&self) -> QString;

        /// Reference table for the native size of `screen`.
        #[qinvokable]
        #[cxx_name = "referenceTable"]
        fn reference_table(
            &self,
            pattern: &QString,
            encoding: &QString,
            depth: &QString,
            screen: i32,
        ) -> QString;

        /// Renders the pattern at the pattern area size of a new full-screen
        /// window on `screen` and shows it. `metadata` is a JSON object, see
        /// `request_from_json`. The result or error goes to `status`.
        #[qinvokable]
        #[cxx_name = "showPattern"]
        fn show_pattern(
            self: Pin<&mut Self>,
            pattern: &QString,
            encoding: &QString,
            depth: &QString,
            screen: i32,
            metadata: &QString,
        );
    }
}

use core::pin::Pin;

use cxx_qt_lib::QString;
use nitpick_core::driver::{
    ContentLightLevel, Hdr10Request, OutputLimits, Query, check_frame, check_open,
};
use nitpick_core::image::Image;
use nitpick_core::metadata::{self, Hdr10Metadata, MasteringPrimaries};
use nitpick_core::output::{SwapChainFormat, swap_chain_bytes};
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::{BitDepth, D65, Encoding, Format};
use serde_json::{Value, json};

use crate::display::{self, Backend, ffi};

#[derive(Default)]
pub struct LauncherRust {
    status: QString,
}

fn parse(
    pattern: &QString,
    encoding: &QString,
    depth: &QString,
) -> Result<(PatternKind, Format), String> {
    let encoding: Encoding = encoding.to_string().parse()?;
    let depth: BitDepth = depth.to_string().parse()?;
    Ok((pattern.to_string().parse()?, Format::new(encoding, depth)))
}

/// Output query of `screen` from its current native device state.
fn screen_query(screen: i32) -> Query {
    display::query(Backend::current(), &display::device_report(screen))
}

fn screen_size(screen: i32) -> Result<(usize, usize), String> {
    let size = |v: i32| usize::try_from(v).map_err(|_| format!("invalid screen size {v}"));
    let width = ffi::screen_pixel_width(screen).map_err(|e| e.to_string())?;
    let height = ffi::screen_pixel_height(screen).map_err(|e| e.to_string())?;
    Ok((size(width)?, size(height)?))
}

/// Parses the launcher JSON
/// `{enabled, primaries, maxMastering, minMastering, fromPattern, maxCll, maxFall}`
/// (luminances in cd/m²). `None` if metadata is off.
fn request_from_json(json: &str) -> Result<Option<Hdr10Request>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("metadata: {e}"))?;
    let number = |key: &str| {
        v[key]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .ok_or_else(|| format!("metadata: `{key}` must be a number >= 0"))
    };
    if !v["enabled"].as_bool().unwrap_or(false) {
        return Ok(None);
    }
    let primaries: MasteringPrimaries = v["primaries"].as_str().unwrap_or_default().parse()?;
    let light_level = if v["fromPattern"].as_bool().unwrap_or(true) {
        ContentLightLevel::FromFrame
    } else {
        ContentLightLevel::Explicit {
            max_cll: number("maxCll")?,
            max_fall: number("maxFall")?,
        }
    };
    Ok(Some(Hdr10Request {
        primaries: primaries.primaries(),
        white: D65,
        max_mastering_nits: number("maxMastering")?,
        min_mastering_nits: number("minMastering")?,
        light_level,
    }))
}

/// Bridge metadata for `request`. Automatic light levels come from the
/// rendered image.
fn bridge_metadata(
    request: Option<&Hdr10Request>,
    image: &Image,
    depth: BitDepth,
) -> ffi::HdrMetadata {
    let Some(request) = request else {
        return ffi::HdrMetadata {
            enabled: false,
            chromaticity: [0; 8],
            max_mastering: 0,
            min_mastering: 0,
            max_cll: 0,
            max_fall: 0,
            mastering_display: [0; 24],
            content_light_level: [0; 4],
        };
    };
    // QRhi sends every encoding as full-range PQ (see the QRhi design).
    let max = f64::from(depth.max_code());
    let md = request.resolve(|| {
        metadata::light_level(
            image
                .pixels()
                .iter()
                .map(|p| [p.r, p.g, p.b].map(|c| f64::from(c) / max)),
        )
    });
    let [r, g, b] = md.primaries;
    let xy = [r, g, b, md.white].map(|[x, y]| {
        [
            Hdr10Metadata::chromaticity_code(x),
            Hdr10Metadata::chromaticity_code(y),
        ]
    });
    ffi::HdrMetadata {
        enabled: true,
        chromaticity: xy.concat().try_into().unwrap_or([0; 8]),
        max_mastering: Hdr10Metadata::luminance_code(md.max_mastering_nits),
        min_mastering: Hdr10Metadata::luminance_code(md.min_mastering_nits),
        max_cll: Hdr10Metadata::light_level_code(md.max_cll),
        max_fall: Hdr10Metadata::light_level_code(md.max_fall),
        mastering_display: md.mastering_display_bytes(),
        content_light_level: md.content_light_level_bytes(),
    }
}

fn show(
    pattern: &QString,
    encoding: &QString,
    depth: &QString,
    screen: i32,
    metadata: &QString,
) -> Result<String, String> {
    let (kind, format) = parse(pattern, encoding, depth)?;
    let request = request_from_json(&metadata.to_string())?;
    // Rejects an unavailable path, such as macOS 12-bit, before the window
    // opens and before any conversion.
    let query = screen_query(screen);
    let caps = &query.capabilities;
    let mut warnings = check_open(caps, format.depth).map_err(|e| e.to_string())?;
    warnings.extend(check_frame(caps, format, request.as_ref()).map_err(|e| e.to_string())?);
    let swap_chain = SwapChainFormat::for_depth(format.depth);
    let opened = ffi::open_pattern_window(screen, swap_chain == SwapChainFormat::ScRgb)
        .map_err(|e| e.to_string())?;
    let surface = display::surface(display::display_id(screen), format.depth, &opened)
        .map_err(|e| e.to_string())?;
    let (width, height) = surface.size;
    let image = kind
        .render(format, width, height)
        .map_err(|e| e.to_string())?;
    let pixels = swap_chain_bytes(&image, format).map_err(|e| e.to_string())?;
    let md = bridge_metadata(request.as_ref(), &image, format.depth);
    let result = ffi::present_pattern(&pixels, opened.width, opened.height, &md)
        .map_err(|e| e.to_string())?;
    let warnings = if warnings.is_empty() {
        String::new()
    } else {
        format!(" Warnings: {}.", warnings.join("; "))
    };
    Ok(format!(
        "{} ({format}) at {width}x{height}. {}. {}. {result}{warnings} Esc closes the pattern.",
        kind.label(),
        surface.description,
        query.mode.description,
    ))
}

fn id_labels<T: Copy>(
    items: &[T],
    id: impl Fn(T) -> &'static str,
    label: impl Fn(T) -> &'static str,
) -> Value {
    items
        .iter()
        .map(|&x| json!({"id": id(x), "label": label(x)}))
        .collect()
}

// QML invokables must be methods even when they need no state.
#[allow(clippy::unused_self)]
impl qobject::Launcher {
    pub fn catalogue(&self) -> QString {
        let patterns: Vec<_> = PatternKind::ALL
            .iter()
            .map(|p| {
                json!({
                    "id": p.id(),
                    "label": p.label(),
                    "description": p.description(),
                    "encodings": id_labels(p.encodings(), Encoding::id, Encoding::label),
                })
            })
            .collect();
        let depth_label = |d: BitDepth| match SwapChainFormat::for_depth(d) {
            SwapChainFormat::Hdr10 => "10-bit (HDR10 swap chain, exact codes)",
            SwapChainFormat::ScRgb => "12-bit (FP16 scRGB swap chain)",
        };
        let value = json!({
            "patterns": patterns,
            "depths": id_labels(&BitDepth::ALL, BitDepth::id, depth_label),
            "primaries": id_labels(
                &MasteringPrimaries::ALL,
                MasteringPrimaries::id,
                MasteringPrimaries::label,
            ),
        });
        QString::from(&value.to_string())
    }

    pub fn reference_table(
        &self,
        pattern: &QString,
        encoding: &QString,
        depth: &QString,
        screen: i32,
    ) -> QString {
        let text = parse(pattern, encoding, depth)
            .and_then(|(kind, format)| {
                let limits =
                    OutputLimits::for_frame(&screen_query(screen).capabilities, format, None);
                kind.reference_table(format, screen_size(screen)?, &limits)
                    .map_err(|e| e.to_string())
            })
            .unwrap_or_else(|e| e);
        QString::from(&text)
    }

    pub fn show_pattern(
        self: Pin<&mut Self>,
        pattern: &QString,
        encoding: &QString,
        depth: &QString,
        screen: i32,
        metadata: &QString,
    ) {
        let status = show(pattern, encoding, depth, screen, metadata)
            .unwrap_or_else(|e| format!("Error: {e}"));
        self.set_status(QString::from(&status));
    }
}
