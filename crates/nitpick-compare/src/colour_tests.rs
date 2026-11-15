use super::*;

/// Within half a unit of the last printed decimal.
fn assert_printed(got: f64, printed: f64, decimals: i32, what: &str) {
    let half = 0.5 * 10f64.powi(-decimals);
    assert!(
        (got - printed).abs() <= half,
        "{what}: {got} is not {printed} to {decimals} decimals"
    );
}

fn assert_rel(got: f64, want: f64, rel: f64, what: &str) {
    assert!(
        (got - want).abs() <= rel * want.abs().max(1e-300),
        "{what}: {got} != {want}"
    );
}

#[test]
fn pq_endpoints() {
    assert_eq!(pq_eotf(0.0), Ok(0.0));
    assert_eq!(pq_eotf(1.0), Ok(10000.0));
    assert_rel(pq_inverse_eotf(10000.0).unwrap(), 1.0, 1e-15, "peak");
    // The inverse EOTF of 0 is not 0: $c_1^{m_2}$.
    assert_rel(pq_inverse_eotf(0.0).unwrap(), C1.powf(M2), 1e-15, "black");
}

#[test]
fn pq_matches_the_reference_tables() {
    let scalar = nitpick_fixtures::scalar().unwrap();
    for t in &scalar.qrhi {
        for (code, (&signal, &nits)) in t.signal.iter().zip(&t.nits_2020).enumerate() {
            let got = pq_eotf(signal).unwrap();
            assert_rel(got, nits, 1e-12, &format!("{}-bit code {code}", t.depth));
            if nits > 0.0 {
                let back = pq_inverse_eotf(nits).unwrap();
                assert_rel(back, signal, 1e-12, &format!("{}-bit code {code}", t.depth));
            }
        }
    }
}

#[test]
fn pq_rejects_values_outside_its_domain() {
    assert_eq!(pq_eotf(-0.001), Err(Invalid::Signal(-0.001)));
    assert_eq!(pq_eotf(1.001), Err(Invalid::Signal(1.001)));
    assert!(matches!(pq_eotf(f64::NAN), Err(Invalid::NonFinite(_))));
    assert_eq!(pq_inverse_eotf(-1e-9), Err(Invalid::Negative(-1e-9)));
    assert!(matches!(
        pq_inverse_eotf(f64::INFINITY),
        Err(Invalid::NonFinite(_))
    ));
}

#[test]
fn pq_inverse_continues_above_the_peak() {
    let above = pq_inverse_eotf(10100.0).unwrap();
    assert!(above > 1.0 && above.is_finite());
}

#[test]
fn bt709_to_bt2020_matches_bt2087() {
    // ITU-R BT.2087-0, printed to 4 decimals; also in BT.2124 Annex 2.
    let printed = [
        [0.6274, 0.3293, 0.0433],
        [0.0691, 0.9195, 0.0114],
        [0.0164, 0.0880, 0.8956],
    ];
    for (i, row) in printed.iter().enumerate() {
        for (j, &p) in row.iter().enumerate() {
            assert_printed(BT709_TO_BT2020[i][j], p, 4, &format!("[{i}][{j}]"));
        }
    }
}

#[test]
fn xyz_to_bt2020_matches_bt2124() {
    // ITU-R BT.2124-0 Annex 2, conversion 1, printed to 15 decimals.
    let printed = [
        [
            1.716_651_187_971_268,
            -0.355_670_783_776_392,
            -0.253_366_281_373_660,
        ],
        [
            -0.666_684_351_832_489,
            1.616_481_236_634_939,
            0.015_768_545_813_911,
        ],
        [
            0.017_639_857_445_311,
            -0.042_770_613_257_809,
            0.942_103_121_235_474,
        ],
    ];
    for (i, row) in printed.iter().enumerate() {
        for (j, &p) in row.iter().enumerate() {
            // Half a printed unit plus the binary64 evaluation error.
            let got = XYZ_TO_BT2020[i][j];
            assert!(
                (got - p).abs() <= 0.5e-15 + 4.0 * f64::EPSILON,
                "[{i}][{j}]: {got}"
            );
        }
    }
}

#[test]
fn primaries_and_white_map_to_unit_vectors() {
    // The BT.2020 primaries in XYZ convert back to unit vectors.
    let to_xyz = rgb_to_xyz(BT2020, D65);
    for k in 0..3 {
        let unit: [f64; 3] = std::array::from_fn(|i| f64::from(u8::from(i == k)));
        let back = apply(&XYZ_TO_BT2020, apply(&to_xyz, unit));
        for (i, v) in back.iter().enumerate() {
            assert!((v - unit[i]).abs() < 1e-15, "primary {k}: {back:?}");
        }
    }
    let white = Nits2020::from_scrgb([1.0; 3]).unwrap().0;
    for v in white {
        assert!((v - 80.0).abs() < 1e-12, "{white:?}");
    }
    // Rows of a primary conversion with one white point sum to 1.
    for row in *BT709_TO_BT2020 {
        assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-15);
    }
}

