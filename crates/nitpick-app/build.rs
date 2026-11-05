use cxx_qt_build::{CxxQtBuilder, QmlModule};

fn main() {
    CxxQtBuilder::new_qml_module(QmlModule::new("nitpick").qml_file("qml/main.qml"))
        .qt_module("Gui")
        .files(["src/launcher.rs", "src/display.rs"])
        .cpp_file("cpp/pattern_window.cpp")
        .build();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo::rustc-link-lib=d3d11");
        println!("cargo::rustc-link-lib=dxgi");
        println!("cargo::rustc-link-lib=user32");
    }
}
