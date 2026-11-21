//! Capture scenarios of the shared host contract: sessions, frame
//! acceptance, the heartbeat, cancellation and artifacts.

use super::*;
use crate::artifact::{Acceptance, Check, FormatRecord};
use crate::gate;
use crate::marker::{self, Marker};
use crate::protocol::ModeReport;

pub(super) const SCENARIOS: &[Scenario] = scenarios![
    capture_without_adapter_is_unavailable,
    capture_needs_a_marker_and_valid_limits,
    capture_waits_for_three_current_frames,
    differing_frames_restart_the_run,
    deadline_fails_with_the_last_frame,
    surface_in_a_larger_capture_passes_and_resize_fails,
    invalid_capture_fails_and_is_saved,
    session_failure_is_unavailable,
    adapter_without_clock_fails,
    one_capture_at_a_time,
    show_cancels_the_capture,
    open_cancels_the_capture,
    close_cancels_the_capture,
    display_change_cancels_the_capture,
    disconnect_cancels_the_capture,
    external_close_cancels_the_capture,
    quit_cancels_the_capture,
    heartbeat_presents_the_frame_again,
    failed_heartbeat_fails_the_capture,
    show_during_a_heartbeat_drops_it,
    failed_artifact_write_is_an_error,
    full_artifact_queue_fails_the_capture,
    quit_waits_for_artifact_writes,
];

pub(super) const WITHOUT_ARTIFACTS: &[Scenario] = scenarios![
    valid_capture_without_artifacts_is_unavailable,
    invalid_capture_without_artifacts_fails,
];

/// Adapter time of every `show` unless a scenario sets another.
const SHOWN_AT: u64 = 100;

fn cancelled(frame: u64, cause: Cause) -> Event {
    Event::Captured {
        frame,
        result: CaptureOutcome::Cancelled,
        reason: None,
        cause: Some(cause),
        artifact: None,
    }
}

/// A replayed frame with a row pitch below the row size.
fn invalid_frame(image: &Image) -> Capture {
    let mut c = replay::frame(image, replay::time(SHOWN_AT + 10));
    c.descriptor.planes[0].row_pitch = 8;
    c
}

/// Opens, shows frame 1 with the marker and a delay of 1000 ms, and
/// starts its capture. Returns the index of the deadline timer.
async fn capturing(h: &mut Harness) -> usize {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.start_capture(&capture(1)).await;
    assert_eq!(h.timers(), [1000, gate::DEADLINE_MS]);
    1
}

/// The session and its deadline are gone.
fn ended(h: &Harness, deadline: usize) {
    assert_eq!(h.sessions_live(), 0);
    assert!(!h.fire(deadline), "the deadline timer is dropped");
}

async fn capture_without_adapter_is_unavailable(mut h: Harness) {
    h.send(&capture(1));
    assert_eq!(
        error_kind(&h.next().await),
        (Some(1), ErrorKind::StaleFrame)
    );
    h.open().await;
    h.show_with(1, FRAME_A, Some(1000), true).await;
    // Not acknowledged yet.
    h.send(&capture(1));
    assert_eq!(
        error_kind(&h.next().await),
        (Some(1), ErrorKind::StaleFrame)
    );
    assert!(h.reply(0, Ok("ack 1".into())));
    h.expect(submitted(1, 1, 1000)).await;
    h.send(&capture(1));
    h.expect(captured(
        1,
        CaptureOutcome::Unavailable,
        Some(NO_CAPTURE),
        None,
    ))
    .await;
    assert_eq!(h.captures.borrow().targets, [(DISPLAY.to_owned(), 1, 1)]);
    h.send(&capture(2));
    assert_eq!(
        error_kind(&h.next().await),
        (Some(2), ErrorKind::StaleFrame)
    );
}

async fn capture_needs_a_marker_and_valid_limits(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.acknowledged(1, 1, 0).await;
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 0,
    })
    .await;
    h.send(&capture(1));
    assert_eq!(error_kind(&h.next().await), (Some(1), ErrorKind::NoMarker));
    for (heartbeat_ms, deadline_ms) in [(Some(0), None), (None, Some(0))] {
        h.send(&Request::Capture {
            frame: 1,
            heartbeat_ms,
            deadline_ms,
        });
        assert_eq!(error_kind(&h.next().await), (Some(1), ErrorKind::Malformed));
    }
    assert_eq!(h.captures.borrow().targets.len(), 0);
}

