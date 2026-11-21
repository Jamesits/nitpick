use nitpick_fixtures::profile::{
    self, ClipState, IddCalibration, ModelCalibration, ModelCase, ModelEncoding, ModelExpected,
    ModelSpace, ProfileManifest,
};

use super::*;

fn manifest() -> ProfileManifest {
    profile::manifest().expect("profile manifest")
}

fn primaries(name: &str) -> Primaries {
    match name {
        "bt709" => Primaries::BT709,
        "bt2020" => Primaries::BT2020,
        _ => panic!("primaries {name}"),
    }
}

fn sdr_transfer(t: profile::SdrTransfer) -> SdrTransfer {
    match t {
        profile::SdrTransfer::Srgb => SdrTransfer::Srgb,
        profile::SdrTransfer::Gamma22 => SdrTransfer::Gamma22,
    }
}

fn source(s: &ModelSpace) -> Source {
    let encoding = match s.encoding {
        ModelEncoding::Linear { nits_per_unit } => SourceEncoding::Linear { nits_per_unit },
        ModelEncoding::Pq => SourceEncoding::Pq,
        ModelEncoding::Sdr {
            transfer,
            white_nits,
        } => SourceEncoding::Sdr(Sdr {
            transfer: sdr_transfer(transfer),
            white_nits,
        }),
    };
    Source {
        primaries: primaries(&s.primaries),
        encoding,
    }
}

fn wire(c: &ModelCase) -> Wire {
    let s = &c.wire.space;
    let encoding = match s.encoding {
        ModelEncoding::Pq => WireEncoding::Pq,
        ModelEncoding::Sdr {
            transfer,
            white_nits,
        } => WireEncoding::Sdr(Sdr {
            transfer: sdr_transfer(transfer),
            white_nits,
        }),
        ModelEncoding::Linear { .. } => panic!("{}: linear wire", c.id),
    };
    Wire {
        primaries: primaries(&s.primaries),
        encoding,
        depth: c.wire.depth,
    }
}

/// The MHC2 values of a profile fixture, read through the profile adapter.
fn profile_calibration(m: &ProfileManifest, id: &str) -> Calibration {
    let bytes = m
        .profiles
        .iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("no fixture {id}"))
        .bytes()
        .expect("fixture bytes");
    let mhc2 = nitpick_profile::read_mhc2(&bytes).unwrap_or_else(|e| panic!("{id}: {e}"));
    let lut = mhc2.lut.clone().map(|l| Lut::new(l).expect("LUT"));
    Calibration::new(mhc2.xyz_matrix(), lut).expect("calibration")
}

fn calibration(m: &ProfileManifest, c: &ModelCalibration) -> Calibration {
    match c {
        ModelCalibration::Profile { profile } => profile_calibration(m, profile),
        ModelCalibration::Idd { idd } => {
            let affine =
                Affine::new(idd.matrix, idd.scalar, idd.offset_nits_per_unit).expect("affine");
            Calibration::idd(
                Some(affine),
                idd.lut.clone().map(|l| Lut::new(l).expect("LUT")),
            )
        }
        ModelCalibration::Inline { matrix, lut } => {
            Calibration::new(*matrix, lut.clone().map(|l| Lut::new(l).expect("LUT")))
                .expect("calibration")
        }
    }
}

fn model(m: &ProfileManifest, c: &ModelCase) -> Model {
    Model::new(source(&c.source), calibration(m, &c.calibration), wire(c))
        .unwrap_or_else(|e| panic!("{}: {e}", c.id))
}

/// Largest error of a stage against the 50-digit reference, relative to the
/// largest component of the vector and at least 1: f64 matrices and
/// `powf` give a few units of roundoff; PQ multiplies a relative error of
/// its base by `m2`, about 79.
const REL: f64 = 1e-12;

fn bound(want: [f64; 3]) -> f64 {
    REL * want.iter().fold(1.0, |a: f64, v| a.max(v.abs()))
}

fn assert_close(id: &str, k: usize, stage: &str, got: [f64; 3], want: [f64; 3]) {
    let b = bound(want);
    for (g, w) in got.iter().zip(want) {
        assert!(
            (g - w).abs() <= b,
            "{id} input {k} {stage}: got {got:?}, want {want:?}"
        );
    }
}

fn state(c: Clip) -> ClipState {
    match c {
        Clip::None => ClipState::None,
        Clip::Low => ClipState::Low,
        Clip::High => ClipState::High,
    }
}

