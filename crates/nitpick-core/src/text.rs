//! Bitmap text for pattern labels.
//!
//! Labels use a 1-bit font so that every label pixel has one exact code
//! value; anti-aliased text would add intermediate levels to the scope trace.

use font8x8::{BASIC_FONTS, UnicodeFonts};

use crate::image::Image;
use crate::signal::Rgb;

const GLYPH: usize = 8;

/// Width in pixels of `text` at integer `scale`.
#[must_use]
pub fn text_width(text: &str, scale: usize) -> usize {
    text.chars().count() * GLYPH * scale
}

/// Line height in pixels at integer `scale`.
#[must_use]
pub fn line_height(scale: usize) -> usize {
    GLYPH * scale
}

/// Draws `text` with its top-left corner at `(x, y)`. Only set font bits are
/// written; the background is unchanged. Characters outside Basic Latin draw
/// as `?`.
pub fn draw_text(img: &mut Image, x: usize, y: usize, scale: usize, text: &str, colour: Rgb) {
    for (i, ch) in text.chars().enumerate() {
        let glyph = BASIC_FONTS
            .get(ch)
            .or_else(|| BASIC_FONTS.get('?'))
            .unwrap_or_default();
        let gx = x + i * GLYPH * scale;
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..GLYPH {
                if bits & (1 << col) != 0 {
                    let px = gx + col * scale;
                    let py = y + row * scale;
                    img.fill_rect(px, py, px + scale, py + scale, colour);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
