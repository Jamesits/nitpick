use super::*;

#[test]
fn pq_reference_white_matches_bt2111_3() {
    // BT.2111-3 Tables 3 and 4: 58 % PQ bars (203 cd/m²) are 573 narrow, 594 full.
    assert_eq!(Encoding::PqNarrow.pq_code_from_nits(203.0), Some(573));
    assert_eq!(Encoding::PqFull.pq_code_from_nits(203.0), Some(594));
}

#[test]
fn pq_end_points() {
    assert_eq!(Encoding::PqNarrow.pq_code_from_nits(0.0), Some(64));
    assert_eq!(Encoding::PqNarrow.pq_code_from_nits(10000.0), Some(940));
    assert_eq!(Encoding::PqFull.pq_code_from_nits(0.0), Some(0));
    assert_eq!(Encoding::PqFull.pq_code_from_nits(10000.0), Some(1023));
    assert_eq!(Encoding::HlgNarrow.pq_code_from_nits(100.0), None);
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
fn hlg_75_percent_is_721() {
    assert_eq!(Encoding::HlgNarrow.code_from_signal(0.75), 721);
}

#[test]
fn code_clamps_to_permitted_range() {
    assert_eq!(Encoding::PqNarrow.code_from_signal(-1.0), 4);
    assert_eq!(Encoding::PqNarrow.code_from_signal(2.0), 1019);
    assert_eq!(Encoding::PqFull.code_from_signal(-1.0), 0);
    assert_eq!(Encoding::PqFull.code_from_signal(2.0), 1023);
}

#[test]
fn pack_layout_is_r_low() {
    let v = Rgb10::new(1, 2, 3).pack();
    assert_eq!(v, 0xC000_0000 | (3 << 20) | (2 << 10) | 1);
    assert_eq!(Rgb10::unpack(v), Rgb10::new(1, 2, 3));
}

#[test]
fn ycbcr_grey_is_neutral() {
    let c = ycbcr_narrow(Rgb10::grey(414), Encoding::PqNarrow);
    assert_eq!(
        c,
        YCbCr10 {
            y: 414,
            cb: 512,
            cr: 512
        }
    );
    let c = ycbcr_narrow(Rgb10::grey(1023), Encoding::PqFull);
    assert_eq!(
        c,
        YCbCr10 {
            y: 940,
            cb: 512,
            cr: 512
        }
    );
}

#[test]
fn ycbcr_100_percent_primaries() {
    // BT.2020 NCL, 10-bit narrow: red Y = 64 + 876 * 0.2627 = 294, Cr = 960.
    let red = ycbcr_narrow(Rgb10::new(940, 64, 64), Encoding::PqNarrow);
    assert_eq!(
        red,
        YCbCr10 {
            y: 294,
            cb: 387,
            cr: 960
        }
    );
    let blue = ycbcr_narrow(Rgb10::new(64, 64, 940), Encoding::PqNarrow);
    assert_eq!(blue.cb, 960);
}

#[test]
fn encoding_ids_round_trip() {
    for e in Encoding::ALL {
        assert_eq!(e.id().parse::<Encoding>(), Ok(e));
    }
    assert!("x".parse::<Encoding>().is_err());
}
