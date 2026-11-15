//! Synthetic capture frames for replay tests: the codes of a presented
//! image as an adapter could deliver them.

use std::collections::BTreeMap;

use nitpick_core::image::Image;

use crate::capture::{
    Alpha, ByteOrder, Capture, CaptureDescriptor, Channel, Colour, Cursor, Field, PlaneLayout,
    Rect, SampleKind, Source, SourceKind, TimeBase, Timestamp, Timestamps,
};

/// The native source of every frame.
pub const SOURCE: &str = "replay-window";

/// `ms` milliseconds on the QPC time base.
#[must_use]
pub fn time(ms: u64) -> Timestamp {
    Timestamp {
        base: TimeBase::Qpc,
        ticks: ms,
        ticks_per_second: 1000,
    }
}

/// `image` as interleaved little-endian RGBA with 16-bit unsigned codes
/// and an ignored alpha. The whole image is content, the cursor is hidden
/// and the colour is unknown.
#[must_use]
pub fn frame(image: &Image, at: Timestamp) -> Capture {
    framed(image, at, (0, 0), (image.width(), image.height()))
}

/// [`frame`] at `offset` in an image of `size`. The other pixels have code
/// `0xFFFF`, as window chrome; `content` gives the image area.
///
/// # Panics
/// `image` does not fit at `offset`.
#[must_use]
pub fn framed(
    image: &Image,
    at: Timestamp,
    offset: (usize, usize),
    size: (usize, usize),
) -> Capture {
    let (w, h) = (image.width(), image.height());
    assert!(
        offset.0 + w <= size.0 && offset.1 + h <= size.1,
        "the image fits"
    );
    let pitch = size.0 * 8;
    let mut data = vec![0xFF; pitch * size.1];
    for y in 0..h {
        for x in 0..w {
            let p = image.get(x, y);
            let at = (offset.1 + y) * pitch + (offset.0 + x) * 8;
            for (i, v) in [p.r, p.g, p.b, 0].into_iter().enumerate() {
                data[at + 2 * i..at + 2 * i + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    let fields = [Channel::R, Channel::G, Channel::B, Channel::A]
        .into_iter()
        .zip((0..).step_by(16))
        .map(|(channel, shift)| Field {
            channel,
            shift,
            bits: 16,
            kind: SampleKind::Uint,
        })
        .collect();
    let content = (offset, size) != ((0, 0), (w, h));
    Capture {
        descriptor: CaptureDescriptor {
            width: size.0,
            height: size.1,
            planes: vec![PlaneLayout {
                row_pitch: pitch,
                subsampling: [1, 1],
                block_bytes: 8,
                byte_order: ByteOrder::Little,
                fields,
            }],
            content: content.then(|| Rect::new(offset.0, offset.1, w, h)),
            cursor: Cursor::Hidden,
            alpha: Some(Alpha::Ignored),
            colour: Colour {
                transfer: None,
                primaries: None,
                white: None,
                range: None,
                matrix: None,
                luminance: None,
            },
            timestamps: Timestamps {
                frame: Some(at),
                arrival: None,
            },
            source: Source {
                kind: SourceKind::Window,
                native: SOURCE.into(),
            },
        },
        planes: vec![data],
        raw: serde_json::Map::new(),
        raw_blobs: BTreeMap::new(),
        config: serde_json::Map::new(),
        preview: None,
    }
}
