use super::*;
use crate::signal::{D65, Rgb};

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

#[test]
fn light_level_of_a_frame() {
    // Half the frame at code 1023 (10000 cd/m²), half at 0.
    let mut img = Image::new(4, 2, Rgb::grey(0));
    img.fill_rect(0, 0, 4, 1, Rgb::new(1023, 0, 0));
    let l = light_level(&img, BitDepth::Ten);
    assert!((l.max_cll - 10000.0).abs() < 1e-6);
    assert!((l.max_fall - 5000.0).abs() < 1e-6);
}

#[test]
fn primaries_ids_round_trip() {
    for p in MasteringPrimaries::ALL {
        assert_eq!(p.id().parse::<MasteringPrimaries>(), Ok(p));
    }
}
