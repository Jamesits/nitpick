//! ITU-R BT.2111-3 colour bar test pattern for HDR television.
//!
//! The code values are the integer tables of BT.2111-3 (Table 2 HLG narrow,
//! Table 3 PQ narrow, Table 4 PQ full). The spec defines the layout for
//! 1920×1080, 3840×2160 and 7680×4320; the model uses 1920×1080 and
//! [`PatternModel::render`] scales it.

use crate::pattern::model::{Fill, PatternModel, Rect, Region, Section};
use crate::signal::{Encoding, Rgb10};

const REF_W: usize = 1920;
const REF_H: usize = 1080;

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

/// Code values of one BT.2111-3 table.
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
    bt709: [Rgb10; 6],
    /// PLUGE -2 %, +2 %, +4 %. Full range (Figure 3) has 0 % black in place of -2 %.
    pluge: [u16; 3],
    pluge_names: &'static [&'static str; 3],
    /// Ramp row in reference pixels: constant lead-in, ramp, constant lead-out.
    /// The lead-in starts at x = 240 and the ramp passes 0 % at the green bar.
    ramp_widths: [usize; 3],
    /// Lead-in and lead-out levels: narrow -7 % and 109 %, full 0 % and 100 %.
    /// The ramp holds the codes strictly between them; at 1920 px that is one
    /// pixel per code (Tables 5 and 6: narrow 5 to 1018, full 1 to 1022).
    ramp_from: u16,
    ramp_to: u16,
}

const NARROW_STEPS: [u16; 13] = [
    4, 64, 152, 239, 327, 414, 502, 590, 677, 765, 852, 940, 1019,
];

fn levels(encoding: Encoding) -> Levels {
    let rgb = |r: [u16; 6], g: [u16; 6], b: [u16; 6]| {
        std::array::from_fn(|i| Rgb10::new(r[i], g[i], b[i]))
    };
    match encoding {
        Encoding::HlgNarrow => Levels {
            grey40: 414,
            primary: 721,
            primary_name: "75%",
            steps: NARROW_STEPS,
            step_names: &NARROW_STEP_NAMES,
            bt709: rgb(
                [713, 538, 512, 651, 639, 227],
                [719, 709, 706, 286, 269, 147],
                [316, 718, 296, 705, 164, 702],
            ),
            pluge: [48, 80, 99],
            pluge_names: &["-2%", "+2%", "+4%"],
            ramp_widths: [559, 1014, 107],
            ramp_from: 4,
            ramp_to: 1019,
        },
        Encoding::PqNarrow => Levels {
            grey40: 414,
            primary: 573,
            primary_name: "58%",
            steps: NARROW_STEPS,
            step_names: &NARROW_STEP_NAMES,
            bt709: rgb(
                [569, 485, 474, 537, 531, 318],
                [572, 566, 565, 362, 351, 236],
                [381, 571, 368, 564, 257, 563],
            ),
            pluge: [48, 80, 99],
            pluge_names: &["-2%", "+2%", "+4%"],
            ramp_widths: [559, 1014, 107],
            ramp_from: 4,
            ramp_to: 1019,
        },
        Encoding::PqFull => Levels {
            grey40: 409,
            primary: 594,
            primary_name: "58%",
            steps: [
                0, 0, 102, 205, 307, 409, 512, 614, 716, 818, 921, 1023, 1023,
            ],
            step_names: &FULL_STEP_NAMES,
            bt709: rgb(
                [589, 491, 479, 552, 545, 296],
                [593, 586, 585, 348, 335, 201],
                [370, 592, 355, 584, 225, 582],
            ),
            pluge: [0, 19, 41],
            pluge_names: &["0%", "+2%", "+4%"],
            ramp_widths: [618, 1022, 40],
            ramp_from: 0,
            ramp_to: 1023,
        },
    }
}

/// The seven bars white … blue at a given level.
fn bars(lo: u16, hi: u16) -> [Rgb10; 7] {
    [
        Rgb10::new(hi, hi, hi),
        Rgb10::new(hi, hi, lo),
        Rgb10::new(lo, hi, hi),
        Rgb10::new(lo, hi, lo),
        Rgb10::new(hi, lo, hi),
        Rgb10::new(hi, lo, lo),
        Rgb10::new(lo, lo, hi),
    ]
}

/// Region edges of one row: `start` followed by the running sums of `widths`.
fn edges(start: usize, widths: &[usize]) -> Vec<usize> {
    let mut x = start;
    let mut out = vec![x];
    for w in widths {
        x += w;
        out.push(x);
    }
    out
}

/// Rectangle spanning `[x0, x1)` of pattern row `row` (0 = top).
fn in_row(row: usize, x0: usize, x1: usize) -> Rect {
    Rect::new(x0, ROWS[row], x1, ROWS[row + 1])
}

