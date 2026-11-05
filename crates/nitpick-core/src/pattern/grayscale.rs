//! PQ grayscale / brightness pattern.
//!
//! No standard defines this layout. The levels follow SMPTE ST 2084 and
//! the BT.2408 reference white (203 cd/m²). Regions, on a 0 % background:
//! 1. Two rows of eight absolute-luminance patches, 0 to 10000 cd/m².
//! 2. A strip of 16 near-black steps, 4 codes apart, from 0 %.
//! 3. A ramp over the nominal code range, with ticks at reference luminances.
//!
//! Patches are small (about 2 % of the screen each) to keep the average
//! picture level low, so displays with ABL are less likely to dim them.

use crate::pattern::model::{Align, Fill, PatternModel, Rect, Region, Section};
use crate::signal::{Format, Rgb, pq_eotf};

const REF_W: usize = 1920;
const REF_H: usize = 1080;

/// Luminance patches in cd/m², row-major in two rows of eight.
pub const LUMINANCE_STEPS: [f64; 16] = [
    0.0, 0.1, 0.5, 1.0, 5.0, 10.0, 25.0, 50.0, 100.0, 203.0, 400.0, 600.0, 1000.0, 2000.0, 4000.0,
    10000.0,
];

/// Distance between near-black steps in 10-bit codes. 15 steps of 4 codes
/// reach about 0.1 cd/m², where most displays stop crushing. 12-bit uses
/// 16 codes, so both depths show the same luminances.
pub const NEAR_BLACK_STEP: u16 = 4;
pub const NEAR_BLACK_COUNT: u16 = 16;

/// Luminance of the label text.
const LABEL_NITS: f64 = 100.0;

/// Ramp ticks in cd/m².
const RAMP_TICKS: [f64; 8] = [0.1, 1.0, 10.0, 100.0, 203.0, 1000.0, 4000.0, 10000.0];

// Reference layout at 1920×1080. Text is 16 px high (2× the 8 px font).
const TEXT_H: usize = 16;
const MARGIN_X: usize = 80;
const PATCH_W: usize = 200;
const PATCH_PITCH: usize = 225;
const PATCH_ROWS_Y: [usize; 2] = [80, 380];
const PATCH_H: usize = 200;
const NEAR_BLACK_Y: [usize; 2] = [690, 790];
const RAMP_Y: [usize; 2] = [900, 1000];
const STRIP_W: usize = 1760;

fn pq(format: Format, nits: f64) -> u16 {
    format
        .pq_code_from_nits(nits)
        .expect("grayscale supports PQ encodings only")
}

fn near_black_step(format: Format) -> u16 {
    NEAR_BLACK_STEP << format.depth.shift()
}

fn near_black_codes(format: Format) -> impl Iterator<Item = u16> {
    (0..NEAR_BLACK_COUNT).map(move |k| format.black() + k * near_black_step(format))
}

/// Collects labels and ramp ticks. They form one section, so the reference
/// table lists their level once.
struct Labels {
    name: String,
    colour: Rgb,
    section: Section,
}

impl Labels {
    fn new(format: Format) -> Self {
        Self {
            name: format!("labels {LABEL_NITS} cd/m2"),
            colour: Rgb::grey(pq(format, LABEL_NITS)),
            section: Section::new("labels"),
        }
    }

    /// One text line with its top at `y`, in the column `[x0, x1)`.
    fn text(&mut self, x0: usize, x1: usize, y: usize, text: String, align: Align) {
        self.section.push(Region {
            name: self.name.clone(),
            rect: Rect::new(x0, y, x1, y + TEXT_H),
            fill: Fill::Text {
                text,
                colour: self.colour,
                height: TEXT_H,
                align,
            },
        });
    }

    /// A heading over a strip that starts at `y0`.
    fn heading(&mut self, y0: usize, text: String) {
        self.text(
            MARGIN_X,
            MARGIN_X + STRIP_W,
            y0 - 2 * TEXT_H,
            text,
            Align::Left,
        );
    }

