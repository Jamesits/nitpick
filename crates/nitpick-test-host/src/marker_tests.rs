use std::collections::BTreeMap;

use half::f16;
use nitpick_core::signal::{BitDepth, Encoding};

use super::*;
use crate::capture::{
    ByteOrder, Capture, CaptureDescriptor, Colour, Cursor, Field, PlaneLayout, SampleKind, Source,
    SourceKind, Timestamps,
};
use crate::replay;

const PQ10: Format = Format::new(Encoding::PqFull, BitDepth::Ten);
const SURFACE: (usize, usize) = (64, 40);

fn drawn(value: u8, heartbeat: bool) -> (Marker, Image) {
    let marker = Marker::place(SURFACE, (32, 20), value).unwrap();
    let mut image = Image::new(SURFACE.0, SURFACE.1, Rgb::grey(0));
    marker.draw(&mut image, PQ10, heartbeat);
    (marker, image)
}

/// One plane of 16-bit fields, three channels plus an unused fourth.
fn capture_of(
    image: &Image,
    channels: [Channel; 3],
    kind: SampleKind,
    sample: impl Fn(usize, u16) -> u16,
) -> Capture {
    let (w, h) = (image.width(), image.height());
    let mut data = Vec::with_capacity(w * h * 8);
    for y in 0..h {
        for x in 0..w {
            let p = image.get(x, y);
            for (i, code) in [p.r, p.g, p.b, 0].into_iter().enumerate() {
                data.extend_from_slice(&sample(i, code).to_le_bytes());
            }
        }
    }
    let fields = channels
        .into_iter()
        .zip([0, 16, 32])
        .map(|(channel, shift)| Field {
            channel,
            shift,
            bits: 16,
            kind,
        })
        .collect();
    Capture {
        descriptor: CaptureDescriptor {
            width: w,
            height: h,
            planes: vec![PlaneLayout {
                row_pitch: w * 8,
                subsampling: [1, 1],
                block_bytes: 8,
                byte_order: ByteOrder::Little,
                fields,
            }],
            content: None,
            cursor: Cursor::Hidden,
            alpha: None,
            colour: Colour {
                transfer: None,
                primaries: None,
                white: None,
                range: None,
                matrix: None,
                luminance: None,
            },
            timestamps: Timestamps {
                frame: None,
                arrival: None,
            },
            source: Source {
                kind: SourceKind::Window,
                native: "test".into(),
            },
        },
        planes: vec![data],
        raw: serde_json::Map::new(),
        raw_blobs: BTreeMap::new(),
        config: serde_json::Map::new(),
        preview: None,
    }
}

fn read(marker: &Marker, capture: &Capture) -> Result<u8, String> {
    marker.read(&capture.validate().unwrap(), (0, 0))
}

#[test]
fn placed_below_the_fixture_on_an_even_row() {
    let place = |surface, fixture| Marker::place(surface, fixture, 1).map(|m| m.origin);
    assert_eq!(place((64, 40), (32, 20)), Ok((0, 36)));
    assert_eq!(place((64, 41), (32, 20)), Ok((0, 36)));
    assert_eq!(place((44, 4), (44, 0)), Ok((0, 0)));
    assert_eq!(place((64, 40), (64, 36)), Ok((0, 36)));
    // No room below the fixture, too narrow, too low.
    assert!(place((64, 40), (32, 37)).is_err());
    assert!(place((43, 40), (8, 8)).is_err());
    assert!(place((64, 3), (1, 0)).is_err());
    let m = Marker::place((64, 40), (32, 20), 1).unwrap();
    assert_eq!(m.strip(), Rect::new(0, 36, CELLS * CELL, CELL));
    assert_eq!(m.heartbeat(), Rect::new(10 * CELL, 36, CELL, CELL));
    assert_eq!(m.read_area(), Rect::new(0, 36, 10 * CELL, CELL));
}

