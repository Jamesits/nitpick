//! `nitpick-cli --test-host` as a separate process with its real browser
//! service, driven through the test-host protocol. The mock page of
//! `common` takes the place of the browser.

mod common;

use std::net::SocketAddr;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use common::{CODE, Page, WAIT, assert_samples, decode_png, sample_table};
use nitpick_core::pattern::MIN_SIZE;
use nitpick_test_host::protocol::{
    CaptureOutcome, Cause, ConversionReport, DisplayState, ErrorKind, SupportState, SurfaceState,
    VERSION,
};
use nitpick_test_host::{Event, Fixtures, Request};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{Instant, timeout};

const VIEWPORT: (usize, usize) = MIN_SIZE;
const FRAME_A: &str = "state-frame-a-pq-full-10";
const FRAME_B: &str = "state-frame-b-pq-full-10";

/// The host process. Each test starts its own on a free port.
struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Lines<BufReader<ChildStdout>>,
    addr: SocketAddr,
}

impl Host {
    async fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nitpick-cli"))
            .args(["--test-host", "--code", CODE, "--delay", "0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start nitpick-cli");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().expect("piped")).lines();
        let mut host = Self {
            child,
            stdin,
            stdout,
            addr: SocketAddr::from(([0, 0, 0, 0], 0)),
        };
        host.expect(Event::Hello {
            protocol: VERSION,
            driver: "browser".into(),
        })
        .await;
        let Event::Listening { url } = host.next().await else {
            panic!("expected `listening`");
        };
        host.addr = url["http://".len()..]
            .trim_end_matches('/')
            .parse()
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        host
    }

    async fn line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        stdin.flush().await.unwrap();
    }

    async fn send(&mut self, request: &Request) {
        self.line(&serde_json::to_string(request).unwrap()).await;
    }

    async fn next(&mut self) -> Event {
        let line = timeout(WAIT, self.stdout.next_line())
            .await
            .expect("an event in time")
            .expect("read stdout")
            .expect("stdout open");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    async fn expect(&mut self, want: Event) {
        assert_eq!(self.next().await, want);
    }

    /// Requests are handled in order: returns the events between the
    /// earlier output and the reply to `displays`.
    async fn sync(&mut self) -> Vec<Event> {
        self.send(&Request::Displays {}).await;
        let mut events = Vec::new();
        loop {
            match self.next().await {
                Event::Displays { .. } => return events,
                other => events.push(other),
            }
        }
    }

    /// Connects the page and waits for `ready`. Returns its display ID.
    async fn ready(&mut self) -> (Page, String) {
        let mut page = Page::connect(self.addr).await;
        self.expect(display_event(DisplayState::Connected, None))
            .await;
        self.expect(Event::PairingCode { code: CODE.into() }).await;
        page.authenticate().await;
        self.expect(display_event(DisplayState::Authenticated, None))
            .await;
        page.hello(VIEWPORT).await;
        let Event::Display {
            state: DisplayState::Ready,
            display: Some(display),
            ..
        } = self.next().await
        else {
            panic!("expected `ready`");
        };
        (page, display)
    }

    async fn open(&mut self, display: &str, generation: u64, size: (usize, usize)) {
        self.send(&Request::Open {
            display: display.into(),
            depth: 10,
        })
        .await;
        let event = self.next().await;
        let Event::Surface {
            generation: g,
            state: SurfaceState::Open,
            size: Some(s),
            ..
        } = &event
        else {
            panic!("expected an open surface, got {event:?}");
        };
        assert_eq!((*g, *s), (generation, [size.0, size.1]));
    }

    async fn show(&mut self, frame: u64, fixture: &str, delay_ms: u32) {
        self.show_with(frame, fixture, delay_ms, false).await;
    }

    async fn show_with(&mut self, frame: u64, fixture: &str, delay_ms: u32, marker: bool) {
        self.send(&Request::Show {
            frame,
            fixture: fixture.into(),
            delay_ms: Some(delay_ms),
            metadata: None,
            marker,
        })
        .await;
    }

    async fn exit(mut self) -> ExitStatus {
        // `wait` closes stdin first.
        timeout(WAIT, self.child.wait())
            .await
            .expect("exit")
            .unwrap()
    }
}

fn display_event(state: DisplayState, display: Option<&str>) -> Event {
    Event::Display {
        state,
        display: display.map(Into::into),
        reason: None,
    }
}

fn capture(frame: u64) -> Request {
    Request::Capture {
        frame,
        heartbeat_ms: None,
        deadline_ms: None,
    }
}

fn error_kind(event: &Event) -> (Option<u64>, ErrorKind) {
    match event {
        Event::Error { frame, kind, .. } => (*frame, *kind),
        other => panic!("expected `error`, got {other:?}"),
    }
}

