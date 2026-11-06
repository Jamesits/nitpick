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
    // A grey needs no matrix and no search: the stored bits are the f16
    // rounding of the reference luminance / 80. The smallest distance of
    // a grey to an f16 rounding midpoint is 1e-5 f16 steps (code 1894),
    // far above the f64 error.
    for c in 0..=4095 {
        let px = scrgb_from_codes(Rgb::grey(c), BitDepth::Twelve);
        let want = f16::from_f64(ref_pq_eotf(f64::from(c) / 4095.0) / 80.0).to_bits();
        assert_eq!(px, [want; 3], "code {c}");
        assert_eq!(ref_decode(px, BitDepth::Twelve), [c; 3], "code {c}");
        assert_eq!(
            codes_from_scrgb(px, BitDepth::Twelve),
            Rgb::grey(c),
            "code {c}"
        );
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

// Independent FP16 reference. The vectors and tables below come from a
// 60-digit evaluation (mpmath) of the specification values: ST 2084
// constants, BT.709 / BT.2020 primaries and D65 without adaptation. That
// evaluation does not use the production conversion. The test-local
// helpers repeat only the specification formulas in f64; the vectors check
// them.

/// ST 2084 constants as exact fractions.
const REF_M1: f64 = 2610.0 / 16384.0;
const REF_M2: f64 = 2523.0 / 4096.0 * 128.0;
const REF_C1: f64 = 3424.0 / 4096.0;
const REF_C2: f64 = 2413.0 / 4096.0 * 32.0;
const REF_C3: f64 = 2392.0 / 4096.0 * 32.0;

/// Linear BT.2020 to linear BT.709, both with D65 white.
const REF_TO_709: Matrix = [
    [
        1.660_491_002_108_434_4,
        -0.587_641_138_788_549_5,
        -0.072_849_863_319_884_88,
    ],
    [
        -0.124_550_474_521_590_74,
        1.132_899_897_125_960_2,
        -0.008_349_422_604_369_477,
    ],
    [
        -0.018_150_763_354_905_304,
        -0.100_578_898_008_007_38,
        1.118_729_661_362_912_7,
    ],
];

/// Linear BT.709 to linear BT.2020, both with D65 white.
const REF_TO_2020: Matrix = [
    [
        0.627_403_895_934_699_1,
        0.329_283_038_377_883_7,
        0.043_313_065_687_417_227,
    ],
    [
        0.069_097_289_358_232_08,
        0.919_540_395_075_458_8,
        0.011_362_315_566_309_177,
    ],
    [
        0.016_391_438_875_150_28,
        0.088_013_307_877_225_76,
        0.895_595_253_247_624,
    ],
];

/// Native (unadapted) RGB to XYZ with D65 white.
const REF_XYZ_2020: Matrix = [
    [
        0.636_958_048_301_291_3,
        0.144_616_903_586_208_37,
        0.168_880_975_164_172_06,
    ],
    [
        0.262_700_212_011_267_03,
        0.677_998_071_518_871,
        0.059_301_716_469_861_946,
    ],
    [0.0, 0.028_072_693_049_087_508, 1.060_985_057_710_791],
];
const REF_XYZ_709: Matrix = [
    [
        0.412_390_799_265_959_5,
        0.357_584_339_383_878,
        0.180_480_788_401_834_3,
    ],
    [
        0.212_639_005_871_510_36,
        0.715_168_678_767_755_9,
        0.072_192_315_360_733_71,
    ],
    [
        0.019_330_818_715_591_85,
        0.119_194_779_794_626,
        0.950_532_152_249_660_6,
    ],
];

type Px = [u16; 3];
type ScrgbVector = (BitDepth, Px, [f64; 3], Px, bool);
type SearchCase = (BitDepth, Px, Px, Px, Px, Px);
type TieCase = (BitDepth, Px, [i32; 3], &'static [[i32; 3]]);
type InexactLevel = (&'static str, &'static str, &'static str, Px, Px);

fn ref_pq_eotf(e: f64) -> f64 {
    let p = e.powf(1.0 / REF_M2);
    ((p - REF_C1).max(0.0) / (REF_C2 - REF_C3 * p)).powf(1.0 / REF_M1) * 10000.0
}

fn ref_pq_inverse_eotf(nits: f64) -> f64 {
    let y = (nits / 10000.0).powf(REF_M1);
    ((REF_C1 + REF_C2 * y) / (1.0 + REF_C3 * y)).powf(REF_M2)
}

fn ref_mul(m: &Matrix, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

/// Plain f16 rounding of the reference scRGB value, without the search.
fn ref_base(c: [u16; 3], depth: BitDepth) -> [u16; 3] {
    let max = f64::from(depth.max_code());
    let nits = c.map(|v| ref_pq_eotf(f64::from(v) / max));
    ref_mul(&REF_TO_709, nits).map(|v| f16::from_f64(v / 80.0).to_bits())
}

/// Reference decoder: f16 · 80 cd/m², BT.709 to BT.2020, PQ inverse EOTF,
/// round half away from zero, negative luminance to code 0.
fn ref_decode(px: [u16; 3], depth: BitDepth) -> [u16; 3] {
    let max = f64::from(depth.max_code());
    let lin = px.map(|b| f16::from_bits(b).to_f64() * 80.0);
    ref_mul(&REF_TO_2020, lin).map(|nits| {
        let code = (ref_pq_inverse_eotf(nits.max(0.0)) * max).round();
        assert!((0.0..=max).contains(&code), "{px:?}");
        // In range by the assertion.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let code = code as u16;
        code
    })
}

/// f16 bits as a signed step count from zero. +0 and -0 are one value.
fn ref_key(v: u16) -> i32 {
    let m = i32::from(v & 0x7fff);
    if v & 0x8000 == 0 { m } else { -m }
}

/// Signed distance from `a` to `b` in f16 steps.
fn ref_steps(a: u16, b: u16) -> i32 {
    ref_key(b) - ref_key(a)
}

fn ref_offset(px: [u16; 3], d: [i32; 3]) -> [u16; 3] {
    std::array::from_fn(|i| {
        let v = ref_key(px[i]) + d[i];
        let m = u16::try_from(v.unsigned_abs()).unwrap();
        if v < 0 { 0x8000 | m } else { m }
    })
}

fn max_error(got: [u16; 3], want: [u16; 3]) -> u16 {
    (0..3).map(|i| got[i].abs_diff(want[i])).max().unwrap()
}

fn production(c: [u16; 3], depth: BitDepth) -> [u16; 3] {
    scrgb_from_codes(Rgb::new(c[0], c[1], c[2]), depth)
}

fn assert_close(got: f64, want: f64, rel: f64, what: &str) {
    assert!(
        (got - want).abs() <= rel * want.abs(),
        "{what}: {got} != {want}"
    );
}

#[test]
fn pq_matches_reference_vectors() {
    use BitDepth::{Ten, Twelve};
    for (depth, code, nits) in [
        (Ten, 1, 0.000_040_422_717_645_864_9),
        (Ten, 64, 0.100_853_509_639_412_73),
        (Ten, 520, 100.229_885_531_179_9),
        (Ten, 940, 4_654.397_320_430_553),
        (Ten, 1023, 10_000.0),
        (Twelve, 1, 0.000_003_684_877_640_945_711_6),
        (Twelve, 256, 0.100_678_662_033_548_06),
        (Twelve, 2081, 100.101_964_803_435_8),
        (Twelve, 3760, 4_625.300_239_546_207),
        (Twelve, 4095, 10_000.0),
    ] {
        let e = f64::from(code) / f64::from(depth.max_code());
        let what = format!("{depth} code {code}");
        assert_close(pq_eotf(e), nits, 1e-12, &what);
        assert_close(ref_pq_eotf(e), nits, 1e-12, &what);
    }
    assert!(pq_eotf(0.0) == 0.0 && ref_pq_eotf(0.0) == 0.0);
    for (nits, e) in [
        (0.005, 0.015_076_399_042_368_021),
        (80.0, 0.485_856_765_388_678_94),
        (100.0, 0.508_078_421_517_394_9),
        (203.0, 0.580_688_881_041_607_8),
        (1000.0, 0.751_827_096_247_041_8),
        (10_000.0, 1.0),
    ] {
        let what = format!("{nits} cd/m²");
        assert_close(pq_inverse_eotf(nits), e, 1e-12, &what);
        assert_close(ref_pq_inverse_eotf(nits), e, 1e-12, &what);
    }
}

#[test]
fn native_d65_matrices_match_primary_and_white_vectors() {
    // Exact specification chromaticities, not rounded EDID coordinates.
    assert_eq!(D65, [0.3127, 0.3290]);
    assert_eq!(BT709_PRIMARIES, [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]]);
    assert_eq!(
        BT2020_PRIMARIES,
        [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]]
    );
    let d65_xyz = [0.3127 / 0.3290, 1.0, (1.0 - 0.3127 - 0.3290) / 0.3290];
    for (prim, want) in [
        (BT709_PRIMARIES, REF_XYZ_709),
        (BT2020_PRIMARIES, REF_XYZ_2020),
    ] {
        let m = rgb_to_xyz(prim, D65);
        for i in 0..3 {
            for j in 0..3 {
                assert!((m[i][j] - want[i][j]).abs() < 1e-12, "{m:?}");
            }
        }
        for (j, [x, y]) in prim.iter().enumerate() {
            let mut v = [0.0; 3];
            v[j] = 1.0;
            let [cx, cy, cz] = mul_vec(&m, v);
            let sum = cx + cy + cz;
            assert!((cx / sum - x).abs() < 1e-12 && (cy / sum - y).abs() < 1e-12);
        }
        // RGB white is the native D65 white with Y = 1, not ICC D50
        // (0.9642, 1, 0.8249) or another assumed monitor white.
        let w = mul_vec(&m, [1.0; 3]);
        for i in 0..3 {
            assert!((w[i] - d65_xyz[i]).abs() < 1e-12, "{w:?}");
        }
        assert!((w[0] - 0.9642).abs() > 0.01 && (w[2] - 0.8249).abs() > 0.1);
    }

    let (to_709, to_2020) = matrices();
    for i in 0..3 {
        for j in 0..3 {
            assert!((to_709[i][j] - REF_TO_709[i][j]).abs() < 1e-12);
            assert!((to_2020[i][j] - REF_TO_2020[i][j]).abs() < 1e-12);
        }
    }
    // White stays white. A BT.2020 green is outside BT.709: negative R, B.
    for v in mul_vec(to_709, [1.0; 3]) {
        assert!((v - 1.0).abs() < 1e-12);
    }
    let g = mul_vec(to_709, [0.0, 1.0, 0.0]);
    assert!(g[0] < 0.0 && g[1] > 1.0 && g[2] < 0.0, "{g:?}");
}

#[test]
fn scrgb_matches_reference_vectors() {
    use BitDepth::{Ten, Twelve};
    // (depth, codes, reference scRGB value before f16 rounding, expected
    // stored bits, search used). For codes (0, 1, 0) the plain rounding of
    // R and B is -0; the search writes +0 for an unchanged zero.
    #[rustfmt::skip]
    let vectors: [ScrgbVector; 11] = [
        (Twelve, [4095, 4095, 4095], [125.0; 3], [0x57D0; 3], false),
        (Twelve, [4095, 0, 0], [207.561_375_263_554_3, -15.568_809_315_198_843, -2.268_845_419_363_163], [0x5A7C, 0xCBC9, 0xC08A], false),
        (Twelve, [0, 0, 4095], [-9.106_232_914_985_61, -1.043_677_825_546_184_6, 139.841_207_670_364_1], [0xC88E, 0xBC2D, 0x585F], false),
        (Twelve, [1000, 2000, 3000], [-1.267_700_110_617_149, 1.067_550_191_419_169, 11.617_210_624_667_92], [0xBD12, 0x3C45, 0x49CF], false),
        (Twelve, [3000, 2000, 1000], [16.790_513_167_982_577, -0.143_050_343_404_255_68, -0.226_985_511_991_155_8], [0x4C32, 0xB094, 0xB343], true),
        (Twelve, [0, 1, 0], [-2.706_732_116_527_252_4e-8, 5.218_246_875_436_435e-8, -4.632_761_655_258_32e-9], [0x8001, 0x0001, 0x0000], true),
        (Twelve, [2081, 0, 0], [2.077_730_148_118_504_4, -0.155_846_840_210_143_8, -0.022_711_588_431_352_79], [0x4027, 0xB0FD, 0xA5D0], true),
        (Ten, [1023, 0, 0], [207.561_375_263_554_3, -15.568_809_315_198_843, -2.268_845_419_363_163], [0x5A7C, 0xCBC9, 0xC08A], false),
        (Ten, [0, 0, 1023], [-9.106_232_914_985_61, -1.043_677_825_546_184_6, 139.841_207_670_364_1], [0xC88E, 0xBC2D, 0x585F], false),
        (Ten, [200, 500, 800], [-1.759_231_810_659_453_7, 1.025_477_011_292_073_6, 18.338_533_643_557_264], [0xBF0A, 0x3C1A, 0x4C96], true),
        (Ten, [800, 500, 200], [26.766_534_848_747_01, -0.886_855_501_418_368_4, -0.371_222_966_913_951_1], [0x4EB1, 0xBB18, 0xB5F1], false),
    ];
    for (depth, c, lin, want, searched) in vectors {
        let what = format!("{depth} {c:?}");
        let rounded = lin.map(|v| f16::from_f64(v).to_bits());
        assert_eq!(ref_base(c, depth), rounded, "{what}");
        let got = production(c, depth);
        assert_eq!(got, want, "{what}");
        assert_eq!(ref_decode(got, depth), c, "{what}");
        assert_eq!(searched, got != rounded, "{what}");
        for i in 0..3 {
            let v = f16::from_bits(got[i]);
            assert!(v.is_finite(), "{what}");
            assert!(
                v == f16::ZERO || v.is_sign_negative() == (lin[i] < 0.0),
                "{what}"
            );
            assert!(ref_steps(rounded[i], got[i]).abs() <= SEARCH, "{what}");
        }

        let img = Image::new(1, 1, Rgb::new(c[0], c[1], c[2]));
        let px: Vec<u8> = [want[0], want[1], want[2], f16::ONE.to_bits()]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(scrgb_bytes(&img, depth).unwrap(), px, "{what}");
    }
}

#[test]
fn scrgb_search_selects_reference_bits() {
    use BitDepth::{Ten, Twelve};
    // Every BT.2111 level whose plain f16 rounding does not decode to its
    // codes: (depth, codes, plain rounding, selected bits, decoded plain
    // rounding, decoded selection). The 60-digit evaluation applies the
    // search rule of `scrgb_from_codes`. Decoded values that differ from
    // the codes are the residual errors.
    #[rustfmt::skip]
    let cases: [SearchCase; 41] = [
        (Ten, [0, 594, 0], [0xBDF6, 0x41BF, 0xB415], [0xBDF7, 0x41BF, 0xB415], [8, 594, 0], [0, 594, 0]),
        (Ten, [0, 1023, 0], [0xD497, 0x586D, 0xCA49], [0xD497, 0x586C, 0xCA49], [164, 1023, 94], [0, 1023, 0]),
        (Ten, [0, 1023, 1023], [0xD529, 0x5865, 0x57F4], [0xD52A, 0x5865, 0x57F4], [170, 1023, 1023], [0, 1023, 1023]),
        (Ten, [64, 64, 721], [0xB8B7, 0xAC41, 0x488A], [0xB8B8, 0xAC41, 0x488A], [70, 65, 721], [64, 64, 721]),
        (Ten, [64, 64, 940], [0xC43D, 0xB7C1, 0x5411], [0xC43B, 0xB7BF, 0x5410], [0, 52, 940], [73, 59, 940]),
        (Ten, [64, 573, 64], [0xBCE3, 0x40B8, 0xB2A8], [0xBCE2, 0x40B7, 0xB2A8], [67, 573, 66], [66, 573, 62]),
        (Ten, [64, 573, 573], [0xBD7E, 0x40AF, 0x403D], [0xBD7E, 0x40AF, 0x403B], [68, 573, 573], [64, 573, 573]),
        (Ten, [64, 721, 64], [0xC4C4, 0x4898, 0xBA84], [0xC4C2, 0x4897, 0xBA83], [12, 721, 60], [83, 721, 57]),
        (Ten, [64, 721, 721], [0xC55B, 0x4890, 0x4821], [0xC559, 0x488E, 0x481F], [82, 721, 721], [67, 721, 721]),
        (Ten, [64, 940, 64], [0xD046, 0x541F, 0xC5DA], [0xD047, 0x541F, 0xC5DA], [138, 940, 83], [0, 940, 75]),
        (Ten, [64, 940, 940], [0xD0CE, 0x5417, 0x5368], [0xD0D0, 0x5419, 0x5369], [0, 940, 940], [63, 940, 940]),
        (Ten, [573, 64, 64], [0x42EA, 0xB420, 0xA8AC], [0x42E9, 0xB420, 0xA8AC], [573, 67, 65], [573, 64, 64]),
        (Ten, [573, 64, 573], [0x429C, 0xB468, 0x4095], [0x429D, 0xB468, 0x4095], [573, 61, 573], [573, 64, 573]),
        (Ten, [573, 573, 64], [0x4078, 0x4033, 0xB3DE], [0x4079, 0x4033, 0xB3DE], [573, 573, 63], [573, 573, 64]),
        (Ten, [721, 64, 64], [0x4ABC, 0xBC09, 0xB0AC], [0x4ABA, 0xBC08, 0xB0AA], [721, 66, 63], [721, 63, 64]),
        (Ten, [721, 64, 721], [0x4A70, 0xBC4E, 0x4877], [0x4A70, 0xBC4E, 0x4875], [721, 69, 721], [721, 65, 721]),
        (Ten, [721, 721, 64], [0x485A, 0x4817, 0xBBB2], [0x4858, 0x4817, 0xBBB1], [721, 721, 60], [721, 721, 64]),
        (Ten, [940, 64, 940], [0x55C6, 0xC7BB, 0x5401], [0x55C6, 0xC7BB, 0x53FF], [940, 82, 940], [940, 63, 940]),
        (Ten, [940, 940, 64], [0x53CD, 0x5355, 0xC6E8], [0x53CF, 0x5355, 0xC6E8], [940, 940, 31], [940, 940, 64]),
        (Ten, [1023, 0, 1023], [0x5A34, 0xCC27, 0x584D], [0x5A33, 0xCC27, 0x584D], [1023, 122, 1023], [1023, 0, 1023]),
        (Twelve, [0, 2378, 0], [0xBDF7, 0x41C0, 0xB416], [0xBDF8, 0x41C0, 0xB416], [46, 2378, 0], [0, 2378, 0]),
        (Twelve, [0, 4095, 0], [0xD497, 0x586D, 0xCA49], [0xD497, 0x586C, 0xCA49], [657, 4095, 375], [0, 4095, 0]),
        (Twelve, [0, 4095, 4095], [0xD529, 0x5865, 0x57F4], [0xD52A, 0x5865, 0x57F4], [680, 4095, 4095], [0, 4095, 4095]),
        (Twelve, [256, 256, 2884], [0xB8B2, 0xAC3C, 0x4884], [0xB8B4, 0xAC3D, 0x4886], [247, 253, 2884], [251, 258, 2885]),
        (Twelve, [256, 2292, 256], [0xBCDE, 0x40B3, 0xB2A1], [0xBCDD, 0x40B2, 0xB2A0], [259, 2292, 262], [257, 2292, 258]),
        (Twelve, [256, 2292, 2292], [0xBD79, 0x40AA, 0x4039], [0xBD7B, 0x40AC, 0x403B], [228, 2292, 2292], [250, 2293, 2293]),
        (Twelve, [256, 2884, 256], [0xC4BE, 0x4893, 0xBA7C], [0xC4BC, 0x4891, 0xBA7A], [313, 2884, 278], [300, 2883, 247]),
        (Twelve, [256, 2884, 2884], [0xC555, 0x488A, 0x481C], [0xC553, 0x4889, 0x481A], [0, 2884, 2884], [281, 2884, 2883]),
        (Twelve, [256, 3760, 256], [0xD03F, 0x5418, 0xC5D0], [0xD03E, 0x5417, 0xC5CF], [413, 3760, 335], [381, 3760, 226]),
        (Twelve, [908, 588, 2808], [0xB6E6, 0xAA5A, 0x479B], [0xB6E6, 0xAA59, 0x479B], [907, 588, 2808], [908, 588, 2808]),
        (Twelve, [2292, 256, 256], [0x42E3, 0xB41C, 0xA8A8], [0x42E4, 0xB41D, 0xA8A8], [2292, 263, 256], [2292, 256, 257]),
        (Twelve, [2292, 256, 2292], [0x4295, 0xB463, 0x4091], [0x4297, 0xB464, 0x4090], [2292, 254, 2292], [2292, 256, 2292]),
        (Twelve, [2378, 0, 0], [0x4437, 0xB50F, 0xA9E5], [0x4436, 0xB50F, 0xA9E5], [2378, 0, 31], [2378, 0, 0]),
        (Twelve, [2378, 0, 2378], [0x4408, 0xB566, 0x4196], [0x4407, 0xB566, 0x4196], [2378, 40, 2378], [2378, 0, 2378]),
        (Twelve, [2378, 2378, 0], [0x4172, 0x411E, 0xB4D2], [0x4171, 0x411E, 0xB4D2], [2378, 2378, 11], [2378, 2378, 0]),
        (Twelve, [2884, 256, 256], [0x4AB4, 0xBC04, 0xB0A6], [0x4AB5, 0xBC05, 0xB0A6], [2884, 280, 259], [2884, 250, 262]),
        (Twelve, [2884, 256, 2884], [0x4A68, 0xBC49, 0x4871], [0x4A68, 0xBC49, 0x4872], [2884, 246, 2884], [2884, 254, 2884]),
        (Twelve, [2884, 2884, 256], [0x4855, 0x4812, 0xBBA8], [0x4854, 0x4812, 0xBBA8], [2884, 2884, 267], [2884, 2884, 256]),
        (Twelve, [3760, 256, 3760], [0x55BD, 0xC7AF, 0x53F4], [0x55BD, 0xC7AF, 0x53F3], [3760, 279, 3760], [3760, 250, 3760]),
        (Twelve, [3760, 3760, 256], [0x53C1, 0x534A, 0xC6DD], [0x53BF, 0x534A, 0xC6DD], [3760, 3760, 330], [3760, 3760, 260]),
        (Twelve, [4095, 0, 4095], [0x5A34, 0xCC27, 0x584D], [0x5A33, 0xCC27, 0x584D], [4095, 490, 4095], [4095, 0, 4095]),
    ];
    for (depth, c, base, sel, base_dec, sel_dec) in cases {
        let what = format!("{depth} {c:?}");
        assert_eq!(ref_base(c, depth), base, "{what}");
        assert_eq!(production(c, depth), sel, "{what}");
        for i in 0..3 {
            assert!(ref_steps(base[i], sel[i]).abs() <= SEARCH, "{what}");
        }
        assert_eq!(ref_decode(base, depth), base_dec, "{what}");
        assert_eq!(ref_decode(sel, depth), sel_dec, "{what}");
        assert_ne!(base_dec, c, "{what}: no search needed");
        assert!(max_error(sel_dec, c) <= max_error(base_dec, c), "{what}");
    }
}

#[test]
fn scrgb_search_breaks_ties_by_offset_order() {
    use BitDepth::{Ten, Twelve};
    // (depth, codes, selected offset, other offsets with the same code
    // error and step distance).
    let cases: [TieCase; 4] = [
        (Twelve, [2378, 0, 0], [-1, 0, 0], &[[0, -1, 0], [0, 0, -1]]),
        (Twelve, [908, 588, 2808], [0, 1, 0], &[[1, 0, 0]]),
        (Ten, [721, 721, 64], [-2, 0, 1], &[[0, -1, 2]]),
        (Ten, [0, 594, 0], [-1, 0, 0], &[[0, -1, 0]]),
    ];
    let dist = |d: [i32; 3]| d.iter().map(|v| v.abs()).sum::<i32>();
    for (depth, c, chosen, others) in cases {
        let what = format!("{depth} {c:?}");
        let base = ref_base(c, depth);
        let sel = ref_offset(base, chosen);
        assert_eq!(production(c, depth), sel, "{what}");
        let err = max_error(ref_decode(sel, depth), c);
        for &d in others {
            assert!(chosen < d, "{what}");
            assert_eq!(dist(d), dist(chosen), "{what}");
            let alt = ref_decode(ref_offset(base, d), depth);
            assert_eq!(max_error(alt, c), err, "{what} {d:?}");
        }
    }
}

/// Every BT.2111 level that does not decode exactly, at every size:
/// (format, section, name, codes, decoded codes). All are saturated
/// narrow-range bars.
#[rustfmt::skip]
const INEXACT_LEVELS: [InexactLevel; 36] = [
    ("HLG narrow range, 10-bit", "100% colour bars", "100% blue", [64, 64, 940], [73, 59, 940]),
    ("HLG narrow range, 10-bit", "100% colour bars", "100% cyan", [64, 940, 940], [63, 940, 940]),
    ("HLG narrow range, 10-bit", "100% colour bars", "100% green", [64, 940, 64], [0, 940, 75]),
    ("HLG narrow range, 10-bit", "100% colour bars", "100% magenta", [940, 64, 940], [940, 63, 940]),
    ("HLG narrow range, 10-bit", "100% colour bars", "100% red", [940, 64, 64], [940, 68, 69]),
    ("HLG narrow range, 10-bit", "75% colour bars", "75% cyan", [64, 721, 721], [67, 721, 721]),
    ("HLG narrow range, 10-bit", "75% colour bars", "75% green", [64, 721, 64], [83, 721, 57]),
    ("HLG narrow range, 10-bit", "75% colour bars", "75% magenta", [721, 64, 721], [721, 65, 721]),
    ("HLG narrow range, 10-bit", "75% colour bars", "75% red", [721, 64, 64], [721, 63, 64]),
    ("HLG narrow range, 12-bit", "100% colour bars", "100% blue", [256, 256, 3760], [261, 262, 3760]),
    ("HLG narrow range, 12-bit", "100% colour bars", "100% cyan", [256, 3760, 3760], [0, 3760, 3760]),
    ("HLG narrow range, 12-bit", "100% colour bars", "100% green", [256, 3760, 256], [381, 3760, 226]),
    ("HLG narrow range, 12-bit", "100% colour bars", "100% magenta", [3760, 256, 3760], [3760, 250, 3760]),
    ("HLG narrow range, 12-bit", "100% colour bars", "100% red", [3760, 256, 256], [3760, 273, 276]),
    ("HLG narrow range, 12-bit", "100% colour bars", "100% yellow", [3760, 3760, 256], [3760, 3760, 260]),
    ("HLG narrow range, 12-bit", "75% colour bars", "75% blue", [256, 256, 2884], [251, 258, 2885]),
    ("HLG narrow range, 12-bit", "75% colour bars", "75% cyan", [256, 2884, 2884], [281, 2884, 2883]),
    ("HLG narrow range, 12-bit", "75% colour bars", "75% green", [256, 2884, 256], [300, 2883, 247]),
    ("HLG narrow range, 12-bit", "75% colour bars", "75% magenta", [2884, 256, 2884], [2884, 254, 2884]),
    ("HLG narrow range, 12-bit", "75% colour bars", "75% red", [2884, 256, 256], [2884, 250, 262]),
    ("PQ narrow range, 10-bit", "100% colour bars", "100% blue", [64, 64, 940], [73, 59, 940]),
    ("PQ narrow range, 10-bit", "100% colour bars", "100% cyan", [64, 940, 940], [63, 940, 940]),
    ("PQ narrow range, 10-bit", "100% colour bars", "100% green", [64, 940, 64], [0, 940, 75]),
    ("PQ narrow range, 10-bit", "100% colour bars", "100% magenta", [940, 64, 940], [940, 63, 940]),
    ("PQ narrow range, 10-bit", "100% colour bars", "100% red", [940, 64, 64], [940, 68, 69]),
    ("PQ narrow range, 10-bit", "58% colour bars", "58% green", [64, 573, 64], [66, 573, 62]),
    ("PQ narrow range, 12-bit", "100% colour bars", "100% blue", [256, 256, 3760], [261, 262, 3760]),
    ("PQ narrow range, 12-bit", "100% colour bars", "100% cyan", [256, 3760, 3760], [0, 3760, 3760]),
    ("PQ narrow range, 12-bit", "100% colour bars", "100% green", [256, 3760, 256], [381, 3760, 226]),
    ("PQ narrow range, 12-bit", "100% colour bars", "100% magenta", [3760, 256, 3760], [3760, 250, 3760]),
    ("PQ narrow range, 12-bit", "100% colour bars", "100% red", [3760, 256, 256], [3760, 273, 276]),
    ("PQ narrow range, 12-bit", "100% colour bars", "100% yellow", [3760, 3760, 256], [3760, 3760, 260]),
    ("PQ narrow range, 12-bit", "58% colour bars", "58% blue", [256, 256, 2292], [255, 256, 2292]),
    ("PQ narrow range, 12-bit", "58% colour bars", "58% cyan", [256, 2292, 2292], [250, 2293, 2293]),
    ("PQ narrow range, 12-bit", "58% colour bars", "58% green", [256, 2292, 256], [257, 2292, 258]),
    ("PQ narrow range, 12-bit", "58% colour bars", "58% red", [2292, 256, 256], [2292, 256, 257]),
];

#[test]
fn bt2111_levels_have_reference_residuals() {
    let mut seen = std::collections::HashSet::new();
    for kind in PatternKind::ALL {
        for &enc in kind.encodings() {
            for depth in BitDepth::ALL {
                let f = Format::new(enc, depth);
                let format = f.to_string();
                for size in [(1920, 1080), (3840, 2160), (7680, 4320)] {
                    for level in kind.model(f, size).unwrap().levels() {
                        let Rgb { r, g, b } = level.rgb;
                        let c = [r, g, b];
                        let what = format!("{f} {} {}", level.section, level.name);
                        let sel = production(c, depth);
                        let base = ref_base(c, depth);
                        for i in 0..3 {
                            assert!(f16::from_bits(sel[i]).is_finite(), "{what}");
                            assert!(ref_steps(base[i], sel[i]).abs() <= SEARCH, "{what}");
                        }
                        let got = ref_decode(sel, depth);
                        let base_err = max_error(ref_decode(base, depth), c);
                        assert!(max_error(got, c) <= base_err, "{what}");
                        let key = (format.as_str(), level.section, level.name, c);
                        if let Some(i) = INEXACT_LEVELS
                            .iter()
                            .position(|e| (e.0, e.1, e.2, e.3) == key)
                        {
                            seen.insert(i);
                            assert_eq!(got, INEXACT_LEVELS[i].4, "{what}");
                            assert!(!scrgb_is_exact(level.rgb, depth), "{what}");
                            assert!(f.is_narrow() && r.min(g).min(b) == f.black(), "{what}");
                        } else {
                            assert_eq!(got, c, "{what}");
                            assert!(scrgb_is_exact(level.rgb, depth), "{what}");
                        }
                    }
                }
            }
        }
    }
    assert_eq!(seen.len(), INEXACT_LEVELS.len());
}