async fn capture_waits_for_three_current_frames(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 0).await;
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 0,
    })
    .await;
    h.set_now(200);
    h.marked(2, FRAME_B, 1, 1000).await;
    h.start_capture(&capture(2)).await;
    assert_eq!(h.timers(), [1000, gate::DEADLINE_MS]);
    for (n, ms) in [
        // Before the submission.
        (1, 150),
        // The earlier marker.
        (0, 210),
        (1, 220),
        // A repeated frame time does not count.
        (1, 220),
        (1, 230),
        (1, 240),
    ] {
        assert!(h.replay(n, ms));
    }
    h.expect(captured(2, CaptureOutcome::Pass, None, Some("artifact-1")))
        .await;
    ended(&h, 1);
    {
        let sink = h.sink.borrow();
        let artifact = &sink.submitted[0];
        assert_eq!(
            artifact.check,
            Check {
                result: CaptureOutcome::Pass,
                reason: None
            }
        );
        assert_eq!(
            artifact.capture,
            replay::frame(&h.image(1), replay::time(240))
        );
        assert_eq!(
            artifact.acceptance,
            Acceptance {
                submitted: replay::time(200),
                marker: 2,
                marker_origin: [0, 36],
                frames: [220, 230, 240].map(replay::time).to_vec(),
                received: 6,
                deadline_ms: gate::DEADLINE_MS,
                heartbeat_ms: None,
            }
        );
        let fixtures = Fixtures::load().expect("output fixtures");
        let run = &artifact.run;
        assert_eq!(
            (run.driver.as_str(), run.display.as_str(), run.generation),
            ("mock", DISPLAY, 1)
        );
        assert_eq!(run.surface_size, [SIZE.0, SIZE.1]);
        assert_eq!(run.surface, "mock surface");
        assert_eq!(run.mode, ModeReport::from(&query().mode));
        assert_eq!((run.frame, run.fixture.as_str()), (2, FRAME_B));
        assert_eq!(
            run.format,
            FormatRecord {
                encoding: "pq-full".into(),
                depth: 10
            }
        );
        assert_eq!(run.metadata, fixtures.get(FRAME_B).unwrap().metadata);
        assert_eq!(run.delay_ms, 1000);
        assert_eq!(run.environment, environment(DISPLAY));
    }
    // The delay continues; the frame stays capturable.
    assert!(h.fire(0));
    h.expect(Event::Ready {
        frame: 2,
        delay_ms: 1000,
    })
    .await;
    h.accept(2, 1).await;
    h.expect(captured(2, CaptureOutcome::Pass, None, Some("artifact-2")))
        .await;
    h.finish_saved(&["artifact-1", "artifact-2"]).await;
}

async fn differing_frames_restart_the_run(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 0).await;
    h.start_capture(&capture(1)).await;
    let image = h.image(0);
    let mut changed = image.clone();
    changed.set(5, 5, Rgb::grey(7));
    // Outside the fixture: not compared.
    let mut outside = changed.clone();
    outside.set(SIZE.0 - 1, 0, Rgb::grey(9));
    for (image, ms) in [
        (&image, 110),
        (&image, 120),
        (&changed, 130),
        (&outside, 140),
        (&changed, 150),
    ] {
        assert!(h.deliver(Ok(replay::frame(image, replay::time(ms)))));
    }
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 0,
    })
    .await;
    h.expect(captured(1, CaptureOutcome::Pass, None, Some("artifact-1")))
        .await;
    let acceptance = h.sink.borrow().submitted[0].acceptance.clone();
    assert_eq!(acceptance.frames, [130, 140, 150].map(replay::time));
    assert_eq!(acceptance.received, 5);
    h.finish_saved(&["artifact-1"]).await;
}

async fn deadline_fails_with_the_last_frame(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 0).await;
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 0,
    })
    .await;
    h.marked(2, FRAME_B, 1, 0).await;
    h.expect(Event::Ready {
        frame: 2,
        delay_ms: 0,
    })
    .await;
    h.start_capture(&Request::Capture {
        frame: 2,
        heartbeat_ms: None,
        deadline_ms: Some(5000),
    })
    .await;
    assert_eq!(h.timers(), [5000]);
    // Frame 1 stays on the screen.
    // The host takes frames before the deadline.
    assert!(h.replay(0, 110));
    assert!(h.fire(0));
    let reason = "no accepted frame within 5000 ms; 1 frames received; \
                  last: the frame marker is 1; expected 2";
    h.expect(captured(
        2,
        CaptureOutcome::Fail,
        Some(reason),
        Some("artifact-1"),
    ))
    .await;
    assert_eq!(h.sessions_live(), 0);
    {
        let sink = h.sink.borrow();
        let artifact = &sink.submitted[0];
        assert_eq!(artifact.check.result, CaptureOutcome::Fail);
        assert_eq!(
            artifact.capture,
            replay::frame(&h.image(0), replay::time(110))
        );
        assert_eq!(artifact.acceptance.frames.len(), 0);
        assert_eq!(artifact.acceptance.deadline_ms, 5000);
    }
    // Without a frame there is nothing to save.
    h.start_capture(&Request::Capture {
        frame: 2,
        heartbeat_ms: None,
        deadline_ms: Some(1),
    })
    .await;
    assert!(h.fire(1));
    h.expect(captured(
        2,
        CaptureOutcome::Fail,
        Some("no accepted frame within 1 ms; 0 frames received"),
        None,
    ))
    .await;
    h.finish_saved(&["artifact-1"]).await;
}

