//! WebSocket messages, close codes and limits. See "Interface" in
//! `doc/design/output_driver/browser.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Largest client message. A `hello` is below 1 KiB; the limit stops a
/// client, authenticated or not, from making the server buffer large
/// messages.
pub(crate) const MAX_CLIENT_MESSAGE: usize = 64 * 1024;

/// WebSocket close codes sent by the server. 4000–4999 are free for
/// applications (RFC 6455, section 7.4.2).
pub(crate) mod close {
    pub(crate) const WRONG_CODE: u16 = 4001;
    pub(crate) const AUTH_TIMEOUT: u16 = 4002;
    /// Malformed, unknown, binary or out-of-order message.
    pub(crate) const PROTOCOL: u16 = 4003;
    /// RFC 6455 "internal error": no random data for the code or token.
    pub(crate) const INTERNAL: u16 = 1011;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ServerMessage {
    AuthRequired,
    Authenticated { token: String },
}

/// The server checks only the message type and the `auth` code here.
/// The fields of the other messages belong to the driver.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ClientMessage {
    /// A string, not a number: a number loses leading zeros.
    Auth {
        code: String,
    },
    Hello(Map<String, Value>),
    Applied(Map<String, Value>),
    Error(Map<String, Value>),
}

impl ClientMessage {
    pub(crate) fn parse(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }
}

impl ServerMessage {
    pub(crate) fn to_json(&self) -> String {
        // A tagged enum of strings cannot fail to serialize.
        serde_json::to_string(self).expect("server message serializes")
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
