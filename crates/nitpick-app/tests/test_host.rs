//! `nitpick --test-host` as a separate process: the real Qt event loop, the
//! `QRhi` driver and the protocol on stdin and stdout.
//!
//! The ordinary test uses the Qt `offscreen` platform, which has no screen
//! with a native identity, so it checks the protocol and the event loop
//! without a window. The ignored test opens full-screen windows on the
//! first screen; run it on a desktop with
//! `cargo nextest run -p nitpick-app --test test_host --run-ignored only`.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nitpick_test_host::controller::NO_ARTIFACTS;
use nitpick_test_host::protocol::{CaptureOutcome, Cause, ErrorKind, SurfaceState, VERSION};
use nitpick_test_host::{Event, Request};
// The build script links the generated C++ of the package into every
// target; it calls the Rust side of the bridges in the library.
use nitpick_app as _;

/// Longer than the acknowledgement timeout of the pattern window.
const WAIT: Duration = Duration::from_secs(20);

struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    events: mpsc::Receiver<String>,
}

impl Host {
    fn start(platform: Option<&str>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nitpick"));
        command
            .arg("--test-host")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if let Some(platform) = platform {
            command.env("QT_QPA_PLATFORM", platform);
        }
        let mut child = command.spawn().expect("start nitpick");
        let stdout = child.stdout.take().expect("piped");
        let (tx, events) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let mut host = Self {
            stdin: child.stdin.take(),
            child,
            events,
        };
        host.expect(&Event::Hello {
            protocol: VERSION,
            driver: "qrhi".into(),
        });
        host
    }

    fn line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn send(&mut self, request: &Request) {
        self.line(&serde_json::to_string(request).unwrap());
    }

