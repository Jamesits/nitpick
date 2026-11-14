//! Native ICC assignment check on one selected display: prepare, save the
//! recovery record, assign, read back, restore and read back. Uses only
//! the `ordinary` profile fixtures.
//!
//! ```text
//! cargo run -p nitpick-display --example profile_roundtrip -- state <device key part>
//! cargo run -p nitpick-display --example profile_roundtrip -- assign <device key part> <standard|advanced> <user|system>[/<this-host|any-host>] <fixture id> <record file>
//! cargo run -p nitpick-display --example profile_roundtrip -- restore <record file>
//! ```
//!
//! The host part of the scope is for macOS; Windows has no host scope.
//! Linux has only the target `standard user`.
//! `assign` writes the record before the first change and restores at the
//! end, also after a failure. It keeps the record if restoration fails;
//! `restore` retries from it.

use std::path::Path;
use std::process::ExitCode;

use nitpick_core::device::{
    DeviceError, DeviceId, DisplayDeviceControl, HostScope, IccProfile, ProfileRole, ProfileScope,
    ProfileState, ProfileTarget, UserScope,
};
use nitpick_display::recovery;
use nitpick_fixtures::profile::{ProfileUse, manifest};

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {msg}");
    ExitCode::FAILURE
}

/// The one device whose key contains `part`.
fn device(backend: &mut dyn DisplayDeviceControl, part: &str) -> Result<DeviceId, DeviceError> {
    let mut ids: Vec<DeviceId> = backend
        .devices()?
        .into_iter()
        .map(|d| d.id)
        .filter(|id| id.key.contains(part))
        .collect();
    if ids.len() != 1 {
        return Err(DeviceError::Unsupported {
            operation: "device selection".into(),
            reason: format!("{} device keys contain {part}", ids.len()),
        });
    }
    Ok(ids.remove(0))
}

fn print_state(label: &str, state: &ProfileState) {
    println!("{label}: {state:#?}");
}

fn state(key: &str) -> Result<(), DeviceError> {
    let mut backend = nitpick_display::native();
    let id = device(backend.as_mut(), key)?;
    for target in backend.profile_targets(&id)? {
        match backend.profile_state(&target) {
            Ok(s) => print_state(&format!("{:?} {:?}", target.role, target.scope), &s),
            Err(e) => println!("{:?} {:?}: {e}", target.role, target.scope),
        }
    }
    Ok(())
}

fn ordinary_fixture(id: &str) -> Result<IccProfile, String> {
    let manifest = manifest().map_err(|e| format!("profile manifest: {e}"))?;
    let entry = manifest
        .profiles
        .iter()
        .find(|p| p.id == id)
        .ok_or_else(|| format!("no fixture {id}"))?;
    if entry.usage != ProfileUse::Ordinary {
        return Err(format!("{id} is not an ordinary fixture"));
    }
    let bytes = entry.bytes().map_err(|e| e.to_string())?;
    IccProfile::new(bytes).map_err(|e| e.to_string())
}

fn assign(args: &[String]) -> Result<(), String> {
    let [key, role, scope, fixture, record] = args else {
        return Err(
            "assign needs <device key part> <role> <scope> <fixture id> <record file>".into(),
        );
    };
    let role = match role.as_str() {
        "standard" => ProfileRole::Standard,
        "advanced" => ProfileRole::AdvancedColor,
        other => return Err(format!("unknown role {other}")),
    };
    let (user, host) = scope
        .split_once('/')
        .map_or((scope.as_str(), None), |(u, h)| (u, Some(h)));
    let user = match user {
        "user" => UserScope::Current,
        "system" => UserScope::All,
        other => return Err(format!("unknown user scope {other}")),
    };
    let host = match host {
        None => None,
        Some("this-host") => Some(HostScope::Current),
        Some("any-host") => Some(HostScope::Any),
        Some(other) => return Err(format!("unknown host scope {other}")),
    };
    let profile = ordinary_fixture(fixture)?;
    let record = Path::new(record);
    if record.exists() {
        return Err(format!(
            "{} exists; restore from it or remove it first",
            record.display()
        ));
    }
    let mut backend = nitpick_display::native();
    let id = device(backend.as_mut(), key).map_err(|e| e.to_string())?;
    let target = ProfileTarget {
        device: id,
        role,
        scope: ProfileScope { user, host },
    };
    let change = backend
        .prepare_icc_profile(&target, &profile)
        .map_err(|e| format!("prepare: {e}"))?;
    let text = recovery::encode(&change).map_err(|e| e.to_string())?;
    std::fs::write(record, text).map_err(|e| format!("write record: {e}"))?;
    print_state("original", &change.original);
    println!("resources: {:#?}", change.resources);

    let set = backend.set_icc_profile(&change);
    match &set {
        Ok(s) => print_state("assigned", s),
        Err(e) => println!("assignment failed: {e}"),
    }
    let restored = backend
        .restore_profile(&change)
        .map_err(|e| format!("restore failed, record kept at {}: {e}", record.display()))?;
    print_state("restored", &restored);
    std::fs::remove_file(record).map_err(|e| format!("remove record: {e}"))?;
    set.map(|_| ()).map_err(|e| format!("assignment: {e}"))
}

fn restore(record: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(record).map_err(|e| e.to_string())?;
    let change = recovery::decode(&text).map_err(|e| e.to_string())?;
    let mut backend = nitpick_display::native();
    let restored = backend
        .restore_profile(&change)
        .map_err(|e| e.to_string())?;
    print_state("restored", &restored);
    std::fs::remove_file(record).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("state") if args.len() == 2 => state(&args[1]).map_err(|e| e.to_string()),
        Some("assign") => assign(&args[1..]),
        Some("restore") if args.len() == 2 => restore(&args[1]),
        _ => Err("usage: state <key> | assign ... | restore <record>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}
