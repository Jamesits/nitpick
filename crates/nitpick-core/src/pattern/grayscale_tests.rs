use super::*;
use crate::image::Image;

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

#[test]
fn patches_have_pq_codes() {
    let img = render(Encoding::PqNarrow, 1920, 1080);
    // Patch 9 (203 cd/m²) is row 1, column 1: x 305..505, y 380..580.
    assert_eq!(img.get(400, 480), Rgb10::grey(573));
    // Patch 15 (10000 cd/m²) is row 1, column 7.
    assert_eq!(img.get(1755, 480), Rgb10::grey(940));
    // Patch 0 (0 cd/m²) equals the background.
    assert_eq!(img.get(180, 180), Rgb10::grey(64));
    let img = render(Encoding::PqFull, 1920, 1080);
    assert_eq!(img.get(400, 480), Rgb10::grey(594));
}

#[test]
fn near_black_steps() {
    let img = render(Encoding::PqNarrow, 1920, 1080);
    assert_eq!(img.get(80, 740), Rgb10::grey(64));
    assert_eq!(img.get(190, 740), Rgb10::grey(68));
    assert_eq!(img.get(1839, 740), Rgb10::grey(124));
}

#[test]
fn ramp_spans_nominal_range() {
    for enc in [Encoding::PqNarrow, Encoding::PqFull] {
        let img = render(enc, 1920, 1080);
        assert_eq!(img.get(80, 950), Rgb10::grey(enc.black()));
        assert_eq!(img.get(1839, 950), Rgb10::grey(enc.white()));
        let row: Vec<u16> = (80..1840).map(|x| img.get(x, 950).r).collect();
        assert!(row.windows(2).all(|p| p[0] <= p[1]));
    }
}

#[test]
fn every_pixel_is_neutral() {
    for (w, h) in [(1920, 1080), (3840, 2160), (640, 360)] {
        let img = render(Encoding::PqNarrow, w, h);
        assert!(img.pixels().iter().all(|&p| {
            let c = Rgb10::unpack(p);
            c.r == c.g && c.g == c.b
        }));
    }
}

#[test]
fn patches_cover_every_pixel_value_except_ramp() {
    let enc = Encoding::PqFull;
    let img = render(enc, 1920, 1080);
    let known = level_values(enc);
    for y in (0..1080).filter(|y| !(900..1000).contains(y)) {
        for x in 0..1920 {
            assert!(known.contains(&img.get(x, y).pack()), "({x}, {y})");
        }
    }
}