    fn next(&mut self) -> Event {
        let line = self.events.recv_timeout(WAIT).expect("an event in time");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    fn expect(&mut self, want: &Event) {
        assert_eq!(&self.next(), want);
    }

    fn error(&mut self) -> (Option<u64>, ErrorKind) {
        match self.next() {
            Event::Error { frame, kind, .. } => (frame, kind),
            other => panic!("expected `error`, got {other:?}"),
        }
    }

    fn displays(&mut self) -> Vec<String> {
        match self.next() {
            Event::Displays { displays } => displays.into_iter().map(|d| d.id).collect(),
            other => panic!("expected `displays`, got {other:?}"),
        }
    }

    fn exit(mut self) -> ExitStatus {
        self.stdin = None;
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "no exit");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn capture(frame: u64) -> Request {
    Request::Capture {
        frame,
        heartbeat_ms: None,
        deadline_ms: None,
    }
}

/// The `captured` event of `frame`: its result and reason.
fn captured(host: &mut Host, frame: u64) -> (CaptureOutcome, Option<String>) {
    match host.next() {
        Event::Captured {
            frame: f,
            result,
            reason,
            ..
        } if f == frame => (result, reason),
        other => panic!("expected `captured` of frame {frame}, got {other:?}"),
    }
}

/// Captures the marked, acknowledged `frame` of the pattern window. On
/// Windows, frame acceptance needs a heartbeat, because the window gives
/// no new frames while it shows a static image. The host has no artifact
/// directory, so an accepted frame is `unavailable` with that reason.
fn check_capture(host: &mut Host, frame: u64) {
    if !cfg!(windows) {
        host.send(&capture(frame));
        assert_eq!(captured(host, frame).0, CaptureOutcome::Unavailable);
        return;
    }
    host.send(&Request::Capture {
        frame,
        heartbeat_ms: None,
        deadline_ms: Some(2000),
    });
    let (result, reason) = captured(host, frame);
    eprintln!("without heartbeat: {result:?}, {reason:?}");
    assert_eq!(result, CaptureOutcome::Fail, "{reason:?}");
    host.send(&Request::Capture {
        frame,
        heartbeat_ms: Some(100),
        deadline_ms: Some(10_000),
    });
    assert_eq!(
        captured(host, frame),
        (CaptureOutcome::Unavailable, Some(NO_ARTIFACTS.into()))
    );
}

/// `show` with the frame marker.
fn marked(mut request: Request) -> Request {
    if let Request::Show { marker, .. } = &mut request {
        *marker = true;
    }
    request
}

fn show(frame: u64, fixture: &str, delay_ms: u32) -> Request {
    Request::Show {
        frame,
        fixture: fixture.into(),
        delay_ms: Some(delay_ms),
        metadata: None,
        marker: false,
    }
}

#[test]
fn protocol_runs_on_the_qt_event_loop() {
    let mut host = Host::start(Some("offscreen"));
    host.line("{");
    assert_eq!(host.error(), (None, ErrorKind::Malformed));
    host.send(&Request::Refresh {});
    assert_eq!(host.displays(), Vec::<String>::new());
    host.send(&Request::Query {
        display: "qrhi:none".into(),
    });
    assert_eq!(host.error(), (None, ErrorKind::UnknownDisplay));
    host.send(&show(1, "state-frame-a-pq-full-10", 0));
    assert_eq!(host.error(), (Some(1), ErrorKind::NoSurface));
    host.send(&show(1, "state-frame-a-pq-full-10", 0));
    assert_eq!(host.error(), (Some(1), ErrorKind::FrameOrder));
    host.send(&capture(1));
    assert_eq!(host.error(), (Some(1), ErrorKind::StaleFrame));
    host.send(&Request::Quit {});
    host.expect(&Event::Stopped { error: None });
    assert!(host.exit().success());
}

/// The status of `submitted`; panics on another event.
fn submitted(host: &mut Host, frame: u64, generation: u64, delay_ms: u32) -> String {
    match host.next() {
        Event::Submitted {
            frame: f,
            generation: g,
            delay_ms: d,
            status,
        } if (f, g, d) == (frame, generation, delay_ms) => status,
        other => panic!("expected `submitted` of frame {frame}, got {other:?}"),
    }
}

fn open(host: &mut Host, display: &str, depth: u32, generation: u64) {
    host.send(&Request::Open {
        display: display.into(),
        depth,
    });
    match host.next() {
        Event::Surface {
            generation: g,
            state: SurfaceState::Open,
            ..
        } if g == generation => {}
        other => panic!("expected an open surface, got {other:?}"),
    }
}

#[test]
#[ignore = "opens full-screen windows on the first screen"]
fn presents_fixtures_on_a_screen() {
    let mut host = Host::start(None);
    host.send(&Request::Refresh {});
    let display = host.displays().into_iter().next().expect("a screen");
    host.send(&Request::Query {
        display: display.clone(),
    });
    assert!(matches!(host.next(), Event::Query { .. }));

    open(&mut host, &display, 10, 1);
    host.send(&show(1, "state-frame-a-pq-full-10", 0));
    host.send(&marked(show(2, "state-frame-b-pq-full-10", 300)));
    host.expect(&Event::Cancelled {
        frame: 1,
        cause: Cause::Replaced,
    });
    let status = submitted(&mut host, 2, 1, 300);
    assert!(status.contains("HDR10 swap chain"), "{status}");
    let start = Instant::now();
    host.expect(&Event::Ready {
        frame: 2,
        delay_ms: 300,
    });
    assert!(start.elapsed() >= Duration::from_millis(250));
    check_capture(&mut host, 2);

    // A 12-bit fixture on a 10-bit surface.
    host.send(&show(3, "state-frame-a-pq-full-12", 0));
    assert_eq!(host.error(), (Some(3), ErrorKind::Unsupported));

    host.send(&Request::Open {
        display: display.clone(),
        depth: 12,
    });
    host.expect(&Event::Surface {
        generation: 1,
        state: SurfaceState::Closed,
        display: display.clone(),
        size: None,
        description: None,
        cause: Some(Cause::Opened),
    });
    match host.next() {
        Event::Surface {
            generation: 2,
            state: SurfaceState::Open,
            ..
        } => {}
        // HDR off or no FP16 path on this screen.
        Event::Error { .. } => {
            host.send(&Request::Quit {});
            host.expect(&Event::Stopped { error: None });
            assert!(host.exit().success());
            return;
        }
        other => panic!("unexpected {other:?}"),
    }
    host.send(&marked(show(4, "state-frame-b-pq-full-12", 0)));
    let status = submitted(&mut host, 4, 2, 0);
    assert!(status.contains("FP16 scRGB swap chain"), "{status}");
    host.expect(&Event::Ready {
        frame: 4,
        delay_ms: 0,
    });
    check_capture(&mut host, 4);

    host.send(&Request::Close {});
    host.expect(&Event::Surface {
        generation: 2,
        state: SurfaceState::Closed,
        display,
        size: None,
        description: None,
        cause: Some(Cause::Closed),
    });
    host.send(&Request::Quit {});
    host.expect(&Event::Stopped { error: None });
    assert!(host.exit().success());
}
