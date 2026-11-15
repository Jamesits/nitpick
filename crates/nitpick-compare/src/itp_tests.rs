use super::*;
use crate::colour::pq_eotf;

fn grey(nits: f64) -> Nits2020 {
    Nits2020([nits; 3])
}

fn pq_grey(code: u16, max: u16) -> Nits2020 {
    grey(pq_eotf(f64::from(code) / f64::from(max)).unwrap())
}

#[test]
fn neutral_colours_have_no_chroma() {
    for nits in [0.0, 0.005, 1.0, 100.0, 203.0, 1000.0, 10000.0] {
        let itp = Itp::new(grey(nits)).unwrap();
        assert!(
            itp.t.abs() < 1e-15 && itp.p.abs() < 1e-15,
            "{nits}: {itp:?}"
        );
        let want = pq_inverse_eotf(nits).unwrap();
        assert!((itp.i - want).abs() < 1e-15, "{nits}: {itp:?}");
    }
}

/// For neutral colours `ΔE_ITP = 720 |ΔI|`, and `I` is the PQ signal. So
/// one code step is `720 / (2^n - 1)`.
#[test]
fn neutral_code_steps() {
    for (max, codes) in [
        (1023u16, [1u16, 100, 594, 1022]),
        (4095, [1, 400, 2378, 4094]),
    ] {
        for code in codes {
            let d = delta_e_itp(pq_grey(code, max), pq_grey(code + 1, max)).unwrap();
            let want = 720.0 / f64::from(max);
            assert!((d - want).abs() < 1e-9, "{max}: code {code}: {d}");
        }
    }
}

#[test]
fn black_and_peak_white() {
    assert_eq!(delta_e_itp(grey(0.0), grey(0.0)), Ok(0.0));
    let black_i = (3424.0f64 / 4096.0).powf(2523.0 / 4096.0 * 128.0);
    let d = delta_e_itp(grey(0.0), grey(10000.0)).unwrap();
    assert!((d - 720.0 * (1.0 - black_i)).abs() < 1e-11, "{d}");
}

#[test]
fn the_metric_is_symmetric() {
    let a = Nits2020([12.0, 340.0, 5.0]);
    let b = Nits2020([15.0, 300.0, 9.0]);
    assert_eq!(delta_e_itp(a, b), delta_e_itp(b, a));
}

/// ITU-R BT.2124-0 Annex 4: the 58 % PQ BT.709 blue bar of BT.2111 and a
/// measured XYZ value. Compare at the printed precision only: the example
/// truncates some values and rounds others, so a printed value can be one
/// unit of its last decimal from the full-precision value. The printed `T`
/// and `P` of the expected colour follow from the exact codes.
///
/// **Deviations of the example:**
/// - It applies the EOTF to the normalized codes truncated to 4 decimals.
///   With 296/1023 the red component is 8.758, not 8.753.
/// - The printed `I` of the expected colour, 0.3554, does not follow from
///   the steps: the exact codes, the truncated codes and the printed linear
///   values all give 0.3557. The published 2.363 is step 5 on the printed
///   ITP values; the steps give 2.282.
#[test]
fn bt2124_worked_example() {
    const UNIT: f64 = 1e-4;
    let codes: [f64; 3] = [296.0, 201.0, 582.0];
    let normalized = [0.2893, 0.1964, 0.5689];
    for (c, n) in codes.iter().zip(normalized) {
        // Truncated: 201/1023 = 0.19648.
        assert_eq!((c / 1023.0 * 1e4).floor() / 1e4, n, "{c}");
    }
    let expected = Nits2020::from_pq(normalized).unwrap();
    for (got, (printed, decimals)) in expected.0.iter().zip([(8.753, 3), (2.291, 3), (181.3, 1)]) {
        let half = 0.5 * 10f64.powi(-decimals);
        assert!(
            (got - printed).abs() <= half,
            "linear {got} is not {printed}"
        );
    }
    let exact = Nits2020::from_pq(codes.map(|c| c / 1023.0)).unwrap();
    assert!((exact.0[0] - 8.758).abs() <= 0.000_5, "{exact:?}");

    let measured = Nits2020::from_xyz([36.0, 15.0, 190.0]).unwrap();
    let printed_a = [0.3554, 0.1346, -0.1613];
    let printed_b = [0.3568, 0.1321, -0.1629];
    let steps_a = [0.3557, 0.1346, -0.1613];
    let close = |itp: Itp, printed: [f64; 3]| {
        for (got, want) in [itp.i, itp.t, itp.p].iter().zip(printed) {
            assert!((got - want).abs() < UNIT, "ITP {got} is not {want}");
        }
    };
    close(Itp::new(exact).unwrap(), steps_a);
    // The printed I differs at each input variant.
    for a in [expected, Nits2020([8.753, 2.291, 181.3])] {
        let i = Itp::new(a).unwrap().i;
        assert!((i - steps_a[0]).abs() < UNIT && (i - printed_a[0]).abs() > 2.0 * UNIT);
    }
    let b = Itp::new(measured).unwrap();
    close(b, printed_b);
    assert_eq!(steps_a[1..], printed_a[1..]);

    let itp = |v: [f64; 3]| Itp {
        i: v[0],
        t: v[1],
        p: v[2],
    };
    let from_printed = itp(printed_a).delta_e(itp(printed_b));
    assert!((from_printed - 2.363).abs() <= 0.000_5, "{from_printed}");
    let d = Itp::new(exact).unwrap().delta_e(b);
    assert!((d - 2.282).abs() <= 0.000_5, "{d}");
}

/// A colour outside BT.2020 keeps its negative component: the result
/// differs from the clipped colour.
#[test]
fn negative_rgb_is_not_clipped() {
    let outside = Nits2020([-1.0, 50.0, 50.0]);
    let clipped = Nits2020([0.0, 50.0, 50.0]);
    let d = delta_e_itp(outside, clipped).unwrap();
    assert!(d > 0.1, "{d}");
}

#[test]
fn negative_lms_is_outside_the_domain() {
    let got = Itp::new(Nits2020([-100.0, 0.0, 0.0]));
    assert!(
        matches!(got, Err(Invalid::NegativeLms(lms)) if lms[0] < 0.0),
        "{got:?}"
    );
    assert!(delta_e_itp(Nits2020([-100.0, 0.0, 0.0]), grey(0.0)).is_err());
}

#[test]
fn non_finite_input_is_rejected() {
    for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(matches!(
            Itp::new(Nits2020([1.0, v, 1.0])),
            Err(Invalid::NonFinite(_))
        ));
    }
}

#[test]
fn values_above_the_pq_peak_stay_comparable() {
    let d = delta_e_itp(grey(10000.0), grey(10100.0)).unwrap();
    assert!(d > 0.0 && d.is_finite(), "{d}");
}
