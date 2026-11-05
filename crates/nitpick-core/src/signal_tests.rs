use super::*;

const PQ_N10: Format = Format::new(Encoding::PqNarrow, BitDepth::Ten);
const PQ_F10: Format = Format::new(Encoding::PqFull, BitDepth::Ten);
const PQ_N12: Format = Format::new(Encoding::PqNarrow, BitDepth::Twelve);
const PQ_F12: Format = Format::new(Encoding::PqFull, BitDepth::Twelve);
const HLG_N10: Format = Format::new(Encoding::HlgNarrow, BitDepth::Ten);

#[test]
fn pq_reference_white_matches_bt2111_3() {
    // BT.2111-3 Tables 3 and 4: 58 % PQ bars (203 cd/m²).
    assert_eq!(PQ_N10.pq_code_from_nits(203.0), Some(573));
    assert_eq!(PQ_F10.pq_code_from_nits(203.0), Some(594));
    // The narrow-range 12-bit table value 2292 is 4 × 573; the formula gives 2291.
    assert_eq!(PQ_N12.pq_code_from_nits(203.0), Some(2291));
    assert_eq!(PQ_F12.pq_code_from_nits(203.0), Some(2378));
}

#[test]
fn pq_end_points() {
    assert_eq!(PQ_N10.pq_code_from_nits(0.0), Some(64));
    assert_eq!(PQ_N10.pq_code_from_nits(10000.0), Some(940));
    assert_eq!(PQ_F10.pq_code_from_nits(0.0), Some(0));
    assert_eq!(PQ_F10.pq_code_from_nits(10000.0), Some(1023));
    assert_eq!(PQ_N12.pq_code_from_nits(0.0), Some(256));
    assert_eq!(PQ_N12.pq_code_from_nits(10000.0), Some(3760));
    assert_eq!(PQ_F12.pq_code_from_nits(10000.0), Some(4095));
    assert_eq!(HLG_N10.pq_code_from_nits(100.0), None);
}

#[test]
fn pq_known_values() {
    // Reference values from the ST 2084 formula: 100 cd/m² -> 0.5081, 1000 cd/m² -> 0.7518.
    assert!((pq_inverse_eotf(100.0) - 0.508_078).abs() < 1e-5);
    assert!((pq_inverse_eotf(1000.0) - 0.751_827).abs() < 1e-5);
}

#[test]
fn pq_round_trip() {
    for nits in [0.005, 0.1, 1.0, 100.0, 203.0, 1000.0, 4000.0, 10000.0] {
        let back = pq_eotf(pq_inverse_eotf(nits));
        assert!((back - nits).abs() / nits < 1e-9, "{nits} -> {back}");
    }
}

#[test]
fn hlg_75_percent() {
    // BT.2111-3 Table 2.
    assert_eq!(HLG_N10.code_from_signal(0.75), 721);
    let hlg12 = Format::new(Encoding::HlgNarrow, BitDepth::Twelve);
    assert_eq!(hlg12.code_from_signal(0.75), 2884);
}

#[test]
fn code_clamps_to_permitted_range() {
    // BT.2100 Table 9.
    assert_eq!(PQ_N10.code_from_signal(-1.0), 4);
    assert_eq!(PQ_N10.code_from_signal(2.0), 1019);
    assert_eq!(PQ_F10.code_from_signal(-1.0), 0);
    assert_eq!(PQ_F10.code_from_signal(2.0), 1023);
    assert_eq!(PQ_N12.code_limits(), (16, 4079));
    assert_eq!(PQ_F12.code_limits(), (0, 4095));
}

#[test]
fn signal_round_trip_at_both_depths() {
    for f in [PQ_N10, PQ_F10, PQ_N12, PQ_F12] {
        for code in [f.black(), f.white(), 500] {
            assert_eq!(f.code_from_signal(f.signal_from_code(code)), code, "{f}");
        }
    }
}

#[test]
fn ycbcr_grey_is_neutral() {
    let c = ycbcr_narrow(Rgb::grey(414), PQ_N10);
    assert_eq!(
        c,
        YCbCr {
            y: 414,
            cb: 512,
            cr: 512
        }
    );
    let c = ycbcr_narrow(Rgb::grey(1023), PQ_F10);
    assert_eq!(
        c,
        YCbCr {
            y: 940,
            cb: 512,
            cr: 512
        }
    );
    let c = ycbcr_narrow(Rgb::grey(4095), PQ_F12);
    assert_eq!(
        c,
        YCbCr {
            y: 3760,
            cb: 2048,
            cr: 2048
        }
    );
}

#[test]
fn ycbcr_100_percent_primaries() {
    // BT.2020 NCL, 10-bit narrow: red Y = 64 + 876 * 0.2627 = 294, Cr = 960.
    let red = ycbcr_narrow(Rgb::new(940, 64, 64), PQ_N10);
    assert_eq!(
        red,
        YCbCr {
            y: 294,
            cb: 387,
            cr: 960
        }
    );
    let blue = ycbcr_narrow(Rgb::new(64, 64, 940), PQ_N10);
    assert_eq!(blue.cb, 960);
    // 12-bit: Y = 256 + 3504 * 0.2627 = 1176.50 -> 1177, Cr = 16 * (128 + 112) = 3840.
    let red = ycbcr_narrow(Rgb::new(3760, 256, 256), PQ_N12);
    assert_eq!((red.y, red.cr), (1177, 3840));
}

#[test]
fn ids_round_trip() {
    for e in Encoding::ALL {
        assert_eq!(e.id().parse::<Encoding>(), Ok(e));
    }
    for d in BitDepth::ALL {
        assert_eq!(d.id().parse::<BitDepth>(), Ok(d));
    }
    assert!("x".parse::<Encoding>().is_err());
    assert!("11".parse::<BitDepth>().is_err());
}
