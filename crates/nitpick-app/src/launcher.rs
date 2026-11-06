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
        /// `request_from_json`. `status` shows the submission; the result
        /// or error follows the window's acknowledgement.
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

    impl cxx_qt::Threading for Launcher {}
}

use core::pin::Pin;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use nitpick_core::driver::{
    ContentLightLevel, Hdr10Request, OutputError, OutputLimits, Query, check_frame, check_open,
};
use nitpick_core::metadata::MasteringPrimaries;
use nitpick_core::output::{SwapChainFormat, swap_chain_bytes};
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::{BitDepth, D65, Encoding, Format};
use serde_json::{Value, json};

use crate::display::{self, Backend, ffi};
use crate::hdr_metadata::{self, BridgeSink, SwapChainMetadata};
use crate::submission::{self, Registration};

#[derive(Default)]
pub struct LauncherRust {
    status: QString,
    /// Completion of the last submitted frame. Dropping it cancels the
    /// completion.
    pending: Option<Registration>,
    /// Counts "Show pattern" requests. A queued status update of an older
    /// request does not apply.
    generation: u64,
}

/// A converted pattern for the open window.
struct Prepared {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
    /// Status line after the acknowledgement.
    status: String,
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

fn prepare(
    pattern: &QString,
    encoding: &QString,
    depth: &QString,
    screen: i32,
    metadata: &QString,
) -> Result<Prepared, String> {
    let (kind, format) = parse(pattern, encoding, depth)?;
    let request = request_from_json(&metadata.to_string())?;
    // Rejects an unavailable path before the window opens and before any
    // conversion. Remapped paths, such as Windows with HDR off, only warn.
    let query = screen_query(screen);
    let caps = &query.capabilities;
    let mut warnings = check_open(caps, format.depth).map_err(|e| e.to_string())?;
    warnings.extend(check_frame(caps, format, request.as_ref()).map_err(|e| e.to_string())?);
    let swap_chain = SwapChainFormat::for_depth(format.depth);
    let opened = ffi::open_pattern_window(screen, swap_chain == SwapChainFormat::ScRgb)
        .map_err(|e| e.to_string())?;
    let surface =
        display::surface(display::display_id(screen), &opened).map_err(|e| e.to_string())?;
    warnings.extend(display::surface_warning(format.depth, &opened));
    let (width, height) = surface.size;
    let image = kind
        .render(format, width, height)
        .map_err(|e| e.to_string())?;
    let pixels = swap_chain_bytes(&image, format).map_err(|e| e.to_string())?;
    let md = hdr_metadata::resolve(request.as_ref(), &image, format.depth);
    // The window is new, so its swap chain has no metadata yet.
    let result =
        SwapChainMetadata::default().apply(&mut BridgeSink, Backend::current(), md.as_ref());
    let warnings = if warnings.is_empty() {
        String::new()
    } else {
        format!(" Warnings: {}.", warnings.join("; "))
    };
    Ok(Prepared {
        pixels,
        width: opened.width,
        height: opened.height,
        status: format!(
            "{} ({format}) at {width}x{height}. {}. {}. {result}{warnings} Esc closes the pattern.",
            kind.label(),
            surface.description,
            query.mode.description,
        ),
    })
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
        mut self: Pin<&mut Self>,
        pattern: &QString,
        encoding: &QString,
        depth: &QString,
        screen: i32,
        metadata: &QString,
    ) {
        // Cancel the earlier completion before its window closes, so that
        // its `Disconnected` does not reach the status line.
        let generation = {
            let mut state = self.as_mut().rust_mut();
            state.pending = None;
            state.generation += 1;
            state.generation
        };
        let thread = self.qt_thread();
        let submitted = prepare(pattern, encoding, depth, screen, metadata).and_then(|p| {
            let Prepared {
                pixels,
                width,
                height,
                status,
            } = p;
            let done = move |result: Result<(), OutputError>| {
                let text = match result {
                    Ok(()) => status,
                    Err(e) => format!("Error: {e}"),
                };
                // The launcher may be gone at exit.
                let _ = thread.queue(move |mut launcher| {
                    if launcher.rust().generation == generation {
                        launcher.as_mut().rust_mut().pending = None;
                        launcher.set_status(QString::from(&text));
                    }
                });
            };
            submission::present(&pixels, width, height, done)
        });
        match submitted {
            Ok(registration) => {
                self.as_mut().rust_mut().pending = Some(registration);
                self.set_status(QString::from("Submitting pattern"));
            }
            Err(e) => self.set_status(QString::from(&format!("Error: {e}"))),
        }
    }
}
