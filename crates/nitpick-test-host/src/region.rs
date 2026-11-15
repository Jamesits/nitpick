//! Region selection: where the surface is in a capture, and which fixture
//! pixels a comparison samples. See "Timing and frame identity" in
//! `doc/design/output_driver/testing.md`.
//!
//! The fixture is at the top-left corner of the surface, so fixture and
//! surface coordinates are equal.

use crate::capture::{CaptureDescriptor, Rect};
use crate::fixture::FixtureFrame;

/// The inset of flat-patch samples. One pixel skips a 2×2 chroma block
/// shared with a neighbour patch. One-pixel geometry uses an inset of 0.
pub const PATCH_INSET: usize = 1;

/// The capture position of surface pixel (0, 0).
///
/// # Errors
/// The content does not have the surface size. A capture is never scaled
/// to fit; a resize is a failure.
pub fn surface_origin(
    d: &CaptureDescriptor,
    surface: (usize, usize),
) -> Result<(usize, usize), String> {
    let content = d
        .content
        .unwrap_or_else(|| Rect::new(0, 0, d.width, d.height));
    if (content.width, content.height) != surface {
        return Err(format!(
            "the capture content is {}×{} px; the surface is {}×{} px",
            content.width, content.height, surface.0, surface.1
        ));
    }
    Ok((content.x, content.y))
}

/// The sample pixels of one palette entry, in fixture coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    pub index: usize,
    pub pixels: Vec<(usize, usize)>,
}

/// The sample pixels of each palette entry, in palette order. A pixel is a
/// sample if each pixel within `inset` (horizontally, vertically and
/// diagonally) is in the fixture and has the same palette entry, and it is
/// outside `exclude`. An entry without such pixels has an empty patch.
///
/// With `inset` 0 every fixture pixel outside `exclude` is a sample: use
/// it for one-pixel geometry. With [`PATCH_INSET`] the samples are the
/// interiors of flat patches, without edges.
#[must_use]
pub fn patches(fixture: &FixtureFrame, inset: usize, exclude: &[Rect]) -> Vec<Patch> {
    let (w, h) = fixture.size;
    let index = |x, y| fixture.index(x, y);
    // `row[i]`: the row window of pixel `i` has one palette entry.
    let mut row = vec![false; w * h];
    for y in 0..h {
        for x in inset..w.saturating_sub(inset) {
            let i = index(x, y);
            row[y * w + x] = (x - inset..=x + inset).all(|q| index(q, y) == i);
        }
    }
    let mut out: Vec<Patch> = (0..fixture.palette.len())
        .map(|index| Patch {
            index,
            pixels: Vec::new(),
        })
        .collect();
    for y in inset..h.saturating_sub(inset) {
        for x in 0..w {
            let i = index(x, y);
            let flat = (y - inset..=y + inset).all(|q| row[q * w + x] && index(x, q) == i);
            if flat && !exclude.iter().any(|r| r.contains(x, y)) {
                out[i].pixels.push((x, y));
            }
        }
    }
    out
}

/// The maximum and median of per-sample errors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErrorStats {
    pub count: usize,
    pub max: f64,
    /// The mean of the two middle values for an even count.
    pub median: f64,
}

impl ErrorStats {
    /// `Ok(None)`: no samples.
    ///
    /// # Errors
    /// An error value is not finite.
    pub fn new(mut errors: Vec<f64>) -> Result<Option<Self>, String> {
        if let Some(e) = errors.iter().find(|e| !e.is_finite()) {
            return Err(format!("a sample error is {e}"));
        }
        if errors.is_empty() {
            return Ok(None);
        }
        errors.sort_by(f64::total_cmp);
        let n = errors.len();
        let median = if n % 2 == 1 {
            errors[n / 2]
        } else {
            f64::midpoint(errors[n / 2 - 1], errors[n / 2])
        };
        Ok(Some(Self {
            count: n,
            max: errors[n - 1],
            median,
        }))
    }
}

#[cfg(test)]
#[path = "region_tests.rs"]
mod tests;
