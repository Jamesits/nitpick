//! Text form of the [`PreparedProfileChange`] recovery record. The host
//! stores it before the first write and reads it on the next start.

use nitpick_core::device::{DeviceError, PREPARED_CHANGE_VERSION, PreparedProfileChange};

use crate::common::unsupported;

const OPERATION: &str = "recovery record";

/// JSON. The `version` field is first, so a reader can check it before the
/// other fields.
///
/// # Errors
/// [`DeviceError::Unsupported`] if the change is not of the current
/// version; the encoded form could not be read back.
pub fn encode(change: &PreparedProfileChange) -> Result<String, DeviceError> {
    change.check_version()?;
    serde_json::to_string_pretty(change).map_err(|e| unsupported(OPERATION, e.to_string()))
}

/// Reads the version before the other fields: a record of another version
/// can give the same field names another meaning.
///
/// # Errors
/// [`DeviceError::Unsupported`] for unreadable text, another version, or
/// a profile that fails the header or hash check. Keep the record for
/// investigation.
pub fn decode(text: &str) -> Result<PreparedProfileChange, DeviceError> {
    let unreadable = |e: serde_json::Error| unsupported(OPERATION, format!("unreadable: {e}"));
    let value: serde_json::Value = serde_json::from_str(text).map_err(unreadable)?;
    match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(PREPARED_CHANGE_VERSION) => {}
        Some(v) => {
            return Err(unsupported(
                OPERATION,
                format!("version {v}, expected {PREPARED_CHANGE_VERSION}"),
            ));
        }
        None => return Err(unsupported(OPERATION, "no version field")),
    }
    serde_json::from_value(value).map_err(unreadable)
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
