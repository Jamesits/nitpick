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
    Authenticated {
        token: String,
    },
    /// Fetch and show frame `id`; the PNG has this size.
    Show {
        id: u64,
        width: usize,
        height: usize,
    },
    /// Show a black page.
    Close,
}

/// The `hello` fields are checked by [`crate::client::Hello::parse`], so
/// that an invalid `hello` has its own close reason.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum ClientMessage {
    /// A string, not a number: a number loses leading zeros.
    Auth {
        code: String,
    },
    Hello(Map<String, Value>),
    Applied(Applied),
    Error {
        id: u64,
        message: String,
    },
}

/// The client decoded and laid out frame `id`. The natural size is in image
/// pixels; the layout size is in display pixels.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Applied {
    pub(crate) id: u64,
    pub(crate) natural_width: u64,
    pub(crate) natural_height: u64,
    pub(crate) layout_width: f64,
    pub(crate) layout_height: f64,
}

impl Applied {
    /// Layout sizes are finite and not negative.
    pub(crate) fn is_valid(&self) -> bool {
        [self.layout_width, self.layout_height]
            .iter()
            .all(|v| v.is_finite() && *v >= 0.0)
    }
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
