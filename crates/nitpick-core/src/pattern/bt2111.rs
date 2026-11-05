//! ITU-R BT.2111-3 colour bar test pattern for HDR television.
//!
//! The code values are the integer tables of BT.2111-3 (Table 2 HLG narrow,
//! Table 3 PQ narrow, Table 4 PQ full), 10-bit and 12-bit. The spec defines
//! the layout for 1920×1080, 3840×2160 and 7680×4320 ([`SizeClass`]). The
//! 12-bit ramp row is not an exact multiple between these sizes (Tables 5
//! and 6), so the model is built for one size class and
//! [`PatternModel::render`] scales it to other sizes.
//!
//! Full range (Table 4, Figure 3) has no sub-black or super-white levels. The
//! -7% step, the 109% step and the -2% PLUGE bar are replaced by the 0%
//! step, the 100% step and 0 % black.
//!
//! the BT.2100 formula gives 2291 for 203 cd/m² narrow 12-bit, but Table 3
//! has 2292 (4×573). The pattern uses the table.
//!
//! BT.2111 recommends to identify the signal format in the grey side bars.
//! Nitpick keeps them plain 40% grey.

use crate::pattern::model::{Fill, PatternModel, Rect, Region, Section};
use crate::signal::{BitDepth, Encoding, Format, Rgb};

/// BT.2111-3 Table 1 picture sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SizeClass {
    /// 1920×1080.
    K2,
    /// 3840×2160.
    K4,
    /// 7680×4320.
    K8,
}

impl SizeClass {
    pub const ALL: [Self; 3] = [Self::K2, Self::K4, Self::K8];

    /// Multiple of 1920×1080.
    #[must_use]
    pub fn factor(self) -> usize {
        match self {
            Self::K2 => 1,
            Self::K4 => 2,
            Self::K8 => 4,
        }
    }

    /// The largest class that fits in the output. Smaller outputs use 2K.
    #[must_use]
    pub fn for_output(width: usize, height: usize) -> Self {
        Self::ALL
            .into_iter()
            .rev()
            .find(|c| width >= 1920 * c.factor() && height >= 1080 * c.factor())
            .unwrap_or(Self::K2)
    }

    fn index(self) -> usize {
        match self {
            Self::K2 => 0,
            Self::K4 => 1,
            Self::K8 => 2,
        }
    }
}

/// Row boundaries at 1080 lines: 100 % bars b/12, primary bars b/2, stair b/12,
/// ramp b/12, bottom section b/4.
const ROWS: [usize; 6] = [0, 90, 630, 720, 810, 1080];

/// Rows 1 and 2: grey, white, yellow, cyan, green, magenta, red, blue, grey.
/// Green is 2 px narrower than the other bars so the total is 1920.
const BAR_WIDTHS: [usize; 9] = [240, 206, 206, 206, 204, 206, 206, 206, 240];

/// Row 5: three BT.709 bars, f, g, h, g, h, g, i, j, k, three BT.709 bars.
const BOTTOM_WIDTHS: [usize; 15] = [
    80, 80, 80, 136, 70, 68, 70, 68, 70, 238, 438, 282, 80, 80, 80,
];

const BAR_NAMES: [&str; 7] = ["white", "yellow", "cyan", "green", "magenta", "red", "blue"];
const BT709_NAMES: [&str; 6] = ["yellow", "cyan", "green", "magenta", "red", "blue"];
const NARROW_STEP_NAMES: [&str; 13] = [
    "-7%", "0%", "10%", "20%", "30%", "40%", "50%", "60%", "70%", "80%", "90%", "100%", "109%",
];
const FULL_STEP_NAMES: [&str; 13] = [
    "0%", "0%", "10%", "20%", "30%", "40%", "50%", "60%", "70%", "80%", "90%", "100%", "100%",
];

