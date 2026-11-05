use nitpick_core::device::{
    DeviceError, DeviceId, DeviceInfo, DisplayDeviceControl, HostScope, ProfileRole, ProfileScope,
    ProfileTarget, UserScope,
};

use super::{NativeFailure, Unavailable, resolve, topology_revision};

fn info(key: &str, evidence: &[(&str, &str)]) -> DeviceInfo {
    DeviceInfo {
        id: DeviceId {
            backend: "windows".into(),
            key: key.into(),
        },
        name: "Monitor".into(),
        evidence: evidence
            .iter()
            .map(|(n, v)| ((*n).into(), (*v).into()))
            .collect(),
    }
}

#[test]
fn resolve_one_none_many() {
    let list = [info("a", &[]), info("b", &[]), info("b", &[])];
    let id = |k: &str| info(k, &[]).id;
    assert_eq!(resolve(&list, &id("a"), |d| d).unwrap().id, id("a"));
    assert_eq!(
        resolve(&list, &id("c"), |d| d).unwrap_err(),
        DeviceError::Disconnected(id("c"))
    );
    assert!(matches!(
        resolve(&list, &id("b"), |d| d),
        Err(DeviceError::AmbiguousIdentity(msg)) if msg.starts_with("2 devices")
    ));
}

#[test]
fn revision_ignores_order_and_detects_changes() {
    let a = info("a", &[("edid_ids", "1")]);
    let b = info("b", &[]);
    let base = topology_revision(&[a.clone(), b.clone()]);
    assert_eq!(topology_revision(&[b.clone(), a.clone()]), base);
    assert_ne!(topology_revision(std::slice::from_ref(&a)), base);
    let replaced = info("a", &[("edid_ids", "2")]);
    assert_ne!(topology_revision(&[replaced, b]), base);
}

/// The value is saved in recovery records. Expected value: FNV-1a 64 of
/// `"windows:a\0edid_ids=1\xff"`, computed independently.
#[test]
fn revision_is_stable() {
    assert_eq!(
        topology_revision(&[info("a", &[("edid_ids", "1")])]),
        0xcf1a_37a1_6fa4_107f
    );
}

#[test]
fn native_failure_keeps_api_and_code() {
    let f = NativeFailure {
        api: "QueryDisplayConfig",
        code: 0x57,
    };
    assert_eq!(f.to_string(), "QueryDisplayConfig failed (0x57)");
    assert_eq!(
        DeviceError::from(f),
        DeviceError::Native {
            api: "QueryDisplayConfig".into(),
            code: Some(0x57),
            message: "native call failed".into(),
        }
    );
}

#[test]
fn unavailable_service_fails_every_call_with_reason() {
    let mut b = Unavailable::new("no display backend for plan9");
    let id = info("a", &[]).id;
    let target = ProfileTarget {
        device: id.clone(),
        role: ProfileRole::Standard,
        scope: ProfileScope {
            user: UserScope::Current,
            host: Some(HostScope::Current),
        },
    };
    let check = |e: DeviceError| match e {
        DeviceError::Unsupported { reason, .. } => {
            assert_eq!(reason, "no display backend for plan9");
        }
        other => panic!("unexpected {other:?}"),
    };
    check(b.devices().unwrap_err());
    check(b.query_device(&id).unwrap_err());
    check(b.profile_targets(&id).unwrap_err());
    check(b.profile_state(&target).unwrap_err());
}
