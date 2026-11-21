use super::*;
use crate::{Clip, Prediction, SdrTransfer};

const IDENTITY_3X4: [[f32; 4]; 3] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

fn ramp(matrix: [[f32; 4]; 3], scalar: f32, lut: Option<Vec<[f32; 3]>>) -> Event {
    Event::SetGammaRamp(GammaRamp::Transform3x4(Transform3x4 {
        matrix_enabled: true,
        matrix,
        scalar,
        lut_enabled: lut.is_some(),
        lut: lut.unwrap_or_else(|| vec![[0.0; 3]; LUT_LEN]),
    }))
}

fn scaled(scalar: f32) -> Event {
    ramp(IDENTITY_3X4, scalar, None)
}

fn hdr10() -> Event {
    Event::CommitMode(WireMode {
        colour_space: WireColourSpace::G2084P2020,
        depth: 10,
    })
}

fn record(generation: u64, hdr: FrameHdrMetadata) -> FrameRecord {
    FrameRecord {
        frame_number: 7,
        surface: SurfaceColourSpace::RgbFullG10P709,
        sdr_white_nits: 80,
        hdr,
        generation,
    }
}

fn frame(generation: u64) -> Event {
    Event::Frame(record(generation, FrameHdrMetadata::Absent))
}

fn frame_hdr(generation: u64, hdr: FrameHdrMetadata) -> Event {
    Event::Frame(record(generation, hdr))
}

fn contract() -> IddContract {
    IddContract {
        surface_sdr: None,
        wire_sdr: None,
        wcg_wire_primaries: None,
        offset_nits_per_unit: 80.0,
    }
}

fn srgb_80() -> Sdr {
    Sdr {
        transfer: SdrTransfer::Srgb,
        white_nits: 80.0,
    }
}

fn only(events: &[Event]) -> FrameState {
    let mut frames = replay(events).unwrap();
    assert_eq!(frames.len(), 1);
    frames.remove(0)
}

fn failure(events: &[Event]) -> ReplayError {
    replay(events).unwrap_err()
}

/// scRGB inputs: black, grey, the BT.709 primaries and a colour outside
/// BT.709.
const INPUTS: [[f64; 3]; 6] = [
    [0.0; 3],
    [0.5; 3],
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 0.0, 1.0],
    [-0.25, 1.5, 0.125],
];

fn predictions(model: &Model) -> Vec<Prediction> {
    INPUTS.iter().map(|&v| model.predict(v).unwrap()).collect()
}

fn plain_model(calibration: Calibration) -> Model {
    let f = only(&[hdr10(), scaled(1.0), frame(1)]);
    let source = f.source(&contract()).unwrap();
    let wire = f.wire_model(&contract()).unwrap();
    Model::new(source, calibration, wire).unwrap()
}

#[test]
fn disabled_stages_are_identity() {
    let ramp = Event::SetGammaRamp(GammaRamp::Transform3x4(Transform3x4 {
        matrix_enabled: false,
        matrix: [[f32::NAN; 4]; 3],
        scalar: 0.0,
        lut_enabled: false,
        lut: vec![[f32::NAN; 3]; LUT_LEN],
    }));
    let f = only(&[hdr10(), ramp, frame(1)]);
    let t = f.transform.as_ref().unwrap();
    assert_eq!((t.matrix(), t.scalar(), t.lut()), (None, None, None));
    let model = f.model(&contract()).unwrap();
    assert_eq!(
        predictions(&model),
        predictions(&plain_model(Calibration::identity()))
    );
}

#[test]
fn scalar_multiplies_the_matrix() {
    let model = only(&[hdr10(), scaled(0.5), frame(1)])
        .model(&contract())
        .unwrap();
    let half = [[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]];
    let want = plain_model(Calibration::new(half, None).unwrap());
    assert_eq!(predictions(&model), predictions(&want));
}

/// Linear wire cd/m² of black: `M_wire⁻¹ · s · u · t`.
fn black_wire_nits(scalar: f32, unit: f64) -> [f64; 3] {
    let t = [0.25, 0.5, 0.125];
    let mut m = IDENTITY_3X4;
    for (row, v) in m.iter_mut().zip(t) {
        row[3] = v;
    }
    let c = IddContract {
        offset_nits_per_unit: unit,
        ..contract()
    };
    only(&[hdr10(), ramp(m, scalar, None), frame(1)])
        .model(&c)
        .unwrap()
        .predict([0.0; 3])
        .unwrap()
        .wire_nits
}

