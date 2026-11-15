//! The mock page and the PNG reader of the process tests. A
//! `tokio-tungstenite` client takes the place of the page, so no browser or
//! window system is necessary. `zune-png` shares no code with the `png`
//! encoder.

// Each test binary uses a part.
#![allow(dead_code)]

use std::io::Cursor;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use zune_png::PngDecoder;
use zune_png::zune_core::result::DecodingResult;

pub type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// The fixed pairing code of the host under test.
pub const CODE: &str = "0042";
/// The acknowledgement timeout of the browser service.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound of each wait; longer than [`ACK_TIMEOUT`].
pub const WAIT: Duration = Duration::from_secs(20);

/// The mock page: a WebSocket client.
pub struct Page {
    pub ws: Ws,
    pub token: String,
    addr: SocketAddr,
}

impl Page {
    /// Connects and receives `auth_required`.
    pub async fn connect(addr: SocketAddr) -> Self {
        let (ws, _) = connect_async(format!("ws://{addr}/api/v1/ws"))
            .await
            .expect("WebSocket upgrade");
        let mut page = Self {
            ws,
            token: String::new(),
            addr,
        };
        assert_eq!(page.next_json().await, json!({"type": "auth_required"}));
        page
    }

    /// Sends [`CODE`] and keeps the token.
    pub async fn authenticate(&mut self) {
        self.send(&json!({"type": "auth", "code": CODE})).await;
        let reply = self.next_json().await;
        assert_eq!(reply["type"], "authenticated", "{reply}");
        reply["token"]
            .as_str()
            .expect("token")
            .clone_into(&mut self.token);
    }

    pub async fn send(&mut self, value: &Value) {
        self.ws
            .send(Message::text(value.to_string()))
            .await
            .unwrap();
    }

    /// HDR Chromium at pixel ratio 1, full screen: one CSS pixel per
    /// display pixel.
    pub async fn hello(&mut self, (width, height): (usize, usize)) {
        self.send(&json!({
            "type": "hello",
            "client": {
                "userAgent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                              (KHTML, like Gecko) Chrome/142.0.0.0 Safari/537.36",
                "webview": false
            },
            "screen": {"width": width, "height": height},
            "viewport": {"width": width, "height": height, "scale": 1},
            "pixelRatio": 1,
            "dynamicRange": "high",
            "colorGamut": "rec2020",
            "fullscreen": true
        }))
        .await;
    }

    pub async fn next_json(&mut self) -> Value {
        loop {
            let message = timeout(WAIT, self.ws.next())
                .await
                .expect("server message")
                .expect("WebSocket open")
                .expect("WebSocket message");
            match message {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Ping(_) | Message::Pong(_) => {}
                other => panic!("expected a JSON message, got {other:?}"),
            }
        }
    }

    /// Returns the frame ID and size of the next `show`.
    pub async fn expect_show(&mut self) -> (u64, (usize, usize)) {
        let show = self.next_json().await;
        assert_eq!(show["type"], "show", "{show}");
        let size = |key: &str| usize::try_from(show[key].as_u64().expect("size")).unwrap();
        (
            show["id"].as_u64().expect("id"),
            (size("width"), size("height")),
        )
    }

    pub async fn applied(&mut self, id: u64, (width, height): (usize, usize)) {
        self.send(&json!({
            "type": "applied", "id": id,
            "naturalWidth": width, "naturalHeight": height,
            "layoutWidth": width, "layoutHeight": height
        }))
        .await;
    }

    /// HTTP status and body of the frame request with the bearer token.
    pub async fn fetch(&self, id: u64) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(self.addr).await.unwrap();
        let request = format!(
            "GET /api/v1/frame/{id}.png HTTP/1.1\r\nHost: {}\r\n\
             Authorization: Bearer {}\r\nConnection: close\r\n\r\n",
            self.addr, self.token
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        timeout(WAIT, stream.read_to_end(&mut response))
            .await
            .expect("HTTP response")
            .unwrap();
        let split = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("HTTP head");
        let head = String::from_utf8_lossy(&response[..split]);
        // A `Bytes` body has a `Content-Length`; no chunked decoding needed.
        assert!(
            !head.to_ascii_lowercase().contains("transfer-encoding"),
            "{head}"
        );
        let status = head[9..12].parse().expect("status code");
        (status, response[split + 4..].to_vec())
    }

    /// The server sends no further message; it closes the connection.
    pub async fn expect_end(&mut self) {
        loop {
            match timeout(WAIT, self.ws.next()).await.expect("connection end") {
                None | Some(Ok(Message::Close(_)) | Err(_)) => return,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(other)) => panic!("expected the end, got {other:?}"),
            }
        }
    }
}

/// Size and 16-bit RGB samples of a PNG without alpha.
pub fn decode_png(png: &[u8]) -> ((usize, usize), Vec<u16>) {
    let mut decoder = PngDecoder::new(Cursor::new(png));
    let DecodingResult::U16(samples) = decoder.decode().expect("decode") else {
        panic!("expected 16-bit samples");
    };
    let size = decoder.dimensions().expect("dimensions");
    assert_eq!(samples.len(), size.0 * size.1 * 3, "RGB without alpha");
    (size, samples)
}

/// The reference PNG sample of each code of a format, from the fixtures,
/// not from the encoder.
pub fn sample_table(encoding: &str, depth: u32) -> Vec<u16> {
    let scalar = nitpick_fixtures::scalar().expect("scalar fixture");
    scalar
        .browser
        .into_iter()
        .find(|t| t.encoding == encoding && t.depth == depth)
        .expect("fixture table of the format")
        .sample
}

/// Panics at the first sample that differs, with its pixel position.
pub fn assert_samples(samples: &[u16], expected: &[u16], width: usize, what: &str) {
    assert_eq!(samples.len(), expected.len(), "{what}");
    if let Some(i) = samples.iter().zip(expected).position(|(a, b)| a != b) {
        let pixel = i / 3;
        panic!(
            "{what}: pixel ({}, {}) channel {}: sample {} != expected {}",
            pixel % width,
            pixel / width,
            i % 3,
            samples[i],
            expected[i]
        );
    }
}
