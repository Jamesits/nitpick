//! nitpick: HDR/WCG test pattern launcher.

mod display;
mod hdr_metadata;
// The cxx-qt generated constructor boxes the Rust struct; cxx-qt rejects
// attributes on the bridge module itself.
#[allow(clippy::unnecessary_box_returns)]
mod launcher;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};

fn main() {
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/nitpick/qml/main.qml"));
    }
    if let Some(app) = app.as_mut() {
        app.exec();
    }
}
