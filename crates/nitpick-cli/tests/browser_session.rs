//! The command-line host as a separate process with its real browser
//! service. A `tokio-tungstenite` client takes the place of the page, so no
//! browser or window system is necessary. The test decodes the fetched PNG
//! with `zune-png`, which shares no code with the `png` encoder, and takes
//! the expected samples from the reference fixtures, not from the encoder.

use std::io::Cursor;
use std::net::SocketAddr;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nitpick_core::pattern::{MIN_SIZE, PatternKind};
use nitpick_core::signal::{BitDepth, Encoding, Format};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{Instant, timeout, timeout_at};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use zune_png::PngDecoder;
use zune_png::zune_core::result::DecodingResult;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const CODE: &str = "0042";
/// The acknowledgement timeout of the browser service.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound of each wait; longer than [`ACK_TIMEOUT`].
const WAIT: Duration = Duration::from_secs(20);
const VIEWPORT: (usize, usize) = MIN_SIZE;

/// The `nitpick-cli` process. Each test starts its own on a free port.
struct Cli {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
    /// Every stdout line read so far.
    seen: Vec<String>,
    addr: SocketAddr,
}

impl Cli {
    async fn start(delay_ms: u32) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nitpick-cli"))
            .args(["--code", CODE, "--delay", &delay_ms.to_string()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start nitpick-cli");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().expect("piped")).lines();
        let mut cli = Self {
            child,
            stdin,
            stdout,
            seen: Vec::new(),
            addr: SocketAddr::from(([0, 0, 0, 0], 0)),
        };
        let url = cli.until("url: ").await;
        cli.addr = url["url: http://".len()..]
            .trim_end_matches('/')
            .parse()
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        assert!(cli.addr.ip().is_loopback(), "{url}");
        cli
    }

    async fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
    }

    /// Reads lines up to and including the first one that starts with
    /// `prefix`.
    async fn until(&mut self, prefix: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let line = timeout_at(deadline, self.stdout.next_line())
                .await
                .unwrap_or_else(|_| panic!("no `{prefix}` line; seen: {:#?}", self.seen))
                .expect("read stdout")
                .unwrap_or_else(|| panic!("stdout ended before `{prefix}`: {:#?}", self.seen));
            self.seen.push(line.clone());
            if line.starts_with(prefix) {
                return line;
            }
        }
    }

    /// The host handles console lines and service events in order, so the
    /// reply to `displays` follows all output of earlier events.
    async fn sync(&mut self) {
        self.send("displays").await;
        self.until("display").await;
    }

    fn since(&self, mark: usize) -> &[String] {
        &self.seen[mark..]
    }

    /// Waits for the exit after `quit` or the end of stdin.
    async fn exit(mut self) -> (ExitStatus, Vec<String>) {
        // `wait` closes stdin first.
        let status = timeout(WAIT, self.child.wait())
            .await
            .expect("exit")
            .unwrap();
        while let Ok(Some(line)) = self.stdout.next_line().await {
            self.seen.push(line);
        }
        (status, self.seen)
    }

    async fn quit(mut self) -> (ExitStatus, Vec<String>) {
        self.send("quit").await;
        self.exit().await
    }
}

fn no_errors(lines: &[String]) {
    let errors: Vec<_> = lines.iter().filter(|l| l.starts_with("error:")).collect();
    assert!(errors.is_empty(), "{errors:#?}");
}

fn no_readiness(lines: &[String]) {
    let ready: Vec<_> = lines
        .iter()
        .filter(|l| {
            l.starts_with("status: waiting for measurement delay")
                || l.starts_with("status: ready after delay")
        })
        .collect();
    assert!(ready.is_empty(), "{ready:#?}");
}

/// The mock page: an authenticated WebSocket client.
struct Page {
    ws: Ws,
    token: String,
    addr: SocketAddr,
}

impl Page {
    async fn connect(cli: &mut Cli) -> Self {
        let (ws, _) = connect_async(format!("ws://{}/api/v1/ws", cli.addr))
            .await
            .expect("WebSocket upgrade");
        let mut page = Self {
            ws,
            token: String::new(),
            addr: cli.addr,
        };
        assert_eq!(page.next_json().await, json!({"type": "auth_required"}));
        assert_eq!(
            cli.until("pairing code: ").await,
            format!("pairing code: {CODE}")
        );
        page.send(&json!({"type": "auth", "code": CODE})).await;
        let reply = page.next_json().await;
        assert_eq!(reply["type"], "authenticated", "{reply}");
        page.token = reply["token"].as_str().expect("token").to_owned();
        cli.until("client: authenticated").await;
        page
    }

