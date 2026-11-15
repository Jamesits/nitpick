//! Replays synthetic frame streams through the gate.

use std::collections::BTreeMap;

use half::f16;
use nitpick_core::image::Image;
use nitpick_core::signal::Rgb;

use super::*;
use crate::capture::{
    ByteOrder, Channel, Colour, Field, PlaneLayout, SampleKind, TimeBase, Timestamps,
};
use crate::fixture::Fixtures;
use crate::replay::{self, time};

const FRAME_A: &str = "state-frame-a-pq-full-10";
const SURFACE: (usize, usize) = (64, 40);
const SUBMITTED: u64 = 100;

/// The presented image of `value`, as the host draws it.
fn shown(value: u8, heartbeat: bool) -> (Image, Expected) {
    let fx = Fixtures::load().unwrap();
    let f = fx.get(FRAME_A).unwrap();
    let mut image = f.render(SURFACE).unwrap();
    let marker = Marker::place(SURFACE, f.size, value).unwrap();
    marker.draw(&mut image, f.format, heartbeat);
    let expected = Expected {
        surface: SURFACE,
        fixture: f.size,
        marker,
        submitted: time(SUBMITTED),
    };
    (image, expected)
}

fn frame(image: &Image, ms: u64) -> Capture {
    replay::frame(image, time(ms))
}

fn gate(limit: f64) -> (Image, FrameGate) {
    let (image, expected) = shown(7, false);
    (image, FrameGate::new(expected, limit))
}

fn skipped(offer: &Offer, text: &str) {
    match offer {
        Offer::Skipped(reason) => assert!(reason.contains(text), "{reason}"),
        other => panic!("expected `Skipped` with {text:?}, got {other:?}"),
    }
}

fn rejected(offer: &Offer, text: &str) {
    match offer {
        Offer::Rejected(reason) => assert!(reason.contains(text), "{reason}"),
        other => panic!("expected `Rejected` with {text:?}, got {other:?}"),
    }
}

#[test]
fn accepts_three_current_matching_frames() {
    let (image, mut g) = gate(MATCH_LIMIT);
    assert_eq!(g.offer(frame(&image, 110)), Offer::Counted);
    assert_eq!(g.offer(frame(&image, 120)), Offer::Counted);
    assert_eq!(g.offer(frame(&image, 130)), Offer::Accepted);
    assert_eq!(g.accepted(), [110, 120, 130].map(time));
    assert_eq!(g.frames(), 3);
    assert_eq!(g.reason(), None);
    assert_eq!(g.into_last(), Some(frame(&image, 130)));
}

#[test]
fn frames_up_to_the_submission_are_stale() {
    let (image, mut g) = gate(MATCH_LIMIT);
    skipped(
        &g.offer(frame(&image, SUBMITTED)),
        "not after the submission",
    );
    skipped(&g.offer(frame(&image, 90)), "not after the submission");
    for ms in [110, 120] {
        assert_eq!(g.offer(frame(&image, ms)), Offer::Counted);
    }
    assert_eq!(g.offer(frame(&image, 130)), Offer::Accepted);
    assert_eq!(g.frames(), 5);
}

/// Another tick rate of the same time base compares exactly.
#[test]
fn frame_times_compare_across_tick_rates() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let at = |ticks| {
        let mut c = frame(&image, 0);
        c.descriptor.timestamps.frame = Some(Timestamp {
            base: TimeBase::Qpc,
            ticks,
            ticks_per_second: 10_000_000,
        });
        c
    };
    // 100 ms on the 10 MHz clock: the submission time.
    skipped(&g.offer(at(1_000_000)), "not after the submission");
    assert_eq!(g.offer(at(1_000_001)), Offer::Counted);
}

#[test]
fn a_repeated_frame_time_neither_counts_nor_resets() {
    let (image, mut g) = gate(MATCH_LIMIT);
    assert_eq!(g.offer(frame(&image, 110)), Offer::Counted);
    skipped(&g.offer(frame(&image, 110)), "not after the last frame");
    assert_eq!(g.offer(frame(&image, 120)), Offer::Counted);
    assert_eq!(g.offer(frame(&image, 130)), Offer::Accepted);
    assert_eq!(g.accepted(), [110, 120, 130].map(time));
}

