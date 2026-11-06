use nitpick_core::driver::{ContentLightLevel, Hdr10Request, OutputLimits};
use nitpick_core::signal::Format;
use serde_json::json;

use super::*;
use crate::test_support::*;

fn unknown(s: &Support) -> bool {
    matches!(s, Support::Unknown(_))
}

fn no(s: &Support) -> bool {
    matches!(s, Support::No(_))
}

#[test]
fn valid_hello_is_parsed() {
    let h = hello(&[]);
    assert_eq!(h.client.user_agent, SAFARI_IPAD);
    assert!(!h.client.webview);
    assert_eq!(h.dynamic_range, DynamicRange::High);
    assert_eq!(h.color_gamut, ColorGamut::P3);
    assert!(h.fullscreen);
    assert_eq!(h.surface_size(), (2360, 1640));
}

#[test]
fn surface_size_rounds_display_pixels() {
    let h = hello(&[
        ("viewport.width", Some(json!(1536))),
        ("viewport.height", Some(json!(864.4))),
        ("pixelRatio", Some(json!(1.25))),
    ]);
    // 864.4 × 1.25 = 1080.5 rounds away from zero.
    assert_eq!(h.surface_size(), (1920, 1081));
    let h = hello(&[
        ("viewport.width", Some(json!(0.4))),
        ("viewport.height", Some(json!(0.4))),
        ("pixelRatio", Some(json!(2))),
    ]);
    assert_eq!(h.surface_size(), (1, 1));
}

#[test]
fn invalid_hello_is_rejected() {
    let side = f64::from(MAX_SURFACE_SIDE);
    let cases = [
        ("client", None),
        ("client.userAgent", None),
        ("client.webview", Some(json!("no"))),
        ("screen", None),
        ("viewport.scale", None),
        ("pixelRatio", None),
        ("fullscreen", None),
        ("dynamicRange", Some(json!("hdr"))),
        ("colorGamut", Some(json!("adobe"))),
        ("pixelRatio", Some(json!(0))),
        ("pixelRatio", Some(json!(-2))),
        ("pixelRatio", Some(json!("2"))),
        ("viewport.scale", Some(json!(0))),
        ("viewport.width", Some(json!(-1))),
        ("screen.height", Some(json!(0))),
        // Rounds to 0 display px.
        ("viewport.height", Some(json!(0.2))),
        // One display px over the limit at pixel ratio 2.
        ("viewport.width", Some(json!(side / 2.0 + 0.5))),
        ("viewport.height", Some(json!(1e300))),
    ];
    for (path, change) in cases {
        let value = hello_with(&[(path, change.clone())]);
        assert!(
            Hello::parse(fields(&value)).is_err(),
            "{path} = {change:?} accepted"
        );
    }
    let at_limit = hello_with(&[("viewport.width", Some(json!(side / 2.0)))]);
    assert_eq!(
        Hello::parse(fields(&at_limit)).unwrap().surface_size().0,
        usize::from(MAX_SURFACE_SIDE)
    );
}

#[test]
fn engine_rules_follow_the_user_agent() {
    let cases = [
        (SAFARI_IPAD, "Safari", None),
        (CHROME_WINDOWS, "Chrome", None),
        (EDGE_WINDOWS, "Edge", None),
        (CHROME_LINUX, "Chrome", None),
        (FIREFOX_MACOS, "Firefox", None),
        // Not covered by the design rule: no SDR limit is assumed.
        (FIREFOX_ANDROID, "Firefox", None),
        (
            FIREFOX_WINDOWS,
            "Firefox",
            Some("Firefox on Windows and Linux"),
        ),
        (
            FIREFOX_LINUX,
            "Firefox",
            Some("Firefox on Windows and Linux"),
        ),
        (WEBKITGTK, "WebKitGTK", Some("WebKitGTK")),
        ("curl/8.0", "browser", None),
    ];
    for (ua, name, limit) in cases {
        assert_eq!(engine(ua), name, "{ua}");
        assert_eq!(sdr_engine(ua), limit, "{ua}");
    }
}

#[test]
fn hdr_client_reports_unknown_precision_and_metadata_effect() {
    let q = hello(&[]).query();
    let caps = &q.capabilities;
    assert_eq!(q.mode.hdr_on, Some(true));
    assert_eq!(q.mode.gamut, Some(Gamut::P3));
    assert_eq!(q.mode.wire_depth, None);
    assert_eq!(q.mode.peak_nits, None);
    assert_eq!(q.mode.min_nits, None);
    assert_eq!(q.mode.sdr_white_nits, None);
    assert!((q.mode.pixel_ratio - 2.0).abs() < f64::EPSILON);
    assert!(!q.mode.description.contains("SDR preview"));

    let depths: Vec<_> = caps.depths.iter().map(|(d, _)| *d).collect();
    assert_eq!(depths, BitDepth::ALL);
    for (_, d) in &caps.depths {
        assert_eq!(d.available, Support::Yes);
        assert_eq!(d.conversion, Conversion::Png16);
        assert!(unknown(&d.exact), "{d:?}");
    }
    let encodings: Vec<_> = caps.encodings.iter().map(|(e, _)| *e).collect();
    assert_eq!(encodings, Encoding::ALL);
    for (e, s) in &caps.encodings {
        assert_eq!(s.input, Support::Yes);
        assert!(unknown(&s.interpretation), "{e:?}");
        assert_eq!(s.auto_light_level == Support::Yes, e.is_pq(), "{e:?}");
        if !e.is_pq() {
            assert!(no(&s.auto_light_level));
        }
    }
    assert!(unknown(&caps.hdr_metadata));
    assert!(unknown(&caps.pixel_exact));
}