    /// Connects, sends `hello` and selects the client. Returns its display
    /// ID.
    async fn ready(cli: &mut Cli) -> (Self, String) {
        let mut page = Self::connect(cli).await;
        page.hello(VIEWPORT).await;
        let line = cli.until("client: ready: ").await;
        let display = line["client: ready: ".len()..]
            .split_once(' ')
            .expect("display ID")
            .0
            .to_owned();
        assert!(
            line.contains(&format!("{}×{} px", VIEWPORT.0, VIEWPORT.1)),
            "{line}"
        );
        cli.send(&format!("select {display}")).await;
        assert_eq!(
            cli.until("selected: ").await,
            format!("selected: {display}")
        );
        (page, display)
    }

    async fn send(&mut self, value: &Value) {
        self.ws
            .send(Message::text(value.to_string()))
            .await
            .unwrap();
    }

    /// HDR Chromium at pixel ratio 1, full screen: one CSS pixel per
    /// display pixel.
    async fn hello(&mut self, (width, height): (usize, usize)) {
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

    async fn next_json(&mut self) -> Value {
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
    async fn expect_show(&mut self) -> (u64, (usize, usize)) {
        let show = self.next_json().await;
        assert_eq!(show["type"], "show", "{show}");
        let size = |key: &str| usize::try_from(show[key].as_u64().expect("size")).unwrap();
        (
            show["id"].as_u64().expect("id"),
            (size("width"), size("height")),
        )
    }

    async fn applied(&mut self, id: u64, (width, height): (usize, usize)) {
        self.send(&json!({
            "type": "applied", "id": id,
            "naturalWidth": width, "naturalHeight": height,
            "layoutWidth": width, "layoutHeight": height
        }))
        .await;
    }

    /// HTTP status and body of the frame request with the bearer token.
    async fn fetch(&self, id: u64) -> (u16, Vec<u8>) {
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
    async fn expect_end(&mut self) {
        loop {
            match timeout(WAIT, self.ws.next()).await.expect("connection end") {
                None | Some(Ok(Message::Close(_)) | Err(_)) => return,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(other)) => panic!("expected the end, got {other:?}"),
            }
        }
    }
}

/// Renders `pattern` with the core and maps each code through the
/// reference PNG sample table.
fn expected_samples(pattern: PatternKind, format: Format, (w, h): (usize, usize)) -> Vec<u16> {
    let scalar = nitpick_fixtures::scalar().expect("scalar fixture");
    let table = scalar
        .browser
        .iter()
        .find(|t| t.encoding == format.encoding.id() && t.depth == format.depth.bits())
        .expect("fixture table of the format");
    let image = pattern.render(format, w, h).expect("render");
    image
        .pixels()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b])
        .map(|code| table.sample[usize::from(code)])
        .collect()
}

fn assert_png(png: &[u8], pattern: PatternKind, format: Format, size: (usize, usize)) {
    let mut decoder = PngDecoder::new(Cursor::new(png));
    let DecodingResult::U16(samples) = decoder.decode().expect("decode") else {
        panic!("expected 16-bit samples");
    };
    assert_eq!(decoder.dimensions(), Some(size));
    let expected = expected_samples(pattern, format, size);
    assert_eq!(samples.len(), expected.len(), "RGB without alpha");
    if let Some(i) = samples.iter().zip(&expected).position(|(a, b)| a != b) {
        let pixel = i / 3;
        panic!(
            "{format}: pixel ({}, {}) channel {}: sample {} != expected {}",
            pixel % size.0,
            pixel / size.0,
            i % 3,
            samples[i],
            expected[i]
        );
    }
}

fn frame_tag(id: u64) -> String {
    format!("Frame {id} applied by the client")
}

