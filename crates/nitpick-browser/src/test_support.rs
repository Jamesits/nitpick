//! Test data shared by the unit tests of this crate.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::client::Hello;
use crate::service::{EventSender, ServiceEvents};
use crate::session::{Incoming, Pairing, SessionHub, Transport, run_session};

/// TEST-NET-1 (RFC 5737).
pub(crate) const PEER: SocketAddr =
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 50_000);

pub(crate) const SAFARI_IPAD: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";
pub(crate) const CHROME_WINDOWS: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
pub(crate) const EDGE_WINDOWS: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36 Edg/142.0.0.0";
pub(crate) const CHROME_LINUX: &str = "Mozilla/5.0 (X11; Linux x86_64) \
    AppleWebKit/537.36 (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36";
pub(crate) const FIREFOX_WINDOWS: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:145.0) Gecko/20100101 Firefox/145.0";
pub(crate) const FIREFOX_LINUX: &str =
    "Mozilla/5.0 (X11; Linux x86_64; rv:145.0) Gecko/20100101 Firefox/145.0";
pub(crate) const FIREFOX_MACOS: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 15.6; rv:145.0) Gecko/20100101 Firefox/145.0";
pub(crate) const FIREFOX_ANDROID: &str =
    "Mozilla/5.0 (Android 15; Mobile; rv:145.0) Gecko/145.0 Firefox/145.0";
pub(crate) const WEBKITGTK: &str = "Mozilla/5.0 (X11; Ubuntu; Linux x86_64) \
    AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Safari/605.1.15";

/// iPad Safari in full screen: 1180×820 CSS px at pixel ratio 2.
pub(crate) fn hello_json() -> Value {
    json!({
        "type": "hello",
        "client": {"userAgent": SAFARI_IPAD, "webview": false},
        "screen": {"width": 1180, "height": 820},
        "viewport": {"width": 1180, "height": 820, "scale": 1},
        "pixelRatio": 2,
        "dynamicRange": "high",
        "colorGamut": "p3",
        "fullscreen": true
    })
}

/// [`hello_json`] with `changes` applied; `None` removes the field.
pub(crate) fn hello_with(changes: &[(&str, Option<Value>)]) -> Value {
    let mut value = hello_json();
    for (path, change) in changes {
        let mut parts: Vec<&str> = path.split('.').collect();
        let last = parts.pop().expect("non-empty path");
        let mut object = &mut value;
        for part in parts {
            object = &mut object[part];
        }
        let object = object.as_object_mut().expect("object");
        match change {
            Some(v) => {
                object.insert(last.into(), v.clone());
            }
            None => {
                object.remove(last);
            }
        }
    }
    value
}

/// Fields as the session passes them to [`Hello::parse`].
pub(crate) fn fields(value: &Value) -> Map<String, Value> {
    let mut map = value.as_object().expect("object").clone();
    map.remove("type");
    map
}

pub(crate) fn hello(changes: &[(&str, Option<Value>)]) -> Hello {
    Hello::parse(fields(&hello_with(changes))).expect("valid hello")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outgoing {
    Text(Value),
    Close(u16, &'static str),
}

pub(crate) struct MockTransport {
    from_client: mpsc::UnboundedReceiver<Incoming>,
    to_client: mpsc::UnboundedSender<Outgoing>,
}

impl Transport for MockTransport {
    async fn recv(&mut self) -> Incoming {
        self.from_client
            .recv()
            .await
            .unwrap_or_else(|| Incoming::Closed("client closed".into()))
    }

    fn send(&mut self, text: String) -> impl Future<Output = Result<(), String>> + Send {
        let value = serde_json::from_str(&text).expect("server sends JSON");
        let result = self
            .to_client
            .send(Outgoing::Text(value))
            .map_err(|_| "client gone".into());
        std::future::ready(result)
    }

    async fn close(&mut self, code: u16, reason: &'static str) {
        let _ = self.to_client.send(Outgoing::Close(code, reason));
    }
}

/// The client end of a mock connection.
pub(crate) struct Client {
    pub(crate) tx: mpsc::UnboundedSender<Incoming>,
    pub(crate) rx: mpsc::UnboundedReceiver<Outgoing>,
    pub(crate) task: JoinHandle<()>,
}

impl Client {
    pub(crate) fn send_text(&self, text: &str) {
        self.tx.send(Incoming::Text(text.into())).unwrap();
    }

    pub(crate) fn send_json(&self, value: &Value) {
        self.send_text(&value.to_string());
    }

    pub(crate) fn auth(&self, code: &str) {
        self.send_json(&json!({"type": "auth", "code": code}));
    }

    pub(crate) async fn next(&mut self) -> Option<Outgoing> {
        self.rx.recv().await
    }

    /// Waits for the session to end and the reservation to be released.
    pub(crate) async fn finished(self) -> Vec<Outgoing> {
        let Self { tx, mut rx, task } = self;
        drop(tx);
        task.await.unwrap();
        let mut rest = Vec::new();
        while let Ok(message) = rx.try_recv() {
            rest.push(message);
        }
        rest
    }
}

pub(crate) fn hub(pairing: Pairing) -> (Arc<SessionHub>, ServiceEvents) {
    let (sender, events) = EventSender::channel();
    (SessionHub::new(sender, pairing), events)
}

pub(crate) fn connect(hub: &Arc<SessionHub>) -> Client {
    let reservation = hub.reserve(PEER).expect("slot free");
    let (tx, from_client) = mpsc::unbounded_channel();
    let (to_client, rx) = mpsc::unbounded_channel();
    let transport = MockTransport {
        from_client,
        to_client,
    };
    Client {
        tx,
        rx,
        task: tokio::spawn(run_session(transport, reservation)),
    }
}

pub(crate) fn auth_required() -> Outgoing {
    Outgoing::Text(json!({"type": "auth_required"}))
}

pub(crate) async fn expect_token(client: &mut Client) -> String {
    match client.next().await {
        Some(Outgoing::Text(value)) if value["type"] == "authenticated" => {
            value["token"].as_str().expect("token string").to_owned()
        }
        other => panic!("expected authenticated, got {other:?}"),
    }
}

/// Authenticates a new connection with the fixed code.
pub(crate) async fn authenticated_client(hub: &Arc<SessionHub>, code: &str) -> (Client, String) {
    let mut client = connect(hub);
    assert_eq!(client.next().await, Some(auth_required()));
    client.auth(code);
    let token = expect_token(&mut client).await;
    (client, token)
}
