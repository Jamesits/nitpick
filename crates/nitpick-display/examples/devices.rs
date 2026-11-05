//! Prints every local display device and its query. Read-only.
//!
//! `cargo run -p nitpick-display --example devices`

fn main() {
    let mut backend = nitpick_display::native();
    let devices = match backend.devices() {
        Ok(devices) => devices,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    for device in devices {
        println!("{} ({})", device.name, device.id);
        match backend.query_device(&device.id) {
            Ok(query) => println!("{query:#?}"),
            Err(e) => println!("  query failed: {e}"),
        }
    }
}