/// The fixture at the top-left corner of the surface, nominal black
/// elsewhere, through the reference sample table.
fn assert_fixture_png(png: &[u8], fixture: &str, size: (usize, usize)) {
    let fixtures = Fixtures::load().unwrap();
    let frame = fixtures.get(fixture).unwrap();
    let image = frame.render(size).unwrap();
    let table = sample_table(frame.format.encoding.id(), frame.format.depth.bits());
    let expected: Vec<u16> = image
        .pixels()
        .iter()
        .flat_map(|p| [p.r, p.g, p.b])
        .map(|code| table[usize::from(code)])
        .collect();
    let (decoded, samples) = decode_png(png);
    assert_eq!(decoded, size);
    assert_samples(&samples, &expected, size.0, fixture);
}

#[tokio::test(flavor = "current_thread")]
async fn drives_query_open_show_and_replacement() {
    let mut host = Host::start().await;
    let (mut page, display) = host.ready().await;

    let Event::Displays { displays } = ({
        host.send(&Request::Displays {}).await;
        host.next().await
    }) else {
        panic!("expected `displays`");
    };
    assert_eq!(displays.len(), 1);
    assert_eq!(displays[0].id, display);
    assert_eq!(displays[0].size, [VIEWPORT.0, VIEWPORT.1]);

    host.send(&Request::Query {
        display: display.clone(),
    })
    .await;
    let Event::Query { capabilities, .. } = host.next().await else {
        panic!("expected `query`");
    };
    assert_eq!(capabilities.depths.len(), 2);
    assert!(
        capabilities
            .depths
            .iter()
            .all(|d| d.conversion == ConversionReport::Png16)
    );
    // Known only after the layout report of `applied`.
    assert_eq!(capabilities.pixel_exact.state, SupportState::Unknown);

    host.line("nonsense").await;
    assert_eq!(error_kind(&host.next().await), (None, ErrorKind::Malformed));
    host.show(1, FRAME_A, 0).await;
    assert_eq!(
        error_kind(&host.next().await),
        (Some(1), ErrorKind::NoSurface)
    );

    host.open(&display, 1, VIEWPORT).await;
    host.show(2, FRAME_A, 0).await;
    let (first, size) = page.expect_show().await;
    assert_eq!(size, VIEWPORT);
    let (status, png) = page.fetch(first).await;
    assert_eq!(status, 200);
    assert_fixture_png(&png, FRAME_A, size);

    // Replacement before the acknowledgement.
    host.show(3, FRAME_B, 0).await;
    host.expect(Event::Cancelled {
        frame: 2,
        cause: Cause::Replaced,
    })
    .await;
    let (second, _) = page.expect_show().await;
    assert!(second > first);
    page.applied(first, size).await;
    assert_eq!(host.sync().await, [], "a late acknowledgement");
    let (_, png) = page.fetch(second).await;
    assert_fixture_png(&png, FRAME_B, size);
    page.applied(second, size).await;
    let Event::Submitted {
        frame: 3,
        generation: 1,
        delay_ms: 0,
        status,
    } = host.next().await
    else {
        panic!("expected `submitted` of frame 3");
    };
    assert!(
        status.contains(&format!("Frame {second} applied by the client")),
        "{status}"
    );
    host.expect(Event::Ready {
        frame: 3,
        delay_ms: 0,
    })
    .await;

    host.send(&Request::Quit {}).await;
    host.expect(Event::Surface {
        generation: 1,
        state: SurfaceState::Closed,
        display,
        size: None,
        description: None,
        cause: Some(Cause::Stopped),
    })
    .await;
    host.expect(Event::Stopped { error: None }).await;
    assert!(host.exit().await.success());
}

#[tokio::test(flavor = "current_thread")]
async fn reports_capture_readiness_and_close() {
    let mut host = Host::start().await;
    let (mut page, display) = host.ready().await;
    host.open(&display, 1, VIEWPORT).await;
    host.show_with(3, FRAME_B, 0, true).await;
    let (id, size) = page.expect_show().await;
    page.applied(id, size).await;
    assert!(matches!(
        host.next().await,
        Event::Submitted { frame: 3, .. }
    ));
    host.expect(Event::Ready {
        frame: 3,
        delay_ms: 0,
    })
    .await;

    host.send(&capture(3)).await;
    let Event::Captured {
        frame: 3,
        result: CaptureOutcome::Unavailable,
        reason: Some(reason),
        artifact: None,
        cause: None,
    } = host.next().await
    else {
        panic!("expected `captured`");
    };
    assert!(reason.contains("no capture adapter"), "{reason}");
    host.send(&capture(2)).await;
    assert_eq!(
        error_kind(&host.next().await),
        (Some(2), ErrorKind::StaleFrame)
    );
    host.show(3, FRAME_A, 0).await;
    assert_eq!(
        error_kind(&host.next().await),
        (Some(3), ErrorKind::FrameOrder)
    );

    // Readiness after the delay.
    host.show(4, FRAME_A, 200).await;
    let (third, size) = page.expect_show().await;
    let acked = Instant::now();
    page.applied(third, size).await;
    assert!(matches!(
        host.next().await,
        Event::Submitted {
            frame: 4,
            delay_ms: 200,
            ..
        }
    ));
    host.expect(Event::Ready {
        frame: 4,
        delay_ms: 200,
    })
    .await;
    assert!(acked.elapsed() >= Duration::from_millis(200));

    host.send(&Request::Close {}).await;
    host.expect(Event::Surface {
        generation: 1,
        state: SurfaceState::Closed,
        display: display.clone(),
        size: None,
        description: None,
        cause: Some(Cause::Closed),
    })
    .await;
    assert_eq!(page.next_json().await, json!({"type": "close"}));

    host.send(&Request::Quit {}).await;
    host.expect(Event::Stopped { error: None }).await;
    assert!(host.exit().await.success());
    page.expect_end().await;
}