/// A clip state is decided only if the value `before` clipping is farther
/// than the stage bound from each limit. On a limit, f64 roundoff can put
/// the value on either side; the value after clipping is the same.
fn assert_clip(
    what: &str,
    got: [Clip; 3],
    want: [ClipState; 3],
    before: [f64; 3],
    limits: [f64; 2],
) {
    let b = bound(before);
    for ch in 0..3 {
        if limits.iter().all(|l| (before[ch] - l).abs() > b) {
            assert_eq!(state(got[ch]), want[ch], "{what} channel {ch}");
        }
    }
}

fn check(id: &str, k: usize, peak: f64, got: &Prediction, want: &ModelExpected) {
    assert_close(id, k, "source_nits", got.source_nits, want.source_nits);
    assert_close(id, k, "wire_nits", got.wire_nits, want.wire_nits);
    assert_close(id, k, "signal", got.signal, want.signal);
    assert_close(id, k, "lut", got.lut, want.lut);
    assert_close(id, k, "output", got.output, want.output);
    assert_eq!(got.code, want.code, "{id} input {k} code");
    let what = format!("{id} input {k} encode clip");
    assert_clip(
        &what,
        got.encode_clip,
        want.clip.encode,
        want.wire_nits,
        [0.0, peak],
    );
    let what = format!("{id} input {k} output clip");
    assert_clip(
        &what,
        got.output_clip,
        want.clip.output,
        want.lut,
        [0.0, 1.0],
    );
}

#[test]
fn model_matches_reference() {
    let m = manifest();
    assert_ne!(m.model.len(), 0);
    for c in &m.model {
        let model = model(&m, c);
        let peak = match model.wire.encoding {
            WireEncoding::Pq => colour::PQ_PEAK_NITS,
            WireEncoding::Sdr(s) => s.white_nits,
        };
        assert_eq!(c.input.len(), c.expected.len(), "{}", c.id);
        for (k, (input, want)) in c.input.iter().zip(&c.expected).enumerate() {
            let got = model
                .predict(*input)
                .unwrap_or_else(|e| panic!("{} input {k}: {e}", c.id));
            check(&c.id, k, peak, &got, want);
        }
    }
}

fn srgb_80() -> Sdr {
    Sdr {
        transfer: SdrTransfer::Srgb,
        white_nits: 80.0,
    }
}

/// `mhc2_swap_rg` exchanges red and green in the BT.709 basis. A
/// transposed matrix, or the XYZ matrix applied to RGB, gives other colours.
#[test]
fn rgb_exchange_maps_red_to_green() {
    let m = manifest();
    let space = Source {
        primaries: Primaries::BT709,
        encoding: SourceEncoding::Sdr(srgb_80()),
    };
    let wire = Wire {
        primaries: Primaries::BT709,
        encoding: WireEncoding::Sdr(srgb_80()),
        depth: 10,
    };
    let model = Model::new(space, profile_calibration(&m, "mhc2_swap_rg"), wire).unwrap();
    let code = |rgb| model.predict(rgb).unwrap().code;
    assert_eq!(code([1.0, 0.0, 0.0]), [0, 1023, 0]);
    assert_eq!(code([0.0, 1.0, 0.0]), [1023, 0, 0]);
    assert_eq!(code([0.0, 0.0, 1.0]), [0, 0, 1023]);
}

/// The model does not use the ICC TRC of the profile: `mhc2_identity` has
/// the sRGB curve, and a gamma 2.2 wire still gives gamma 2.2 codes.
#[test]
fn icc_trc_is_not_a_model_stage() {
    let m = manifest();
    let scrgb = Source {
        primaries: Primaries::BT709,
        encoding: SourceEncoding::Linear {
            nits_per_unit: 80.0,
        },
    };
    let wire = |transfer| Wire {
        primaries: Primaries::BT709,
        encoding: WireEncoding::Sdr(Sdr {
            transfer,
            white_nits: 80.0,
        }),
        depth: 12,
    };
    // 4095 · 12.92 · 0.001 = 52.9; 4095 · 0.001^(1 / 2.2) = 177.3.
    for (t, code) in [(SdrTransfer::Srgb, 53), (SdrTransfer::Gamma22, 177)] {
        let with_profile = Model::new(scrgb, profile_calibration(&m, "mhc2_identity"), wire(t))
            .unwrap()
            .predict([0.001; 3])
            .unwrap();
        let plain = Model::new(scrgb, Calibration::identity(), wire(t))
            .unwrap()
            .predict([0.001; 3])
            .unwrap();
        assert_eq!(with_profile.code, plain.code, "{t:?}");
        assert_eq!(plain.code, [code; 3], "{t:?}");
    }
}