/// Code values of one BT.2111-3 table column (encoding and bit depth).
struct Levels {
    grey40: u16,
    /// Level of the primary bars: 75 % HLG, 58 % PQ.
    primary: u16,
    primary_name: &'static str,
    /// Stair: -7 %, 0 %, 10 % … 100 %, 109 %. Full range has no sub-black or
    /// super-white; Figure 3 puts the 0 % and 100 % steps in their places.
    steps: [u16; 13],
    step_names: &'static [&'static str; 13],
    /// BT.709 colour bars in the BT.2020 container, yellow … blue.
    bt709: [Rgb; 6],
    /// PLUGE -2 %, +2 %, +4 %. Full range (Figure 3) has 0 % black in place of -2 %.
    pluge: [u16; 3],
    pluge_names: &'static [&'static str; 3],
}

const NARROW_STEPS: [u16; 13] = [
    4, 64, 152, 239, 327, 414, 502, 590, 677, 765, 852, 940, 1019,
];

fn rgb_columns(r: [u16; 6], g: [u16; 6], b: [u16; 6]) -> [Rgb; 6] {
    std::array::from_fn(|i| Rgb::new(r[i], g[i], b[i]))
}

/// 10-bit levels.
fn levels10(encoding: Encoding) -> Levels {
    match encoding {
        Encoding::HlgNarrow => Levels {
            grey40: 414,
            primary: 721,
            primary_name: "75%",
            steps: NARROW_STEPS,
            step_names: &NARROW_STEP_NAMES,
            bt709: rgb_columns(
                [713, 538, 512, 651, 639, 227],
                [719, 709, 706, 286, 269, 147],
                [316, 718, 296, 705, 164, 702],
            ),
            pluge: [48, 80, 99],
            pluge_names: &["-2%", "+2%", "+4%"],
        },
        Encoding::PqNarrow => Levels {
            grey40: 414,
            primary: 573,
            primary_name: "58%",
            steps: NARROW_STEPS,
            step_names: &NARROW_STEP_NAMES,
            bt709: rgb_columns(
                [569, 485, 474, 537, 531, 318],
                [572, 566, 565, 362, 351, 236],
                [381, 571, 368, 564, 257, 563],
            ),
            pluge: [48, 80, 99],
            pluge_names: &["-2%", "+2%", "+4%"],
        },
        Encoding::PqFull => Levels {
            grey40: 409,
            primary: 594,
            primary_name: "58%",
            steps: [
                0, 0, 102, 205, 307, 409, 512, 614, 716, 818, 921, 1023, 1023,
            ],
            step_names: &FULL_STEP_NAMES,
            bt709: rgb_columns(
                [589, 491, 479, 552, 545, 296],
                [593, 586, 585, 348, 335, 201],
                [370, 592, 355, 584, 225, 582],
            ),
            pluge: [0, 19, 41],
            pluge_names: &["0%", "+2%", "+4%"],
        },
    }
}

/// Levels of one table column. The narrow-range 12-bit values of Tables 2
/// and 3 are exactly 4× the 10-bit values. Table 4 (full range) is not.
fn levels(format: Format) -> Levels {
    let lv = levels10(format.encoding);
    match (format.encoding, format.depth) {
        (_, BitDepth::Ten) => lv,
        (Encoding::PqFull, BitDepth::Twelve) => Levels {
            grey40: 1638,
            primary: 2378,
            steps: [
                0, 0, 410, 819, 1229, 1638, 2048, 2457, 2867, 3276, 3686, 4095, 4095,
            ],
            bt709: rgb_columns(
                [2359, 1967, 1918, 2209, 2181, 1186],
                [2373, 2348, 2342, 1391, 1339, 806],
                [1483, 2371, 1423, 2339, 901, 2331],
            ),
            pluge: [0, 75, 164],
            ..lv
        },
        (_, BitDepth::Twelve) => Levels {
            grey40: lv.grey40 << 2,
            primary: lv.primary << 2,
            steps: lv.steps.map(|c| c << 2),
            bt709: lv.bt709.map(|c| c.shl(2)),
            pluge: lv.pluge.map(|c| c << 2),
            ..lv
        },
    }
}

/// Ramp row (Tables 5 and 6) in pixels of one size class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RampRow {
    /// Width of the lead-in at the lowest permitted code (B).
    pub lead_in: usize,
    /// Width of the ramp (C).
    pub ramp: usize,
    /// Width of the lead-out at the highest permitted code (D).
    pub lead_out: usize,
    /// The ramp codes are `first, first + step, …, last`: the codes strictly
    /// between the lead-in and lead-out levels, in steps of `step`.
    pub first: u16,
    pub last: u16,
    pub step: u16,
}

