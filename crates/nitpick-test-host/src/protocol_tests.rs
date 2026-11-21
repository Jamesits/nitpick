use nitpick_core::driver::{Capabilities, Conversion, DepthSupport, ModeSettings, Query};
use nitpick_core::signal::BitDepth;
use nitpick_fixtures::LightLevelRequest;
use serde_json::json;

use super::*;

fn metadata() -> MetadataRequest {
    MetadataRequest {
        primaries: [[0.708, 0.292], [0.17, 0.797], [0.131, 0.046]],
        white: [0.3127, 0.329],
        max_mastering_nits: 1000.0,
        min_mastering_nits: 0.0001,
        light_level: LightLevelRequest::Explicit {
            max_cll: 800.0,
            max_fall: 200.0,
        },
    }
}

fn device() -> DeviceId {
    DeviceId {
        backend: "windows".into(),
        key: r"\\?\DISPLAY#DEL40E7#1".into(),
    }
}

fn target() -> ProfileTarget {
    ProfileTarget {
        device: device(),
        role: nitpick_core::device::ProfileRole::AdvancedColor,
        scope: nitpick_core::device::ProfileScope {
            user: nitpick_core::device::UserScope::Current,
            host: None,
        },
    }
}

/// The test process writes these lines; their form is the interface.
#[test]
fn device_requests_have_a_fixed_form() {
    let line = json!({
        "request": "profile_assign",
        "target": {
            "device": {"backend": "windows", "key": r"\\?\DISPLAY#DEL40E7#1"},
            "role": "AdvancedColor",
            "scope": {"user": "Current", "host": null},
        },
        "profile": "ordinary_srgb",
    })
    .to_string();
    assert_eq!(
        parse_request(&line),
        Ok(Some(Request::ProfileAssign {
            target: target(),
            profile: "ordinary_srgb".into(),
        }))
    );
    let e = Event::device_error("profile assignment", &DeviceError::Timeout);
    assert_eq!(
        serde_json::to_value(&e).unwrap(),
        json!({"event": "error", "kind": "device", "device_error": "timeout",
               "message": "profile assignment: device operation timed out"})
    );
}

#[test]
fn requests_round_trip() {
    let requests = [
        Request::Displays {},
        Request::Refresh {},
        Request::Query {
            display: "browser:client-1".into(),
        },
        Request::Open {
            display: "qrhi:\\\\.\\DISPLAY1".into(),
            depth: 12,
        },
        Request::Show {
            frame: 3,
            fixture: "state-frame-a-pq-full-10".into(),
            delay_ms: None,
            metadata: None,
            marker: false,
        },
        Request::Show {
            frame: 4,
            fixture: "x".into(),
            delay_ms: Some(0),
            metadata: Some(None),
            marker: false,
        },
        Request::Show {
            frame: 5,
            fixture: "x".into(),
            delay_ms: Some(10),
            metadata: Some(Some(metadata())),
            marker: true,
        },
        Request::Capture {
            frame: 5,
            heartbeat_ms: None,
            deadline_ms: None,
        },
        Request::Capture {
            frame: 5,
            heartbeat_ms: Some(100),
            deadline_ms: Some(5000),
        },
        Request::Close {},
        Request::Quit {},
        Request::DeviceList {},
        Request::DeviceMap {
            display: "qrhi:\\\\.\\DISPLAY1".into(),
        },
        Request::DeviceQuery { device: device() },
        Request::DeviceSetup { device: device() },
        Request::DeviceRun { device: device() },
        Request::ProfileTargets { device: device() },
        Request::ProfileState { target: target() },
        Request::ProfileAssign {
            target: target(),
            profile: "ordinary_srgb".into(),
        },
        Request::ProfileRestore {},
    ];
    for request in requests {
        let line = serde_json::to_string(&request).unwrap();
        assert_eq!(parse_request(&line), Ok(Some(request)), "{line}");
    }
}

#[test]
fn show_separates_absent_and_null_metadata() {
    let parse = |v: serde_json::Value| parse_request(&v.to_string()).unwrap().unwrap();
    let Request::Show { metadata, .. } =
        parse(json!({"request": "show", "frame": 1, "fixture": "x"}))
    else {
        panic!()
    };
    assert_eq!(metadata, None);
    let Request::Show { metadata, .. } =
        parse(json!({"request": "show", "frame": 1, "fixture": "x", "metadata": null}))
    else {
        panic!()
    };
    assert_eq!(metadata, Some(None));
}

#[test]
fn optional_capture_fields_default_to_absent() {
    let line = json!({"request": "capture", "frame": 2}).to_string();
    assert_eq!(
        parse_request(&line),
        Ok(Some(Request::Capture {
            frame: 2,
            heartbeat_ms: None,
            deadline_ms: None
        }))
    );
    let line = json!({"request": "show", "frame": 1, "fixture": "x"}).to_string();
    let Ok(Some(Request::Show { marker, .. })) = parse_request(&line) else {
        panic!()
    };
    assert!(!marker);
}