async fn surface_in_a_larger_capture_passes_and_resize_fails(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.start_capture(&capture(1)).await;
    // Window chrome around the content.
    let image = h.image(0);
    for ms in [110, 120, 130] {
        let frame = replay::framed(&image, replay::time(ms), (3, 5), (70, 50));
        assert!(h.deliver(Ok(frame)));
    }
    h.expect(captured(1, CaptureOutcome::Pass, None, Some("artifact-1")))
        .await;

    h.start_capture(&capture(1)).await;
    let larger = Image::new(SIZE.0 + 16, SIZE.1 + 10, Rgb::grey(0));
    assert!(h.deliver(Ok(replay::frame(&larger, replay::time(140)))));
    h.expect(captured(
        1,
        CaptureOutcome::Fail,
        Some("the capture content is 80×50 px; the surface is 64×40 px"),
        Some("artifact-2"),
    ))
    .await;
    assert_eq!(h.sessions_live(), 0);
    h.finish_saved(&["artifact-1", "artifact-2"]).await;
}

async fn invalid_capture_fails_and_is_saved(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.start_capture(&capture(1)).await;
    let bad = invalid_frame(&h.image(0));
    let reason = bad.validate().unwrap_err().to_string();
    assert!(h.deliver(Ok(bad.clone())));
    h.expect(captured(
        1,
        CaptureOutcome::Fail,
        Some(&reason),
        Some("artifact-1"),
    ))
    .await;
    {
        let sink = h.sink.borrow();
        assert_eq!(sink.submitted[0].check.reason, Some(reason));
        assert_eq!(sink.submitted[0].capture, bad);
    }
    h.finish_saved(&["artifact-1"]).await;
}

async fn session_failure_is_unavailable(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.start_capture(&capture(1)).await;
    assert!(h.replay(0, 110));
    assert!(h.deliver(Err("the window is gone".into())));
    h.expect(captured(
        1,
        CaptureOutcome::Unavailable,
        Some("the window is gone"),
        None,
    ))
    .await;
    assert_eq!(h.sessions_live(), 0);
    h.start_capture(&capture(1)).await;
    h.end_session();
    h.expect(captured(
        1,
        CaptureOutcome::Unavailable,
        Some("the capture session ended"),
        None,
    ))
    .await;
    assert_eq!(h.sink.borrow().submitted.len(), 0);
}

async fn adapter_without_clock_fails(mut h: Harness) {
    h.captures.borrow_mut().available = true;
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.send(&capture(1));
    h.expect(captured(
        1,
        CaptureOutcome::Fail,
        Some("the capture adapter gave no time before the presentation"),
        None,
    ))
    .await;
    assert_eq!(h.captures.borrow().sessions.len(), 1);
    assert_eq!(h.sessions_live(), 0);
}

async fn one_capture_at_a_time(mut h: Harness) {
    let deadline = capturing(&mut h).await;
    h.send(&capture(1));
    assert_eq!(
        error_kind(&h.next().await),
        (Some(1), ErrorKind::CapturePending)
    );
    assert_eq!(h.captures.borrow().sessions.len(), 1);
    for ms in [110, 120, 130] {
        assert!(h.replay(0, ms));
    }
    h.expect(captured(1, CaptureOutcome::Pass, None, Some("artifact-1")))
        .await;
    ended(&h, deadline);
    h.finish_saved(&["artifact-1"]).await;
}

async fn show_cancels_the_capture(mut h: Harness) {
    let deadline = capturing(&mut h).await;
    h.show(2, FRAME_B, Some(0)).await;
    h.expect(cancelled(1, Cause::Replaced)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Replaced,
    })
    .await;
    ended(&h, deadline);
    assert!(!h.replay(0, 110), "the session is dropped");
}

