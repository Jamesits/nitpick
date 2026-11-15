//! A Windows Graphics Capture frame as a [`Capture`]: the mapped
//! `R16G16B16A16_FLOAT` texture, unchanged. The module has no Windows
//! dependency, so that its tests run on each OS.
//!
//! The colour fields stay `None`. WGC gives no colour space with a frame,
//! and FP16 storage alone does not establish scRGB or its 80 cd/m² scale.

use nitpick_test_host::capture::{
    ByteOrder, Capture, CaptureDescriptor, Channel, Colour, Cursor, Field, PlaneLayout, Rect,
    SampleKind, Source, SourceKind, TimeBase, Timestamp, Timestamps,
};
use serde_json::{Map, Value, json};

/// `Windows.Foundation.TimeSpan` units: 100 ns.
pub const TIME_SPAN_TICKS_PER_SECOND: u64 = 10_000_000;

/// Bytes of one `R16G16B16A16_FLOAT` pixel.
pub const RGBA16F_BYTES: usize = 8;

/// One frame of the frame pool, after the copy to CPU memory.
#[derive(Debug, Clone, PartialEq)]
pub struct WgcFrame {
    /// The texture size in pixels; the frame pool size.
    pub width: usize,
    pub height: usize,
    /// `D3D11_MAPPED_SUBRESOURCE::RowPitch`.
    pub row_pitch: usize,
    /// The mapped rows with the padding of the API. The last row can end
    /// without its padding.
    pub bytes: Vec<u8>,
    /// `Direct3D11CaptureFrame::ContentSize`. After a resize of the window
    /// it differs from the texture size until the frame pool is recreated.
    pub content_size: (i32, i32),
    /// `Direct3D11CaptureFrame::SystemRelativeTime`, in 100 ns units.
    pub system_relative_time: i64,
    /// When the frame handler received the frame.
    pub arrival: Timestamp,
    /// The `HWND` of the captured window.
    pub window: isize,
    /// The session has `IsCursorCaptureEnabled` false.
    pub cursor_hidden: bool,
    /// Other API values, for example the texture description.
    pub raw: Map<String, Value>,
    pub config: Map<String, Value>,
}

/// Little-endian binary16 R, G, B, A in this order.
#[must_use]
pub fn rgba16f_plane(row_pitch: usize) -> PlaneLayout {
    let field = |channel, shift| Field {
        channel,
        shift,
        bits: 16,
        kind: SampleKind::Float,
    };
    PlaneLayout {
        row_pitch,
        subsampling: [1, 1],
        block_bytes: RGBA16F_BYTES,
        byte_order: ByteOrder::Little,
        fields: vec![
            field(Channel::R, 0),
            field(Channel::G, 16),
            field(Channel::B, 32),
            field(Channel::A, 48),
        ],
    }
}

/// `SystemRelativeTime` as a QPC time. `None` for a negative value.
///
/// Microsoft documents the value only as a `TimeSpan` relative to the
/// system, not as a QPC value. The adapter treats it as the QPC time in
/// 100 ns units, so that it compares with `QueryPerformanceCounter` at
/// another tick rate; the capture test of `nitpick-app` checks it against
/// QPC. It can be some milliseconds after the arrival of the frame.
#[must_use]
pub fn frame_time(system_relative_time: i64) -> Option<Timestamp> {
    Some(Timestamp {
        base: TimeBase::Qpc,
        ticks: u64::try_from(system_relative_time).ok()?,
        ticks_per_second: TIME_SPAN_TICKS_PER_SECOND,
    })
}

/// The part of the texture that holds the window content, from the top-left
/// corner. A content size above the texture size is cut to the texture; a
/// negative size gives 0, which validation rejects.
#[must_use]
pub fn content_rect(texture: (usize, usize), content_size: (i32, i32)) -> Rect {
    let clip = |v: i32, max: usize| usize::try_from(v).unwrap_or(0).min(max);
    Rect::new(
        0,
        0,
        clip(content_size.0, texture.0),
        clip(content_size.1, texture.1),
    )
}

/// The source of a window capture: the `HWND` in hexadecimal.
#[must_use]
pub fn window_source(window: isize) -> Source {
    Source {
        kind: SourceKind::Window,
        native: format!("hwnd:{window:#x}"),
    }
}

impl WgcFrame {
    /// The capture of the frame: the bytes unchanged, the content rectangle
    /// from `ContentSize`, and no colour contract.
    #[must_use]
    pub fn into_capture(self) -> Capture {
        let mut raw = self.raw;
        raw.insert(
            "content_size".into(),
            json!([self.content_size.0, self.content_size.1]),
        );
        raw.insert(
            "system_relative_time".into(),
            json!(self.system_relative_time),
        );
        raw.insert("row_pitch".into(), json!(self.row_pitch));
        Capture {
            descriptor: CaptureDescriptor {
                width: self.width,
                height: self.height,
                planes: vec![rgba16f_plane(self.row_pitch)],
                content: Some(content_rect((self.width, self.height), self.content_size)),
                cursor: if self.cursor_hidden {
                    Cursor::Hidden
                } else {
                    Cursor::Unknown
                },
                // WGC does not state the meaning of the alpha channel.
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
                    frame: frame_time(self.system_relative_time),
                    arrival: Some(self.arrival),
                },
                source: window_source(self.window),
            },
            planes: vec![self.bytes],
            raw,
            raw_blobs: std::collections::BTreeMap::new(),
            config: self.config,
            preview: None,
        }
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
