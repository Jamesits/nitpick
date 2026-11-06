//! QML-facing launcher object: pattern catalogue, display list, reference
//! tables and the "Show pattern" request. The lifecycle is in
//! `session.rs`.

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
        /// JSON array: `[{key, name, width, height}]`, from the last
        /// `refreshDisplays`.
        #[qproperty(QString, displays)]
        /// Changes when new device reports arrive; the reference table
        /// depends on it.
        #[qproperty(i32, device_revision, cxx_name = "deviceRevision")]
        type Launcher = super::LauncherRust;

        /// JSON object: `{patterns: [{id, label, description, encodings: [{id, label}]}],
        /// depths: [{id, label}], primaries: [{id, label}]}`.
        #[qinvokable]
        fn catalogue(&self) -> QString;

        /// Reference table for the native size of the display `display`
        /// (a `key` of `displays`).
        #[qinvokable]
        #[cxx_name = "referenceTable"]
        fn reference_table(
            &self,
            pattern: &QString,
            encoding: &QString,
            depth: &QString,
            display: &QString,
        ) -> QString;

        /// A display change: cancels the pending presentation, updates
        /// `displays` and reads the device state on a worker.
        #[qinvokable]
        #[cxx_name = "refreshDisplays"]
        fn refresh_displays(self: Pin<&mut Self>);

        /// Reads the device state of `display` on a worker, renders the
        /// pattern at the pattern area size of a new full-screen window
        /// and shows it. `metadata` is a JSON object, see
        /// `request_from_json`. `status` follows each step.
        #[qinvokable]
        #[cxx_name = "showPattern"]
        fn show_pattern(
            self: Pin<&mut Self>,
            pattern: &QString,
            encoding: &QString,
            depth: &QString,
            display: &QString,
            metadata: &QString,
        );
    }

    impl cxx_qt::Threading for Launcher {}
}

use core::pin::Pin;
use std::sync::Arc;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use nitpick_core::driver::{ContentLightLevel, Hdr10Request};
use nitpick_core::metadata::MasteringPrimaries;
use nitpick_core::output::SwapChainFormat;
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::{BitDepth, D65, Encoding, Format};
use serde_json::{Value, json};

use crate::display;
use crate::session::{Event, Session, ShowRequest};

#[derive(Default)]
pub struct LauncherRust {
    status: QString,
    displays: QString,
    device_revision: i32,
    /// Created at the first request, which gives the Qt thread handle.
    session: Option<Session>,
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
        display: &QString,
    ) -> QString {
        let text = match (parse(pattern, encoding, depth), &self.session) {
            (Err(e), _) => e,
            (Ok(_), None) => "No display selected.".into(),
            (Ok((kind, format)), Some(s)) => s.reference_table(kind, format, &display.to_string()),
        };
        QString::from(&text)
    }

    pub fn refresh_displays(mut self: Pin<&mut Self>) {
        let session = self.as_mut().session();
        if session.refresh().is_err() {
            // The nested event loop of `open` runs this; retry after it.
            let _ = self.qt_thread().queue(qobject::Launcher::refresh_displays);
            return;
        }
        let list: Vec<Value> = session
            .displays()
            .iter()
            .map(|d| {
                json!({
                    "key": d.id.key,
                    "name": d.name,
                    "width": d.size.0,
                    "height": d.size.1,
                })
            })
            .collect();
        self.set_displays(QString::from(&Value::from(list).to_string()));
    }

    pub fn show_pattern(
        mut self: Pin<&mut Self>,
        pattern: &QString,
        encoding: &QString,
        depth: &QString,
        display: &QString,
        metadata: &QString,
    ) {
        let request = parse(pattern, encoding, depth).and_then(|(kind, format)| {
            Ok(ShowRequest {
                kind,
                format,
                metadata: request_from_json(&metadata.to_string())?,
                display: display::display_for_key(display.to_string()),
            })
        });
        match request {
            Ok(r) => self.as_mut().session().show(r),
            Err(e) => self.set_status(QString::from(&format!("Error: {e}"))),
        }
    }

    fn session(mut self: Pin<&mut Self>) -> &mut Session {
        if self.session.is_none() {
            let thread = self.qt_thread();
            let events = thread.clone();
            // Events go through the queue in order, so a later status
            // always arrives last. The launcher may be gone at exit.
            let session = Session::native(Arc::new(thread), move |event| {
                let _ = events.queue(move |launcher| match event {
                    Event::Status(text) => launcher.set_status(QString::from(&text)),
                    Event::Devices => {
                        let revision = launcher.device_revision().wrapping_add(1);
                        launcher.set_device_revision(revision);
                    }
                });
            });
            self.as_mut().rust_mut().session = Some(session);
        }
        self.rust_mut()
            .get_mut()
            .session
            .as_mut()
            .expect("created above")
    }
}
