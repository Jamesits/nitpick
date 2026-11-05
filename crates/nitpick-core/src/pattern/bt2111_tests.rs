use super::*;
use crate::image::Image;
use crate::pattern::layout::scale;

fn render(encoding: Encoding, width: usize, height: usize) -> Image {
    model(encoding).render(width, height)
}

fn level_values(encoding: Encoding) -> std::collections::HashSet<u32> {
    model(encoding)
        .levels()
        .iter()
        .map(|l| l.rgb.pack())
        .collect()
}

fn grey(img: &Image, x: usize, y: usize) -> u16 {
    let c = img.get(x, y);
    assert!(c.r == c.g && c.g == c.b, "not grey at ({x}, {y}): {c:?}");
    c.r
}

#[test]
fn hd_layout_pq_narrow() {
    let img = render(Encoding::PqNarrow, 1920, 1080);
    // Row 1: grey 0..240, 100 % white 240..446, green 858..1062.
    assert_eq!(grey(&img, 239, 0), 414);
    assert_eq!(grey(&img, 240, 0), 940);
    assert_eq!(img.get(858, 89), Rgb10::new(64, 940, 64));
    assert_eq!(img.get(1061, 89), Rgb10::new(64, 940, 64));
    assert_eq!(img.get(1062, 0), Rgb10::new(940, 64, 940));
    assert_eq!(grey(&img, 1680, 0), 414);
    // Row 2: 58 % bars.
    assert_eq!(img.get(1474, 90), Rgb10::new(64, 64, 573));
    assert_eq!(img.get(1679, 629), Rgb10::new(64, 64, 573));
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
    assert_eq!(img.get(0, 810), Rgb10::new(569, 572, 381));
    assert_eq!(grey(&img, 240, 810), 64);
    assert_eq!(grey(&img, 376, 810), 48);
    assert_eq!(grey(&img, 514, 810), 80);
    assert_eq!(grey(&img, 652, 810), 99);
    assert_eq!(grey(&img, 722, 810), 64);
    assert_eq!(grey(&img, 960, 1079), 573);
    assert_eq!(img.get(1919, 1079), Rgb10::new(318, 236, 563));
}

#[test]
fn ramp_passes_zero_at_green_bar() {
    // BT.2111: ramp 0 % aligns with the left edge of the green bar (x = 858).
    let img = render(Encoding::PqNarrow, 1920, 1080);
    assert_eq!(grey(&img, 857, 720), 63);
    assert_eq!(grey(&img, 858, 720), 64);
    let img = render(Encoding::PqFull, 1920, 1080);
    assert_eq!(grey(&img, 857, 720), 0);
    assert_eq!(grey(&img, 858, 720), 1);
    assert_eq!(grey(&img, 1879, 720), 1022);
    assert_eq!(grey(&img, 1880, 720), 1023);
}

#[test]
fn uhd_is_exact_double_of_hd() {
    // Includes the ramp: BT.2111-3 Tables 5 and 6 repeat each ramp code twice at 4K.
    for enc in Encoding::ALL {
        let hd = render(enc, 1920, 1080);
        let uhd = render(enc, 3840, 2160);
        for y in (0..1080).step_by(7) {
            for x in 0..1920 {
                assert_eq!(
                    hd.get(x, y),
                    uhd.get(2 * x + 1, 2 * y + 1),
                    "{enc} ({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn ramp_is_monotonic_and_reaches_end_points() {
    for enc in Encoding::ALL {
        for (w, h) in [(1920, 1080), (3840, 2160), (2560, 1440)] {
            let img = render(enc, w, h);
            let y = scale(765, h, 1080);
            let row: Vec<u16> = (0..w).map(|x| grey(&img, x, y)).collect();
            let start = scale(240, w, 1920);
            assert!(row[start..].windows(2).all(|p| p[0] <= p[1]), "{enc} {w}");
            let (lo, hi) = enc.code_limits();
            assert_eq!(row[start], if enc.is_narrow() { lo } else { 0 });
            assert_eq!(row[w - 1], hi);
        }
    }
}

#[test]
fn full_range_has_no_sub_black() {
    let img = render(Encoding::PqFull, 1920, 1080);
    assert_eq!(grey(&img, 376, 810), 0);
    assert_eq!(grey(&img, 514, 810), 19);
    assert_eq!(grey(&img, 652, 810), 41);
    assert_eq!(img.get(240, 0), Rgb10::grey(1023));
    assert_eq!(img.get(240, 90), Rgb10::grey(594));
}

#[test]
fn hlg_primary_bars_are_75_percent() {
    let img = render(Encoding::HlgNarrow, 1920, 1080);
    assert_eq!(img.get(446, 300), Rgb10::new(721, 721, 64));
    assert_eq!(img.get(80, 900), Rgb10::new(538, 709, 718));
}

#[test]
fn patches_cover_every_pixel_value_except_ramp() {
    for enc in Encoding::ALL {
        let img = render(enc, 1920, 1080);
        let known = level_values(enc);
        for y in (0..1080).filter(|y| !(720..810).contains(y)) {
            for x in 0..1920 {
                assert!(known.contains(&img.get(x, y).pack()), "{enc} ({x}, {y})");
            }
        }
    }
}

#[test]
fn ramp_matches_bt2111_3_tables_5_and_6() {
    // Narrow (Table 5, 2K 10-bit): C = 1014 px holds 5 to 1018, E = 59 px holds 5 to 63,
    // F = 935 px holds 5 to 939.
    let img = render(Encoding::PqNarrow, 1920, 1080);
    let ramp: Vec<u16> = (799..1813).map(|x| grey(&img, x, 760)).collect();
    assert_eq!(ramp, (5..=1018).collect::<Vec<u16>>());
    // Full (Table 6, 2K 10-bit): C = 1022 px holds 1 to 1022.
    let img = render(Encoding::PqFull, 1920, 1080);
    let ramp: Vec<u16> = (858..1880).map(|x| grey(&img, x, 760)).collect();
    assert_eq!(ramp, (1..=1022).collect::<Vec<u16>>());
    // 4K 10-bit: C = 2028 px, each code on 2 px.
    let img = render(Encoding::PqNarrow, 3840, 2160);
    assert_eq!(grey(&img, 1598, 1500), 5);
    assert_eq!(grey(&img, 1599, 1500), 5);
    assert_eq!(grey(&img, 1600, 1500), 6);
    assert_eq!(grey(&img, 3625, 1500), 1018);
    assert_eq!(grey(&img, 3626, 1500), 1019);
}
