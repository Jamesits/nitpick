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
    assert!(matches!(
        ClientMessage::parse(r#"{"type":"applied","id":1}"#),
        Ok(ClientMessage::Applied(_))
    ));
    assert!(matches!(
        ClientMessage::parse(r#"{"type":"error","id":1,"message":"x"}"#),
        Ok(ClientMessage::Error(_))
    ));
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
    ] {
        assert!(ClientMessage::parse(text).is_err(), "{text}");
    }
}