/// Each fixture palette entry has its ideal scRGB and its linear BT.2020
/// value from the high-precision generator. Wide-gamut entries have
/// negative scRGB components.
#[test]
fn scrgb_matches_the_fixture_vectors() {
    let mut negative = 0;
    for g in nitpick_fixtures::CASE_GROUPS {
        for case in nitpick_fixtures::group(g).unwrap().cases {
            for (i, p) in case.expected.palette.iter().enumerate() {
                let pairs = [
                    Some((p.qrhi.scrgb, p.qrhi.nits_2020)),
                    p.browser.scrgb.zip(p.browser.nits_2020),
                ];
                for (scrgb, nits) in pairs.into_iter().flatten() {
                    negative += usize::from(scrgb.iter().any(|&v| v < 0.0));
                    let got = Nits2020::from_scrgb(scrgb).unwrap().0;
                    let scale = nits.iter().fold(1e-6f64, |m, v| m.max(v.abs()));
                    for c in 0..3 {
                        assert!(
                            (got[c] - nits[c]).abs() <= 1e-13 * scale,
                            "{} entry {i}: {got:?} != {nits:?}",
                            case.input.id
                        );
                    }
                }
            }
        }
    }
    assert!(negative > 0, "no fixture has a negative scRGB component");
}

#[test]
fn spaces_convert_to_bt2020_nits() {
    let v = [0.25, -0.5, 2.0];
    assert_eq!(Space::Bt2020Nits.to_nits_2020(v), Ok(Nits2020(v)));
    assert_eq!(Space::ScRgb.to_nits_2020(v), Nits2020::from_scrgb(v));
    assert!(Space::ScRgb.to_nits_2020([f64::NAN, 0.0, 0.0]).is_err());
    assert!(Nits2020::new([0.0, f64::NEG_INFINITY, 0.0]).is_err());
    assert!(Nits2020::from_xyz([0.0, 0.0, f64::NAN]).is_err());
}

fn hlg_1000() -> HlgDisplay {
    HlgDisplay::new(1000.0, 0.0, 1.2).unwrap()
}

#[test]
fn hlg_display_values_are_checked() {
    assert!(HlgDisplay::new(0.0, 0.0, 1.2).is_err());
    assert!(HlgDisplay::new(1000.0, -0.1, 1.2).is_err());
    assert!(HlgDisplay::new(1000.0, 1000.0, 1.2).is_err());
    assert!(HlgDisplay::new(1000.0, 0.0, 0.0).is_err());
    assert!(HlgDisplay::new(f64::NAN, 0.0, 1.2).is_err());
    let d = HlgDisplay::new(1000.0, 0.01, 1.2).unwrap();
    assert_eq!(
        (d.peak_nits(), d.black_nits(), d.system_gamma()),
        (1000.0, 0.01, 1.2)
    );
}

#[test]
fn hlg_reference_display_values() {
    let d = hlg_1000();
    // Signal 1 is the peak, 0 is black.
    let white = Nits2020::from_hlg([1.0; 3], &d).unwrap().0;
    assert_rel(white[0], 1000.0, 1e-7, "white");
    assert_eq!(Nits2020::from_hlg([0.0; 3], &d), Ok(Nits2020([0.0; 3])));
    // Signal 1/2 is scene light 1/12: $L_W (1/12)^\gamma$.
    let half = Nits2020::from_hlg([0.5; 3], &d).unwrap().0;
    assert_rel(half[1], 1000.0 * (1.0f64 / 12.0).powf(1.2), 1e-14, "grey");
    // BT.2408: HLG reference white, signal 0.75, is 203 cd/m² on a
    // 1000 cd/m² display.
    let reference = Nits2020::from_hlg([0.75; 3], &d).unwrap().0;
    assert_printed(reference[2], 203.0, 0, "reference white");
    // A colour: the gain comes from the luminance of all three components.
    let red = Nits2020::from_hlg([0.5, 0.0, 0.0], &d).unwrap().0;
    let ys: f64 = 0.2627 / 12.0;
    assert_rel(red[0], 1000.0 * ys.powf(0.2) / 12.0, 1e-14, "red");
    assert_eq!((red[1], red[2]), (0.0, 0.0));
}

#[test]
fn hlg_black_lift_gives_the_black_level() {
    // With $\beta$ from $L_B$, signal 0 shows $L_B$.
    let d = HlgDisplay::new(1000.0, 0.05, 1.2).unwrap();
    let black = Nits2020::from_hlg([0.0; 3], &d).unwrap().0;
    for v in black {
        assert_rel(v, 0.05, 1e-12, "black");
    }
    let white = Nits2020::from_hlg([1.0; 3], &d).unwrap().0;
    assert_rel(white[0], 1000.0, 1e-7, "white");
}

#[test]
fn hlg_rejects_signals_outside_its_domain() {
    let d = hlg_1000();
    assert_eq!(
        Nits2020::from_hlg([1.01, 0.5, 0.5], &d),
        Err(Invalid::Signal(1.01))
    );
    assert!(Nits2020::from_hlg([0.5, f64::NAN, 0.5], &d).is_err());
}
