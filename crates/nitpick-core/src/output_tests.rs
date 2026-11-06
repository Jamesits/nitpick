use super::*;
use crate::pattern::PatternKind;
use crate::signal::{Encoding, Format};

#[test]
fn pack_layout_is_r_low() {
    let v = pack_r10g10b10a2(Rgb::new(1, 2, 3)).unwrap();
    assert_eq!(v, 0xC000_0000 | (3 << 20) | (2 << 10) | 1);
}

/// `R10G10B10A2_UNORM` bytes of one pixel, set bit by bit from the format
/// definition: R in bits 0..10, G in 10..20, B in 20..30, A in 30..32 of a
/// little-endian 32-bit word. Independent of [`pack_r10g10b10a2`].
fn expected_r10g10b10a2(r: u16, g: u16, b: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    let mut set = |bit: usize| out[bit / 8] |= 1 << (bit % 8);
    for (offset, v) in [(0, r), (10, g), (20, b), (30, 0b11)] {
        let width = if offset == 30 { 2 } else { 10 };
        for i in 0..width {
            if v >> i & 1 == 1 {
                set(offset + i);
            }
        }
    }
    out
}

#[test]
fn expected_r10g10b10a2_matches_hand_values() {
    // Black is alpha only; white sets every bit.
    assert_eq!(expected_r10g10b10a2(0, 0, 0), [0x00, 0x00, 0x00, 0xC0]);
    assert_eq!(expected_r10g10b10a2(1023, 1023, 1023), [0xFF; 4]);
    // R = 0x3FF: low byte and bits 8..10. G = 1: bit 10. B = 1: bit 20.
    assert_eq!(expected_r10g10b10a2(1023, 0, 0), [0xFF, 0x03, 0x00, 0xC0]);
    assert_eq!(expected_r10g10b10a2(0, 1, 0), [0x00, 0x04, 0x00, 0xC0]);
    assert_eq!(expected_r10g10b10a2(0, 0, 1), [0x00, 0x00, 0x10, 0xC0]);
}

#[test]
fn every_10_bit_code_packs_in_every_channel() {
    for c in 0..=1023 {
        for (rgb, want) in [
            (Rgb::new(c, 0, 0), expected_r10g10b10a2(c, 0, 0)),
            (Rgb::new(0, c, 0), expected_r10g10b10a2(0, c, 0)),
            (Rgb::new(0, 0, c), expected_r10g10b10a2(0, 0, c)),
            (Rgb::grey(c), expected_r10g10b10a2(c, c, c)),
        ] {
            assert_eq!(
                pack_r10g10b10a2(rgb).unwrap().to_le_bytes(),
                want,
                "{rgb:?}"
            );
            let bytes = hdr10_bytes(&Image::new(1, 1, rgb)).unwrap();
            assert_eq!(bytes, want, "{rgb:?}");
        }
    }
}

#[test]
fn asymmetric_channels_keep_their_order() {
    for (r, g, b) in [
        (1, 2, 3),
        (3, 2, 1),
        (1023, 0, 512),
        (0, 1023, 1),
        (64, 940, 512),
        (0x2AA, 0x155, 0x3C3),
    ] {
        let bytes = hdr10_bytes(&Image::new(1, 1, Rgb::new(r, g, b))).unwrap();
        assert_eq!(bytes, expected_r10g10b10a2(r, g, b), "{r} {g} {b}");
    }
}

#[test]
fn alpha_is_opaque_for_every_code() {
    for c in 0..=1023 {
        for rgb in [Rgb::grey(c), Rgb::new(c, 1023 - c, c / 2)] {
            assert_eq!(pack_r10g10b10a2(rgb).unwrap() >> 30, 0b11, "{rgb:?}");
        }
    }
}

#[test]
fn hdr10_rows_are_contiguous_and_row_major() {
    // Odd width: no row padding in the CPU buffer. A unique value per pixel
    // shows a transposed or shifted row at the row boundary.
    let (w, h) = (5, 3);
    let mut img = Image::new(w, h, Rgb::grey(0));
    let value = |x: usize, y: usize| {
        let i = u16::try_from(y * w + x).unwrap();
        Rgb::new(i + 1, 100 + i, 1023 - i)
    };
    for y in 0..h {
        for x in 0..w {
            img.set(x, y, value(x, y));
        }
    }
    let bytes = hdr10_bytes(&img).unwrap();
    assert_eq!(bytes.len(), w * h * 4);
    for (row, chunk) in bytes.chunks_exact(w * 4).enumerate() {
        for (x, px) in chunk.as_chunks::<4>().0.iter().enumerate() {
            let c = value(x, row);
            assert_eq!(*px, expected_r10g10b10a2(c.r, c.g, c.b), "({x}, {row})");
        }
    }
}

#[test]
fn equal_codes_give_equal_pixels_under_every_encoding() {
    // The scope encodings keep the full-range PQ transport: narrow-range
    // codes are not rescaled, so code 64 stays 64 on the wire.
    let codes = [
        Rgb::grey(0),
        Rgb::grey(64),
        Rgb::grey(940),
        Rgb::grey(1023),
        Rgb::new(64, 940, 512),
    ];
    for depth in BitDepth::ALL {
        let mut img = Image::new(codes.len(), 1, Rgb::grey(0));
        for (x, &c) in codes.iter().enumerate() {
            img.set(
                x,
                0,
                Rgb::new(
                    c.r << depth.shift(),
                    c.g << depth.shift(),
                    c.b << depth.shift(),
                ),
            );
        }
        let want = match depth {
            BitDepth::Ten => codes
                .iter()
                .flat_map(|c| expected_r10g10b10a2(c.r, c.g, c.b))
                .collect(),
            BitDepth::Twelve => scrgb_bytes(&img, depth).unwrap(),
        };
        for enc in Encoding::ALL {
            let got = swap_chain_bytes(&img, Format::new(enc, depth)).unwrap();
            assert_eq!(got, want, "{enc:?} {depth:?}");
        }
    }
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
    let bytes = scrgb_bytes(&img, BitDepth::Twelve).unwrap();
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

#[test]
fn codes_above_the_depth_maximum_are_rejected() {
    for c in [
        Rgb::new(1024, 0, 0),
        Rgb::new(0, 1024, 0),
        Rgb::new(0, 0, 0xffff),
    ] {
        assert_eq!(pack_r10g10b10a2(c), None, "{c:?}");
    }
    assert!(pack_r10g10b10a2(Rgb::grey(1023)).is_some());

    // The first bad pixel in row-major order is reported, after a valid one.
    let mut img = Image::new(3, 2, Rgb::grey(1023));
    img.set(2, 1, Rgb::new(0, 0, 4096));
    img.set(1, 0, Rgb::new(0, 1024, 0));
    let err = hdr10_bytes(&img).unwrap_err();
    assert_eq!(
        err,
        CodeRangeError {
            x: 1,
            y: 0,
            code: Rgb::new(0, 1024, 0),
            depth: BitDepth::Ten,
        }
    );
    assert_eq!(
        err.to_string(),
        "pixel (1, 0) code (0, 1024, 0) exceeds 1023 for 10-bit"
    );

    // 1024 is valid at 12 bits; 4096 is not.
    let err = scrgb_bytes(&img, BitDepth::Twelve).unwrap_err();
    assert_eq!((err.x, err.y, err.depth), (2, 1, BitDepth::Twelve));
    for enc in Encoding::ALL {
        for depth in BitDepth::ALL {
            assert!(swap_chain_bytes(&img, Format::new(enc, depth)).is_err());
        }
    }
}
