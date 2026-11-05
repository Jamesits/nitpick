use super::*;

#[test]
fn grayscale_rejects_hlg() {
    let err = PatternKind::Grayscale
        .render(Encoding::HlgNarrow, 1920, 1080)
        .unwrap_err();
    assert!(matches!(err, PatternError::UnsupportedEncoding { .. }));
    assert!(
        PatternKind::Grayscale
            .reference_table(Encoding::HlgNarrow)
            .is_err()
    );
}

#[test]
fn rejects_tiny_output() {
    let err = PatternKind::ColourBars
        .render(Encoding::PqNarrow, 320, 240)
        .unwrap_err();
    assert_eq!(
        err,
        PatternError::TooSmall {
            width: 320,
            height: 240
        }
    );
}

#[test]
fn renders_requested_size() {
    for kind in PatternKind::ALL {
        for &enc in kind.encodings() {
            let img = kind.render(enc, 2560, 1440).unwrap();
            assert_eq!((img.width(), img.height()), (2560, 1440));
            assert_eq!(img.pixels().len(), 2560 * 1440);
        }
    }
}

#[test]
fn reference_table_lists_ycbcr() {
    let t = PatternKind::ColourBars
        .reference_table(Encoding::PqNarrow)
        .unwrap();
    // 100 % red: Y' 294, Cb 387, Cr 960 (see signal tests).
    let red = t.lines().find(|l| l.starts_with("100% red")).unwrap();
    let cols: Vec<&str> = red.split_whitespace().collect();
    assert_eq!(cols[2..], ["940", "64", "64", "294", "387", "960"]);
}

#[test]
fn ids_round_trip() {
    for kind in PatternKind::ALL {
        assert_eq!(kind.id().parse::<PatternKind>(), Ok(kind));
    }
}

#[test]
fn reference_table_groups_by_section() {
    let t = PatternKind::ColourBars
        .reference_table(Encoding::PqFull)
        .unwrap();
    for section in [
        "[100% colour bars]",
        "[58% colour bars]",
        "[stair]",
        "[ramp]",
        "[bottom]",
    ] {
        assert!(t.lines().any(|l| l == section), "missing {section}");
    }
    // Table 4 has no sub-black or super-white rows.
    assert!(!t.contains("-7%") && !t.contains("109%") && !t.contains("-2%"));
}