#[test]
fn gamut_follows_the_media_query() {
    for (value, gamut) in [
        ("rec2020", Gamut::Rec2020),
        ("p3", Gamut::P3),
        ("srgb", Gamut::Srgb),
    ] {
        let q = hello(&[("colorGamut", Some(json!(value)))]).query();
        assert_eq!(q.mode.gamut, Some(gamut));
    }
}

#[test]
fn standard_dynamic_range_is_an_sdr_preview() {
    let q = hello(&[("dynamicRange", Some(json!("standard")))]).query();
    assert_eq!(q.mode.hdr_on, Some(false));
    assert!(
        q.mode.description.contains("SDR preview"),
        "{}",
        q.mode.description
    );
    for (_, s) in &q.capabilities.encodings {
        let Support::No(reason) = &s.interpretation else {
            panic!("interpretation {:?}", s.interpretation);
        };
        assert!(reason.contains("dynamic-range: standard"), "{reason}");
    }
}

#[test]
fn sdr_engines_report_hdr_off_despite_the_media_query() {
    for ua in [FIREFOX_WINDOWS, FIREFOX_LINUX, WEBKITGTK] {
        let h = hello(&[("client.userAgent", Some(json!(ua)))]);
        assert_eq!(h.dynamic_range, DynamicRange::High);
        let q = h.query();
        assert_eq!(q.mode.hdr_on, Some(false), "{ua}");
        assert!(q.mode.description.contains("SDR preview"), "{ua}");
        assert!(
            q.capabilities
                .encodings
                .iter()
                .all(|(_, s)| no(&s.interpretation)),
            "{ua}"
        );
    }
}

#[test]
fn pixel_mapping_rules() {
    let cases = [
        (2.0, 1.0, None),
        (1.0, 1.0, None),
        (1.25, 1.0, Some("not an integer")),
        (1.5, 1.0, Some("not an integer")),
        (2.0, 1.5, Some("zoom 1.5")),
        (2.0, 0.999, Some("zoom 0.999")),
    ];
    for (ratio, scale, rejection) in cases {
        let q = hello(&[
            ("pixelRatio", Some(json!(ratio))),
            ("viewport.scale", Some(json!(scale))),
        ])
        .query();
        match (rejection, &q.capabilities.pixel_exact) {
            // `Yes` needs the layout size from `applied`.
            (None, Support::Unknown(_)) => {}
            (Some(text), Support::No(reason)) => assert!(reason.contains(text), "{reason}"),
            (_, other) => panic!("ratio {ratio}, scale {scale}: {other:?}"),
        }
    }
}

#[test]
fn layout_warning_until_full_screen() {
    let h = hello(&[("fullscreen", Some(json!(false)))]);
    let warning = h.layout_warning().expect("warning");
    assert!(h.query().mode.description.contains(warning));
    assert_eq!(hello(&[]).layout_warning(), None);
}

#[test]
fn display_name_has_engine_and_address() {
    assert_eq!(hello(&[]).display_name(PEER), "Safari at 192.0.2.10");
    let webview = hello(&[
        ("client.userAgent", Some(json!(EDGE_WINDOWS))),
        ("client.webview", Some(json!(true))),
    ]);
    assert_eq!(webview.display_name(PEER), "Edge webview at 192.0.2.10");
}

#[test]
fn geometry_changes_with_size_ratio_and_zoom_only() {
    let base = hello(&[]).geometry();
    for change in [
        ("viewport.width", json!(1000)),
        ("viewport.height", json!(700)),
        ("pixelRatio", json!(3)),
        ("viewport.scale", json!(1.1)),
    ] {
        assert_ne!(hello(&[(change.0, Some(change.1))]).geometry(), base);
    }
    for change in [
        ("dynamicRange", json!("standard")),
        ("colorGamut", json!("srgb")),
        ("fullscreen", json!(false)),
        ("screen.width", json!(2000)),
        // Rounds to the same display size.
        ("viewport.width", json!(1180.1)),
    ] {
        assert_eq!(hello(&[(change.0, Some(change.1))]).geometry(), base);
    }
}

#[test]
fn output_limits_apply_the_browser_rules() {
    let caps = hello(&[]).query().capabilities;
    let pq = Format::new(Encoding::PqNarrow, BitDepth::Ten);
    let auto = Hdr10Request {
        primaries: [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
        white: [0.3127, 0.3290],
        max_mastering_nits: 1000.0,
        min_mastering_nits: 0.0001,
        light_level: ContentLightLevel::FromFrame,
    };
    let limits = OutputLimits::for_frame(&caps, pq, Some(&auto));
    let path = limits.path.expect("PQ path");
    assert_eq!(path.conversion, Conversion::Png16);
    for text in [
        "exact codes",
        "interpretation",
        "HDR10 metadata",
        "pixel-exact",
    ] {
        assert!(
            limits.warnings.iter().any(|w| w.contains(text)),
            "{text}: {:?}",
            limits.warnings
        );
    }

    let hlg = Format::new(Encoding::HlgNarrow, BitDepth::Twelve);
    assert!(
        OutputLimits::for_frame(&caps, hlg, Some(&auto))
            .path
            .is_none()
    );
    let explicit = Hdr10Request {
        light_level: ContentLightLevel::Explicit {
            max_cll: 1000.0,
            max_fall: 400.0,
        },
        ..auto
    };
    assert!(
        OutputLimits::for_frame(&caps, hlg, Some(&explicit))
            .path
            .is_some()
    );
}