fn hdr10() -> Wire {
    Wire {
        primaries: Primaries::BT2020,
        encoding: WireEncoding::Pq,
        depth: 10,
    }
}

fn pq_source() -> Source {
    Source {
        primaries: Primaries::BT2020,
        encoding: SourceEncoding::Pq,
    }
}

#[test]
fn invalid_inputs_are_rejected() {
    let model = Model::new(pq_source(), Calibration::identity(), hdr10()).unwrap();
    assert_eq!(model.predict([1.5, 0.0, 0.0]), Err(ModelError::Signal(1.5)));
    assert_eq!(
        model.predict([0.0, -0.1, 0.0]),
        Err(ModelError::Signal(-0.1))
    );
    assert!(matches!(
        model.predict([0.0, 0.0, f64::NAN]),
        Err(ModelError::NonFinite(_))
    ));

    let sdr = Source {
        primaries: Primaries::BT709,
        encoding: SourceEncoding::Sdr(srgb_80()),
    };
    let model = Model::new(sdr, Calibration::identity(), hdr10()).unwrap();
    assert_eq!(
        model.predict([1.25, 0.0, 0.0]),
        Err(ModelError::Signal(1.25))
    );

    let linear = Source {
        primaries: Primaries::BT709,
        encoding: SourceEncoding::Linear {
            nits_per_unit: 80.0,
        },
    };
    let model = Model::new(linear, Calibration::identity(), hdr10()).unwrap();
    assert!(matches!(
        model.predict([f64::MAX, -f64::MAX, 0.0]),
        Err(ModelError::NonFinite(_))
    ));
}

#[test]
fn invalid_spaces_are_rejected() {
    let id = Calibration::identity;
    for nits in [0.0, -80.0, f64::INFINITY] {
        let s = Source {
            primaries: Primaries::BT709,
            encoding: SourceEncoding::Linear {
                nits_per_unit: nits,
            },
        };
        assert_eq!(Model::new(s, id(), hdr10()), Err(ModelError::Scale(nits)));
        let w = Wire {
            encoding: WireEncoding::Sdr(Sdr {
                transfer: SdrTransfer::Srgb,
                white_nits: nits,
            }),
            ..hdr10()
        };
        assert_eq!(
            Model::new(pq_source(), id(), w),
            Err(ModelError::Scale(nits))
        );
    }
    for depth in [0, 17] {
        let w = Wire { depth, ..hdr10() };
        assert_eq!(
            Model::new(pq_source(), id(), w),
            Err(ModelError::Depth(depth))
        );
    }
    let collinear = Primaries {
        rgb: [[0.3, 0.3], [0.4, 0.4], [0.2, 0.2]],
        white: colour::D65,
    };
    let w = Wire {
        primaries: collinear,
        ..hdr10()
    };
    assert_eq!(Model::new(pq_source(), id(), w), Err(ModelError::Primaries));
    assert!(matches!(
        Calibration::new([[f64::NAN, 0.0, 0.0], [0.0; 3], [0.0; 3]], None),
        Err(ModelError::NonFinite(_))
    ));
}

/// A binary32 value of the callback; the fixtures store only such values.
fn f32_of(v: f64) -> f32 {
    #[allow(clippy::cast_possible_truncation)]
    let f = v as f32;
    assert_eq!(f64::from(f), v, "{v} is not a binary32 value");
    f
}

