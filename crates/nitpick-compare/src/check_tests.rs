use std::cmp::Ordering;

use half::f16;
use nitpick_fixtures::{CASE_GROUPS, Case};

use super::*;

fn all_cases() -> Vec<Case> {
    CASE_GROUPS
        .iter()
        .flat_map(|g| nitpick_fixtures::group(g).unwrap().cases)
        .collect()
}

fn nits(value: [f64; 3], bound: [f64; 3]) -> Expectation {
    Expectation::new(Space::Bt2020Nits, value, bound).unwrap()
}

fn failures(r: &PatchReport) -> &[Failure] {
    match &r.verdict {
        Verdict::Fail(f) => f,
        v => panic!("not a failure: {v:?}"),
    }
}

#[test]
fn expectations_need_finite_values_and_bounds() {
    let v = [100.0; 3];
    assert!(Expectation::new(Space::ScRgb, [f64::NAN, 0.0, 0.0], [0.0; 3]).is_err());
    for b in [-1e-9, f64::NAN, f64::INFINITY] {
        let got = Expectation::new(Space::ScRgb, v, [0.0, b, 0.0]);
        assert!(
            matches!(got, Err(Invalid::Bound(x)) if x.to_bits() == b.to_bits()),
            "{got:?}"
        );
    }
    let e = Expectation::new(Space::ScRgb, v, [0.0, 1.0, 2.0]).unwrap();
    assert_eq!(
        (e.space(), e.value(), e.bound()),
        (Space::ScRgb, v, [0.0, 1.0, 2.0])
    );
}

#[test]
fn samples_within_the_bounds_pass() {
    let e = nits([100.0, 50.0, 10.0], [0.0625; 3]);
    let r = check_patch(&e, &[[100.0625, 49.9375, 10.0], [100.0, 50.0, 10.03125]]);
    assert_eq!(r.verdict, Verdict::Pass);
    assert_eq!(r.worst, Some([0.0625, -0.0625, 0.03125]));
    let c = r.component.unwrap();
    assert_eq!((c[2].max, c[2].median), (0.031_25, 0.015_625));
    let de = r.delta_e_itp.unwrap().unwrap();
    assert!(de.max > 0.0 && de.max < PERCEPTUAL_TARGET, "{de:?}");
    assert_eq!(r.precision_limit, None);
}

#[test]
fn a_component_above_its_bound_fails_with_its_sign() {
    let e = nits([100.0; 3], [0.5; 3]);
    let r = check_patch(&e, &[[100.0, 99.0, 100.2], [100.0, 99.8, 100.0]]);
    assert_eq!(
        failures(&r),
        [Failure::Component {
            channel: 1,
            error: -1.0,
            bound: 0.5
        }]
    );
}

#[test]
fn a_perceptual_difference_above_the_target_fails() {
    let e = nits([100.0; 3], [0.2; 3]);
    assert!(e.quantization_delta_e_itp().unwrap() < PERCEPTUAL_TARGET);
    let r = check_patch(&e, &[[110.0, 100.0, 100.0]]);
    let [
        Failure::Component { channel: 0, .. },
        Failure::Perceptual { delta_e_itp, limit },
    ] = failures(&r)
    else {
        panic!("{r:?}");
    };
    assert!(*delta_e_itp > *limit && *limit == PERCEPTUAL_TARGET);
}

#[test]
fn quantization_above_the_target_is_reported_as_a_limit() {
    // Bound box of a few percent: quantization alone exceeds the target.
    let e = nits([100.0; 3], [3.0; 3]);
    let q = e.quantization_delta_e_itp().unwrap();
    assert!(q > PERCEPTUAL_TARGET, "{q}");
    // A corner sample is at the limit: a pass with the limit attached.
    let r = check_patch(&e, &[[103.0, 97.0, 103.0]]);
    assert_eq!(r.verdict, Verdict::Pass);
    assert_eq!(r.precision_limit, Some(q));
    // The limit does not suppress a component failure.
    let r = check_patch(&e, &[[103.5, 100.0, 100.0]]);
    assert!(matches!(
        failures(&r),
        [Failure::Component { channel: 0, .. }]
    ));
    assert_eq!(r.precision_limit, Some(q));
}