/// Rows 1 and 2: 40 % grey side panels around seven bars at `level`.
fn bar_section(lv: &Levels, row: usize, pct: &str, black: u16, level: u16) -> Section {
    let cols = edges(0, &BAR_WIDTHS);
    let grey = Rgb10::grey(lv.grey40);
    let mut section = Section::new(format!("{pct} colour bars"));
    section.push(Region::solid(
        "40% grey",
        in_row(row, cols[0], cols[1]),
        grey,
    ));
    for (i, (name, colour)) in BAR_NAMES.iter().zip(bars(black, level)).enumerate() {
        let rect = in_row(row, cols[i + 1], cols[i + 2]);
        section.push(Region::solid(format!("{pct} {name}"), rect, colour));
    }
    section.push(Region::solid(
        "40% grey",
        in_row(row, cols[8], cols[9]),
        grey,
    ));
    section
}

/// Row 3: primary white sides, the first step under the white bar, then
/// two steps per bar from yellow to blue. Each step is half of its bar.
fn stair_section(lv: &Levels) -> Section {
    let cols = edges(0, &BAR_WIDTHS);
    let primary = Rgb10::grey(lv.primary);
    let primary_white = format!("{} white", lv.primary_name);
    let mut section = Section::new("stair");
    section.push(Region::solid(
        &primary_white,
        in_row(2, cols[0], cols[1]),
        primary,
    ));
    let mut step_edges = vec![cols[1], cols[2]];
    for bar in 2..8 {
        step_edges.push(cols[bar] + (cols[bar + 1] - cols[bar]) / 2);
        step_edges.push(cols[bar + 1]);
    }
    for (k, (name, code)) in lv.step_names.iter().zip(lv.steps).enumerate() {
        let rect = in_row(2, step_edges[k], step_edges[k + 1]);
        section.push(Region::solid(
            format!("{name} step"),
            rect,
            Rgb10::grey(code),
        ));
    }
    section.push(Region::solid(
        &primary_white,
        in_row(2, cols[8], cols[9]),
        primary,
    ));
    section
}

/// Row 4: 0 % black, lead-in, ramp, lead-out (Tables 5 and 6).
fn ramp_section(lv: &Levels, black: u16) -> Section {
    let x = edges(240, &lv.ramp_widths);
    let mut section = Section::new("ramp");
    section
        .push(Region::solid(
            "0% black",
            in_row(3, 0, x[0]),
            Rgb10::grey(black),
        ))
        .push(Region::solid(
            "ramp lead-in",
            in_row(3, x[0], x[1]),
            Rgb10::grey(lv.ramp_from),
        ))
        .push(Region {
            name: "ramp".into(),
            rect: in_row(3, x[1], x[2]),
            fill: Fill::Ramp {
                first: lv.ramp_from + 1,
                last: lv.ramp_to - 1,
            },
        })
        .push(Region::solid(
            "ramp lead-out",
            in_row(3, x[2], x[3]),
            Rgb10::grey(lv.ramp_to),
        ));
    section
}

/// Row 5: BT.709 bars, black levels, primary white, BT.709 bars.
fn bottom_section(lv: &Levels, black: u16) -> Section {
    let pct = lv.primary_name;
    let bt709 = |i: usize| (format!("{pct} BT.709 {}", BT709_NAMES[i]), lv.bt709[i]);
    let pluge = |i: usize| {
        let name = format!("{} black", lv.pluge_names[i]);
        (name, Rgb10::grey(lv.pluge[i]))
    };
    let zero = || ("0% black".to_string(), Rgb10::grey(black));
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
        (format!("{pct} white"), Rgb10::grey(lv.primary)),
        zero(),
        bt709(3),
        bt709(4),
        bt709(5),
    ];
    let x = edges(0, &BOTTOM_WIDTHS);
    let mut section = Section::new("bottom");
    for (i, (name, colour)) in regions.into_iter().enumerate() {
        section.push(Region::solid(name, in_row(4, x[i], x[i + 1]), colour));
    }
    section
}

/// Builds the pattern model. Region names follow the BT.2111-3 table rows.
#[must_use]
pub fn model(encoding: Encoding) -> PatternModel {
    let lv = levels(encoding);
    let (black, white) = (encoding.black(), encoding.white());
    PatternModel {
        title: format!("BT.2111-3 colour bars, {}", encoding.label()),
        encoding,
        reference: (REF_W, REF_H),
        background: Rgb10::grey(black),
        sections: vec![
            bar_section(&lv, 0, "100%", black, white),
            bar_section(&lv, 1, lv.primary_name, black, lv.primary),
            stair_section(&lv),
            ramp_section(&lv, black),
            bottom_section(&lv, black),
        ],
    }
}

#[cfg(test)]
#[path = "bt2111_tests.rs"]
mod tests;
