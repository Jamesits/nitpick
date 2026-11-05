use super::*;
use crate::image::Image;
use crate::pattern::layout::scale;

const PQ_N10: Format = Format::new(Encoding::PqNarrow, BitDepth::Ten);
const PQ_F10: Format = Format::new(Encoding::PqFull, BitDepth::Ten);
const PQ_N12: Format = Format::new(Encoding::PqNarrow, BitDepth::Twelve);
const PQ_F12: Format = Format::new(Encoding::PqFull, BitDepth::Twelve);

fn all_formats() -> impl Iterator<Item = Format> {
    Encoding::ALL
        .into_iter()
        .flat_map(|e| BitDepth::ALL.map(|d| Format::new(e, d)))
}

fn render(format: Format, width: usize, height: usize) -> Image {
    model(format, SizeClass::for_output(width, height)).render(width, height)
}

fn level_values(format: Format, size: SizeClass) -> std::collections::HashSet<Rgb> {
    model(format, size).levels().iter().map(|l| l.rgb).collect()
}

fn grey(img: &Image, x: usize, y: usize) -> u16 {
    let c = img.get(x, y);
    assert!(c.r == c.g && c.g == c.b, "not grey at ({x}, {y}): {c:?}");
    c.r
}

#[test]
fn hd_layout_pq_narrow() {
    let img = render(PQ_N10, 1920, 1080);
    // Row 1: grey 0..240, 100 % white 240..446, green 858..1062.
    assert_eq!(grey(&img, 239, 0), 414);
    assert_eq!(grey(&img, 240, 0), 940);
    assert_eq!(img.get(858, 89), Rgb::new(64, 940, 64));
    assert_eq!(img.get(1061, 89), Rgb::new(64, 940, 64));
    assert_eq!(img.get(1062, 0), Rgb::new(940, 64, 940));
    assert_eq!(grey(&img, 1680, 0), 414);
    // Row 2: 58 % bars.
    assert_eq!(img.get(1474, 90), Rgb::new(64, 64, 573));
    assert_eq!(img.get(1679, 629), Rgb::new(64, 64, 573));
    // Row 3: -7 % under white, 0 % starts at yellow, 109 % ends at 1680.
    assert_eq!(grey(&img, 0, 630), 573);
    assert_eq!(grey(&img, 445, 630), 4);
    assert_eq!(grey(&img, 446, 630), 64);
    assert_eq!(grey(&img, 549, 630), 152);
    assert_eq!(grey(&img, 1679, 719), 1019);
    assert_eq!(grey(&img, 1680, 719), 573);
    // Row 4: black, lead-in at -7 %, ramp, lead-out at 109 %.
    assert_eq!(grey(&img, 239, 720), 64);
    assert_eq!(grey(&img, 240, 720), 4);
    assert_eq!(grey(&img, 798, 720), 4);
    assert_eq!(grey(&img, 799, 720), 5);
    assert_eq!(grey(&img, 1812, 720), 1018);
    assert_eq!(grey(&img, 1813, 720), 1019);
    assert_eq!(grey(&img, 1919, 809), 1019);
    // Row 5.
    assert_eq!(img.get(0, 810), Rgb::new(569, 572, 381));
    assert_eq!(grey(&img, 240, 810), 64);
    assert_eq!(grey(&img, 376, 810), 48);
    assert_eq!(grey(&img, 514, 810), 80);
    assert_eq!(grey(&img, 652, 810), 99);
    assert_eq!(grey(&img, 722, 810), 64);
    assert_eq!(grey(&img, 960, 1079), 573);
    assert_eq!(img.get(1919, 1079), Rgb::new(318, 236, 563));
}

#[test]
fn twelve_bit_levels_match_bt2111_3() {
    // Spot checks of the 12-bit columns of Tables 2, 3 and 4.
    let img = render(PQ_N12, 1920, 1080);
    assert_eq!(grey(&img, 0, 0), 1656);
    assert_eq!(img.get(446, 300), Rgb::new(2292, 2292, 256));
    assert_eq!(grey(&img, 445, 630), 16);
    assert_eq!(grey(&img, 1679, 630), 4076);
    assert_eq!(img.get(0, 810), Rgb::new(2276, 2288, 1524));
    assert_eq!(grey(&img, 376, 810), 192);
    assert_eq!(grey(&img, 652, 810), 396);
    let img = render(PQ_F12, 1920, 1080);
    assert_eq!(grey(&img, 0, 0), 1638);
    assert_eq!(grey(&img, 240, 90), 2378);
    assert_eq!(grey(&img, 549, 630), 410);
    assert_eq!(grey(&img, 1062, 630), 2457);
    assert_eq!(img.get(1760, 810), Rgb::new(2181, 1339, 901));
    assert_eq!(grey(&img, 514, 810), 75);
    assert_eq!(grey(&img, 652, 810), 164);
    let img = render(
        Format::new(Encoding::HlgNarrow, BitDepth::Twelve),
        1920,
        1080,
    );
    assert_eq!(img.get(1840, 900), Rgb::new(908, 588, 2808));
}

