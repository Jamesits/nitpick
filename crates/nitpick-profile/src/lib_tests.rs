use nitpick_fixtures::Resolved;
use nitpick_fixtures::profile::{self, Mhc2Expected, ProfileEntry, ProfileManifest, decode_s15f16};

use super::*;

fn manifest() -> ProfileManifest {
    profile::manifest().expect("profile manifest")
}

fn entry(id: &str) -> ProfileEntry {
    manifest()
        .profiles
        .into_iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("no fixture {id}"))
}

fn bytes(id: &str) -> Vec<u8> {
    entry(id).bytes().expect("fixture bytes")
}

fn dec<const N: usize>(v: [i32; N]) -> [f64; N] {
    v.map(decode_s15f16)
}

fn assert_mhc2(id: &str, got: &Mhc2, want: &Mhc2Expected) {
    assert_eq!(
        got.min_nits,
        decode_s15f16(want.min_nits.encoded),
        "{id}: min"
    );
    assert_eq!(
        got.peak_nits,
        decode_s15f16(want.peak_nits.encoded),
        "{id}: peak"
    );
    let matrix = want.matrix.as_ref().map(|m| m.encoded.map(dec));
    assert_eq!(got.matrix, matrix, "{id}: matrix");
    let lut = want.lut.as_ref().map(|l| {
        l.encoded
            .clone()
            .map(|c| c.into_iter().map(decode_s15f16).collect())
    });
    assert_eq!(got.lut, lut, "{id}: lut");
}

#[test]
fn engine_meets_minimum() {
    assert!(
        engine_version() >= MIN_ENGINE_VERSION,
        "{}",
        engine_version()
    );
}

#[test]
fn mhc2_matches_manifest() {
    for e in manifest().profiles {
        let got = read_mhc2(&e.bytes().expect("fixture bytes"));
        match (&e.mhc2, got) {
            (Resolved::Value(want), Ok(got)) => assert_mhc2(&e.id, &got, want),
            (Resolved::Error { error }, Err(got)) => assert_eq!(got.kind(), error, "{}", e.id),
            (want, got) => panic!("{}: want {want:?}, got {got:?}", e.id),
        }
    }
}

#[test]
fn malformed_cases_report_their_defect() {
    use Defect::{LutCount, LutSignature, OutOfBounds, Truncated, TypeSignature};
    let cases = [
        ("mhc2_truncated_header", Truncated),
        ("mhc2_wrong_type_signature", TypeSignature),
        ("mhc2_wrong_lut_signature", LutSignature(Channel::Green)),
        ("mhc2_file_relative_offsets", OutOfBounds(Element::Matrix)),
        ("mhc2_matrix_offset_wrap", OutOfBounds(Element::Matrix)),
        (
            "mhc2_lut_offset_wrap",
            OutOfBounds(Element::Lut(Channel::Red)),
        ),
        ("mhc2_matrix_outside_tag", OutOfBounds(Element::Matrix)),
        (
            "mhc2_lut_outside_tag",
            OutOfBounds(Element::Lut(Channel::Blue)),
        ),
        ("mhc2_lut_count_4097", LutCount),
    ];
    for (id, defect) in cases {
        assert_eq!(
            read_mhc2(&bytes(id)),
            Err(ProfileError::MalformedTag(Tag::Mhc2, defect)),
            "{id}"
        );
    }
}

#[test]
fn one_entry_lut_is_unsupported() {
    assert_eq!(
        read_mhc2(&bytes("mhc2_lut_count_1")),
        Err(ProfileError::Unsupported(
            Tag::Mhc2,
            Unsupported::SingleEntryLut
        ))
    );
}

#[test]
fn missing_and_invalid_never_become_identity() {
    assert_eq!(
        read_mhc2(&bytes("ordinary_srgb")),
        Err(ProfileError::MissingTag(Tag::Mhc2))
    );
    assert_eq!(
        read_mhc2(&bytes("icc_truncated")),
        Err(ProfileError::InvalidProfile)
    );
    assert_eq!(
        read_display(&bytes("icc_truncated")),
        Err(ProfileError::InvalidProfile)
    );
    assert_eq!(read_mhc2(&[]), Err(ProfileError::InvalidProfile));
}

#[test]
fn relocated_tag_gives_combined_values() {
    assert_eq!(
        read_mhc2(&bytes("mhc2_relocated")),
        read_mhc2(&bytes("mhc2_combined"))
    );
}

#[test]
fn xyz_matrix_ignores_fourth_column() {
    let m = read_mhc2(&bytes("mhc2_signed")).expect("signed");
    let stored = m.matrix.expect("matrix");
    assert!(stored.iter().any(|r| r[3] != 0.0));
    assert_eq!(m.xyz_matrix(), stored.map(|r| [r[0], r[1], r[2]]));
}

#[test]
fn null_fields_select_identity() {
    let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    for case in ["mhc2_zero_matrix_offset", "mhc2_metadata_only"] {
        let m = read_mhc2(&bytes(case)).expect(case);
        assert_eq!((m.matrix, m.xyz_matrix()), (None, id), "{case}");
    }
    for case in ["mhc2_zero_lut_count", "mhc2_metadata_only"] {
        assert_eq!(read_mhc2(&bytes(case)).expect(case).lut, None, "{case}");
    }
}

#[test]
fn display_tags_match_manifest() {
    for e in manifest().profiles {
        let Some(want) = &e.display else { continue };
        let got = read_display(&e.bytes().expect("fixture bytes")).expect(&e.id);
        assert_eq!(got.media_white, dec(want.media_white.encoded), "{}", e.id);
        assert_eq!(got.chad, Some(want.chad.encoded.map(dec)), "{}", e.id);
        assert_eq!(got.colorants, want.colorants.encoded.map(dec), "{}", e.id);
        let trc = ParametricCurve {
            function: want.trc.function,
            params: want
                .trc
                .encoded
                .iter()
                .copied()
                .map(decode_s15f16)
                .collect(),
        };
        assert_eq!(got.trc, [trc.clone(), trc.clone(), trc], "{}", e.id);
        let lumi = want.lumi.as_ref().map(|l| decode_s15f16(l.encoded));
        assert_eq!(got.lumi, lumi, "{}", e.id);
    }
}
