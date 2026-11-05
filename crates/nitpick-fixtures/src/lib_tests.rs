use std::collections::BTreeSet;

use super::*;

fn all_cases() -> Vec<Case> {
    CASE_GROUPS
        .iter()
        .flat_map(|g| group(g).unwrap().cases)
        .collect()
}

#[test]
fn fixture_files_are_complete() {
    let mut want: BTreeSet<String> = CASE_GROUPS.iter().map(|g| format!("{g}.json")).collect();
    want.extend(["scalar.json".to_owned(), "invalid.json".to_owned()]);
    let on_disk: BTreeSet<String> = std::fs::read_dir(fixture_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(on_disk, want);
}

#[test]
fn ids_are_unique() {
    let mut ids = Vec::new();
    for g in CASE_GROUPS {
        let file = group(g).unwrap();
        ids.extend(file.cases.into_iter().map(|c| c.input.id));
        ids.extend(file.sequences.into_iter().map(|s| s.id));
    }
    ids.extend(invalid().unwrap().cases.into_iter().map(|c| c.id));
    let unique: BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn valid_cases_pass_validation_and_match_layout() {
    for c in all_cases() {
        let id = &c.input.id;
        validate(&c.input).unwrap_or_else(|e| panic!("{id}: {e}"));
        assert_eq!(c.expected.palette.len(), c.input.palette.len(), "{id}");
        let mut counts = vec![0; c.input.palette.len()];
        for i in raster(&c.input) {
            counts[i] += 1;
        }
        assert_eq!(counts, c.expected.pixel_counts, "{id}");
        assert_eq!(c.requires.depth, c.input.format.depth, "{id}");
        assert_eq!(c.requires.encoding, c.input.format.encoding, "{id}");
        assert_eq!(c.requires.hdr_metadata, c.input.metadata.is_some(), "{id}");
    }
}

#[test]
fn invalid_cases_are_rejected_with_their_error() {
    let file = invalid().unwrap();
    assert!(!file.cases.is_empty());
    let mut seen = BTreeSet::new();
    for c in &file.cases {
        let got = validate(&c.input).expect_err(&c.id);
        assert_eq!(got.id(), c.error, "{}", c.id);
        seen.insert(got.id());
    }
    assert_eq!(
        seen.len(),
        FixtureError::ALL.len(),
        "an error kind has no case"
    );
}

#[test]
fn scalar_tables_cover_every_code() {
    let s = scalar().unwrap();
    assert_eq!(s.qrhi.len(), 2);
    assert_eq!(s.browser.len(), 6);
    for t in &s.qrhi {
        let n = 1 << t.depth;
        assert_eq!(t.count, n);
        assert_eq!(
            (t.signal.len(), t.nits_2020.len(), t.scrgb_grey.len()),
            (n, n, n)
        );
        assert_eq!(t.r10g10b10a2.is_some(), t.depth == 10);
    }
    for t in &s.browser {
        let n = 1 << t.depth;
        assert_eq!((t.count, t.sample.len(), t.clip.len()), (n, n, n));
        assert_eq!(t.nits_2020.is_some(), t.encoding != "hlg-narrow");
    }
}

#[test]
fn decimal_values_parse_exactly() {
    // Shortest round-trip decimals from the generator; `float_roundtrip`
    // must parse them to the same binary64 value as a Rust literal.
    let s = scalar().unwrap();
    let ten = s.qrhi.iter().find(|t| t.depth == 10).unwrap();
    assert_eq!(
        ten.nits_2020[940].to_bits(),
        4_654.397_320_430_553_f64.to_bits()
    );
    assert_eq!(ten.nits_2020[1023].to_bits(), 10000.0_f64.to_bits());
}

#[test]
fn groups_cover_the_required_vectors() {
    let cases = all_cases();
    let ids: BTreeSet<&str> = cases.iter().map(|c| c.input.id.as_str()).collect();
    for enc in ["pq-full", "pq-narrow", "hlg-narrow"] {
        for depth in [10, 12] {
            for prefix in ["codes-endpoints", "mixed-bt2111"] {
                assert!(ids.contains(format!("{prefix}-{enc}-{depth}").as_str()));
            }
        }
    }
    // Gamut vectors include negative and greater-than-one scRGB values.
    let scrgb = cases
        .iter()
        .filter(|c| c.input.id.starts_with("gamut-"))
        .flat_map(|c| c.expected.palette.iter().flat_map(|p| p.qrhi.scrgb));
    let (lo, hi) = scrgb.fold((0.0f64, 0.0f64), |(lo, hi), v| (lo.min(v), hi.max(v)));
    assert!(lo < 0.0 && hi > 1.0);
    // Browser endpoint cases clip on both sides.
    let clips: BTreeSet<_> = cases
        .iter()
        .filter(|c| c.input.id.starts_with("codes-endpoints-pq-narrow"))
        .flat_map(|c| c.expected.palette.iter().flat_map(|p| p.browser.clip))
        .map(|c| c as u8)
        .collect();
    assert_eq!(clips.len(), 3);
}

#[test]
fn state_sequences_reference_state_frames() {
    let file = group("state").unwrap();
    let frames: BTreeSet<&str> = file.cases.iter().map(|c| c.input.id.as_str()).collect();
    assert!(file.cases.iter().all(|c| c.marker.is_some()));
    let markers: BTreeSet<u32> = file
        .cases
        .iter()
        .filter_map(|c| c.marker.as_ref().map(|m| m.value))
        .collect();
    assert_eq!(markers.len(), 2);
    assert!(!file.sequences.is_empty());
    for s in &file.sequences {
        for step in &s.steps {
            if let Step::Show(show) = step {
                let ShowStep { frame, expect, .. } = show.as_ref();
                assert!(frames.contains(frame.as_str()), "{}: {frame}", s.id);
                assert_eq!(
                    expect.error.is_some(),
                    expect.outcome == Outcome::Error,
                    "{}",
                    s.id
                );
            }
        }
    }
}

#[test]
fn hlg_automatic_light_level_is_unsupported_in_the_browser() {
    for c in all_cases() {
        let browser = &c.expected.light_level.browser;
        let unsupported = matches!(browser, Resolved::Error { error } if error == "unsupported");
        assert_eq!(
            unsupported,
            c.input.format.encoding == "hlg-narrow",
            "{}",
            c.input.id
        );
    }
}