/// The callback records of an IDD fixture case, through the replay.
fn idd_events(c: &ModelCase, idd: &IddCalibration) -> Vec<idd::Event> {
    use idd::{Event, FrameHdrMetadata, FrameRecord, GammaRamp, SurfaceColourSpace, Transform3x4};
    let surface = match c.source.encoding {
        ModelEncoding::Linear { nits_per_unit } if nits_per_unit == colour::SCRGB_WHITE_NITS => {
            SurfaceColourSpace::RgbFullG10P709
        }
        _ => panic!("{}: not an scRGB source", c.id),
    };
    let colour_space = match c.wire.space.encoding {
        ModelEncoding::Pq => idd::WireColourSpace::G2084P2020,
        ModelEncoding::Sdr { .. } => idd::WireColourSpace::G22P709,
        ModelEncoding::Linear { .. } => panic!("{}: linear wire", c.id),
    };
    let lut = idd.lut.as_ref().map(|l| {
        (0..l[0].len())
            .map(|k| [f32_of(l[0][k]), f32_of(l[1][k]), f32_of(l[2][k])])
            .collect()
    });
    let ramp = Transform3x4 {
        matrix_enabled: true,
        matrix: idd.matrix.map(|r| r.map(f32_of)),
        scalar: f32_of(idd.scalar),
        lut_enabled: lut.is_some(),
        lut: lut.unwrap_or_else(|| vec![[0.0; 3]; idd::LUT_LEN]),
    };
    vec![
        Event::CommitMode(idd::WireMode {
            colour_space,
            depth: c.wire.depth,
        }),
        Event::SetGammaRamp(GammaRamp::Transform3x4(ramp)),
        Event::Frame(FrameRecord {
            frame_number: 1,
            surface,
            sdr_white_nits: 80,
            hdr: FrameHdrMetadata::Absent,
            generation: 1,
        }),
    ]
}

/// The replay of callback records gives the reference values of each IDD
/// case: record values, surface and wire colour space and the contract.
#[test]
fn idd_records_match_the_reference() {
    let m = manifest();
    let mut seen = 0;
    for c in &m.model {
        let ModelCalibration::Idd { idd } = &c.calibration else {
            continue;
        };
        seen += 1;
        let wire_sdr = match wire(c).encoding {
            WireEncoding::Sdr(s) => Some(s),
            WireEncoding::Pq => None,
        };
        let contract = idd::IddContract {
            surface_sdr: None,
            wire_sdr,
            wcg_wire_primaries: None,
            offset_nits_per_unit: idd.offset_nits_per_unit,
        };
        let frames = idd::replay(&idd_events(c, idd)).unwrap_or_else(|e| panic!("{}: {e}", c.id));
        let model = frames[0]
            .model(&contract)
            .unwrap_or_else(|e| panic!("{}: {e}", c.id));
        let peak = wire_sdr.map_or(colour::PQ_PEAK_NITS, |s| s.white_nits);
        for (k, (input, want)) in c.input.iter().zip(&c.expected).enumerate() {
            check(&c.id, k, peak, &model.predict(*input).unwrap(), want);
        }
    }
    assert_ne!(seen, 0);
}

/// `mhc2_signed` stores a nonzero fourth column. The MHC2 path ignores it;
/// the same values as an IDD 3×4 matrix add it as an offset.
#[test]
fn mhc2_fourth_column_is_not_an_idd_offset() {
    let m = manifest();
    let entry = m.profiles.iter().find(|p| p.id == "mhc2_signed").unwrap();
    let mhc2 = nitpick_profile::read_mhc2(&entry.bytes().unwrap()).unwrap();
    let stored = mhc2.matrix.unwrap();
    assert!(stored.iter().any(|r| r[3] != 0.0));
    let scrgb = Source {
        primaries: Primaries::BT709,
        encoding: SourceEncoding::Linear {
            nits_per_unit: 80.0,
        },
    };
    let mhc2_model = Model::new(
        scrgb,
        Calibration::new(mhc2.xyz_matrix(), None).unwrap(),
        hdr10(),
    )
    .unwrap();
    let affine = Affine::new(stored, 1.0, 80.0).unwrap();
    let idd_model = Model::new(scrgb, Calibration::idd(Some(affine), None), hdr10()).unwrap();
    let to_wire = colour::invert(&colour::rgb_to_xyz(colour::BT2020, colour::D65));
    let offset = colour::apply(&to_wire, stored.map(|r| r[3] * 80.0));
    for rgb in [[0.0; 3], [0.5, 1.0, 0.25]] {
        let a = mhc2_model.predict(rgb).unwrap().wire_nits;
        let b = idd_model.predict(rgb).unwrap().wire_nits;
        let diff = std::array::from_fn(|k| b[k] - a[k]);
        assert_close("mhc2_signed", 0, "offset", diff, offset);
    }
    assert_eq!(mhc2_model.predict([0.0; 3]).unwrap().wire_nits, [0.0; 3]);
}
