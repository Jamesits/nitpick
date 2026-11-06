use super::*;
use crate::driver::{Capabilities, DepthSupport, EncodingSupport, OutputPath, Support};
use crate::signal::BitDepth;

const HD: (usize, usize) = (1920, 1080);

fn f10(e: Encoding) -> Format {
    Format::new(e, BitDepth::Ten)
}

fn f12(e: Encoding) -> Format {
    Format::new(e, BitDepth::Twelve)
}

fn limits(conversion: Conversion) -> OutputLimits {
    OutputLimits {
        path: Some(OutputPath {
            description: "mock path".into(),
            conversion,
        }),
        warnings: Vec::new(),
    }
}

fn direct() -> OutputLimits {
    limits(Conversion::Direct)
}

/// Mock path report: every feature `Yes`, `conversion` at both depths.
fn caps(conversion: Conversion) -> Capabilities {
    Capabilities {
        depths: BitDepth::ALL
            .into_iter()
            .map(|d| {
                let s = DepthSupport {
                    available: Support::Yes,
                    path: format!("mock {conversion:?}"),
                    conversion,
                    exact: Support::Yes,
                };
                (d, s)
            })
            .collect(),
        encodings: Encoding::ALL
            .into_iter()
            .map(|e| {
                let s = EncodingSupport {
                    input: Support::Yes,
                    interpretation: Support::Yes,
                    auto_light_level: Support::Yes,
                };
                (e, s)
            })
            .collect(),
        hdr_metadata: Support::Yes,
        pixel_exact: Support::Yes,
    }
}

/// `QRhi` report: 10-bit HDR10, 12-bit FP16 scRGB.
fn qrhi_caps() -> Capabilities {
    let mut c = caps(Conversion::Direct);
    c.depths[1].1.conversion = Conversion::ScRgb80;
    c.depths[1].1.path = "mock FP16".into();
    c
}

fn table(kind: PatternKind, format: Format, caps: &Capabilities) -> String {
    let l = OutputLimits::for_frame(caps, format, None);
    kind.reference_table(format, HD, &l).unwrap()
}

fn row<'a>(t: &'a str, name: &str) -> &'a str {
    t.lines().find(|l| l.starts_with(name)).unwrap()
}

