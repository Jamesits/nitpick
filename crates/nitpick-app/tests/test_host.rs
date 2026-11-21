//! `nitpick --test-host` as a separate process: the real Qt event loop, the
//! `QRhi` driver and the protocol on stdin and stdout.
//!
//! The ordinary test uses the Qt `offscreen` platform, which has no screen
//! with a native identity, so it checks the protocol and the event loop
//! without a window. The ignored test opens full-screen windows on the
//! first screen; run it on a desktop with
//! `cargo nextest run -p nitpick-app --test test_host --run-ignored only`.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nitpick_test_host::artifact::read;
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

/// `nitpick --test-host`, with an artifact directory if `artifacts`.
fn host_command(platform: Option<&str>, artifacts: Option<&Path>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nitpick"));
    command.arg("--test-host");
    if let Some(dir) = artifacts {
        command.arg("--artifacts").arg(dir);
    }
    if let Some(platform) = platform {
        command.env("QT_QPA_PLATFORM", platform);
    }
    command
}

impl Host {
    fn start(platform: Option<&str>, artifacts: Option<&Path>) -> Self {
        let mut child = host_command(platform, artifacts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start nitpick");
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

/// The `captured` event of `frame`: its result, reason and artifact. With
/// an artifact, waits for its `saved` event.
fn captured(host: &mut Host, frame: u64) -> (CaptureOutcome, Option<String>, Option<String>) {
    let (result, reason, artifact) = match host.next() {
        Event::Captured {
            frame: f,
            result,
            reason,
            artifact,
            ..
        } if f == frame => (result, reason, artifact),
        other => panic!("expected `captured` of frame {frame}, got {other:?}"),
    };
    if let Some(name) = &artifact {
        host.expect(&Event::Saved {
            frame,
            artifact: name.clone(),
        });
    }
    (result, reason, artifact)
}

/// The shown frame that a capture artifact must record.
struct Shown<'a> {
    display: &'a str,
    generation: u64,
    frame: u64,
    fixture: &'a str,
    depth: u32,
}

/// Captures the marked, acknowledged frame of the pattern window, with
/// the artifacts in `root`. On Windows, frame acceptance needs a
/// heartbeat, because the window gives no new frames while it shows a
/// static image.
fn check_capture(host: &mut Host, shown: &Shown<'_>, root: &Path) {
    let frame = shown.frame;
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
    let (result, reason, artifact) = captured(host, frame);
    eprintln!("without heartbeat: {result:?}, {reason:?}, {artifact:?}");
    assert_eq!(result, CaptureOutcome::Fail, "{reason:?}");
    // At the deadline the last frame goes to the artifact, if one arrived.
    if let Some(name) = &artifact {
        let failed = read(&root.join(name)).unwrap();
        assert_eq!(failed.check.result, CaptureOutcome::Fail);
        assert_eq!(failed.check.reason, reason);
    }
    host.send(&Request::Capture {
        frame,
        heartbeat_ms: Some(100),
        deadline_ms: Some(10_000),
    });
    let (result, reason, artifact) = captured(host, frame);
    assert_eq!((result, &reason), (CaptureOutcome::Pass, &None));
    let name = artifact.expect("an artifact of `pass`");
    check_artifact(&root.join(&name), shown);
}

/// The files and the records of a `pass` artifact.
fn check_artifact(dir: &Path, shown: &Shown<'_>) {
    for file in ["result.json", "plane-0.bin"] {
        assert!(dir.join(file).is_file(), "{file} in {}", dir.display());
    }
    let artifact = read(dir).unwrap();
    assert_eq!(artifact.check.result, CaptureOutcome::Pass);
    let run = &artifact.run;
    assert_eq!(
        (
            run.driver.as_str(),
            run.display.as_str(),
            run.generation,
            run.frame,
            run.fixture.as_str(),
            run.format.depth,
        ),
        (
            "qrhi",
            shown.display,
            shown.generation,
            shown.frame,
            shown.fixture,
            shown.depth
        )
    );
    assert!(run.surface.contains("swap chain"), "{}", run.surface);
    let env = &run.environment;
    eprintln!("environment: {env:?}; HDR on: {:?}", run.mode.hdr_on);
    assert!(
        env.os.as_deref().is_some_and(|os| !os.is_empty()),
        "{env:?}"
    );
    assert!(
        env.gpu
            .as_deref()
            .is_some_and(|gpu| gpu.contains(", driver ")),
        "{env:?}"
    );
    assert!(
        env.renderer
            .as_deref()
            .is_some_and(|r| r.starts_with("Qt 6.") && r.ends_with("QRhi D3D11")),
        "{env:?}"
    );
    assert_eq!((&env.edid_sha256, &env.profile_sha256), (&None, &None));
    let acceptance = &artifact.acceptance;
    assert_eq!(acceptance.frames.len(), 3);
    assert!(acceptance.received >= 3);
    assert_eq!(acceptance.heartbeat_ms, Some(100));
    let capture = &artifact.capture;
    let content = capture
        .descriptor
        .content
        .map_or((capture.descriptor.width, capture.descriptor.height), |r| {
            (r.width, r.height)
        });
    assert_eq!([content.0, content.1], run.surface_size);
    assert_eq!(capture.planes.len(), 1);
    assert!(capture.config.contains_key("api"), "{:?}", capture.config);
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

/// An artifact name of an earlier run could repeat.
#[test]
fn rejects_a_non_empty_artifact_directory() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("earlier"), b"").unwrap();
    let output = host_command(Some("offscreen"), Some(tmp.path()))
        .stdin(Stdio::null())
        .output()
        .expect("run nitpick");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty(), "no `hello` before the check");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("is not empty"), "{stderr}");
}

#[test]
fn protocol_runs_on_the_qt_event_loop() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("artifacts");
    let mut host = Host::start(Some("offscreen"), Some(&root));
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
    assert!(root.read_dir().unwrap().next().is_none());
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
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut host = Host::start(None, Some(root));
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
    let shown = Shown {
        display: &display,
        generation: 1,
        frame: 2,
        fixture: "state-frame-b-pq-full-10",
        depth: 10,
    };
    check_capture(&mut host, &shown, root);

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
    let shown = Shown {
        display: &display,
        generation: 2,
        frame: 4,
        fixture: "state-frame-b-pq-full-12",
        depth: 12,
    };
    check_capture(&mut host, &shown, root);

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