/// Shows a frame, checks the PNG and acknowledges it. Returns the frame ID.
async fn show_and_check(
    cli: &mut Cli,
    page: &mut Page,
    pattern: PatternKind,
    format: Format,
) -> u64 {
    cli.send("show").await;
    cli.until("status: submitting pattern").await;
    let (id, size) = page.expect_show().await;
    assert_eq!(size, VIEWPORT);
    let (status, png) = page.fetch(id).await;
    assert_eq!(status, 200);
    assert_png(&png, pattern, format, size);
    id
}

#[tokio::test(flavor = "current_thread")]
async fn presents_replaces_and_shuts_down() {
    const DELAY: Duration = Duration::from_millis(200);
    let mut cli = Cli::start(200).await;
    let (mut page, _) = Page::ready(&mut cli).await;

    let first_format = Format::new(Encoding::PqNarrow, BitDepth::Ten);
    let first = show_and_check(&mut cli, &mut page, PatternKind::ColourBars, first_format).await;

    // Replacement before the acknowledgement.
    cli.send("format pq-full 12").await;
    cli.until("format: ").await;
    cli.send("show").await;
    cli.until("status: presentation cancelled").await;
    let (second, size) = page.expect_show().await;
    assert!(second > first);
    assert_eq!(page.fetch(first).await.0, 404, "only the latest frame");
    let mark = cli.seen.len();
    page.applied(first, size).await;
    cli.sync().await;
    no_readiness(cli.since(mark));

    let (status, png) = page.fetch(second).await;
    assert_eq!(status, 200);
    let second_format = Format::new(Encoding::PqFull, BitDepth::Twelve);
    assert_png(&png, PatternKind::ColourBars, second_format, size);
    let acked = Instant::now();
    page.applied(second, size).await;
    let waiting = cli
        .until("status: waiting for measurement delay: 200 ms.")
        .await;
    assert!(waiting.contains(&frame_tag(second)), "{waiting}");
    assert!(waiting.contains("Pixel-exact layout"), "{waiting}");
    let ready = cli.until("status: ready after delay: 200 ms.").await;
    assert!(acked.elapsed() >= DELAY);
    assert!(ready.contains(&frame_tag(second)), "{ready}");

    // Replacement of a ready frame, with a new pattern.
    cli.send("pattern pq-grayscale").await;
    cli.until("pattern: ").await;
    cli.send("format pq-narrow 10").await;
    cli.until("format: ").await;
    let third = show_and_check(&mut cli, &mut page, PatternKind::Grayscale, first_format).await;
    assert!(third > second);
    assert_eq!(page.fetch(second).await.0, 404);
    page.applied(third, VIEWPORT).await;
    let ready = cli.until("status: ready after delay: 200 ms.").await;
    assert!(ready.contains(&frame_tag(third)), "{ready}");
    assert!(!cli.seen.iter().any(|l| l.contains(&frame_tag(first))));

    let (exit, seen) = cli.quit().await;
    // The host closes the surface before the service stops.
    assert_eq!(page.next_json().await, json!({"type": "close"}));
    page.expect_end().await;
    assert!(exit.success(), "{exit}");
    no_errors(&seen);
}

#[tokio::test(flavor = "current_thread")]
async fn acknowledgement_timeout_starts_no_delay() {
    let mut cli = Cli::start(200).await;
    let (mut page, _) = Page::ready(&mut cli).await;

    // The service starts its timer after this command, so the elapsed time
    // from here is an upper bound of the timer.
    let shown = Instant::now();
    cli.send("show").await;
    let (id, size) = page.expect_show().await;
    let mark = cli.seen.len();
    let failed = cli.until("error: presentation failed").await;
    assert!(shown.elapsed() >= ACK_TIMEOUT);
    assert_eq!(
        failed,
        "error: presentation failed: driver acknowledgement timed out"
    );

    // A late acknowledgement changes nothing.
    page.applied(id, size).await;
    cli.sync().await;
    no_readiness(cli.since(mark));

    // The session stays usable.
    cli.send("show").await;
    let (next, size) = page.expect_show().await;
    assert!(next > id);
    page.applied(next, size).await;
    let ready = cli.until("status: ready after delay: 200 ms.").await;
    assert!(ready.contains(&frame_tag(next)), "{ready}");

    let (exit, _) = cli.quit().await;
    assert!(exit.success(), "{exit}");
}

