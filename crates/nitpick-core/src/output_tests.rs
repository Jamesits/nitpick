use super::*;
use crate::pattern::PatternKind;
use crate::signal::{Encoding, Format};

#[test]
fn pack_layout_is_r_low() {
    let v = pack_r10g10b10a2(Rgb::new(1, 2, 3));
    assert_eq!(v, 0xC000_0000 | (3 << 20) | (2 << 10) | 1);
}

#[test]
fn hdr10_bytes_are_little_endian() {
    let img = Image::new(2, 1, Rgb::new(1, 2, 3));
    let bytes = hdr10_bytes(&img);
    assert_eq!(bytes.len(), 8);
    assert_eq!(
        bytes[..4],
        pack_r10g10b10a2(Rgb::new(1, 2, 3)).to_le_bytes()
    );
}

#[test]
fn matrices_are_inverse() {
    let (a, b) = matrices();
    let id = mul(a, b);
    for (i, row) in id.iter().enumerate() {
        for (j, v) in row.iter().enumerate() {
            let want = if i == j { 1.0 } else { 0.0 };
            assert!((v - want).abs() < 1e-12, "{id:?}");
        }
    }
    // BT.2087 rounded BT.709 to BT.2020 first row: 0.6274, 0.3293, 0.0433.
    assert!((b[0][0] - 0.6274).abs() < 1e-4 && (b[0][2] - 0.0433).abs() < 1e-4);
}

#[test]
fn scrgb_white_and_black() {
    // 203 cd/m² grey is 203 / 80 in scRGB; code 0 is 0.
    let c = Format::new(Encoding::PqFull, BitDepth::Twelve)
        .pq_code_from_nits(203.0)
        .unwrap();
    let [r, g, b] = scrgb_from_codes(Rgb::grey(c), BitDepth::Twelve);
    assert_eq!(r, g);
    assert_eq!(g, b);
    assert!((f64::from(f16::from_bits(r)) * 80.0 - 203.0).abs() < 1.0);
    let zero = scrgb_from_codes(Rgb::grey(0), BitDepth::Twelve);
    assert_eq!(codes_from_scrgb(zero, BitDepth::Twelve), Rgb::grey(0));
}

#[test]
fn every_12_bit_grey_round_trips() {
    for c in 0..=4095 {
        let px = scrgb_from_codes(Rgb::grey(c), BitDepth::Twelve);
        assert_eq!(
            codes_from_scrgb(px, BitDepth::Twelve),
            Rgb::grey(c),
            "code {c}"
        );
    }
}

#[test]
fn only_saturated_narrow_range_bars_fail_to_round_trip() {
    // A narrow-range bar has a component at 0 % (code 64 or 256), which is
    // a small positive luminance on the full-range wire. The f16 rounding
    // error of its bright components is larger than one code step there.
    let mut failures = 0;
    for kind in PatternKind::ALL {
        for &enc in kind.encodings() {
            for depth in BitDepth::ALL {
                let f = Format::new(enc, depth);
                for size in [(1920, 1080), (3840, 2160), (7680, 4320)] {
                    for level in kind.model(f, size).unwrap().levels() {
                        if scrgb_is_exact(level.rgb, depth) {
                            continue;
                        }
                        failures += 1;
                        let c = level.rgb;
                        let min = c.r.min(c.g).min(c.b);
                        assert!(
                            f.is_narrow() && min == f.black() && c.max_component() > min,
                            "{f} {} {}",
                            level.section,
                            level.name
                        );
                    }
                }
            }
        }
    }
    assert!(failures > 0);
}

#[test]
fn full_range_and_bt709_levels_round_trip() {
    for depth in BitDepth::ALL {
        for enc in Encoding::ALL {
            let f = Format::new(enc, depth);
            for level in PatternKind::ColourBars
                .model(f, (1920, 1080))
                .unwrap()
                .levels()
            {
                if !f.is_narrow() || level.name.contains("BT.709") {
                    assert!(scrgb_is_exact(level.rgb, depth), "{f} {}", level.name);
                }
            }
        }
    }
}

#[test]
fn scrgb_bytes_layout() {
    let img = Image::new(3, 1, Rgb::grey(0));
    let bytes = scrgb_bytes(&img, BitDepth::Twelve);
    assert_eq!(bytes.len(), 24);
    // R, G, B = 0.0, A = 1.0 (0x3C00).
    assert_eq!(bytes[..8], [0, 0, 0, 0, 0, 0, 0x00, 0x3C]);
}

#[test]
fn ordered_bits_step_through_zero() {
    let one_below_zero = from_ordered(ordered(0) - 1);
    assert!(f16::from_bits(one_below_zero) < f16::ZERO);
    assert_eq!(from_ordered(ordered(one_below_zero) + 1), 0);
}