#[test]
fn draws_references_bits_and_heartbeat() {
    let (marker, image) = drawn(0b1010_0110, true);
    let (set, clear) = (level(PQ10, true), level(PQ10, false));
    assert_eq!(set, Rgb::grey(PQ10.code_from_signal(0.5)));
    assert_eq!(clear, Rgb::grey(0));
    let cells = [1, 0, 1, 0, 1, 0, 0, 1, 1, 0, 1];
    for (i, bit) in cells.into_iter().enumerate() {
        let want = if bit == 1 { set } else { clear };
        let r = marker.cell(i);
        for y in r.y..r.y + CELL {
            for x in r.x..r.x + CELL {
                assert_eq!(image.get(x, y), want, "cell {i} ({x}, {y})");
            }
        }
    }
    // Nothing outside the strip.
    for y in 0..SURFACE.1 {
        for x in 0..SURFACE.0 {
            if !marker.strip().contains(x, y) {
                assert_eq!(image.get(x, y), Rgb::grey(0));
            }
        }
    }
    let mut cleared = image.clone();
    marker.draw_heartbeat(&mut cleared, PQ10, false);
    assert_eq!(cleared, drawn(0b1010_0110, false).1);
}

#[test]
fn narrow_range_levels() {
    let f = Format::new(Encoding::PqNarrow, BitDepth::Ten);
    assert_eq!(level(f, false), Rgb::grey(64));
    assert_eq!(level(f, true), Rgb::grey(502));
}

#[test]
fn reads_every_value_with_or_without_heartbeat() {
    for value in 0..=u8::MAX {
        for heartbeat in [false, true] {
            let (marker, image) = drawn(value, heartbeat);
            let capture = replay::frame(&image, replay::time(1));
            assert_eq!(read(&marker, &capture), Ok(value));
        }
    }
}

/// Any increasing conversion of the codes reads the same value, also with
/// negative and above-one float samples.
#[test]
fn read_needs_no_colour_contract() {
    let (marker, image) = drawn(0x5A, false);
    let rgb = [Channel::R, Channel::G, Channel::B];
    let scaled = capture_of(&image, rgb, SampleKind::Float, |_, code| {
        f16::from_f32(-0.25 + 40.0 * (f32::from(code) / 1023.0).powf(2.4)).to_bits()
    });
    assert_eq!(read(&marker, &scaled), Ok(0x5A));
    // YCbCr: only Y counts.
    let ycc = [Channel::Y, Channel::Cb, Channel::Cr];
    let ycbcr = capture_of(&image, ycc, SampleKind::Uint, |i, code| {
        if i == 0 { code + 64 } else { 1023 - code }
    });
    assert_eq!(read(&marker, &ycbcr), Ok(0x5A));
}

#[test]
fn unreadable_markers_are_errors() {
    let (marker, image) = drawn(0xFF, false);
    // A bit cell half way between the references, as in a torn frame.
    let mut torn = image.clone();
    let r = marker.cell(5);
    let mid = level(PQ10, true).r / 2;
    torn.fill_rect(r.x, r.y, r.x + CELL, r.y + CELL, Rgb::grey(mid));
    let e = read(&marker, &replay::frame(&torn, replay::time(1))).unwrap_err();
    assert!(e.contains("bit 3"), "{e}");
    // A bit cell near a reference reads.
    let near = level(PQ10, true).r * 4 / 5;
    torn.fill_rect(r.x, r.y, r.x + CELL, r.y + CELL, Rgb::grey(near));
    assert_eq!(
        read(&marker, &replay::frame(&torn, replay::time(1))),
        Ok(0xFF)
    );
    // No marker: equal references.
    let black = Image::new(SURFACE.0, SURFACE.1, Rgb::grey(0));
    assert!(read(&marker, &replay::frame(&black, replay::time(1))).is_err());
    // The marker is outside a smaller capture.
    let small = Image::new(SURFACE.0, 30, Rgb::grey(0));
    let e = read(&marker, &replay::frame(&small, replay::time(1))).unwrap_err();
    assert!(e.contains("outside"), "{e}");
    // A non-finite sample.
    let rgb = [Channel::R, Channel::G, Channel::B];
    let nan = capture_of(&image, rgb, SampleKind::Float, |_, code| {
        if code == 0 {
            f16::NAN.to_bits()
        } else {
            f16::ONE.to_bits()
        }
    });
    assert!(read(&marker, &nan).is_err());
}

#[test]
fn read_follows_the_surface_origin() {
    let (marker, image) = drawn(0x3C, false);
    let framed = replay::framed(&image, replay::time(1), (5, 7), (80, 60));
    let view = framed.validate().unwrap();
    assert_eq!(marker.read(&view, (5, 7)), Ok(0x3C));
    assert_ne!(marker.read(&view, (0, 0)), Ok(0x3C));
}