/// The frame gets the same `error` event whether the driver result or the
/// service event comes first. Each round can take either order.
#[tokio::test(flavor = "current_thread")]
async fn disconnect_during_presentation_fails_the_frame() {
    let mut host = Host::start().await;
    for round in 0..3u64 {
        let (mut page, display) = host.ready().await;
        host.open(&display, round + 1, VIEWPORT).await;
        host.show(round + 1, FRAME_A, 0).await;
        page.expect_show().await;
        page.ws.close(None).await.unwrap();
        let mut events = Vec::new();
        loop {
            let event = host.next().await;
            let closed = matches!(
                event,
                Event::Surface {
                    state: SurfaceState::Closed,
                    ..
                }
            );
            events.push(event);
            if closed {
                break;
            }
        }
        let outcomes: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, Event::Error { .. } | Event::Cancelled { .. }))
            .collect();
        assert_eq!(
            outcomes,
            [&Event::Error {
                frame: Some(round + 1),
                kind: ErrorKind::Disconnected,
                message: "display disconnected".into(),
            }],
            "{events:#?}"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::Display {
                    state: DisplayState::Disconnected,
                    ..
                }
            )),
            "{events:#?}"
        );
        assert!(matches!(
            events.last(),
            Some(Event::Surface {
                cause: Some(Cause::Disconnected),
                ..
            })
        ));
    }
    // The end of stdin is `quit`.
    host.stdin = None;
    host.expect(Event::Stopped { error: None }).await;
    assert!(host.exit().await.success());
}

#[tokio::test(flavor = "current_thread")]
async fn viewport_change_makes_the_surface_stale() {
    let mut host = Host::start().await;
    let (mut page, display) = host.ready().await;
    host.open(&display, 1, VIEWPORT).await;
    host.show(1, FRAME_A, 0).await;
    page.expect_show().await;

    let resized = (800, 450);
    page.hello(resized).await;
    // The driver result and the service event can come in either order.
    let mut events = Vec::new();
    for _ in 0..4 {
        events.push(host.next().await);
    }
    let errors: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, Event::Error { .. } | Event::Cancelled { .. }))
        .collect();
    assert_eq!(errors.len(), 1, "{events:#?}");
    assert_eq!(error_kind(errors[0]), (Some(1), ErrorKind::Platform));
    let Event::Error { message, .. } = errors[0] else {
        unreachable!()
    };
    assert!(message.contains("viewport changed"), "{message}");
    let rest: Vec<_> = events
        .iter()
        .filter(|e| !matches!(e, Event::Error { .. }))
        .cloned()
        .collect();
    assert_eq!(
        rest,
        [
            display_event(DisplayState::Changed, Some(&display)),
            Event::Surface {
                generation: 1,
                state: SurfaceState::Stale,
                display: display.clone(),
                size: None,
                description: None,
                cause: Some(Cause::Changed),
            },
            display_event(DisplayState::Ready, Some(&display)),
        ]
    );

    host.show(2, FRAME_B, 0).await;
    assert_eq!(
        error_kind(&host.next().await),
        (Some(2), ErrorKind::StaleSurface)
    );
    host.send(&Request::Open {
        display: display.clone(),
        depth: 10,
    })
    .await;
    assert!(matches!(
        host.next().await,
        Event::Surface {
            generation: 1,
            state: SurfaceState::Closed,
            cause: Some(Cause::Opened),
            ..
        }
    ));
    let Event::Surface {
        generation: 2,
        size: Some(size),
        ..
    } = host.next().await
    else {
        panic!("expected generation 2");
    };
    assert_eq!(size, [resized.0, resized.1]);
    host.show(3, FRAME_B, 0).await;
    let (id, size) = page.expect_show().await;
    assert_eq!(size, resized);
    let (_, png) = page.fetch(id).await;
    assert_fixture_png(&png, FRAME_B, size);
    page.applied(id, size).await;
    assert!(matches!(
        host.next().await,
        Event::Submitted {
            frame: 3,
            generation: 2,
            ..
        }
    ));
    host.expect(Event::Ready {
        frame: 3,
        delay_ms: 0,
    })
    .await;

    host.send(&Request::Quit {}).await;
    assert!(matches!(
        host.next().await,
        Event::Surface {
            generation: 2,
            state: SurfaceState::Closed,
            cause: Some(Cause::Stopped),
            ..
        }
    ));
    host.expect(Event::Stopped { error: None }).await;
    assert!(host.exit().await.success());
}