fn assert_near(got: [f64; 3], want: [f64; 3]) {
    let bound = 1e-12 * want.iter().fold(1.0, |a: f64, v| a.max(v.abs()));
    for (g, w) in got.iter().zip(want) {
        assert!((g - w).abs() <= bound, "got {got:?}, want {want:?}");
    }
}

#[test]
fn affine_column_is_an_offset_with_the_scalar() {
    let to_wire = colour::invert(&colour::rgb_to_xyz(colour::BT2020, colour::D65));
    // s · u · t = 2 · 80 · (0.25, 0.5, 0.125).
    let want = colour::apply(&to_wire, [40.0, 80.0, 20.0]);
    assert_near(black_wire_nits(2.0, 80.0), want);
    assert_near(black_wire_nits(1.0, 160.0), want);
    assert_near(black_wire_nits(1.0, 80.0), want.map(|v| v / 2.0));
}

#[test]
fn replacement_and_reset_make_new_generations() {
    let events = [
        hdr10(),
        scaled(0.5),
        frame(1),
        scaled(0.25),
        frame(2),
        scaled(0.25),
        frame(3),
        Event::SetGammaRamp(GammaRamp::Default),
        frame(4),
    ];
    let frames = replay(&events).unwrap();
    let t: Vec<_> = frames
        .iter()
        .map(|f| f.transform.as_deref().unwrap())
        .collect();
    assert_eq!(
        t.iter().map(|t| t.generation()).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    // A replacement does not change the state of an earlier frame.
    assert_eq!(t[0].scalar(), Some(0.5));
    assert_eq!(t[1].scalar(), Some(0.25));
    assert_eq!(t[1].matrix(), t[2].matrix());
    assert_eq!(t[3].kind(), RampKind::Default);
    assert_eq!((t[3].matrix(), t[3].lut()), (None, None));
    let reset = frames[3].model(&contract()).unwrap();
    assert_eq!(
        predictions(&reset),
        predictions(&plain_model(Calibration::identity()))
    );
}

#[test]
fn frame_must_use_the_current_generation() {
    let e = failure(&[hdr10(), scaled(0.5), frame(0)]);
    assert_eq!(e.index, 2);
    assert_eq!(
        e.error,
        IddError::Generation {
            recorded: 0,
            replayed: 1
        }
    );
    let e = failure(&[hdr10(), frame(1)]);
    assert_eq!(
        e.error,
        IddError::Generation {
            recorded: 1,
            replayed: 0
        }
    );
}

#[test]
fn frame_before_the_first_ramp_has_no_model() {
    let f = only(&[hdr10(), frame(0)]);
    assert_eq!(f.transform, None);
    assert_eq!(f.model(&contract()), Err(IddError::NoTransform));
}

#[test]
fn frame_needs_a_committed_mode() {
    let e = failure(&[scaled(1.0), frame(1)]);
    assert_eq!((e.index, e.error), (1, IddError::NoMode));
}

fn meta(max_cll: u16) -> Hdr10Metadata {
    Hdr10Metadata {
        primaries: [[35400, 14600], [8500, 39850], [6550, 2300]],
        white_point: [15635, 16450],
        max_mastering_nits: 1000,
        min_mastering: 50,
        max_content_light_level: max_cll,
        max_frame_average_light_level: 400,
    }
}

fn set_default(m: Hdr10Metadata) -> Event {
    Event::SetDefaultHdrMetadata(DefaultHdrMetadata::Hdr10(m))
}

#[test]
fn metadata_selection_follows_new_unchanged_and_default() {
    use FrameHdrMetadata::{Absent, Default, New, Unchanged};
    let (a, b, c) = (meta(1000), meta(600), meta(4000));
    let events = [
        set_default(a),
        hdr10(),
        frame_hdr(0, Default),
        frame_hdr(0, New(b)),
        frame_hdr(0, Unchanged),
        // "Unchanged" keeps the value of the last frame, not the new default.
        set_default(c),
        frame_hdr(0, Unchanged),
        // New metadata did not replace the default.
        frame_hdr(0, Default),
        frame_hdr(0, Unchanged),
        frame_hdr(0, Absent),
    ];
    let got: Vec<_> = replay(&events).unwrap().iter().map(|f| f.hdr).collect();
    assert_eq!(
        got,
        [Some(a), Some(b), Some(b), Some(b), Some(c), Some(c), None]
    );
}

#[test]
fn unchanged_metadata_needs_metadata_on_the_last_frame() {
    use FrameHdrMetadata::{Absent, New, Unchanged};
    let cases: [(&[Event], usize); 4] = [
        (&[hdr10(), frame_hdr(0, Unchanged)], 1),
        // The default is not the metadata of a last frame.
        (&[set_default(meta(1)), hdr10(), frame_hdr(0, Unchanged)], 2),
        (
            &[
                hdr10(),
                frame_hdr(0, New(meta(1))),
                frame_hdr(0, Absent),
                frame_hdr(0, Unchanged),
            ],
            3,
        ),
        // A mode commit starts again.
        (
            &[
                hdr10(),
                frame_hdr(0, New(meta(1))),
                hdr10(),
                frame_hdr(0, Unchanged),
            ],
            3,
        ),
    ];
    for (events, index) in cases {
        let e = failure(events);
        assert_eq!((e.index, e.error), (index, IddError::NoPreviousMetadata));
    }
}

#[test]
fn default_metadata_needs_a_default() {
    let e = failure(&[hdr10(), frame_hdr(0, FrameHdrMetadata::Default)]);
    assert_eq!((e.index, e.error), (1, IddError::NoDefaultMetadata));
}

#[test]
fn uninitialized_records_are_rejected() {
    let cases = [
        (
            Event::SetGammaRamp(GammaRamp::Uninitialized),
            Field::GammaRamp,
        ),
        (
            Event::SetDefaultHdrMetadata(DefaultHdrMetadata::Uninitialized),
            Field::DefaultHdrMetadata,
        ),
        (
            Event::CommitMode(WireMode {
                colour_space: WireColourSpace::Uninitialized,
                depth: 10,
            }),
            Field::WireColourSpace,
        ),
        (
            frame_hdr(0, FrameHdrMetadata::Uninitialized),
            Field::FrameHdrMetadata,
        ),
    ];
    for (event, field) in cases {
        let e = failure(&[hdr10(), event]);
        assert_eq!((e.index, e.error), (1, IddError::Uninitialized(field)));
    }
}

#[test]
fn invalid_enabled_values_are_rejected() {
    let mut nan_matrix = IDENTITY_3X4;
    nan_matrix[1][3] = f32::NAN;
    let mut nan_lut = vec![[0.5; 3]; LUT_LEN];
    nan_lut[9][2] = f32::INFINITY;
    let short = Event::SetGammaRamp(GammaRamp::Transform3x4(Transform3x4 {
        matrix_enabled: true,
        matrix: IDENTITY_3X4,
        scalar: 1.0,
        lut_enabled: false,
        lut: vec![[0.0; 3]; 256],
    }));
    let cases = [
        (ramp(nan_matrix, 1.0, None), "matrix"),
        (ramp(IDENTITY_3X4, f32::NAN, None), "scalar"),
        (ramp(IDENTITY_3X4, 1.0, Some(nan_lut)), "LUT"),
        (short, "length"),
    ];
    for (event, what) in cases {
        let mut state = Replay::new();
        state.apply(&scaled(0.5)).unwrap();
        let before = state.clone();
        let e = state.apply(&event).unwrap_err();
        match what {
            "length" => assert_eq!(e, IddError::LutLength(256)),
            _ => assert!(
                matches!(e, IddError::Model(ModelError::NonFinite(_))),
                "{what}: {e:?}"
            ),
        }
        // A failed event leaves the state unchanged.
        assert_eq!(state.generation(), before.generation(), "{what}");
        assert_eq!(state.transform, before.transform, "{what}");
    }
}

#[test]
fn legacy_ramp_is_a_wire_lut() {
    let mut r = Box::new([[0u16; LEGACY_LUT_LEN]; 3]);
    for k in 0..LEGACY_LUT_LEN {
        let v = u16::try_from(k).unwrap() * 257;
        r[0][k] = v;
        r[1][k] = u16::MAX - v;
        r[2][k] = v / 2;
    }
    let events = [
        hdr10(),
        Event::SetGammaRamp(GammaRamp::Rgb256x3x16(r)),
        frame(1),
    ];
    let f = only(&events);
    let t = f.transform.as_deref().unwrap();
    assert_eq!((t.kind(), t.matrix()), (RampKind::Rgb256x3x16, None));
    let lut = t.lut().unwrap().channels();
    assert_eq!(lut[0].len(), LEGACY_LUT_LEN);
    assert_eq!((lut[0][0], lut[0][255]), (0.0, 1.0));
    assert_eq!((lut[1][0], lut[1][255]), (1.0, 0.0));
    assert_eq!(lut[0][51], 51.0 / 255.0);
}

#[test]
fn sdr_white_does_not_scale_an_hdr_surface() {
    let mut bright = record(1, FrameHdrMetadata::Absent);
    bright.sdr_white_nits = 240;
    let events = [hdr10(), scaled(1.0), frame(1), Event::Frame(bright)];
    let frames = replay(&events).unwrap();
    let [a, b] = [&frames[0], &frames[1]].map(|f| predictions(&f.model(&contract()).unwrap()));
    assert_eq!(a, b);
    assert_eq!(a[1].source_nits, [40.0; 3]);
}

fn with_spaces(surface: SurfaceColourSpace, wire: WireColourSpace) -> FrameState {
    let mut r = record(1, FrameHdrMetadata::Absent);
    r.surface = surface;
    let mode = Event::CommitMode(WireMode {
        colour_space: wire,
        depth: 8,
    });
    only(&[mode, scaled(1.0), Event::Frame(r)])
}

#[test]
fn sdr_spaces_need_the_contract() {
    use SurfaceColourSpace::{Other, RgbFullG10P709, RgbFullG22P709, RgbFullG2084P2020};
    use WireColourSpace::{G22P709, G22P709Wcg, G2084P2020};
    let none = contract();
    let full = IddContract {
        surface_sdr: Some(srgb_80()),
        wire_sdr: Some(srgb_80()),
        wcg_wire_primaries: Some(Primaries::BT2020),
        ..none
    };
    let missing = |s, w, field| {
        assert_eq!(
            with_spaces(s, w).model(&none),
            Err(IddError::Missing(field)),
            "{s:?} {w:?}"
        );
    };
    missing(RgbFullG22P709, G2084P2020, Field::SurfaceSdr);
    missing(RgbFullG10P709, G22P709, Field::WireSdr);
    missing(RgbFullG10P709, G22P709Wcg, Field::WcgWirePrimaries);
    assert_eq!(
        with_spaces(Other(5), G2084P2020).model(&full),
        Err(IddError::UnsupportedSurface(5))
    );
    let bad_unit = IddContract {
        offset_nits_per_unit: 0.0,
        ..full
    };
    assert_eq!(
        with_spaces(RgbFullG10P709, G2084P2020).model(&bad_unit),
        Err(IddError::Model(ModelError::Scale(0.0)))
    );

    // SDR white on an sRGB wire: 1.0 scRGB is 80 cd/m², signal 1.
    let f = with_spaces(RgbFullG10P709, G22P709);
    let p = f.model(&full).unwrap().predict([1.0; 3]).unwrap();
    assert_eq!(p.code, [255; 3]);
    assert_eq!(p.encode_clip, [Clip::None; 3]);
    // The WCG wire uses the contract primaries: BT.709 red is inside
    // BT.2020, so it is not at a corner.
    let f = with_spaces(RgbFullG10P709, G22P709Wcg);
    let p = f.model(&full).unwrap().predict([1.0, 0.0, 0.0]).unwrap();
    assert!(p.code[1] > 0 && p.code[0] < 255, "{:?}", p.code);
    // A PQ surface decodes as PQ.
    let f = with_spaces(RgbFullG2084P2020, G2084P2020);
    let p = f.model(&none).unwrap().predict([0.5; 3]).unwrap();
    assert!(
        p.signal.iter().all(|s| (s - 0.5).abs() < 1e-12),
        "{:?}",
        p.signal
    );
}