async fn open_cancels_the_capture(mut h: Harness) {
    let deadline = capturing(&mut h).await;
    h.send(&Request::Open {
        display: DISPLAY.into(),
        depth: 10,
    });
    h.expect(cancelled(1, Cause::Opened)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Opened,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Opened))
        .await;
    h.expect(open_surface(2, SIZE)).await;
    ended(&h, deadline);
}

async fn close_cancels_the_capture(mut h: Harness) {
    let deadline = capturing(&mut h).await;
    h.send(&Request::Close {});
    h.expect(cancelled(1, Cause::Closed)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Closed,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Closed))
        .await;
    ended(&h, deadline);
}

async fn display_change_cancels_the_capture(mut h: Harness) {
    let deadline = capturing(&mut h).await;
    h.event(changed());
    h.expect(changed_display()).await;
    h.expect(cancelled(1, Cause::Changed)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Changed,
    })
    .await;
    h.expect(surface(1, SurfaceState::Stale, Cause::Changed))
        .await;
    ended(&h, deadline);
}

async fn disconnect_cancels_the_capture(mut h: Harness) {
    let deadline = capturing(&mut h).await;
    h.event(disconnected());
    h.expect(disconnected_display()).await;
    h.expect(cancelled(1, Cause::Disconnected)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Disconnected,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Disconnected))
        .await;
    ended(&h, deadline);
}

async fn external_close_cancels_the_capture(mut h: Harness) {
    let deadline = capturing(&mut h).await;
    close_outside(&h);
    h.expect(cancelled(1, Cause::Disconnected)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Disconnected,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Disconnected))
        .await;
    ended(&h, deadline);
}

async fn quit_cancels_the_capture(mut h: Harness) {
    let deadline = capturing(&mut h).await;
    h.send(&Request::Quit {});
    h.expect(cancelled(1, Cause::Stopped)).await;
    h.expect(Event::Cancelled {
        frame: 1,
        cause: Cause::Stopped,
    })
    .await;
    h.expect(surface(1, SurfaceState::Closed, Cause::Stopped))
        .await;
    h.expect(Event::Stopped { error: None }).await;
    ended(&h, deadline);
}

/// Shows frame 1 with the marker and starts its capture with a heartbeat
/// of 50 ms. Then the timers are the deadline and the first heartbeat.
async fn heartbeat_capture(h: &mut Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 0).await;
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 0,
    })
    .await;
    h.start_capture(&Request::Capture {
        frame: 1,
        heartbeat_ms: Some(50),
        deadline_ms: None,
    })
    .await;
    assert_eq!(h.timers(), [gate::DEADLINE_MS, 50]);
}

async fn heartbeat_presents_the_frame_again(mut h: Harness) {
    heartbeat_capture(&mut h).await;
    assert!(h.fire(1));
    h.until("heartbeat", |m| m.presented.len() == 2).await;
    let (shown, beat) = (h.image(0), h.image(1));
    let format = h.mock.borrow().presented[1].format;
    let fx = Fixtures::load().expect("output fixtures");
    let marker = Marker::place(SIZE, fx.get(FRAME_A).unwrap().size, 1).unwrap();
    let cell = marker.heartbeat();
    for y in 0..SIZE.1 {
        for x in 0..SIZE.0 {
            let want = if cell.contains(x, y) {
                marker::level(format, true)
            } else {
                shown.get(x, y)
            };
            assert_eq!(beat.get(x, y), want, "({x}, {y})");
        }
    }
    assert!(h.reply(1, Ok("beat".into())));
    h.wait("next heartbeat", |h| h.timers().len() == 3).await;
    assert_eq!(h.timers()[2], 50);
    // The heartbeat cell is not compared.
    for (n, ms) in [(0, 110), (1, 120), (0, 130)] {
        assert!(h.replay(n, ms));
    }
    h.expect(captured(1, CaptureOutcome::Pass, None, Some("artifact-1")))
        .await;
    assert_eq!(
        h.sink.borrow().submitted[0].acceptance.heartbeat_ms,
        Some(50)
    );
    assert!(!h.fire(2), "the heartbeat timer is dropped");
    assert_eq!(h.mock.borrow().presented.len(), 2);
    h.finish_saved(&["artifact-1"]).await;
}

