use nitpick_fixtures::profile::{self, ProfileManifest, RenderingIntent, TransformCase};

use super::*;

fn bytes(id: &str) -> Vec<u8> {
    manifest()
        .profiles
        .into_iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("no fixture {id}"))
        .bytes()
        .expect("fixture bytes")
}

fn manifest() -> ProfileManifest {
    profile::manifest().expect("profile manifest")
}

fn options(input: Format, output: Format) -> TransformOptions {
    TransformOptions {
        input,
        output,
        intent: Intent::RelativeColorimetric,
        black_point_compensation: false,
        adaptation_state: 1.0,
    }
}

/// A fixture endpoint and its format: RGB for a profile, XYZ for the PCS.
fn endpoint(icc: Option<&[u8]>) -> (Endpoint<'_>, Format) {
    match icc {
        Some(b) => (Endpoint::Profile(b), Format::RgbF64),
        None => (Endpoint::PcsXyz, Format::XyzF64),
    }
}

fn run(case: &TransformCase) -> Vec<[f64; 3]> {
    let icc = |id: &Option<String>| id.as_deref().map(bytes);
    let (src, dst) = (icc(&case.source), icc(&case.target));
    let (source, input) = endpoint(src.as_deref());
    let (target, output) = endpoint(dst.as_deref());
    let o = TransformOptions {
        intent: match case.intent {
            RenderingIntent::Perceptual => Intent::Perceptual,
            RenderingIntent::RelativeColorimetric => Intent::RelativeColorimetric,
            RenderingIntent::Saturation => Intent::Saturation,
            RenderingIntent::AbsoluteColorimetric => Intent::AbsoluteColorimetric,
        },
        black_point_compensation: case.black_point_compensation,
        adaptation_state: case.adaptation_state,
        ..options(input, output)
    };
    transform(source, target, &o, &case.input).unwrap_or_else(|e| panic!("{}: {e}", case.id))
}

/// Largest component error against the reference: 16 times the f32 unit
/// roundoff $2^{-24}$.
///
/// Little CMS rounds each stage result to f32: input curve, the matrices
/// (it joins adjacent ones) and output curve. Cancellation in a matrix
/// gives an absolute linear error near $2^{-24}$ also for a small result,
/// and the inverse output curve multiplies it by its slope, which is
/// largest near black. The measured worst case on Windows is 4.3e-7, in
/// `srgb_to_d50_absolute_unadapted` at output 0.059 (gamma 2.2).
const BOUND: f64 = 1.0 / (1 << 20) as f64;

#[test]
fn transforms_match_reference() {
    for case in manifest().transforms {
        let got = run(&case);
        assert_eq!(got.len(), case.expected.len(), "{}", case.id);
        for (k, (g, w)) in got.iter().zip(&case.expected).enumerate() {
            for (g, w) in g.iter().zip(w) {
                assert!(
                    (g - w).abs() <= BOUND,
                    "{} pixel {k}: got {g:?}, want {w:?}",
                    case.id
                );
            }
        }
    }
}

fn device_values() -> Vec<[f64; 3]> {
    manifest()
        .transforms
        .into_iter()
        .find(|c| c.id == "srgb_to_pcs")
        .expect("srgb_to_pcs case")
        .input
}

/// Results through the PCS in both directions; equal results mean that
/// the profiles give the same transforms.
fn round_trip(icc: &[u8]) -> (Vec<[f64; 3]>, Vec<[f64; 3]>) {
    let p = Endpoint::Profile(icc);
    let to_pcs = transform(
        p,
        Endpoint::PcsXyz,
        &options(Format::RgbF64, Format::XyzF64),
        &device_values(),
    )
    .expect("to PCS");
    let back = transform(
        Endpoint::PcsXyz,
        p,
        &options(Format::XyzF64, Format::RgbF64),
        &to_pcs,
    )
    .expect("from PCS");
    (to_pcs, back)
}

#[test]
fn mhc2_tag_does_not_change_transform() {
    let want = round_trip(&bytes("mhc2_identity"));
    let set: Vec<_> = manifest()
        .profiles
        .into_iter()
        .filter(|p| p.usage == profile::ProfileUse::Mhc2)
        .collect();
    assert!(set.len() >= 4);
    for e in set {
        assert_eq!(round_trip(&e.bytes().expect("bytes")), want, "{}", e.id);
    }
}

#[test]
fn vcgt_tag_does_not_change_transform() {
    assert_eq!(
        round_trip(&bytes("vcgt_srgb")),
        round_trip(&bytes("ordinary_srgb"))
    );
}

#[test]
fn format_must_match_profile() {
    let srgb = bytes("ordinary_srgb");
    for (input, output) in [
        (Format::XyzF64, Format::XyzF64),
        (Format::RgbF64, Format::RgbF64),
    ] {
        assert_eq!(
            transform(
                Endpoint::Profile(&srgb),
                Endpoint::PcsXyz,
                &options(input, output),
                &[[0.5; 3]],
            ),
            Err(ProfileError::Transform)
        );
    }
}

#[test]
fn adaptation_state_outside_unit_range_is_rejected() {
    let srgb = bytes("ordinary_srgb");
    for adaptation_state in [-0.1, 1.5, f64::NAN] {
        let o = TransformOptions {
            intent: Intent::AbsoluteColorimetric,
            adaptation_state,
            ..options(Format::RgbF64, Format::RgbF64)
        };
        assert_eq!(
            transform(Endpoint::Profile(&srgb), Endpoint::Profile(&srgb), &o, &[]),
            Err(ProfileError::InvalidOption),
            "{adaptation_state}"
        );
    }
}

#[test]
fn invalid_profile_is_rejected() {
    let o = options(Format::RgbF64, Format::XyzF64);
    for icc in [bytes("icc_truncated"), Vec::new()] {
        assert_eq!(
            transform(Endpoint::Profile(&icc), Endpoint::PcsXyz, &o, &[]),
            Err(ProfileError::InvalidProfile)
        );
    }
}
