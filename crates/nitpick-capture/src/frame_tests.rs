use std::cmp::Ordering;

use half::f16;
use nitpick_test_host::capture::{DescriptorError, Sample};

use super::*;

/// Distinct values in each channel: negative, above 1, subnormal, signed
/// zero and the largest finite value.
const VALUES: [f32; 8] = [-0.5, 1.75, 12.5, -0.0, 6.0e-8, 65504.0, -3.25, 0.25];

fn value(x: usize, y: usize, c: usize) -> f16 {
    f16::from_f32(VALUES[(x * 3 + y * 5 + c) % VALUES.len()])
}

/// Rows of `width` pixels with `pad` bytes after each row; the last row
/// without its padding if `trim`.
fn rows(width: usize, height: usize, pad: usize, trim: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    for y in 0..height {
        for x in 0..width {
            for c in 0..4 {
                bytes.extend(value(x, y, c).to_le_bytes());
            }
        }
        if !(trim && y + 1 == height) {
            bytes.extend(std::iter::repeat_n(0xA5, pad));
        }
    }
    bytes
}

fn frame(width: usize, height: usize, pad: usize, trim: bool) -> WgcFrame {
    WgcFrame {
        width,
        height,
        row_pitch: width * RGBA16F_BYTES + pad,
        bytes: rows(width, height, pad, trim),
        content_size: (
            i32::try_from(width).unwrap(),
            i32::try_from(height).unwrap(),
        ),
        system_relative_time: 1_234_567,
        arrival: Timestamp {
            base: TimeBase::Qpc,
            ticks: 999,
            ticks_per_second: 1_000,
        },
        window: 0x1_2340,
        cursor_hidden: true,
        raw: Map::new(),
        config: Map::new(),
    }
}

#[test]
fn samples_keep_sign_and_magnitude_at_their_positions() {
    for (pad, trim) in [(0, false), (24, false), (24, true)] {
        let capture = frame(5, 3, pad, trim).into_capture();
        let view = capture.validate().expect("valid descriptor");
        for y in 0..3 {
            for x in 0..5 {
                for (c, channel) in [Channel::R, Channel::G, Channel::B, Channel::A]
                    .into_iter()
                    .enumerate()
                {
                    let want = value(x, y, c).to_f32();
                    let Some(Sample::Float(got)) = view.sample(channel, x, y) else {
                        panic!("no float sample");
                    };
                    assert_eq!(got.to_bits(), want.to_bits(), "({x}, {y}) {channel:?}");
                }
            }
        }
    }
}

#[test]
fn descriptor_states_no_colour_contract() {
    let d = frame(2, 2, 0, false).into_capture().descriptor;
    assert_eq!(
        d.colour,
        Colour {
            transfer: None,
            primaries: None,
            white: None,
            range: None,
            matrix: None,
            luminance: None,
        }
    );
    assert_eq!(d.alpha, None);
    assert_eq!(d.cursor, Cursor::Hidden);
    assert_eq!(
        d.source,
        Source {
            kind: SourceKind::Window,
            native: "hwnd:0x12340".into(),
        }
    );
}

#[test]
fn cursor_is_unknown_without_the_cursor_setting() {
    let mut f = frame(2, 2, 0, false);
    f.cursor_hidden = false;
    assert_eq!(f.into_capture().descriptor.cursor, Cursor::Unknown);
}

#[test]
fn raw_metadata_keeps_the_api_values() {
    let mut f = frame(4, 2, 16, false);
    f.content_size = (3, 2);
    f.raw
        .insert("display_name".into(), json!("nitpick pattern"));
    let raw = f.into_capture().raw;
    assert_eq!(raw["content_size"], json!([3, 2]));
    assert_eq!(raw["system_relative_time"], json!(1_234_567));
    assert_eq!(raw["row_pitch"], json!(48));
    assert_eq!(raw["display_name"], json!("nitpick pattern"));
}

#[test]
fn content_rectangle_follows_content_size() {
    assert_eq!(content_rect((8, 6), (8, 6)), Rect::new(0, 0, 8, 6));
    // The window became smaller; the pool still has the old size.
    assert_eq!(content_rect((8, 6), (5, 4)), Rect::new(0, 0, 5, 4));
    // The window became larger: the texture holds only a part.
    assert_eq!(content_rect((8, 6), (10, 3)), Rect::new(0, 0, 8, 3));
    assert_eq!(content_rect((8, 6), (-1, 6)), Rect::new(0, 0, 0, 6));
}

#[test]
fn empty_content_fails_validation() {
    let mut f = frame(2, 2, 0, false);
    f.content_size = (0, 2);
    assert_eq!(
        f.into_capture().validate().err(),
        Some(DescriptorError::Content)
    );
}

#[test]
fn frame_time_is_qpc_in_time_span_units() {
    assert_eq!(frame_time(-1), None);
    let t = frame_time(30_000_000).expect("time");
    assert_eq!(t.base, TimeBase::Qpc);
    // 3 s at a QPC frequency of 3 MHz.
    let qpc = |ticks| Timestamp {
        base: TimeBase::Qpc,
        ticks,
        ticks_per_second: 3_000_000,
    };
    assert_eq!(t.cmp_time(&qpc(9_000_000)), Some(Ordering::Equal));
    assert_eq!(t.cmp_time(&qpc(9_000_001)), Some(Ordering::Less));
    let d = frame(1, 1, 0, false).into_capture().descriptor;
    assert_eq!(d.timestamps.frame, frame_time(1_234_567));
}
