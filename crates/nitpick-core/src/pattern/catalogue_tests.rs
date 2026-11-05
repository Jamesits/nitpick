use super::*;
use crate::signal::BitDepth;

const HD: (usize, usize) = (1920, 1080);

fn f10(e: Encoding) -> Format {
    Format::new(e, BitDepth::Ten)
}

#[test]
fn grayscale_rejects_hlg() {
    let err = PatternKind::Grayscale
        .render(f10(Encoding::HlgNarrow), 1920, 1080)
        .unwrap_err();
    assert!(matches!(err, PatternError::UnsupportedEncoding { .. }));
    assert!(
        PatternKind::Grayscale
            .reference_table(f10(Encoding::HlgNarrow), HD)
            .is_err()
    );
}

#[test]
fn rejects_tiny_output() {
    let err = PatternKind::ColourBars
        .render(f10(Encoding::PqNarrow), 320, 240)
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
            for depth in BitDepth::ALL {
                let img = kind.render(Format::new(enc, depth), 2560, 1440).unwrap();
                assert_eq!((img.width(), img.height()), (2560, 1440));
                assert_eq!(img.pixels().len(), 2560 * 1440);
            }
        }
    }
}

#[test]
fn reference_table_lists_ycbcr() {
    let t = PatternKind::ColourBars
        .reference_table(f10(Encoding::PqNarrow), HD)
        .unwrap();
    // 100 % red: Y' 294, Cb 387, Cr 960 (see signal tests).
    let red = t.lines().find(|l| l.starts_with("100% red")).unwrap();
    let cols: Vec<&str> = red.split_whitespace().collect();
    assert_eq!(cols[2..], ["940", "64", "64", "294", "387", "960"]);
    let t = PatternKind::ColourBars
        .reference_table(Format::new(Encoding::PqNarrow, BitDepth::Twelve), HD)
        .unwrap();
    let red = t.lines().find(|l| l.starts_with("100% red")).unwrap();
    let cols: Vec<&str> = red.split_whitespace().collect();
    assert_eq!(cols[2..6], ["3760", "256", "256", "1177"]);
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
        .reference_table(f10(Encoding::PqFull), HD)
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

#[test]
fn reference_table_follows_the_12_bit_ramp_of_the_size_class() {
    let f = Format::new(Encoding::PqNarrow, BitDepth::Twelve);
    let first = |size| {
        let t = PatternKind::ColourBars.reference_table(f, size).unwrap();
        let line = t
            .lines()
            .find(|l| l.starts_with("ramp first"))
            .unwrap()
            .to_owned();
        line.split_whitespace().nth(2).unwrap().to_owned()
    };
    assert_eq!(first(HD), "20");
    assert_eq!(first((3840, 2160)), "18");
    assert_eq!(first((7680, 4320)), "17");
}

#[test]
fn reference_table_marks_levels_the_fp16_swap_chain_cannot_carry() {
    let t = PatternKind::ColourBars
        .reference_table(Format::new(Encoding::PqNarrow, BitDepth::Twelve), HD)
        .unwrap();
    let red = t.lines().find(|l| l.starts_with("100% red")).unwrap();
    assert!(red.ends_with(" *"), "{red}");
    let white = t.lines().find(|l| l.starts_with("100% white")).unwrap();
    assert!(!white.ends_with('*'), "{white}");
    let t = PatternKind::ColourBars
        .reference_table(f10(Encoding::PqNarrow), HD)
        .unwrap();
    assert!(!t.contains('*'));
}
