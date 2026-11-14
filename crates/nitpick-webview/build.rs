// tauri-build needs `icons/icon.ico` for the Windows resource, and
// `generate_context!` needs `icons/icon.png` on other targets.
fn main() {
    tauri_build::build();
}
