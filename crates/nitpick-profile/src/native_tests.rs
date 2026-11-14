//! Little CMS behaviour that the adapter works around or relies on. These
//! pin known deviations from the Microsoft MHC2 layout and undocumented
//! raw-tag behaviour; a change in the linked build must fail here.

use nitpick_fixtures::profile;

use super::*;

fn open(id: &str) -> Profile {
    let e = profile::manifest()
        .expect("profile manifest")
        .profiles
        .into_iter()
        .find(|p| p.id == id)
        .unwrap_or_else(|| panic!("no fixture {id}"));
    Profile::open(&e.bytes().expect("fixture bytes")).expect(id)
}

#[test]
fn mhc2_read_fails_for_lut_count_zero() {
    for id in ["mhc2_zero_lut_count", "mhc2_metadata_only"] {
        assert_eq!(open(id).mhc2(), None, "{id}");
    }
}

#[test]
fn mhc2_read_ignores_tag_size_and_lut_signature() {
    // The read of `mhc2_truncated_header` and `mhc2_file_relative_offsets`
    // fails only because an offset points outside the file: the bytes after
    // the truncated tag give the blue LUT offset, and the doubled tag start
    // passes the file end.
    for id in [
        "mhc2_wrong_lut_signature",
        "mhc2_matrix_offset_wrap",
        "mhc2_lut_offset_wrap",
        "mhc2_matrix_outside_tag",
        "mhc2_lut_outside_tag",
    ] {
        assert!(open(id).mhc2().is_some(), "{id}");
    }
}

#[test]
fn mhc2_read_rejects_counts_above_4096_and_type_mismatch() {
    for id in ["mhc2_lut_count_4097", "mhc2_wrong_type_signature"] {
        assert_eq!(open(id).mhc2(), None, "{id}");
    }
}

#[test]
fn mhc2_read_gives_identity_for_matrix_offset_zero() {
    let m = open("mhc2_zero_matrix_offset").mhc2().expect("read");
    assert_eq!(
        m.matrix,
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0]
        ]
    );
}

#[test]
fn raw_read_after_cooked_read_serializes_again() {
    let p = open("mhc2_relocated");
    let stored = p.raw_tag(Tag::Mhc2).expect("raw");
    assert_eq!(p.raw_tag(Tag::Mhc2).as_ref(), Some(&stored));
    assert!(p.mhc2().is_some());
    let again = p.raw_tag(Tag::Mhc2).expect("raw after cooked");
    assert_ne!(again, stored);
}
