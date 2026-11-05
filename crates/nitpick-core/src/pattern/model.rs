//! Resolution-independent pattern model: pattern → sections → regions.
//!
//! Generators (BT.2111, grayscale, …) only build a [`PatternModel`]. Every
//! algorithm that needs pattern values (rendering, the reference table,
//! later measurement comparison or format conversion) reads the model, not
//! the generator and not the pixels.
//!
//! Geometry is in reference pixels (for example 1920×1080). [`PatternModel::render`]
//! scales each rectangle edge with [`crate::pattern::layout::scale`]; regions that
//! share an edge in reference space share it at every output size.

use std::collections::HashSet;

use crate::image::Image;
use crate::pattern::layout::scale;
use crate::signal::{Encoding, Rgb10};
use crate::text::{draw_text, text_width};

/// Half-open rectangle `[x0, x1) × [y0, y1)` in reference pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl Rect {
    #[must_use]
    pub const fn new(x0: usize, y0: usize, x1: usize, y1: usize) -> Self {
        Self { x0, y0, x1, y1 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Centre,
}

/// How a region is filled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fill {
    Solid(Rgb10),
    /// Horizontal grey ramp over the inclusive codes `first..=last`, spread
    /// evenly over the output width `w`: pixel `i` is
    /// `first + floor(i * n / w)` with `n = last - first + 1`.
    /// With `w = n` every pixel has its own code.
    Ramp {
        first: u16,
        last: u16,
    },
    /// 1-bit text, one line, top-aligned in the region. `height` is the glyph
    /// height in reference pixels; the output uses the largest integer
    /// multiple of the 8 px font that fits the scaled height (minimum 1).
    Text {
        text: String,
        colour: Rgb10,
        height: usize,
        align: Align,
    },
}

impl Fill {
    /// Value of ramp pixel `i` of an output ramp `width` pixels wide.
    #[must_use]
    pub fn ramp_value(first: u16, last: u16, i: usize, width: usize) -> u16 {
        let n = u64::from(last - first) + 1;
        let v = u64::from(first) + (i as u64) * n / (width.max(1) as u64);
        // i < width, so v <= last <= 1023.
        #[allow(clippy::cast_possible_truncation)]
        let v = (v as u16).min(last);
        v
    }
}

/// A named area with one fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub name: String,
    pub rect: Rect,
    pub fill: Fill,
}

impl Region {
    #[must_use]
    pub fn solid(name: impl Into<String>, rect: Rect, colour: Rgb10) -> Self {
        Self {
            name: name.into(),
            rect,
            fill: Fill::Solid(colour),
        }
    }
}

/// A named group of regions, for example one row of colour bars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub regions: Vec<Region>,
}

impl Section {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            regions: Vec::new(),
        }
    }

    pub fn push(&mut self, region: Region) -> &mut Self {
        self.regions.push(region);
        self
    }
}

/// A complete pattern for one encoding. Sections and regions draw in order;
/// later regions cover earlier ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternModel {
    pub title: String,
    pub encoding: Encoding,
    /// Reference size the geometry is defined at.
    pub reference: (usize, usize),
    /// Fill of every pixel that no region covers.
    pub background: Rgb10,
    pub sections: Vec<Section>,
}

/// One row of the reference table: a flat level of the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level<'a> {
    pub section: &'a str,
    pub name: &'a str,
    pub rgb: Rgb10,
}

impl PatternModel {
    /// All regions with their section, in draw order.
    pub fn regions(&self) -> impl Iterator<Item = (&Section, &Region)> {
        self.sections
            .iter()
            .flat_map(|s| s.regions.iter().map(move |r| (s, r)))
    }

    /// Flat levels, one per distinct `(section, name)`. A ramp gives its
    /// first and last code. Text gives its colour.
    #[must_use]
    pub fn levels(&self) -> Vec<Level<'_>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (section, region) in self.regions() {
            let entries: Vec<(&str, Rgb10)> = match &region.fill {
                Fill::Solid(c) | Fill::Text { colour: c, .. } => vec![(&region.name, *c)],
                Fill::Ramp { first, last } => vec![
                    ("ramp first", Rgb10::grey(*first)),
                    ("ramp last", Rgb10::grey(*last)),
                ],
            };
            for (name, rgb) in entries {
                if seen.insert((section.name.as_str(), name)) {
                    out.push(Level {
                        section: &section.name,
                        name,
                        rgb,
                    });
                }
            }
        }
        out
    }

    /// Renders at `width × height` output pixels.
    #[must_use]
    pub fn render(&self, width: usize, height: usize) -> Image {
        let (rw, rh) = self.reference;
        let sx = |v| scale(v, width, rw);
        let sy = |v| scale(v, height, rh);
        let mut img = Image::new(width, height, self.background);
        for (_, region) in self.regions() {
            let r = region.rect;
            let (x0, y0, x1, y1) = (sx(r.x0), sy(r.y0), sx(r.x1), sy(r.y1));
            match &region.fill {
                Fill::Solid(c) => img.fill_rect(x0, y0, x1, y1, *c),
                &Fill::Ramp { first, last } => {
                    let w = x1.saturating_sub(x0);
                    img.fill_columns(x0, y0, x1, y1, |x| {
                        Rgb10::grey(Fill::ramp_value(first, last, x - x0, w))
                    });
                }
                Fill::Text {
                    text,
                    colour,
                    height: h,
                    align,
                } => {
                    let s = (sy(*h) / 8).max(1);
                    let x = match align {
                        Align::Left => x0,
                        Align::Centre => {
                            usize::midpoint(x0, x1).saturating_sub(text_width(text, s) / 2)
                        }
                    };
                    draw_text(&mut img, x, y0, s, text, *colour);
                }
            }
        }
        img
    }
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
