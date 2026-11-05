use std::fmt::Write as _;

use nitpick_core::device::{
    ContentHash, DeviceError, DeviceId, DeviceInfo, HostScope, IccProfile, PREPARED_CHANGE_VERSION,
    PlannedResource, PreparedProfileChange, ProfileRef, ProfileRole, ProfileScope,
    ProfileSelection, ProfileState, ProfileTarget, Report, ResourceKind, UserScope,
};

use super::{decode, encode};

fn icc_bytes(tag: u8) -> Vec<u8> {
    let mut b = vec![0u8; 132];
    b[..4].copy_from_slice(&132u32.to_be_bytes());
    b[36..40].copy_from_slice(b"acsp");
    b[131] = tag;
    b
}

fn change(selection: ProfileSelection) -> PreparedProfileChange {
    let id = DeviceId {
        backend: "windows".into(),
        key: r"\\?\DISPLAY#ABC1234#5&1&0&UID4352".into(),
    };
    let target = ProfileTarget {
        device: id.clone(),
        role: ProfileRole::AdvancedColor,
        scope: ProfileScope {
            user: UserScope::Current,
            host: Some(HostScope::Any),
        },
    };
    let existing = ProfileRef {
        native_id: "user.icm".into(),
        hash: Some(ContentHash::of(b"user")),
    };
    PreparedProfileChange {
        version: PREPARED_CHANGE_VERSION,
        device: DeviceInfo {
            id,
            name: "Monitor".into(),
            evidence: vec![("edid_ids".into(), "1234:5678".into())],
        },
        topology_revision: u64::MAX,
        target: target.clone(),
        original: ProfileState {
            target,
            associations: Report::known(
                vec![
                    existing,
                    ProfileRef {
                        native_id: "unread.icm".into(),
                        hash: None,
                    },
                ],
                "ColorProfileGetDisplayList",
            ),
            selection,
            revision: 7,
        },
        profile: IccProfile::new(icc_bytes(1)).unwrap(),
        resources: vec![PlannedResource {
            kind: ResourceKind::InstalledProfile,
            native_id: "nitpick-0001.icm".into(),
        }],
    }
}

fn unsupported_reason(e: DeviceError) -> String {
    match e {
        DeviceError::Unsupported { operation, reason } => {
            assert_eq!(operation, "recovery record");
            reason
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn round_trip_keeps_every_selection_kind() {
    let selections = [
        ProfileSelection::NoOverride,
        ProfileSelection::Override(ProfileRef {
            native_id: "a.icm".into(),
            hash: Some(ContentHash::of(b"a")),
        }),
        ProfileSelection::Inherited {
            from: ProfileScope {
                user: UserScope::All,
                host: None,
            },
            profile: ProfileRef {
                native_id: "b.icm".into(),
                hash: None,
            },
        },
    ];
    for selection in selections {
        let c = change(selection);
        let text = encode(&c).unwrap();
        assert_eq!(decode(&text).unwrap(), c);
    }
}

#[test]
fn version_is_the_first_field() {
    let text = encode(&change(ProfileSelection::NoOverride)).unwrap();
    let first = text.lines().nth(1).unwrap().trim();
    assert_eq!(first, format!("\"version\": {PREPARED_CHANGE_VERSION},"));
}

#[test]
fn other_version_is_rejected_before_other_fields() {
    let text = encode(&change(ProfileSelection::NoOverride)).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value["version"] = (PREPARED_CHANGE_VERSION + 1).into();
    // A field of the next version that this version cannot read.
    value["original"] = "new layout".into();
    let reason = unsupported_reason(decode(&value.to_string()).unwrap_err());
    assert_eq!(
        reason,
        format!(
            "version {}, expected {PREPARED_CHANGE_VERSION}",
            PREPARED_CHANGE_VERSION + 1
        )
    );

    value.as_object_mut().unwrap().remove("version");
    assert_eq!(
        unsupported_reason(decode(&value.to_string()).unwrap_err()),
        "no version field"
    );
}

#[test]
fn encode_rejects_other_version() {
    let mut c = change(ProfileSelection::NoOverride);
    c.version = 0;
    assert!(matches!(encode(&c), Err(DeviceError::Unsupported { .. })));
}

#[test]
fn corrupt_records_are_unreadable() {
    let text = encode(&change(ProfileSelection::NoOverride)).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();

    let mut wrong_hash = value.clone();
    wrong_hash["profile"]["hash"] = hex(&ContentHash::of(b"other").0).into();
    let reason = unsupported_reason(decode(&wrong_hash.to_string()).unwrap_err());
    assert!(reason.contains("differs from profile hash"), "{reason}");

    let mut bad_header = value.clone();
    let mut bytes = icc_bytes(1);
    bytes[36] = b'x';
    bad_header["profile"]["bytes"] = hex(&bytes).into();
    let reason = unsupported_reason(decode(&bad_header.to_string()).unwrap_err());
    assert!(reason.contains("acsp"), "{reason}");

    let reason = unsupported_reason(decode(&text[..text.len() / 2]).unwrap_err());
    assert!(reason.starts_with("unreadable"), "{reason}");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
