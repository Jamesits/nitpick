use super::*;

fn sdr(transfer: SdrTransfer) -> Sdr {
    Sdr {
        transfer,
        white_nits: 80.0,
    }
}

#[test]
fn srgb_is_linear_near_black() {
    for v in [0.0, 0.0002, 0.001, 0.003] {
        assert!((SdrTransfer::Srgb.encode(v) - 12.92 * v).abs() <= 1e-17);
    }
    assert!((SdrTransfer::Srgb.decode(0.02) - 0.02 / 12.92).abs() <= 1e-18);
}

#[test]
fn srgb_and_gamma22_differ_near_black() {
    // Gamma 2.2: 0.001^(1/2.2) = 0.0432876...; sRGB: 0.01292.
    let g = SdrTransfer::Gamma22.encode(0.001);
    assert!((g - 0.043_287_6).abs() < 1e-7, "{g}");
    assert!((SdrTransfer::Srgb.encode(0.001) - 0.012_92).abs() < 1e-15);
}

#[test]
fn transfers_invert() {
    for t in [SdrTransfer::Srgb, SdrTransfer::Gamma22] {
        for v in [0.0, 0.001, 0.0032, 0.18, 0.5, 1.0] {
            let back = t.decode(t.encode(v));
            assert!((back - v).abs() <= 1e-15, "{t:?} {v}: {back}");
        }
    }
}

#[test]
fn sdr_white_is_signal_one() {
    for t in [SdrTransfer::Srgb, SdrTransfer::Gamma22] {
        let (s, clip) = encode_sdr(sdr(t), 80.0);
        assert!((s - 1.0).abs() <= 1e-15, "{t:?}: {s}");
        assert_eq!(clip, Clip::None);
    }
}

#[test]
fn sdr_clips_below_black_and_above_white() {
    let s = sdr(SdrTransfer::Gamma22);
    assert_eq!(encode_sdr(s, -1e-9), (0.0, Clip::Low));
    assert_eq!(encode_sdr(s, 0.0), (0.0, Clip::None));
    assert_eq!(encode_sdr(s, 80.001), (1.0, Clip::High));
}

#[test]
fn pq_clips_at_zero_and_at_the_peak() {
    assert_eq!(encode_pq(-1e-9), (0.0, Clip::Low));
    // Not the inverse EOTF of 0, which is above 0.
    assert_eq!(encode_pq(0.0), (0.0, Clip::None));
    assert!(encode_pq(1e-12).0 > 7e-7);
    assert_eq!(encode_pq(10_000.0), (1.0, Clip::None));
    assert_eq!(encode_pq(10_000.001), (1.0, Clip::High));
}