/// Lead-in (B) and lead-out (D) widths of Tables 5 and 6 at 2K, 4K and 8K.
/// The 10-bit widths are exact multiples of 2K. The 12-bit widths are not,
/// because the 12-bit ramp has one pixel per 4, 2 and 1 codes.
const RAMP_LEAD: [[[usize; 2]; 3]; 4] = [
    // narrow 10-bit
    [[559, 107], [1118, 214], [2236, 428]],
    // narrow 12-bit
    [[559, 106], [1117, 212], [2233, 425]],
    // full 10-bit
    [[618, 40], [1236, 80], [2472, 160]],
    // full 12-bit
    [[618, 39], [1236, 77], [2472, 154]],
];

/// The ramp row of BT.2111-3 Tables 5 and 6 for `format` at `size`.
#[must_use]
pub fn ramp_row(format: Format, size: SizeClass) -> RampRow {
    let k = size.factor();
    let twelve = format.depth == BitDepth::Twelve;
    let table = usize::from(!format.is_narrow()) * 2 + usize::from(twelve);
    let [lead_in, lead_out] = RAMP_LEAD[table][size.index()];
    let (from, to) = format.code_limits();
    let step = match (format.depth, size) {
        (BitDepth::Ten, _) | (BitDepth::Twelve, SizeClass::K8) => 1,
        (BitDepth::Twelve, SizeClass::K4) => 2,
        (BitDepth::Twelve, SizeClass::K2) => 4,
    };
    let first = from + step;
    let last = first + (to - 1 - first) / step * step;
    RampRow {
        lead_in,
        ramp: 1680 * k - lead_in - lead_out,
        lead_out,
        first,
        last,
        step,
    }
}

/// The seven bars white … blue at a given level.
fn bars(lo: u16, hi: u16) -> [Rgb; 7] {
    [
        Rgb::new(hi, hi, hi),
        Rgb::new(hi, hi, lo),
        Rgb::new(lo, hi, hi),
        Rgb::new(lo, hi, lo),
        Rgb::new(hi, lo, hi),
        Rgb::new(hi, lo, lo),
        Rgb::new(lo, lo, hi),
    ]
}

/// Geometry of one size class: 2K positions multiplied by `k`.
struct Grid {
    k: usize,
}

impl Grid {
    /// Region edges of one row: `start` followed by the running sums of `widths`.
    fn edges(&self, start: usize, widths: &[usize]) -> Vec<usize> {
        let mut x = start * self.k;
        let mut out = vec![x];
        for w in widths {
            x += w * self.k;
            out.push(x);
        }
        out
    }

    /// Rectangle spanning `[x0, x1)` of pattern row `row` (0 = top).
    fn in_row(&self, row: usize, x0: usize, x1: usize) -> Rect {
        Rect::new(x0, ROWS[row] * self.k, x1, ROWS[row + 1] * self.k)
    }
}

/// Rows 1 and 2: 40 % grey side panels around seven bars at `level`.
fn bar_section(g: &Grid, lv: &Levels, row: usize, pct: &str, black: u16, level: u16) -> Section {
    let cols = g.edges(0, &BAR_WIDTHS);
    let grey = Rgb::grey(lv.grey40);
    let mut section = Section::new(format!("{pct} colour bars"));
    section.push(Region::solid(
        "40% grey",
        g.in_row(row, cols[0], cols[1]),
        grey,
    ));
    for (i, (name, colour)) in BAR_NAMES.iter().zip(bars(black, level)).enumerate() {
        let rect = g.in_row(row, cols[i + 1], cols[i + 2]);
        section.push(Region::solid(format!("{pct} {name}"), rect, colour));
    }
    section.push(Region::solid(
        "40% grey",
        g.in_row(row, cols[8], cols[9]),
        grey,
    ));
    section
}