#[test]
fn an_earlier_marker_restarts_the_run() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let (old, _) = shown(6, false);
    assert_eq!(g.offer(frame(&image, 110)), Offer::Counted);
    skipped(
        &g.offer(frame(&old, 120)),
        "the frame marker is 6; expected 7",
    );
    assert_eq!(g.reason(), Some("the frame marker is 6; expected 7"));
    assert_eq!(g.offer(frame(&image, 130)), Offer::Counted);
    assert_eq!(g.offer(frame(&image, 140)), Offer::Counted);
    assert_eq!(g.offer(frame(&image, 150)), Offer::Accepted);
    assert_eq!(g.accepted(), [130, 140, 150].map(time));
}

#[test]
fn an_unreadable_marker_is_stale() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let black = Image::new(SURFACE.0, SURFACE.1, Rgb::grey(0));
    skipped(&g.offer(frame(&black, 110)), "reference cells");
    assert_eq!(g.offer(frame(&image, 120)), Offer::Counted);
}

#[test]
fn differing_frames_restart_the_run() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let mut changed = image.clone();
    changed.set(31, 19, Rgb::grey(1));
    assert_eq!(g.offer(frame(&image, 110)), Offer::Counted);
    assert_eq!(g.offer(frame(&image, 120)), Offer::Counted);
    assert_eq!(g.offer(frame(&changed, 130)), Offer::Counted);
    assert_eq!(
        g.reason(),
        Some("the frame differs from the previous frame by 1 (limit 0)")
    );
    assert_eq!(g.offer(frame(&changed, 140)), Offer::Counted);
    assert_eq!(g.offer(frame(&changed, 150)), Offer::Accepted);
    assert_eq!(g.accepted(), [130, 140, 150].map(time));
}

#[test]
fn differences_within_the_limit_match() {
    let (image, mut g) = gate(2.0);
    let shifted = |d: u16| {
        let mut i = image.clone();
        i.set(0, 10, Rgb::grey(d));
        i
    };
    assert_eq!(g.offer(frame(&shifted(0), 110)), Offer::Counted);
    assert_eq!(g.offer(frame(&shifted(2), 120)), Offer::Counted);
    // 5 − 2 = 3 is above the limit.
    assert_eq!(g.offer(frame(&shifted(5), 130)), Offer::Counted);
    assert_eq!(g.accepted(), [time(130)]);
}

/// Only the fixture area counts: the black fill and the heartbeat cell
/// can change.
#[test]
fn changes_outside_the_fixture_match() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let (beat, _) = shown(7, true);
    let mut fill = image.clone();
    fill.set(SURFACE.0 - 1, 0, Rgb::grey(900));
    assert_eq!(g.offer(frame(&image, 110)), Offer::Counted);
    assert_eq!(g.offer(frame(&beat, 120)), Offer::Counted);
    assert_eq!(g.offer(frame(&fill, 130)), Offer::Accepted);
}

#[test]
fn the_cursor_is_excluded_or_rejects() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let mut covered = image.clone();
    covered.set(20, 10, Rgb::grey(1023));
    let with_cursor = |image: &Image, ms, rect| {
        let mut c = frame(image, ms);
        c.descriptor.cursor = Cursor::Shown { rect };
        c
    };
    let over_patch = Rect::new(18, 8, 4, 4);
    assert_eq!(g.offer(frame(&image, 110)), Offer::Counted);
    assert_eq!(
        g.offer(with_cursor(&covered, 120, over_patch)),
        Offer::Counted
    );
    // The cursor of the earlier frame is excluded too.
    assert_eq!(g.offer(frame(&image, 130)), Offer::Accepted);

    let (image, mut g) = gate(MATCH_LIMIT);
    skipped(
        &g.offer(with_cursor(&image, 110, Rect::new(30, 30, 16, 16))),
        "covers the frame marker",
    );
    // On the heartbeat cell only.
    let beat = Marker::place(SURFACE, (32, 20), 7).unwrap().heartbeat();
    assert_eq!(g.offer(with_cursor(&image, 120, beat)), Offer::Counted);
    let mut unknown = frame(&image, 130);
    unknown.descriptor.cursor = Cursor::Unknown;
    rejected(&g.offer(unknown), "cursor position is unknown");
}

