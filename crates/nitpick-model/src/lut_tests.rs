use super::*;

/// Unequal channels with 5 entries: knots at 0, 1/4, 1/2, 3/4 and 1. All
/// values are exact in binary64, so the expected results are exact.
fn lut() -> Lut {
    Lut::new([
        vec![0.0, 0.25, 0.5, 0.75, 1.0],
        vec![0.125, 0.125, 0.5, 0.5, 0.875],
        vec![1.0, 0.5, 0.25, -0.5, 1.5],
    ])
    .unwrap()
}

#[test]
fn knots_give_the_entries() {
    let l = lut();
    for (k, x) in [0.0, 0.25, 0.5, 0.75, 1.0].into_iter().enumerate() {
        let want = l.channels().clone().map(|c| c[k]);
        assert_eq!(l.apply([x; 3]), want, "knot {k}");
    }
}

#[test]
fn midpoints_interpolate_linearly() {
    let l = lut();
    assert_eq!(l.apply([0.125; 3]), [0.125, 0.125, 0.75]);
    assert_eq!(l.apply([0.625; 3]), [0.625, 0.5, -0.125]);
    // A quarter between the knots 3/4 and 1.
    assert_eq!(l.apply([0.8125; 3]), [0.8125, 0.59375, 0.0]);
}

#[test]
fn input_outside_the_domain_is_clamped() {
    let l = lut();
    assert_eq!(l.apply([-0.5; 3]), l.apply([0.0; 3]));
    assert_eq!(l.apply([1.5; 3]), l.apply([1.0; 3]));
}

#[test]
fn entries_outside_the_unit_range_are_kept() {
    // The model clamps the result, not the LUT.
    assert_eq!(lut().apply([1.0; 3])[2], 1.5);
    assert_eq!(lut().apply([0.75; 3])[2], -0.5);
}

#[test]
fn two_entries_and_4096_entries_are_accepted() {
    let id = Lut::new([vec![0.0, 1.0], vec![0.0, 1.0], vec![0.0, 1.0]]).unwrap();
    assert_eq!(id.apply([0.3, 0.6, 1.0]), [0.3, 0.6, 1.0]);

    // Entry k is k / 4095, so every input gives itself.
    let ramp: Vec<f64> = (0..4096u16).map(|k| f64::from(k) / 4095.0).collect();
    let l = Lut::new([ramp.clone(), ramp.clone(), ramp]).unwrap();
    let x = [1000.0 / 4095.0, 2001.0 / 8190.0, 1.0];
    for (got, want) in l.apply(x).into_iter().zip(x) {
        assert!((got - want).abs() <= 1e-15, "{got} != {want}");
    }
}

#[test]
fn invalid_luts_are_rejected() {
    let ch = |n: usize| vec![0.0; n];
    assert_eq!(
        Lut::new([ch(1), ch(1), ch(1)]),
        Err(ModelError::LutLength(1))
    );
    assert_eq!(
        Lut::new([ch(4097), ch(4097), ch(4097)]),
        Err(ModelError::LutLength(4097))
    );
    assert_eq!(
        Lut::new([ch(2), ch(3), ch(2)]),
        Err(ModelError::LutChannels)
    );
    assert!(matches!(
        Lut::new([ch(2), vec![0.0, f64::NAN], ch(2)]),
        Err(ModelError::NonFinite(_))
    ));
}
