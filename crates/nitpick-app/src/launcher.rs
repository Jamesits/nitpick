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

        /// JSON array: `[{id, label, description, encodings: [{id, label}]}]`.
        #[qinvokable]
        fn catalogue(&self) -> QString;

        #[qinvokable]
        #[cxx_name = "referenceTable"]
        fn reference_table(&self, pattern: &QString, encoding: &QString) -> QString;

        /// Renders the pattern at the native size of `screen` and shows it.
        /// The result or error goes to `status`.
        #[qinvokable]
        #[cxx_name = "showPattern"]
        fn show_pattern(self: Pin<&mut Self>, pattern: &QString, encoding: &QString, screen: i32);
    }
}

use core::pin::Pin;

use cxx_qt_lib::QString;
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::Encoding;

use crate::display::ffi;

#[derive(Default)]
pub struct LauncherRust {
    status: QString,
}

fn parse(pattern: &QString, encoding: &QString) -> Result<(PatternKind, Encoding), String> {
    Ok((pattern.to_string().parse()?, encoding.to_string().parse()?))
}

fn show(pattern: &QString, encoding: &QString, screen: i32) -> Result<String, String> {
    let (kind, enc) = parse(pattern, encoding)?;
    let width = ffi::screen_pixel_width(screen).map_err(|e| e.to_string())?;
    let height = ffi::screen_pixel_height(screen).map_err(|e| e.to_string())?;
    let size = |v: i32| usize::try_from(v).map_err(|_| format!("invalid screen size {v}"));
    let image = kind
        .render(enc, size(width)?, size(height)?)
        .map_err(|e| e.to_string())?;
    let output =
        ffi::show_pattern(image.pixels(), width, height, screen).map_err(|e| e.to_string())?;
    Ok(format!(
        "{} ({}) at {width}x{height}. {output}. Esc closes the pattern.",
        kind.label(),
        enc.label()
    ))
}

// QML invokables must be methods even when they need no state.
#[allow(clippy::unused_self)]
impl qobject::Launcher {
    pub fn catalogue(&self) -> QString {
        let patterns: Vec<_> = PatternKind::ALL
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id(),
                    "label": p.label(),
                    "description": p.description(),
                    "encodings": p.encodings().iter().map(|e| serde_json::json!({
                        "id": e.id(),
                        "label": e.label(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        QString::from(&serde_json::Value::from(patterns).to_string())
    }

    pub fn reference_table(&self, pattern: &QString, encoding: &QString) -> QString {
        let text = parse(pattern, encoding)
            .and_then(|(kind, enc)| kind.reference_table(enc).map_err(|e| e.to_string()))
            .unwrap_or_else(|e| e);
        QString::from(&text)
    }

    pub fn show_pattern(self: Pin<&mut Self>, pattern: &QString, encoding: &QString, screen: i32) {
        let status = show(pattern, encoding, screen).unwrap_or_else(|e| format!("Error: {e}"));
        self.set_status(QString::from(&status));
    }
}