#[test]
fn ramp_passes_zero_at_green_bar() {
    // BT.2111: ramp 0 % aligns with the left edge of the green bar (x = 858).
    let img = render(PQ_N10, 1920, 1080);
    assert_eq!(grey(&img, 857, 720), 63);
    assert_eq!(grey(&img, 858, 720), 64);
    let img = render(PQ_F10, 1920, 1080);
    assert_eq!(grey(&img, 857, 720), 0);
    assert_eq!(grey(&img, 858, 720), 1);
    assert_eq!(grey(&img, 1879, 720), 1022);
    assert_eq!(grey(&img, 1880, 720), 1023);
    // 12-bit narrow: the first code at or above 0 % (256) is at the green bar.
    for size in SizeClass::ALL {
        let k = size.factor();
        let img = render(PQ_N12, 1920 * k, 1080 * k);
        let y = 760 * k;
        assert!(grey(&img, 858 * k - 1, y) < 256, "{size:?}");
        assert_eq!(grey(&img, 858 * k, y), 256, "{size:?}");
    }
}

#[test]
fn uhd_is_exact_double_of_hd_at_10_bit() {
    // Includes the ramp: BT.2111-3 Tables 5 and 6 repeat each 10-bit ramp code twice at 4K.
    for enc in Encoding::ALL {
        let f = Format::new(enc, BitDepth::Ten);
        let hd = render(f, 1920, 1080);
        let uhd = render(f, 3840, 2160);
        for y in (0..1080).step_by(7) {
            for x in 0..1920 {
                assert_eq!(
                    hd.get(x, y),
                    uhd.get(2 * x + 1, 2 * y + 1),
                    "{f} ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn ramp_is_monotonic_and_reaches_end_points() {
    for f in all_formats() {
        for (w, h) in [(1920, 1080), (3840, 2160), (2560, 1440)] {
            let img = render(f, w, h);
            let y = scale(765, h, 1080);
            let row: Vec<u16> = (0..w).map(|x| grey(&img, x, y)).collect();
            let start = scale(240, w, 1920);
            assert!(row[start..].windows(2).all(|p| p[0] <= p[1]), "{f} {w}");
            let (lo, hi) = f.code_limits();
            assert_eq!(row[start], lo, "{f} {w}");
            assert_eq!(row[w - 1], hi, "{f} {w}");
        }
    }
}

#[test]
fn full_range_has_no_sub_black() {
    let img = render(PQ_F10, 1920, 1080);
    assert_eq!(grey(&img, 376, 810), 0);
    assert_eq!(grey(&img, 514, 810), 19);
    assert_eq!(grey(&img, 652, 810), 41);
    assert_eq!(img.get(240, 0), Rgb::grey(1023));
    assert_eq!(img.get(240, 90), Rgb::grey(594));
}

#[test]
fn hlg_primary_bars_are_75_percent() {
    let img = render(Format::new(Encoding::HlgNarrow, BitDepth::Ten), 1920, 1080);
    assert_eq!(img.get(446, 300), Rgb::new(721, 721, 64));
    assert_eq!(img.get(80, 900), Rgb::new(538, 709, 718));
}

#[test]
fn patches_cover_every_pixel_value_except_ramp() {
    for f in all_formats() {
        let img = render(f, 1920, 1080);
        let known = level_values(f, SizeClass::K2);
        for y in (0..1080).filter(|y| !(720..810).contains(y)) {
            for x in 0..1920 {
                assert!(known.contains(&img.get(x, y)), "{f} ({x}, {y})");
            }
        }
    }
}

#[test]
fn ramp_rows_match_bt2111_3_tables_5_and_6() {
    // (format, size) -> (B, C, D, first, last, step). C and the code ranges are
    // the values and notes of Tables 5 and 6.
    let cases = [
        (PQ_N10, SizeClass::K2, (559, 1014, 107, 5, 1018, 1)),
        (PQ_N10, SizeClass::K4, (1118, 2028, 214, 5, 1018, 1)),
        (PQ_N10, SizeClass::K8, (2236, 4056, 428, 5, 1018, 1)),
        (PQ_N12, SizeClass::K2, (559, 1015, 106, 20, 4076, 4)),
        (PQ_N12, SizeClass::K4, (1117, 2031, 212, 18, 4078, 2)),
        (PQ_N12, SizeClass::K8, (2233, 4062, 425, 17, 4078, 1)),
        (PQ_F10, SizeClass::K2, (618, 1022, 40, 1, 1022, 1)),
        (PQ_F10, SizeClass::K4, (1236, 2044, 80, 1, 1022, 1)),
        (PQ_F10, SizeClass::K8, (2472, 4088, 160, 1, 1022, 1)),
        (PQ_F12, SizeClass::K2, (618, 1023, 39, 4, 4092, 4)),
        (PQ_F12, SizeClass::K4, (1236, 2047, 77, 2, 4094, 2)),
        (PQ_F12, SizeClass::K8, (2472, 4094, 154, 1, 4094, 1)),
    ];
    for (f, size, (b, c, d, first, last, step)) in cases {
        let r = ramp_row(f, size);
        assert_eq!(
            (r.lead_in, r.ramp, r.lead_out, r.first, r.last, r.step),
            (b, c, d, first, last, step),
            "{f} {size:?}"
        );
    }
}

#[test]
fn ramp_pixels_match_bt2111_3_tables_5_and_6() {
    // Narrow (Table 5, 2K 10-bit): C = 1014 px holds 5 to 1018.
    let img = render(PQ_N10, 1920, 1080);
    let ramp: Vec<u16> = (799..1813).map(|x| grey(&img, x, 760)).collect();
    assert_eq!(ramp, (5..=1018).collect::<Vec<u16>>());
    // Full (Table 6, 2K 10-bit): C = 1022 px holds 1 to 1022.
    let img = render(PQ_F10, 1920, 1080);
    let ramp: Vec<u16> = (858..1880).map(|x| grey(&img, x, 760)).collect();
    assert_eq!(ramp, (1..=1022).collect::<Vec<u16>>());
    // 4K 10-bit: C = 2028 px, each code on 2 px.
    let img = render(PQ_N10, 3840, 2160);
    assert_eq!(grey(&img, 1598, 1500), 5);
    assert_eq!(grey(&img, 1599, 1500), 5);
    assert_eq!(grey(&img, 1600, 1500), 6);
    assert_eq!(grey(&img, 3625, 1500), 1018);
    assert_eq!(grey(&img, 3626, 1500), 1019);
    // 4K 12-bit narrow: lead-in 16 from x = 480, ramp 18, 20, … 4078 from
    // x = 1597, one code per pixel, lead-out 4079.
    let img = render(PQ_N12, 3840, 2160);
    let x0 = 480 + 1117;
    assert_eq!(grey(&img, x0 - 1, 1500), 16);
    let ramp: Vec<u16> = (x0..x0 + 2031).map(|x| grey(&img, x, 1500)).collect();
    assert_eq!(ramp, (18..=4078).step_by(2).collect::<Vec<u16>>());
    assert_eq!(grey(&img, x0 + 2031, 1500), 4079);
    // 2K 12-bit full: ramp 4, 8, … 4092 from x = 858.
    let img = render(PQ_F12, 1920, 1080);
    let ramp: Vec<u16> = (858..858 + 1023).map(|x| grey(&img, x, 760)).collect();
    assert_eq!(ramp, (4..=4092).step_by(4).collect::<Vec<u16>>());
}

#[test]
fn size_class_selection() {
    assert_eq!(SizeClass::for_output(1920, 1080), SizeClass::K2);
    assert_eq!(SizeClass::for_output(2560, 1440), SizeClass::K2);
    assert_eq!(SizeClass::for_output(3840, 2160), SizeClass::K4);
    assert_eq!(SizeClass::for_output(5120, 2160), SizeClass::K4);
    assert_eq!(SizeClass::for_output(7680, 4320), SizeClass::K8);
    assert_eq!(SizeClass::for_output(1280, 720), SizeClass::K2);
}
