//! The command-line host as a separate process with its real browser
//! service and the mock page of `common`. The test takes the expected
//! samples from the reference fixtures, not from the encoder.

mod common;

use std::net::SocketAddr;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use common::{ACK_TIMEOUT, CODE, Page, WAIT, assert_samples, decode_png, sample_table};
use nitpick_core::pattern::{MIN_SIZE, PatternKind};
use nitpick_core::signal::{BitDepth, Encoding, Format};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{Instant, timeout, timeout_at};

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

/// Connects the mock page and authenticates it.
async fn connect(cli: &mut Cli) -> Page {
    let mut page = Page::connect(cli.addr).await;
    assert_eq!(
        cli.until("pairing code: ").await,
        format!("pairing code: {CODE}")
    );
    page.authenticate().await;
    cli.until("client: authenticated").await;
    page
}

/// Connects, sends `hello` and selects the client. Returns its display ID.
async fn ready(cli: &mut Cli) -> (Page, String) {
    let mut page = connect(cli).await;
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

/// Renders `pattern` with the core and maps each code through the
/// reference PNG sample table.
fn expected_samples(pattern: PatternKind, format: Format, (w, h): (usize, usize)) -> Vec<u16> {
    let table = sample_table(format.encoding.id(), format.depth.bits());
    let image = pattern.render(format, w, h).expect("render");
    image
        .pixels()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b])
        .map(|code| table[usize::from(code)])
        .collect()
}

fn assert_png(png: &[u8], pattern: PatternKind, format: Format, size: (usize, usize)) {
    let (decoded, samples) = decode_png(png);
    assert_eq!(decoded, size);
    let expected = expected_samples(pattern, format, size);
    assert_samples(&samples, &expected, size.0, &format.to_string());
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
    let (mut page, _) = ready(&mut cli).await;

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
    let (mut page, _) = ready(&mut cli).await;

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
    let (mut page, _) = ready(&mut cli).await;

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
async fn disconnect_fails_the_presentation() {
    let mut cli = Cli::start(200).await;
    let (mut page, display) = ready(&mut cli).await;

    cli.send("show").await;
    page.expect_show().await;
    let mark = cli.seen.len();
    page.ws.close(None).await.unwrap();
    cli.until("client: disconnected: ").await;
    cli.sync().await;
    let lines = cli.since(mark);
    assert_eq!(lines.last().map(String::as_str), Some("displays: none"));
    no_readiness(lines);
    // The same line whether the driver result or the service event comes
    // first.
    assert_eq!(
        lines
            .iter()
            .filter(
                |l| l.starts_with("status: presentation") || l.starts_with("error: presentation")
            )
            .collect::<Vec<_>>(),
        ["error: presentation failed: display disconnected"],
        "{lines:#?}"
    );

    // The disconnect clears the selection; a new session is a new display.
    cli.send("show").await;
    cli.until("error: no client selected").await;
    let (_page, new_display) = ready(&mut cli).await;
    assert_ne!(new_display, display);

    // The end of stdin is the same as `quit`.
    cli.stdin = None;
    let (exit, _) = cli.exit().await;
    assert!(exit.success(), "{exit}");
}

#[tokio::test(flavor = "current_thread")]
async fn disconnect_cancels_delay() {
    let mut cli = Cli::start(60_000).await;
    let (mut page, _) = ready(&mut cli).await;

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
    let (mut page, display) = ready(&mut cli).await;

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
