use serde_json::json;

use super::*;

#[test]
fn server_messages_have_the_interface_shape() {
    assert_eq!(
        ServerMessage::AuthRequired.to_json(),
        r#"{"type":"auth_required"}"#
    );
    let json: Value =
        serde_json::from_str(&ServerMessage::Authenticated { token: "ab".into() }.to_json())
            .unwrap();
    assert_eq!(json, json!({"type": "authenticated", "token": "ab"}));
    let show = ServerMessage::Show {
        id: 3,
        width: 2360,
        height: 1640,
    };
    let json: Value = serde_json::from_str(&show.to_json()).unwrap();
    assert_eq!(
        json,
        json!({"type": "show", "id": 3, "width": 2360, "height": 1640})
    );
    assert_eq!(ServerMessage::Close.to_json(), r#"{"type":"close"}"#);
}

#[test]
fn auth_code_keeps_leading_zeros() {
    assert_eq!(
        ClientMessage::parse(r#"{"type":"auth","code":"0042"}"#).unwrap(),
        ClientMessage::Auth {
            code: "0042".into()
        }
    );
}

#[test]
fn numeric_auth_code_is_rejected() {
    assert!(ClientMessage::parse(r#"{"type":"auth","code":42}"#).is_err());
}

#[test]
fn other_client_messages_keep_their_fields() {
    let ClientMessage::Hello(fields) =
        ClientMessage::parse(r#"{"type":"hello","pixelRatio":2}"#).unwrap()
    else {
        panic!("not hello");
    };
    assert_eq!(fields.get("pixelRatio"), Some(&json!(2)));
    assert_eq!(
        ClientMessage::parse(
            r#"{"type":"applied","id":1,"naturalWidth":4,"naturalHeight":2,
                "layoutWidth":4,"layoutHeight":2.5}"#
        )
        .unwrap(),
        ClientMessage::Applied(Applied {
            id: 1,
            natural_width: 4,
            natural_height: 2,
            layout_width: 4.0,
            layout_height: 2.5,
        })
    );
    assert_eq!(
        ClientMessage::parse(r#"{"type":"error","id":1,"message":"x"}"#).unwrap(),
        ClientMessage::Error {
            id: 1,
            message: "x".into()
        }
    );
}

#[test]
fn applied_layout_must_be_finite_and_not_negative() {
    let applied = Applied {
        id: 1,
        natural_width: 4,
        natural_height: 2,
        layout_width: 0.0,
        layout_height: 2.0,
    };
    assert!(applied.is_valid());
    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(
            !Applied {
                layout_width: bad,
                ..applied
            }
            .is_valid()
        );
        assert!(
            !Applied {
                layout_height: bad,
                ..applied
            }
            .is_valid()
        );
    }
}

#[test]
fn unknown_or_malformed_messages_are_rejected() {
    for text in [
        "",
        "not json",
        "[]",
        r#"{"code":"0042"}"#,
        r#"{"type":"show","id":1}"#,
        r#"{"type":"auth"}"#,
        r#"{"type":"applied","id":1}"#,
        r#"{"type":"applied","id":-1,"naturalWidth":4,"naturalHeight":2,"layoutWidth":4,"layoutHeight":2}"#,
        r#"{"type":"applied","id":"1","naturalWidth":4,"naturalHeight":2,"layoutWidth":4,"layoutHeight":2}"#,
        r#"{"type":"error","id":1}"#,
    ] {
        assert!(ClientMessage::parse(text).is_err(), "{text}");
    }
}
