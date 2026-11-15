//! The binary frame marker that the host draws outside the fixture for a
//! capture, with its heartbeat cell. See "Timing and frame identity" in
//! `doc/design/output_driver/testing.md`.
//!
//! This marker is separate from the marker inside the state fixtures: the
//! host draws it for every fixture, and its value changes with each `show`.

use nitpick_core::image::Image;
use nitpick_core::signal::{Format, Rgb};

use crate::capture::{CaptureView, Channel, Rect};

/// Cell edge in surface pixels.
pub const CELL: usize = 4;
/// Value bits. The value is the `show` sequence number modulo 256, not the
/// frame ID: consecutive `show` requests then always have different values.
pub const BITS: usize = 8;
/// Left to right: a set reference cell, a clear reference cell, the value
/// bits (most significant first) and the heartbeat cell.
pub const CELLS: usize = BITS + 3;
const SET_REF: usize = 0;
const CLEAR_REF: usize = 1;
const FIRST_BIT: usize = 2;
const HEARTBEAT: usize = FIRST_BIT + BITS;
/// Pixels at each cell edge that a read skips, so that a 2×2 chroma block
/// shared with a neighbour cell does not count.
pub const READ_INSET: usize = 1;
/// A cell reads as a bit only within this fraction of the reference
/// difference from one reference; otherwise the marker is unreadable.
const MARGIN: f64 = 0.25;

/// The marker of one `show` on one surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Marker {
    /// Top-left corner on the surface.
    pub origin: (usize, usize),
    pub value: u8,
}

impl Marker {
    /// Places the marker at the bottom-left corner of `surface`, below the
    /// fixture at the top-left corner. The row is even, so that cells start
    /// on 2×2 chroma blocks if the surface starts on one.
    ///
    /// # Errors
    /// The surface has no room for the marker below the fixture.
    pub fn place(
        surface: (usize, usize),
        fixture: (usize, usize),
        value: u8,
    ) -> Result<Self, String> {
        let (w, h) = surface;
        match h.checked_sub(CELL).map(|y| y & !1) {
            Some(y) if w >= CELLS * CELL && fixture.1 <= y => Ok(Self {
                origin: (0, y),
                value,
            }),
            _ => Err(format!(
                "no room for the {}×{CELL} px frame marker below the {}×{} px fixture \
                 on the {w}×{h} px surface",
                CELLS * CELL,
                fixture.0,
                fixture.1
            )),
        }
    }

    #[must_use]
    pub fn cell(&self, i: usize) -> Rect {
        Rect::new(self.origin.0 + i * CELL, self.origin.1, CELL, CELL)
    }

    /// All cells.
    #[must_use]
    pub fn strip(&self) -> Rect {
        Rect::new(self.origin.0, self.origin.1, CELLS * CELL, CELL)
    }

    /// The cells that [`Self::read`] uses: all except the heartbeat cell.
    #[must_use]
    pub fn read_area(&self) -> Rect {
        Rect::new(self.origin.0, self.origin.1, HEARTBEAT * CELL, CELL)
    }

    #[must_use]
    pub fn heartbeat(&self) -> Rect {
        self.cell(HEARTBEAT)
    }

    /// Draws all cells; the heartbeat cell is set if `heartbeat`.
    pub fn draw(&self, image: &mut Image, format: Format, heartbeat: bool) {
        for i in 0..CELLS {
            let set = match i {
                SET_REF => true,
                CLEAR_REF => false,
                HEARTBEAT => heartbeat,
                bit => self.value >> (BITS - 1 - (bit - FIRST_BIT)) & 1 == 1,
            };
            fill(image, self.cell(i), level(format, set));
        }
    }

    /// Draws only the heartbeat cell.
    pub fn draw_heartbeat(&self, image: &mut Image, format: Format, set: bool) {
        fill(image, self.heartbeat(), level(format, set));
    }

    /// Reads the value from a capture with surface pixel (0, 0) at
    /// `origin`. The reference cells give the threshold, so the read does
    /// not depend on the capture colour contract: any increasing conversion
    /// of the codes reads the same value.
    ///
    /// # Errors
    /// A cell is outside the capture, a sample is not finite, the set
    /// reference is not above the clear reference, or a bit cell is not
    /// near one reference (for example in a frame that mixes two markers).
    pub fn read(&self, view: &CaptureView<'_>, origin: (usize, usize)) -> Result<u8, String> {
        let level = |i| cell_level(view, origin, self.cell(i));
        let (set, clear) = (level(SET_REF)?, level(CLEAR_REF)?);
        let span = set - clear;
        if span <= 0.0 {
            return Err(format!(
                "the marker reference cells read {set} (set) and {clear} (clear)"
            ));
        }
        let mut value = 0u8;
        for bit in 0..BITS {
            let t = (level(FIRST_BIT + bit)? - clear) / span;
            let one = if t >= 1.0 - MARGIN {
                true
            } else if t <= MARGIN {
                false
            } else {
                return Err(format!(
                    "marker bit {bit} is between the references ({t:.3})"
                ));
            };
            value = value << 1 | u8::from(one);
        }
        Ok(value)
    }
}

/// Set: half of the nominal signal range. Clear: nominal black.
///
/// **Deviation from the fixture:** automatic light levels include the
/// marker. The set level is far below peak, so it changes `MaxCLL` only for
/// a fixture darker than that level.
#[must_use]
pub fn level(format: Format, set: bool) -> Rgb {
    Rgb::grey(if set {
        format.code_from_signal(0.5)
    } else {
        format.black()
    })
}

fn fill(image: &mut Image, r: Rect, colour: Rgb) {
    image.fill_rect(r.x, r.y, r.x + r.width, r.y + r.height, colour);
}

/// The mean of R + G + B, or of Y, over the cell without its edge.
fn cell_level(view: &CaptureView<'_>, origin: (usize, usize), cell: Rect) -> Result<f64, String> {
    let inner = cell
        .inset(READ_INSET)
        .expect("a cell is wider than its inset");
    let channels = view.colour_channels();
    let channels = if channels[0] == Channel::Y {
        &channels[..1]
    } else {
        &channels[..]
    };
    let (mut sum, mut n) = (0.0, 0u32);
    for y in inner.y..inner.y + inner.height {
        for x in inner.x..inner.x + inner.width {
            for &c in channels {
                let s = view
                    .sample(c, origin.0 + x, origin.1 + y)
                    .ok_or("the frame marker is outside the capture")?
                    .value();
                if !s.is_finite() {
                    return Err(format!("a frame marker sample is {s}"));
                }
                sum += s;
            }
            n += 1;
        }
    }
    Ok(sum / f64::from(n))
}

#[cfg(test)]
#[path = "marker_tests.rs"]
mod tests;