async fn failed_heartbeat_fails_the_capture(mut h: Harness) {
    heartbeat_capture(&mut h).await;
    assert!(h.replay(0, 110));
    assert!(h.fire(1));
    h.until("heartbeat", |m| m.presented.len() == 2).await;
    assert!(h.reply(1, Err(OutputError::Timeout)));
    h.expect(captured(
        1,
        CaptureOutcome::Fail,
        Some("the heartbeat presentation failed: driver acknowledgement timed out"),
        Some("artifact-1"),
    ))
    .await;
    assert_eq!(h.sessions_live(), 0);
    h.finish_saved(&["artifact-1"]).await;
}

async fn show_during_a_heartbeat_drops_it(mut h: Harness) {
    heartbeat_capture(&mut h).await;
    assert!(h.fire(1));
    h.until("heartbeat", |m| m.presented.len() == 2).await;
    assert_eq!(h.live(), 1);
    h.show(2, FRAME_B, Some(0)).await;
    h.expect(cancelled(1, Cause::Replaced)).await;
    assert!(!h.reply(1, Ok("beat".into())), "the heartbeat is dropped");
    assert_eq!(h.live(), 1);
    assert!(h.reply(2, Ok("ack 2".into())));
    h.expect(submitted(2, 1, 0)).await;
    h.expect(Event::Ready {
        frame: 2,
        delay_ms: 0,
    })
    .await;
}

async fn failed_artifact_write_is_an_error(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.accept(1, 0).await;
    h.expect(captured(1, CaptureOutcome::Pass, None, Some("artifact-1")))
        .await;
    h.finish_write(Err("disk full".into()));
    let event = h.next().await;
    assert_eq!(error_kind(&event), (Some(1), ErrorKind::Artifact));
    let Event::Error { message, .. } = event else {
        unreachable!()
    };
    assert!(message.contains("artifact-1") && message.contains("disk full"));
}

async fn full_artifact_queue_fails_the_capture(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    for n in 1..=QUEUE {
        h.accept(1, 0).await;
        let name = format!("artifact-{n}");
        h.expect(captured(1, CaptureOutcome::Pass, None, Some(&name)))
            .await;
    }
    h.accept(1, 0).await;
    h.expect(captured(
        1,
        CaptureOutcome::Fail,
        Some("mock queue full"),
        None,
    ))
    .await;
    // An invalid capture keeps its own reason.
    h.start_capture(&capture(1)).await;
    let bad = invalid_frame(&h.image(0));
    let reason = bad.validate().unwrap_err().to_string();
    assert!(h.deliver(Ok(bad)));
    h.expect(captured(
        1,
        CaptureOutcome::Fail,
        Some(&format!("{reason}; not saved: mock queue full")),
        None,
    ))
    .await;
    h.finish_write(Ok(()));
    h.expect(Event::Saved {
        frame: 1,
        artifact: "artifact-1".into(),
    })
    .await;
    h.accept(1, 0).await;
    h.expect(captured(1, CaptureOutcome::Pass, None, Some("artifact-3")))
        .await;
    // The host waits for queued writes before it stops.
    h.finish_saved(&["artifact-2", "artifact-3"]).await;
}

async fn quit_waits_for_artifact_writes(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 0).await;
    h.expect(Event::Ready {
        frame: 1,
        delay_ms: 0,
    })
    .await;
    h.accept(1, 0).await;
    h.expect(captured(1, CaptureOutcome::Pass, None, Some("artifact-1")))
        .await;
    h.send(&Request::Quit {});
    h.expect(surface(1, SurfaceState::Closed, Cause::Stopped))
        .await;
    for _ in 0..10 {
        YieldNow(false).await;
    }
    assert!(h.out.try_recv().is_err(), "`stopped` before the write");
    h.finish_write(Ok(()));
    h.expect(Event::Saved {
        frame: 1,
        artifact: "artifact-1".into(),
    })
    .await;
    h.expect(Event::Stopped { error: None }).await;
}

async fn valid_capture_without_artifacts_is_unavailable(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.accept(1, 0).await;
    h.expect(captured(
        1,
        CaptureOutcome::Unavailable,
        Some(NO_ARTIFACTS),
        None,
    ))
    .await;
    assert_eq!(h.sink.borrow().submitted.len(), 0);
}

async fn invalid_capture_without_artifacts_fails(mut h: Harness) {
    h.adapter(SHOWN_AT);
    h.open().await;
    h.marked(1, FRAME_A, 1, 1000).await;
    h.start_capture(&capture(1)).await;
    let bad = invalid_frame(&h.image(0));
    let reason = bad.validate().unwrap_err().to_string();
    assert!(h.deliver(Ok(bad)));
    h.expect(captured(1, CaptureOutcome::Fail, Some(&reason), None))
        .await;
}
