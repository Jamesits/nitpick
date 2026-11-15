//! nitpick: HDR/WCG test pattern launcher. With `--test-host` as the first
//! argument, the test host of `nitpick_app::test_host`.

use std::process::ExitCode;

use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QUrl};
use nitpick_app::test_host;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some(test_host::FLAG) {
        return test_host::main();
    }
    let mut app = QGuiApplication::new();
    let mut engine = QQmlApplicationEngine::new();
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/nitpick/qml/main.qml"));
    }
    if let Some(app) = app.as_mut() {
        app.exec();
    }
    ExitCode::SUCCESS
}
