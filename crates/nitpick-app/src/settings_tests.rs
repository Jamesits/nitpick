use super::{DEFAULT_DELAY_MS, Settings};

fn file() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nitpick").join("launcher.json");
    (dir, path)
}

#[test]
fn default_without_a_file() {
    let (_dir, path) = file();
    assert_eq!(DEFAULT_DELAY_MS, 1000);
    assert_eq!(Settings::at(&path).measurement_delay_ms(), 1000);
}

#[test]
fn delay_persists_across_restart() {
    let (_dir, path) = file();
    Settings::at(&path).set_measurement_delay_ms(2500).unwrap();
    // A new launcher process reads the same file.
    assert_eq!(Settings::at(&path).measurement_delay_ms(), 2500);
    Settings::at(&path).set_measurement_delay_ms(0).unwrap();
    assert_eq!(Settings::at(&path).measurement_delay_ms(), 0);
}

#[test]
fn invalid_values_give_the_default() {
    let (_dir, path) = file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for text in [
        "not json",
        "[]",
        r#"{"measurementDelayMs": -1}"#,
        r#"{"measurementDelayMs": 1.5}"#,
        r#"{"measurementDelayMs": "1000"}"#,
        r#"{"measurementDelayMs": 4294967296}"#,
    ] {
        std::fs::write(&path, text).unwrap();
        assert_eq!(Settings::at(&path).measurement_delay_ms(), 1000, "{text}");
    }
}

#[test]
fn saving_keeps_other_keys() {
    let (_dir, path) = file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"other": true, "measurementDelayMs": 5}"#).unwrap();
    Settings::at(&path).set_measurement_delay_ms(7).unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        saved,
        serde_json::json!({"other": true, "measurementDelayMs": 7})
    );
}

#[test]
fn no_config_directory_saves_nothing() {
    let settings = Settings { path: None };
    assert_eq!(settings.measurement_delay_ms(), 1000);
    assert!(settings.set_measurement_delay_ms(5).is_err());
}
