use std::collections::BTreeSet;

use super::*;
use crate::LightLevelRequest;

fn entries() -> Vec<ProfileEntry> {
    manifest().unwrap().profiles
}

/// Half of one 15.16 step: the largest rounding error of the generator.
const HALF_STEP: f64 = 1.0 / 131_072.0;

fn assert_near(id: &str, intended: f64, encoded: i32) {
    let err = (decode_s15f16(encoded) - intended).abs();
    assert!(err <= HALF_STEP, "{id}: {intended} stored as {encoded}");
}

#[test]
fn manifest_lists_every_file() {
    let mut want: BTreeSet<String> = entries().into_iter().map(|e| e.file).collect();
    want.insert("manifest.json".to_owned());
    let on_disk: BTreeSet<String> = std::fs::read_dir(profile_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(on_disk, want);
}

#[test]
fn files_match_size_and_hash() {
    for e in entries() {
        e.bytes().unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(e.file, format!("{}.icc", e.id));
    }
}

#[test]
fn stale_file_is_rejected() {
    let mut e = entries().remove(0);
    e.sha256 = "0".repeat(64);
    assert_eq!(e.bytes().unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn ids_are_unique() {
    let ids: Vec<String> = entries().into_iter().map(|e| e.id).collect();
    let unique: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn error_kinds_are_known() {
    for e in entries() {
        if let Resolved::Error { error } = &e.mhc2 {
            assert!(
                [
                    "missing_tag",
                    "malformed_tag",
                    "unsupported",
                    "invalid_profile"
                ]
                .contains(&error.as_str()),
                "{}: {error}",
                e.id
            );
        }
        assert_eq!(
            e.display.is_none(),
            e.mhc2
                == Resolved::Error {
                    error: "invalid_profile".into()
                },
            "{}",
            e.id
        );
    }
}

#[test]
fn ordinary_profiles_have_no_mhc2() {
    let ordinary: Vec<_> = entries()
        .into_iter()
        .filter(|e| e.usage == ProfileUse::Ordinary)
        .collect();
    assert!(ordinary.len() >= 2);
    for e in ordinary {
        assert_eq!(
            e.mhc2,
            Resolved::Error {
                error: "missing_tag".into()
            },
            "{}",
            e.id
        );
    }
}

#[test]
fn mhc2_set_shares_display_metadata() {
    let set: Vec<_> = entries()
        .into_iter()
        .filter(|e| e.usage == ProfileUse::Mhc2)
        .collect();
    assert!(set.len() >= 4);
    let display = set[0].display.clone().unwrap();
    assert!(display.lumi.is_some());
    let luminance = |e: &ProfileEntry| match &e.mhc2 {
        Resolved::Value(m) => (m.min_nits.clone(), m.peak_nits.clone()),
        Resolved::Error { error } => panic!("{}: {error}", e.id),
    };
    for e in &set {
        assert_eq!(e.display.as_ref(), Some(&display), "{}", e.id);
        assert_eq!(luminance(e), luminance(&set[0]), "{}", e.id);
    }
}

#[test]
fn encoded_values_are_within_half_a_step() {
    for e in entries() {
        let id = e.id.as_str();
        if let Some(d) = &e.display {
            let pairs = d
                .media_white
                .intended
                .iter()
                .zip(d.media_white.encoded)
                .chain(
                    d.chad
                        .intended
                        .iter()
                        .flatten()
                        .zip(d.chad.encoded.into_iter().flatten()),
                )
                .chain(
                    d.colorants
                        .intended
                        .iter()
                        .flatten()
                        .zip(d.colorants.encoded.into_iter().flatten()),
                )
                .chain(d.trc.intended.iter().zip(d.trc.encoded.iter().copied()));
            for (i, enc) in pairs {
                assert_near(id, *i, enc);
            }
            if let Some(l) = &d.lumi {
                assert_near(id, l.intended, l.encoded);
            }
        }
        let Resolved::Value(m) = &e.mhc2 else {
            continue;
        };
        assert_near(id, m.min_nits.intended, m.min_nits.encoded);
        assert_near(id, m.peak_nits.intended, m.peak_nits.encoded);
        if let Some(mx) = &m.matrix {
            for (i, enc) in mx
                .intended
                .iter()
                .flatten()
                .zip(mx.encoded.iter().flatten())
            {
                assert_near(id, *i, *enc);
            }
        }
        if let Some(lut) = &m.lut {
            let n = lut.encoded[0].len();
            assert!((2..=4096).contains(&n), "{id}: {n} entries");
            for (ri, re) in lut.intended.iter().zip(&lut.encoded) {
                assert_eq!((ri.len(), re.len()), (n, n), "{id}");
                for (i, enc) in ri.iter().zip(re) {
                    assert_near(id, *i, *enc);
                }
            }
        }
    }
}

#[test]
fn luminance_fields_are_distinct() {
    let m = manifest().unwrap();
    let entry = m
        .profiles
        .iter()
        .find(|e| e.usage == ProfileUse::Mhc2)
        .unwrap();
    let Resolved::Value(mhc2) = &entry.mhc2 else {
        panic!("{}", entry.id)
    };
    let LightLevelRequest::Explicit { max_cll, max_fall } = m.frame_metadata.light_level else {
        panic!("frame light level is not explicit")
    };
    let values = [
        mhc2.min_nits.intended,
        mhc2.peak_nits.intended,
        entry
            .display
            .as_ref()
            .unwrap()
            .lumi
            .as_ref()
            .unwrap()
            .intended,
        m.frame_metadata.max_mastering_nits,
        m.frame_metadata.min_mastering_nits,
        max_cll,
        max_fall,
    ];
    let unique: BTreeSet<u64> = values.iter().map(|v| v.to_bits()).collect();
    assert_eq!(unique.len(), values.len());
}