    /// A 2 px vertical tick below `y`.
    fn tick(&mut self, x: usize, y: usize) {
        let rect = Rect::new(x - 1, y, x + 1, y + TEXT_H);
        self.section
            .push(Region::solid(&self.name, rect, self.colour));
    }
}

fn luminance_section(format: Format, labels: &mut Labels) -> Section {
    let mut section = Section::new("luminance");
    for (i, &nits) in LUMINANCE_STEPS.iter().enumerate() {
        let x0 = MARGIN_X + (i % 8) * PATCH_PITCH;
        let y0 = PATCH_ROWS_Y[i / 8];
        let (x1, y1) = (x0 + PATCH_W, y0 + PATCH_H);
        let code = pq(format, nits);
        let rect = Rect::new(x0, y0, x1, y1);
        section.push(Region::solid(
            format!("{nits} cd/m2"),
            rect,
            Rgb::grey(code),
        ));
        labels.text(x0, x1, y1 + 8, format!("{nits} cd/m2"), Align::Centre);
        labels.text(x0, x1, y1 + 32, format!("code {code}"), Align::Centre);
    }
    section
}

fn near_black_section(format: Format, labels: &mut Labels) -> Section {
    let [y0, y1] = NEAR_BLACK_Y;
    labels.heading(
        y0,
        format!(
            "NEAR BLACK: 0% + {} CODES PER STEP",
            near_black_step(format)
        ),
    );
    let step_w = STRIP_W / usize::from(NEAR_BLACK_COUNT);
    let mut section = Section::new("near black");
    for (k, code) in near_black_codes(format).enumerate() {
        let x0 = MARGIN_X + k * step_w;
        let nits = pq_eotf(format.signal_from_code(code));
        let rect = Rect::new(x0, y0, x0 + step_w, y1);
        section.push(Region::solid(
            format!("{nits:.4} cd/m2"),
            rect,
            Rgb::grey(code),
        ));
        labels.text(x0, x0 + step_w, y1 + 8, code.to_string(), Align::Centre);
    }
    section
}

fn ramp_section(format: Format, labels: &mut Labels) -> Section {
    let [y0, y1] = RAMP_Y;
    let (first, last) = (format.black(), format.white());
    labels.heading(y0, format!("RAMP: CODE {first} TO {last}"));
    let mut section = Section::new("ramp");
    section.push(Region {
        name: "ramp".into(),
        rect: Rect::new(MARGIN_X, y0, MARGIN_X + STRIP_W, y1),
        fill: Fill::Ramp {
            first,
            last,
            step: 1,
        },
    });
    for nits in RAMP_TICKS {
        let code = pq(format, nits);
        // First ramp column that reaches the tick code.
        let Some(i) = (0..STRIP_W).find(|&i| Fill::ramp_value(first, last, 1, i, STRIP_W) >= code)
        else {
            continue;
        };
        let x = MARGIN_X + i;
        labels.tick(x, y1);
        labels.text(x - 100, x + 100, y1 + 24, nits.to_string(), Align::Centre);
    }
    section
}

/// Builds the pattern model.
///
/// # Panics
/// If the encoding is not PQ.
#[must_use]
pub fn model(format: Format) -> PatternModel {
    let mut labels = Labels::new(format);
    labels.text(
        MARGIN_X,
        MARGIN_X + STRIP_W,
        30,
        format!(
            "PQ GRAYSCALE  {}  labels {LABEL_NITS} cd/m2",
            format.label().to_uppercase()
        ),
        Align::Left,
    );
    let mut sections = vec![
        luminance_section(format, &mut labels),
        near_black_section(format, &mut labels),
        ramp_section(format, &mut labels),
    ];
    sections.push(labels.section);
    PatternModel {
        title: format!("PQ grayscale, {format}"),
        format,
        reference: (REF_W, REF_H),
        background: Rgb::grey(format.black()),
        sections,
    }
}

#[cfg(test)]
#[path = "grayscale_tests.rs"]
mod tests;