#[test]
fn a_failed_check_keeps_its_expectation() {
    let e = nits([100.0; 3], [0.1; 3]);
    let samples = [[101.0, 100.0, 100.0]];
    let first = check_patch(&e, &samples);
    let second = check_patch(&e, &samples);
    assert_eq!(first, second);
    assert_eq!(first.expectation, e);
    assert_eq!(e.bound(), [0.1; 3]);
    assert!(matches!(first.verdict, Verdict::Fail(_)));
}

#[test]
fn non_finite_samples_fail() {
    let e = nits([100.0; 3], [1.0; 3]);
    for v in [f64::NAN, f64::INFINITY] {
        let r = check_patch(&e, &[[100.0; 3], [100.0, v, 100.0]]);
        assert_eq!(failures(&r), [Failure::NonFinite { sample: 1 }]);
        assert_eq!((r.worst, r.component, r.delta_e_itp), (None, None, None));
    }
}

#[test]
fn a_patch_without_samples_is_unavailable() {
    let r = check_patch(&nits([1.0; 3], [0.0; 3]), &[]);
    assert!(matches!(r.verdict, Verdict::Unavailable(_)));
}

/// A sample outside the metric domain keeps its component errors; the
/// metric is reported as invalid, and nothing is clipped.
#[test]
fn a_sample_outside_the_metric_domain_reports_an_invalid_metric() {
    let e = nits([0.0; 3], [0.01; 3]);
    let r = check_patch(&e, &[[-0.01, 0.0, 0.0]]);
    assert_eq!(r.verdict, Verdict::Pass);
    assert!(matches!(r.delta_e_itp, Some(Err(Invalid::NegativeLms(_)))));
    assert_eq!(r.worst, Some([-0.01, 0.0, 0.0]));
    assert!(r.quantization_delta_e_itp.is_err());
    // A sample inside the domain: the bound box is not, so the
    // perceptual check does not apply.
    let r = check_patch(&e, &[[0.01; 3]]);
    assert_eq!(r.verdict, Verdict::Pass);
    assert!(r.delta_e_itp.unwrap().unwrap().max > PERCEPTUAL_TARGET);
    let r = check_patch(&e, &[[-0.02, 0.0, 0.0]]);
    assert!(matches!(
        failures(&r),
        [Failure::Component { channel: 0, .. }]
    ));
}

fn neighbours(c: f64) -> (f64, f64) {
    let n = f16::from_f64(c);
    let v = n.to_f64();
    match v.total_cmp(&c) {
        Ordering::Equal => (v, v),
        Ordering::Less => (v, next(n, 1).to_f64()),
        Ordering::Greater => (next(n, -1).to_f64(), v),
    }
}

/// The binary16 value `k` steps from `v` in value order; +0 and -0 are
/// one value.
fn next(v: f16, k: i32) -> f16 {
    let bits = v.to_bits();
    let magnitude = i32::from(bits & 0x7fff);
    let ordered = if bits & 0x8000 == 0 {
        magnitude
    } else {
        -magnitude
    } + k;
    let magnitude = u16::try_from(ordered.unsigned_abs()).unwrap();
    f16::from_bits(if ordered < 0 {
        0x8000 | magnitude
    } else {
        magnitude
    })
}

/// Binary64 spacing at `c`.
fn ulp(c: f64) -> f64 {
    (c.abs().max(f64::MIN_POSITIVE)).next_up() - c.abs().max(f64::MIN_POSITIVE)
}

/// The capture bound is the farther binary16 neighbour of the ideal value.
fn capture_bound(c: f64) -> f64 {
    let (lo, hi) = neighbours(c);
    (c - lo).max(hi - c)
}

/// The FP16 search bound: the farthest value within 2 steps of the
/// nearest binary16 value.
fn search_bound(c: f64) -> f64 {
    let base = f16::from_f64(c);
    (-2..=2)
        .map(|k| (next(base, k).to_f64() - c).abs())
        .fold(0.0, f64::max)
}

fn assert_bound(stored: f64, derived: f64, c: f64, what: &str) {
    // The stored bound also covers the binary64 rounding of `c`, and both
    // values are rounded to binary64.
    assert!(
        derived <= stored && stored <= derived + ulp(c) + 2.0 * ulp(stored),
        "{what}: stored {stored}, derived {derived}"
    );
}

