//! Launcher settings saved across restarts: a JSON object in
//! `<config dir>/nitpick/launcher.json`.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::delay::{self, DEFAULT_DELAY_MS};

const DELAY_KEY: &str = "measurementDelayMs";
const PORT_KEY: &str = "browserPort";
const ADDRESS_KEY: &str = "browserAddress";

pub struct Settings {
    /// `None` if the OS has no config directory; nothing is saved then.
    path: Option<PathBuf>,
}

impl Settings {
    #[must_use]
    pub fn native() -> Self {
        Self {
            path: dirs::config_dir().map(|d| d.join("nitpick").join("launcher.json")),
        }
    }

    #[cfg(test)]
    pub fn at(path: &std::path::Path) -> Self {
        Self {
            path: Some(path.to_owned()),
        }
    }

    /// The saved delay in ms, or the default if the file or the value is
    /// missing or invalid.
    #[must_use]
    pub fn measurement_delay_ms(&self) -> u32 {
        self.read()
            .get(DELAY_KEY)
            .and_then(Value::as_i64)
            .and_then(|ms| i32::try_from(ms).ok())
            .and_then(|ms| delay::delay_from(ms).ok())
            .unwrap_or(DEFAULT_DELAY_MS)
    }

    /// Saves the delay and keeps the other keys.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn set_measurement_delay_ms(&self, ms: u32) -> Result<(), String> {
        self.write(DELAY_KEY, ms.into())
    }

    /// The saved TCP port of the browser service; 0 (the OS selects a
    /// port) if missing or invalid.
    #[must_use]
    pub fn browser_port(&self) -> u16 {
        self.read()
            .get(PORT_KEY)
            .and_then(Value::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .unwrap_or(0)
    }

    /// The saved interface address for remote displays; empty if missing
    /// or invalid. Remote displays stay off at each start.
    #[must_use]
    pub fn browser_address(&self) -> String {
        self.read()
            .get(ADDRESS_KEY)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }

    /// Saves the port and the address, and keeps the other keys.
    ///
    /// # Errors
    /// The file cannot be written.
    pub fn set_browser(&self, port: u16, address: &str) -> Result<(), String> {
        self.write(PORT_KEY, port.into())?;
        self.write(ADDRESS_KEY, address.into())
    }

    fn write(&self, key: &str, value: Value) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Err("no configuration directory".into());
        };
        let mut map = self.read();
        map.insert(key.into(), value);
        let text = Value::Object(map).to_string();
        let error = |e: std::io::Error| format!("{}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(error)?;
        }
        // Rename: an interrupted write leaves the old file.
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(error)?;
        std::fs::rename(&temporary, path).map_err(error)
    }

    fn read(&self) -> Map<String, Value> {
        self.path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .and_then(|v: Value| match v {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
