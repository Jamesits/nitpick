//! The command-line host must build without a window system. This checks
//! the normal and build dependency graph of the package for the host
//! target; each CI operating system checks its own target.

use std::process::Command;

/// Package name prefixes of window-system, GUI and webview crates, and of
/// the workspace crates that use them. `nitpick-display` reads native
/// display state through window-system APIs.
const FORBIDDEN: [&str; 15] = [
    "nitpick-app",
    "nitpick-display",
    "nitpick-webview",
    "cxx-qt",
    "qt",
    "tauri",
    "wry",
    "tao",
    "winit",
    "gtk",
    "gdk",
    "webkit2gtk",
    "x11",
    "wayland",
    "objc2-app-kit",
];

#[test]
fn dependency_graph_has_no_window_system() {
    let output = Command::new(env!("CARGO"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args([
            "tree",
            "--locked",
            "--package",
            env!("CARGO_PKG_NAME"),
            "--edges",
            "no-dev",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ])
        .output()
        .expect("run cargo tree");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let tree = String::from_utf8(output.stdout).unwrap();
    let names: Vec<&str> = tree
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .collect();
    assert!(names.contains(&"nitpick-browser"), "{tree}");
    let forbidden: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| FORBIDDEN.iter().any(|f| name.starts_with(f)))
        .collect();
    assert!(forbidden.is_empty(), "{forbidden:?}");
}