/// The stored bounds derive from binary16 neighbours alone. A bound that
/// was widened by hand does not match.
#[test]
fn fixture_bounds_derive_from_binary16_neighbours() {
    let mut search = 0;
    for case in all_cases() {
        let depth = case.input.format.depth;
        for (i, p) in case.expected.palette.iter().enumerate() {
            let what = format!("{} entry {i}", case.input.id);
            for c in 0..3 {
                let ideal = p.qrhi.scrgb[c];
                let stored = p.qrhi.scrgb_bound[c];
                if depth == 10 {
                    assert_bound(stored, capture_bound(ideal), ideal, &what);
                } else {
                    let envelope = search_bound(ideal);
                    assert_bound(stored, envelope, ideal, &what);
                    // Counted once: no second binary16 rounding on top.
                    let (lo, hi) = neighbours(ideal);
                    if hi > lo {
                        assert!(stored < envelope + (hi - lo) / 2.0, "{what}");
                    }
                    search += 1;
                }
                if let (Some(s), Some(b)) = (p.browser.scrgb, p.browser.scrgb_bound) {
                    assert_bound(b[c], capture_bound(s[c]), s[c], &what);
                }
            }
            assert_eq!(
                p.browser.scrgb_bound.is_none(),
                case.input.format.encoding == "hlg-narrow",
                "{what}"
            );
        }
    }
    assert!(search > 0);
}

/// The ideal value passes; at 10 bits, a capture at the farther binary16
/// neighbour passes and one step beyond fails.
#[test]
fn fixture_limits_accept_quantization_and_reject_more() {
    for case in all_cases() {
        for (i, p) in case.expected.palette.iter().enumerate() {
            let what = format!("{} entry {i}", case.input.id);
            let e = Expectation::new(Space::ScRgb, p.qrhi.scrgb, p.qrhi.scrgb_bound).unwrap();
            assert!(
                !matches!(check_patch(&e, &[p.qrhi.scrgb]).verdict, Verdict::Fail(_)),
                "{what}"
            );
            if case.input.format.depth != 10 {
                continue;
            }
            // The farther neighbour, and the direction away from the ideal.
            let far: [(f64, i32); 3] = std::array::from_fn(|c| {
                let (lo, hi) = neighbours(p.qrhi.scrgb[c]);
                if p.qrhi.scrgb[c] - lo > hi - p.qrhi.scrgb[c] {
                    (lo, -1)
                } else {
                    (hi, 1)
                }
            });
            let beyond = far.map(|(v, k)| next(f16::from_f64(v), k).to_f64());
            let far = far.map(|(v, _)| v);
            let r = check_patch(&e, &[far]);
            assert!(
                r.worst
                    .unwrap()
                    .iter()
                    .zip(e.bound())
                    .all(|(w, b)| w.abs() <= b),
                "{what}: {r:?}"
            );
            let r = check_patch(&e, &[beyond]);
            let fails = matches!(&r.verdict, Verdict::Fail(f) if f.iter().any(|f| matches!(f, Failure::Component { .. })));
            assert!(fails, "{what}: {r:?}");
        }
    }
}

/// Quantization alone stays within the perceptual target at every fixture
/// entry where the metric is defined. It is undefined only where the FP16
/// search range reaches below 0 next to black.
#[test]
fn fixture_quantization_is_within_the_target() {
    for case in all_cases() {
        let depth = case.input.format.depth;
        for (i, p) in case.expected.palette.iter().enumerate() {
            let paths = [
                Some((p.qrhi.scrgb, p.qrhi.scrgb_bound, depth == 12)),
                p.browser
                    .scrgb
                    .zip(p.browser.scrgb_bound)
                    .map(|(v, b)| (v, b, false)),
            ];
            for (v, b, search) in paths.into_iter().flatten() {
                let e = Expectation::new(Space::ScRgb, v, b).unwrap();
                match e.quantization_delta_e_itp() {
                    Ok(q) => assert!(q <= PERCEPTUAL_TARGET, "{} entry {i}: {q}", case.input.id),
                    Err(err) => assert!(search, "{} entry {i}: {err}", case.input.id),
                }
            }
        }
    }
}