/// Row 3: primary white sides, the first step under the white bar, then
/// two steps per bar from yellow to blue. Each step is half of its bar.
fn stair_section(g: &Grid, lv: &Levels) -> Section {
    let cols = g.edges(0, &BAR_WIDTHS);
    let primary = Rgb::grey(lv.primary);
    let primary_white = format!("{} white", lv.primary_name);
    let mut section = Section::new("stair");
    section.push(Region::solid(
        &primary_white,
        g.in_row(2, cols[0], cols[1]),
        primary,
    ));
    let mut step_edges = vec![cols[1], cols[2]];
    for bar in 2..8 {
        step_edges.push(cols[bar] + (cols[bar + 1] - cols[bar]) / 2);
        step_edges.push(cols[bar + 1]);
    }
    for (k, (name, code)) in lv.step_names.iter().zip(lv.steps).enumerate() {
        let rect = g.in_row(2, step_edges[k], step_edges[k + 1]);
        section.push(Region::solid(format!("{name} step"), rect, Rgb::grey(code)));
    }
    section.push(Region::solid(
        &primary_white,
        g.in_row(2, cols[8], cols[9]),
        primary,
    ));
    section
}

/// Row 4: 0 % black, lead-in, ramp, lead-out (Tables 5 and 6). The lead-in
/// starts at x = 240 (2K) and the ramp passes 0 % at the green bar.
fn ramp_section(g: &Grid, format: Format, size: SizeClass) -> Section {
    let r = ramp_row(format, size);
    let (from, to) = format.code_limits();
    let x0 = 240 * g.k;
    let x = [x0, x0 + r.lead_in, x0 + r.lead_in + r.ramp, 1920 * g.k];
    let mut section = Section::new("ramp");
    section
        .push(Region::solid(
            "0% black",
            g.in_row(3, 0, x[0]),
            Rgb::grey(format.black()),
        ))
        .push(Region::solid(
            "ramp lead-in",
            g.in_row(3, x[0], x[1]),
            Rgb::grey(from),
        ))
        .push(Region {
            name: "ramp".into(),
            rect: g.in_row(3, x[1], x[2]),
            fill: Fill::Ramp {
                first: r.first,
                last: r.last,
                step: r.step,
            },
        })
        .push(Region::solid(
            "ramp lead-out",
            g.in_row(3, x[2], x[3]),
            Rgb::grey(to),
        ));
    section
}

/// Row 5: BT.709 bars, black levels, primary white, BT.709 bars.
fn bottom_section(g: &Grid, lv: &Levels, black: u16) -> Section {
    let pct = lv.primary_name;
    let bt709 = |i: usize| (format!("{pct} BT.709 {}", BT709_NAMES[i]), lv.bt709[i]);
    let pluge = |i: usize| {
        let name = format!("{} black", lv.pluge_names[i]);
        (name, Rgb::grey(lv.pluge[i]))
    };
    let zero = || ("0% black".to_string(), Rgb::grey(black));
    let regions = [
        bt709(0),
        bt709(1),
        bt709(2),
        zero(),
        pluge(0),
        zero(),
        pluge(1),
        zero(),
        pluge(2),
        zero(),
        (format!("{pct} white"), Rgb::grey(lv.primary)),
        zero(),
        bt709(3),
        bt709(4),
        bt709(5),
    ];
    let x = g.edges(0, &BOTTOM_WIDTHS);
    let mut section = Section::new("bottom");
    for (i, (name, colour)) in regions.into_iter().enumerate() {
        section.push(Region::solid(name, g.in_row(4, x[i], x[i + 1]), colour));
    }
    section
}

/// Builds the pattern model at the reference size of `size`. Region names
/// follow the BT.2111-3 table rows.
#[must_use]
pub fn model(format: Format, size: SizeClass) -> PatternModel {
    let lv = levels(format);
    let g = Grid { k: size.factor() };
    let (black, white) = (format.black(), format.white());
    PatternModel {
        title: format!("BT.2111-3 colour bars, {format}"),
        format,
        reference: (1920 * g.k, 1080 * g.k),
        background: Rgb::grey(black),
        sections: vec![
            bar_section(&g, &lv, 0, "100%", black, white),
            bar_section(&g, &lv, 1, lv.primary_name, black, lv.primary),
            stair_section(&g, &lv),
            ramp_section(&g, format, size),
            bottom_section(&g, &lv, black),
        ],
    }
}

#[cfg(test)]
#[path = "bt2111_tests.rs"]
mod tests;