#[tokio::test(flavor = "current_thread")]
async fn delay_outlasts_the_acknowledgement_timeout() {
    let delay = ACK_TIMEOUT + Duration::from_secs(1);
    let delay_ms = u32::try_from(delay.as_millis()).unwrap();
    let mut cli = Cli::start(delay_ms).await;
    let (mut page, _) = Page::ready(&mut cli).await;

    cli.send("show").await;
    let (id, size) = page.expect_show().await;
    let acked = Instant::now();
    page.applied(id, size).await;
    cli.until(&format!(
        "status: waiting for measurement delay: {delay_ms} ms."
    ))
    .await;
    let ready = cli
        .until(&format!("status: ready after delay: {delay_ms} ms."))
        .await;
    assert!(acked.elapsed() >= delay);
    assert!(ready.contains(&frame_tag(id)), "{ready}");

    let (exit, seen) = cli.quit().await;
    assert!(exit.success(), "{exit}");
    no_errors(&seen);
}

#[tokio::test(flavor = "current_thread")]
async fn disconnect_cancels_presentation() {
    let mut cli = Cli::start(200).await;
    let (mut page, display) = Page::ready(&mut cli).await;

    cli.send("show").await;
    page.expect_show().await;
    let mark = cli.seen.len();
    page.ws.close(None).await.unwrap();
    cli.until("client: disconnected: ").await;
    cli.sync().await;
    let lines = cli.since(mark);
    assert_eq!(lines.last().map(String::as_str), Some("displays: none"));
    no_readiness(lines);
    // The driver result and the service event race; either ends the frame.
    assert!(
        lines.iter().any(|l| l == "status: presentation cancelled"
            || l == "error: presentation failed: display disconnected"),
        "{lines:#?}"
    );

    // The disconnect clears the selection; a new session is a new display.
    cli.send("show").await;
    cli.until("error: no client selected").await;
    let (_page, new_display) = Page::ready(&mut cli).await;
    assert_ne!(new_display, display);

    // The end of stdin is the same as `quit`.
    cli.stdin = None;
    let (exit, _) = cli.exit().await;
    assert!(exit.success(), "{exit}");
}

#[tokio::test(flavor = "current_thread")]
async fn disconnect_cancels_delay() {
    let mut cli = Cli::start(60_000).await;
    let (mut page, _) = Page::ready(&mut cli).await;

    cli.send("show").await;
    let (id, size) = page.expect_show().await;
    page.applied(id, size).await;
    cli.until("status: waiting for measurement delay: 60000 ms.")
        .await;
    let mark = cli.seen.len();
    page.ws.close(None).await.unwrap();
    cli.until("client: disconnected: ").await;
    cli.until("status: measurement delay cancelled").await;
    cli.sync().await;
    no_readiness(cli.since(mark));

    let (exit, seen) = cli.quit().await;
    assert!(exit.success(), "{exit}");
    no_errors(&seen);
}

#[tokio::test(flavor = "current_thread")]
async fn viewport_change_cancels_delay_and_shows_again() {
    let mut cli = Cli::start(60_000).await;
    let (mut page, display) = Page::ready(&mut cli).await;

    cli.send("show").await;
    let (first, size) = page.expect_show().await;
    page.applied(first, size).await;
    cli.until("status: waiting for measurement delay: 60000 ms.")
        .await;
    // Applies to the frame shown again after the change.
    cli.send("delay 0").await;
    cli.until("delay: 0 ms").await;

    let resized = (800, 450);
    page.hello(resized).await;
    assert_eq!(
        cli.until("client: viewport changed: ").await,
        format!("client: viewport changed: {display}")
    );
    cli.until("status: measurement delay cancelled").await;
    let (second, size) = page.expect_show().await;
    assert!(second > first);
    assert_eq!(size, resized);
    let (status, png) = page.fetch(second).await;
    assert_eq!(status, 200);
    let format = Format::new(Encoding::PqNarrow, BitDepth::Ten);
    assert_png(&png, PatternKind::ColourBars, format, size);
    page.applied(second, size).await;
    let ready = cli.until("status: ready after delay: 0 ms.").await;
    assert!(ready.contains(&frame_tag(second)), "{ready}");
    assert!(ready.contains("800×450 display px"), "{ready}");

    let (exit, seen) = cli.quit().await;
    assert!(exit.success(), "{exit}");
    no_errors(&seen);
}