#[test]
fn rejects_invalid_lines() {
    assert_eq!(parse_request(""), Ok(None));
    assert_eq!(parse_request(" \t"), Ok(None));
    for line in [
        "show",
        "{}",
        "[]",
        r#"{"request":"bogus"}"#,
        r#"{"request":"query"}"#,
        r#"{"request":"open","display":"d","depth":"10"}"#,
        r#"{"request":"show","frame":1}"#,
        r#"{"request":"show","frame":1.5,"fixture":"x"}"#,
        r#"{"request":"show","frame":1,"fixture":"x","delay_ms":-1}"#,
        r#"{"request":"query","display":"d","extra":true}"#,
        r#"{"request":"quit","extra":true}"#,
        r#"{"request":"show","frame":1,"fixture":"x","metadata":{"white":[0.3,0.3]}}"#,
        r#"{"request":"show","frame":1,"fixture":"x","marker":1}"#,
        r#"{"request":"capture","frame":1,"heartbeat_ms":-1}"#,
        r#"{"request":"capture","frame":1,"deadline":10}"#,
    ] {
        assert!(parse_request(line).is_err(), "{line}");
    }
}

/// The test process parses these lines; their form is the interface.
#[test]
fn events_have_a_fixed_form() {
    let cases = [
        (
            Event::Hello {
                protocol: VERSION,
                driver: "qrhi".into(),
            },
            json!({"event": "hello", "protocol": 1, "driver": "qrhi"}),
        ),
        (
            Event::Surface {
                generation: 2,
                state: SurfaceState::Closed,
                display: "browser:client-1".into(),
                size: None,
                description: None,
                cause: Some(Cause::Disconnected),
            },
            json!({"event": "surface", "generation": 2, "state": "closed",
                   "display": "browser:client-1", "cause": "disconnected"}),
        ),
        (
            Event::output_error(Some(7), &OutputError::Disconnected),
            json!({"event": "error", "frame": 7, "kind": "disconnected",
                   "message": "display disconnected"}),
        ),
        (
            Event::Captured {
                frame: 7,
                result: CaptureOutcome::Unavailable,
                reason: Some("none".into()),
                artifact: None,
                cause: None,
            },
            json!({"event": "captured", "frame": 7, "result": "unavailable", "reason": "none"}),
        ),
        (
            Event::Captured {
                frame: 7,
                result: CaptureOutcome::Pass,
                reason: None,
                artifact: Some("000001-frame-7".into()),
                cause: None,
            },
            json!({"event": "captured", "frame": 7, "result": "pass",
                   "artifact": "000001-frame-7"}),
        ),
        (
            Event::Captured {
                frame: 7,
                result: CaptureOutcome::Cancelled,
                reason: None,
                artifact: None,
                cause: Some(Cause::Replaced),
            },
            json!({"event": "captured", "frame": 7, "result": "cancelled", "cause": "replaced"}),
        ),
        (
            Event::Saved {
                frame: 7,
                artifact: "000001-frame-7".into(),
            },
            json!({"event": "saved", "frame": 7, "artifact": "000001-frame-7"}),
        ),
        (Event::Stopped { error: None }, json!({"event": "stopped"})),
    ];
    for (event, want) in cases {
        let line = event.to_line();
        assert!(!line.contains('\n'));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&line).unwrap(),
            want
        );
        assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), event);
    }
}

#[test]
fn error_kinds_follow_the_driver_error() {
    let unsupported = OutputError::Unsupported {
        feature: "f".into(),
        reason: "r".into(),
    };
    assert_eq!(ErrorKind::of(&unsupported), ErrorKind::Unsupported);
    assert_eq!(
        ErrorKind::of(&OutputError::Disconnected),
        ErrorKind::Disconnected
    );
    assert_eq!(ErrorKind::of(&OutputError::Timeout), ErrorKind::Timeout);
    assert_eq!(
        ErrorKind::of(&OutputError::Platform("p".into())),
        ErrorKind::Platform
    );
}

#[test]
fn query_keeps_states_reasons_and_unread_values() {
    let query = Query {
        capabilities: Capabilities {
            depths: vec![(
                BitDepth::Twelve,
                DepthSupport {
                    available: Support::Unknown("scale".into()),
                    path: "FP16".into(),
                    conversion: Conversion::ScRgb80,
                    exact: Support::No("FP16".into()),
                },
            )],
            encodings: Vec::new(),
            hdr_metadata: Support::Yes,
            pixel_exact: Support::No("ratio".into()),
        },
        mode: ModeSettings {
            hdr_on: None,
            gamut: Some(Gamut::Rec2020),
            wire_depth: Some(10),
            peak_nits: None,
            min_nits: None,
            sdr_white_nits: Some(240.0),
            pixel_ratio: 1.5,
            description: "mode".into(),
        },
    };
    let value: serde_json::Value =
        serde_json::from_str(&Event::query("qrhi:a".into(), &query).to_line()).unwrap();
    assert_eq!(
        value,
        json!({
            "event": "query",
            "display": "qrhi:a",
            "capabilities": {
                "depths": [{
                    "depth": 12,
                    "available": {"state": "unknown", "reason": "scale"},
                    "path": "FP16",
                    "conversion": "sc_rgb80",
                    "exact": {"state": "no", "reason": "FP16"}
                }],
                "encodings": [],
                "hdr_metadata": {"state": "yes"},
                "pixel_exact": {"state": "no", "reason": "ratio"}
            },
            "mode": {
                "hdr_on": null, "gamut": "rec2020", "wire_depth": 10,
                "peak_nits": null, "min_nits": null, "sdr_white_nits": 240.0,
                "pixel_ratio": 1.5, "description": "mode"
            }
        })
    );
}
