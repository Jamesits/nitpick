//! Error and state handling shared by the platform backends.

use std::fmt;

use nitpick_core::device::{
    DeviceError, DeviceId, DeviceInfo, DeviceQuery, DisplayDeviceControl, DisplaySetup, IccProfile,
    PreparedProfileChange, ProfileState, ProfileTarget, Report,
};

/// A failed native call for one field. The field becomes
/// [`nitpick_core::device::Report::Unknown`] with this reason; the rest of
/// the query continues.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeFailure {
    pub api: &'static str,
    /// Win32 error, HRESULT, `OSStatus` or errno.
    pub code: i64,
}

impl fmt::Display for NativeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} failed ({:#x})", self.api, self.code)
    }
}

impl From<NativeFailure> for DeviceError {
    fn from(e: NativeFailure) -> Self {
        Self::Native {
            api: e.api.into(),
            code: Some(e.code),
            message: "native call failed".into(),
        }
    }
}

/// Result of one native call that a query can do without.
pub type Field<T> = Result<T, NativeFailure>;

pub fn unsupported(operation: impl Into<String>, reason: impl Into<String>) -> DeviceError {
    DeviceError::Unsupported {
        operation: operation.into(),
        reason: reason.into(),
    }
}

/// The single entry with identity `id`.
///
/// # Errors
/// [`DeviceError::Disconnected`] if none matches;
/// [`DeviceError::AmbiguousIdentity`] if more than one matches. A backend
/// lists duplicates instead of choosing one, so this error reaches the
/// caller before any write.
pub fn resolve<'a, T>(
    entries: &'a [T],
    id: &DeviceId,
    info: impl Fn(&T) -> &DeviceInfo,
) -> Result<&'a T, DeviceError> {
    let mut matches = entries.iter().filter(|e| info(e).id == *id);
    let first = matches
        .next()
        .ok_or_else(|| DeviceError::Disconnected(id.clone()))?;
    let others = matches.count();
    if others > 0 {
        return Err(DeviceError::AmbiguousIdentity(format!(
            "{} devices report {id}",
            others + 1
        )));
    }
    Ok(first)
}

/// Topology revision from the identity of every current device.
///
/// FNV-1a 64 instead of the std hasher: the value is saved in recovery
/// records, so it must not change between builds or runs.
#[must_use]
pub fn topology_revision(devices: &[DeviceInfo]) -> u64 {
    let mut parts: Vec<String> = devices
        .iter()
        .map(|d| {
            let mut s = d.id.to_string();
            for (name, value) in &d.evidence {
                s.push('\0');
                s.push_str(name);
                s.push('=');
                s.push_str(value);
            }
            s
        })
        .collect();
    // Native enumeration order can change without a topology change.
    parts.sort_unstable();
    fnv1a64(&parts)
}

/// FNV-1a 64 over `parts` in order. Stable across builds and runs, so
/// recovery records can store it.
#[must_use]
pub fn fnv1a64<S: AsRef<str>>(parts: &[S]) -> u64 {
    let mut hash = FNV_OFFSET;
    for part in parts {
        // 0xff separates parts; it cannot occur in UTF-8.
        for &b in part.as_ref().as_bytes().iter().chain(&[0xff]) {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
    }
    hash
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A setup whose fields the backend does not read. Each field is unknown
/// with `reason`, so a baseline check cannot pass on it.
#[must_use]
pub fn unread_setup(device: &DeviceId, reason: &str) -> DisplaySetup {
    DisplaySetup {
        device: device.clone(),
        gamma_ramp: Report::unknown(reason),
        os_calibration_loader: Report::unknown(reason),
        external_calibration_loader: Report::unknown(reason),
        gpu_gamut_clamp: Report::unknown(reason),
        dithering: Report::unknown(reason),
    }
}

/// FNV-1a 64 of `bytes`; see [`fnv1a64`].
#[must_use]
pub fn fnv1a64_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(FNV_PRIME)
    })
}

/// Backend for a platform or service that is not available. Every call
/// fails with [`DeviceError::Unsupported`] and the reason, so the host can
/// report it instead of an empty device list.
#[derive(Debug, Clone)]
pub struct Unavailable {
    reason: String,
}

impl Unavailable {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    fn fail<T>(&self, operation: &str) -> Result<T, DeviceError> {
        Err(unsupported(operation, self.reason.clone()))
    }
}

impl DisplayDeviceControl for Unavailable {
    fn devices(&mut self) -> Result<Vec<DeviceInfo>, DeviceError> {
        self.fail("display enumeration")
    }

    fn query_device(&mut self, _: &DeviceId) -> Result<DeviceQuery, DeviceError> {
        self.fail("display query")
    }

    fn display_setup(&mut self, _: &DeviceId) -> Result<DisplaySetup, DeviceError> {
        self.fail("display setup")
    }

    fn profile_targets(&mut self, _: &DeviceId) -> Result<Vec<ProfileTarget>, DeviceError> {
        self.fail("profile targets")
    }

    fn profile_state(&mut self, _: &ProfileTarget) -> Result<ProfileState, DeviceError> {
        self.fail("profile state")
    }

    fn prepare_icc_profile(
        &mut self,
        _: &ProfileTarget,
        _: &IccProfile,
    ) -> Result<PreparedProfileChange, DeviceError> {
        self.fail("profile preparation")
    }

    fn set_icc_profile(&mut self, _: &PreparedProfileChange) -> Result<ProfileState, DeviceError> {
        self.fail("profile assignment")
    }

    fn restore_profile(&mut self, _: &PreparedProfileChange) -> Result<ProfileState, DeviceError> {
        self.fail("profile restoration")
    }
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
