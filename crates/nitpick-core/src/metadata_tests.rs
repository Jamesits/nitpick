use super::*;
use crate::signal::D65;

fn p3_1000() -> Hdr10Metadata {
    Hdr10Metadata {
        primaries: MasteringPrimaries::P3D65.primaries(),
        white: D65,
        max_mastering_nits: 1000.0,
        min_mastering_nits: 0.0001,
        max_cll: 1000.0,
        max_fall: 400.0,
    }
}

#[test]
fn st2086_units() {
    assert_eq!(Hdr10Metadata::chromaticity_code(0.3127), 15635);
    assert_eq!(Hdr10Metadata::chromaticity_code(2.0), 50000);
    assert_eq!(Hdr10Metadata::luminance_code(1000.0), 10_000_000);
    assert_eq!(Hdr10Metadata::luminance_code(0.0001), 1);
    assert_eq!(Hdr10Metadata::light_level_code(1e9), 65535);
}

#[test]
fn mastering_display_bytes_are_gbr_big_endian() {
    let b = p3_1000().mastering_display_bytes();
    let u16_at = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
    // Green first: P3 green x 0.265 -> 13250, y 0.690 -> 34500.
    assert_eq!((u16_at(0), u16_at(2)), (13250, 34500));
    // Blue, then red.
    assert_eq!((u16_at(4), u16_at(6)), (7500, 3000));
    assert_eq!((u16_at(8), u16_at(10)), (34000, 16000));
    // White point.
    assert_eq!((u16_at(12), u16_at(14)), (15635, 16450));
    let u32_at = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    assert_eq!(u32_at(16), 10_000_000);
    assert_eq!(u32_at(20), 1);
}

#[test]
fn content_light_level_bytes() {
    assert_eq!(
        p3_1000().content_light_level_bytes(),
        [0x03, 0xE8, 0x01, 0x90]
    );
}

// Expected luminances come from the ST 2084 EOTF evaluated with 40-digit
// arithmetic (mpmath), not from `pq_eotf`.
const NITS_940_OF_1023: f64 = 4_654.397_320_430_553;
const NITS_0_75: f64 = 983.377_855_587_097_7;
const NITS_0_5: f64 = 92.245_708_994_064_08;
const NITS_0_25: f64 = 5.154_176_009_833_053;

fn assert_level(l: LightLevel, max_cll: f64, max_fall: f64) {
    assert!((l.max_cll - max_cll).abs() < 1e-9, "MaxCLL {}", l.max_cll);
    assert!(
        (l.max_fall - max_fall).abs() < 1e-9,
        "MaxFALL {}",
        l.max_fall
    );
}

fn uniform(e: f64, n: usize) -> impl Iterator<Item = [f64; 3]> {
    std::iter::repeat_n([e; 3], n)
}

#[test]
fn code_940_full_range_pq() {
    // QRhi: D / (2^n - 1) at 10 bits.
    let e = 940.0 / 1023.0;
    assert_level(
        light_level(uniform(e, 6)),
        NITS_940_OF_1023,
        NITS_940_OF_1023,
    );
}

#[test]
fn code_940_narrow_range_mapped_to_png_peak() {
    // Browser: (940 - 64) / 876 = 1 gives the PNG sample 65535.
    let v = (65535.0 * ((940.0 - 64.0) / 876.0_f64)).round();
    assert_level(light_level(uniform(v / 65535.0, 6)), 10000.0, 10000.0);
}

#[test]
fn unequal_channels_use_the_maximum() {
    for px in [[0.75, 0.5, 0.25], [0.25, 0.75, 0.5], [0.5, 0.25, 0.75]] {
        assert_level(light_level([px]), NITS_0_75, NITS_0_75);
    }
}

#[test]
fn mixed_pixels() {
    let px = [
        [0.5, 0.0, 0.0],
        [0.75, 0.25, 0.0],
        [0.0, 0.0, 0.5],
        [0.25; 3],
    ];
    let mean = (NITS_0_75 + 2.0 * NITS_0_5 + NITS_0_25) / 4.0;
    assert_level(light_level(px), NITS_0_75, mean);
    // Interleaved values: the cached EOTF result must not leak between
    // pixels.
    let px = [[0.5; 3], [0.75; 3], [0.5; 3], [0.25; 3]];
    assert_level(light_level(px), NITS_0_75, mean);
}

#[test]
fn half_peak_half_black() {
    let px = uniform(1.0, 4).chain(uniform(0.0, 4));
    assert_level(light_level(px), 10000.0, 5000.0);
}

#[test]
fn out_of_range_samples_are_clamped() {
    assert_level(light_level([[1.5, -0.5, 0.0]]), 10000.0, 10000.0);
    assert_level(light_level([[-0.5; 3]]), 0.0, 0.0);
}

#[test]
fn empty_frame() {
    assert_level(light_level(std::iter::empty()), 0.0, 0.0);
}

#[test]
fn primaries_ids_round_trip() {
    for p in MasteringPrimaries::ALL {
        assert_eq!(p.id().parse::<MasteringPrimaries>(), Ok(p));
    }
}