#[test]
fn grayscale_rejects_hlg() {
    let err = PatternKind::Grayscale
        .render(f10(Encoding::HlgNarrow), 1920, 1080)
        .unwrap_err();
    assert!(matches!(err, PatternError::UnsupportedEncoding { .. }));
    assert!(
        PatternKind::Grayscale
            .reference_table(f10(Encoding::HlgNarrow), HD, &direct())
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
        .reference_table(f10(Encoding::PqNarrow), HD, &direct())
        .unwrap();
    // 100 % red: Y' 294, Cb 387, Cr 960 (see signal tests).
    let red = t.lines().find(|l| l.starts_with("100% red")).unwrap();
    let cols: Vec<&str> = red.split_whitespace().collect();
    assert_eq!(cols[2..], ["940", "64", "64", "294", "387", "960"]);
    let t = PatternKind::ColourBars
        .reference_table(f12(Encoding::PqNarrow), HD, &direct())
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
        .reference_table(f10(Encoding::PqFull), HD, &direct())
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
        let t = PatternKind::ColourBars
            .reference_table(f, size, &direct())
            .unwrap();
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
    let t = table(
        PatternKind::ColourBars,
        f12(Encoding::PqNarrow),
        &qrhi_caps(),
    );
    assert!(t.contains("Output: mock FP16."), "{t}");
    assert!(row(&t, "100% red").ends_with(" *"));
    assert!(!row(&t, "100% white").ends_with('*'));
    assert!(t.contains(
        "
* The FP16 scRGB swap chain"
    ));
    assert!(!t.contains("clipped"));
}

#[test]
fn only_the_fp16_path_has_star_marks() {
    let paths = [
        (qrhi_caps(), BitDepth::Twelve),
        (qrhi_caps(), BitDepth::Ten),
        (caps(Conversion::Png16), BitDepth::Ten),
        (caps(Conversion::Png16), BitDepth::Twelve),
        (caps(Conversion::Direct), BitDepth::Twelve),
    ];
    for (i, (c, depth)) in paths.iter().enumerate() {
        for kind in PatternKind::ALL {
            for &e in kind.encodings() {
                let t = table(kind, Format::new(e, *depth), c);
                let starred = t.lines().any(|l| l.ends_with(" *"));
                if i > 0 {
                    assert!(!starred, "{kind:?} {e} path {i}");
                }
            }
        }
    }
}

#[test]
fn browser_marks_nominal_range_excursions_as_clipped_at_both_depths() {
    let png = caps(Conversion::Png16);
    for depth in BitDepth::ALL {
        for e in [Encoding::PqNarrow, Encoding::HlgNarrow] {
            let t = table(PatternKind::ColourBars, Format::new(e, depth), &png);
            for name in ["-7%", "109%", "-2%", "ramp first", "ramp last"] {
                let r = row(&t, name);
                assert!(r.ends_with(" clipped"), "{depth} {e}: {r}");
            }
            for name in ["0%", "100%", "100% white", "100% red", "+2%"] {
                let r = row(&t, name);
                assert!(!r.ends_with("clipped"), "{depth} {e}: {r}");
            }
            assert!(t.contains(
                "
clipped: a component is below 0 %"
            ));
            assert!(!t.contains('*'));
        }
        let t = table(
            PatternKind::ColourBars,
            Format::new(Encoding::PqFull, depth),
            &png,
        );
        assert!(!t.contains("clipped"), "{t}");
        let t = table(
            PatternKind::Grayscale,
            Format::new(Encoding::PqNarrow, depth),
            &png,
        );
        // Its ramp covers the nominal range only.
        assert!(!t.contains("clipped"), "{t}");
    }
}

#[test]
fn unavailable_path_claims_no_exact_output() {
    let mut c = qrhi_caps();
    c.depths[1].1.available = Support::No("absolute FP16 luminance scale unavailable".into());
    let t = table(PatternKind::ColourBars, f12(Encoding::PqNarrow), &c);
    assert!(t.contains("Output: unavailable. No claim"), "{t}");
    assert!(t.contains("Warning: 12-bit not supported: absolute FP16 luminance scale"));
    assert!(!t.contains('*') && !t.contains("clipped"));
    assert!(!t.contains("mock FP16"));
    // Unsupported encoding: also unavailable.
    let mut c = caps(Conversion::Png16);
    c.encodings.retain(|(e, _)| *e != Encoding::HlgNarrow);
    let t = table(PatternKind::ColourBars, f10(Encoding::HlgNarrow), &c);
    assert!(t.contains("Output: unavailable."));
    assert!(!t.contains("clipped"));
    // Query failure.
    let l = OutputLimits::unavailable("display disconnected");
    let t = PatternKind::ColourBars
        .reference_table(f10(Encoding::PqNarrow), HD, &l)
        .unwrap();
    assert!(t.contains("Output: unavailable.") && t.contains("Warning: display disconnected"));
}

#[test]
fn reference_table_shows_interpretation_and_precision_warnings() {
    let mut c = qrhi_caps();
    c.encodings[2].1.interpretation = Support::No("sent with PQ signalling".into());
    c.depths[0].1.exact = Support::Unknown("compositor".into());
    let t = table(PatternKind::ColourBars, f10(Encoding::HlgNarrow), &c);
    assert!(
        t.contains("Warning: HLG narrow range interpretation: no (sent with PQ signalling)"),
        "{t}"
    );
    assert!(t.contains("Warning: 10-bit (mock Direct) exact codes: unknown (compositor)"));
}