#[test]
fn the_surface_is_cropped_but_never_scaled() {
    let (image, mut g) = gate(MATCH_LIMIT);
    for ms in [110, 120] {
        let c = replay::framed(&image, time(ms), (3, 5), (70, 50));
        assert_eq!(g.offer(c), Offer::Counted);
    }
    // The content moves inside the capture.
    let c = replay::framed(&image, time(130), (0, 0), (70, 50));
    assert_eq!(g.offer(c), Offer::Accepted);

    let (image, mut g) = gate(MATCH_LIMIT);
    let larger = Image::new(SURFACE.0 * 2, SURFACE.1 * 2, Rgb::grey(0));
    assert_eq!(g.offer(frame(&image, 110)), Offer::Counted);
    rejected(&g.offer(frame(&larger, 120)), "128×80 px");
    assert_eq!(g.accepted(), []);
    assert_eq!(g.into_last(), Some(frame(&larger, 120)));
}

#[test]
fn missing_or_foreign_time_rejects() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let mut none = frame(&image, 110);
    none.descriptor.timestamps.frame = None;
    rejected(&g.offer(none), "no timestamp");

    let (image, mut g) = gate(MATCH_LIMIT);
    let mut other = frame(&image, 110);
    other.descriptor.timestamps.frame = Some(Timestamp {
        base: TimeBase::Monotonic,
        ticks: 110,
        ticks_per_second: 1000,
    });
    rejected(&g.offer(other), "time base");
}

#[test]
fn another_source_rejects() {
    let (image, mut g) = gate(MATCH_LIMIT);
    assert_eq!(g.offer(frame(&image, 110)), Offer::Counted);
    let mut other = frame(&image, 120);
    other.descriptor.source.native = "another-window".into();
    rejected(&g.offer(other), "source changed");
}

#[test]
fn an_invalid_descriptor_rejects_and_is_kept() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let mut bad = frame(&image, 110);
    bad.descriptor.planes[0].row_pitch = 8;
    let defect = bad.validate().unwrap_err().to_string();
    rejected(&g.offer(bad.clone()), &defect);
    assert_eq!(g.into_last(), Some(bad));
}

/// An FP16 capture with an unknown, increasing conversion of the codes.
#[test]
fn float_captures_with_any_increasing_conversion() {
    let (image, mut g) = gate(MATCH_LIMIT);
    let float = |ms| {
        let (w, h) = (image.width(), image.height());
        let mut data = Vec::with_capacity(w * h * 8);
        for p in image.pixels() {
            for code in [p.r, p.g, p.b] {
                let v = f16::from_f32(12.5 * (f32::from(code) / 1023.0).powi(3) - 0.01);
                data.extend_from_slice(&v.to_le_bytes());
            }
            data.extend_from_slice(&f16::ONE.to_le_bytes());
        }
        let mut c = frame(&image, ms);
        c.descriptor.planes = vec![PlaneLayout {
            row_pitch: w * 8,
            subsampling: [1, 1],
            block_bytes: 8,
            byte_order: ByteOrder::Little,
            fields: [Channel::R, Channel::G, Channel::B, Channel::A]
                .into_iter()
                .zip([0, 16, 32, 48])
                .map(|(channel, shift)| Field {
                    channel,
                    shift,
                    bits: 16,
                    kind: SampleKind::Float,
                })
                .collect(),
        }];
        c.descriptor.colour = Colour {
            transfer: None,
            primaries: None,
            white: None,
            range: None,
            matrix: None,
            luminance: None,
        };
        c.descriptor.timestamps = Timestamps {
            frame: Some(time(ms)),
            arrival: None,
        };
        c.planes = vec![data];
        c.raw_blobs = BTreeMap::new();
        c
    };
    assert_eq!(g.offer(float(110)), Offer::Counted);
    assert_eq!(g.offer(float(120)), Offer::Counted);
    assert_eq!(g.offer(float(130)), Offer::Accepted);
}
