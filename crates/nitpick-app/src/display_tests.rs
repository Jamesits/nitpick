use nitpick_core::device::{
    DeviceCapabilities, DeviceId, DeviceInfo, DeviceMode, DeviceQuery, EdrHeadroom, Report,
};
use nitpick_core::driver::{Capabilities, Gamut, OutputLimits, Support, check_frame, check_open};
use nitpick_core::signal::{BitDepth, Encoding, Format};

use super::{
    Backend, DeviceReport, NO_FP16_SCALE, capabilities, display_id, ffi, mode, surface,
    surface_warning,
};

const INFO_2: &str = "DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2";

fn known<T>(value: T) -> Report<T> {
    Report::known(value, INFO_2)
}

fn unknown<T>() -> Report<T> {
    Report::unknown("not reported")
}

/// A Windows HDR display with HDR on, 10 bits on the wire.
fn hdr_on() -> DeviceQuery {
    DeviceQuery {
        device: DeviceInfo {
            id: DeviceId {
                backend: "windows".into(),
                key: "monitor".into(),
            },
            name: "Monitor".into(),
            evidence: Vec::new(),
        },
        capabilities: DeviceCapabilities {
            hdr: known(true),
            sdr_wcg: known(true),
            wire_depths: unknown(),
            icc_profiles: unknown(),
        },
        mode: DeviceMode {
            hdr_on: known(true),
            gamut: known(Gamut::Rec2020),
            wire_depth: known(10),
            peak_nits: known(1000.0),
            min_nits: known(0.005),
            full_frame_nits: known(600.0),
            sdr_white_nits: known(240.0),
            native_size: known((3840, 2160)),
            edr: unknown(),
        },
        topology_revision: 1,
    }
}

/// Windows SDR WCG: Advanced Color active, HDR off.
fn sdr_wcg() -> DeviceQuery {
    let mut q = hdr_on();
    q.mode.hdr_on = known(false);
    q.mode.gamut = unknown();
    q.mode.sdr_white_nits = unknown();
    q
}

/// A macOS display: no HDR, depth or luminance reports; EDR headroom only.
fn mac() -> DeviceQuery {
    let mut q = hdr_on();
    q.capabilities.hdr = unknown();
    q.mode = DeviceMode {
        hdr_on: unknown(),
        gamut: unknown(),
        wire_depth: unknown(),
        peak_nits: unknown(),
        min_nits: unknown(),
        full_frame_nits: unknown(),
        sdr_white_nits: unknown(),
        native_size: known((3024, 1964)),
        edr: known(EdrHeadroom {
            current: 1.2,
            potential: 16.0,
            reference: None,
        }),
    };
    q
}

fn caps(backend: Backend, device: DeviceQuery) -> Capabilities {
    capabilities(backend, &Ok(device))
}

fn depth(caps: &Capabilities, d: BitDepth) -> &super::DepthSupport {
    &caps.depths.iter().find(|(k, _)| *k == d).unwrap().1
}

fn unknown_reason(s: &Support) -> &str {
    match s {
        Support::Unknown(reason) => reason,
        other => panic!("not Unknown: {other:?}"),
    }
}

/// `check_open` accepts `d` and warns about it.
fn open_warning(caps: &Capabilities, d: BitDepth) -> String {
    let warnings = check_open(caps, d).unwrap();
    warnings.join("; ")
}

fn is_unknown(s: &Support) -> bool {
    matches!(s, Support::Unknown(_))
}

fn no_reason(s: &Support) -> &str {
    match s {
        Support::No(reason) => reason,
        other => panic!("not No: {other:?}"),
    }
}

/// The pattern is shown, but the table and the status line warn that the
/// luminance scale is not absolute.
#[test]
fn macos_twelve_bit_warns_with_the_scale_reason() {
    for device in [Ok(mac()), Ok(hdr_on()), Err("no report".to_owned())] {
        let caps = capabilities(Backend::Metal, &device);
        let twelve = depth(&caps, BitDepth::Twelve);
        assert!(unknown_reason(&twelve.available).starts_with(NO_FP16_SCALE));
        assert!(no_reason(&twelve.exact).contains(NO_FP16_SCALE));
        assert!(open_warning(&caps, BitDepth::Twelve).contains(NO_FP16_SCALE));
        let format = Format::new(Encoding::PqFull, BitDepth::Twelve);
        let limits = OutputLimits::for_frame(&caps, format, None);
        assert!(limits.path.is_some());
        assert!(limits.warnings[0].contains(NO_FP16_SCALE));
        // 10-bit HDR10 stays available.
        assert!(check_open(&caps, BitDepth::Ten).is_ok());
        assert_eq!(depth(&caps, BitDepth::Ten).available, Support::Yes);
    }
}

#[test]
fn windows_hdr_on_offers_both_paths() {
    let caps = caps(Backend::D3d11, hdr_on());
    for d in BitDepth::ALL {
        assert_eq!(depth(&caps, d).available, Support::Yes);
    }
    // A 10-bit wire for 12-bit codes truncates them.
    assert!(no_reason(&depth(&caps, BitDepth::Twelve).exact).contains("10-bit wire"));
    let m = mode(&Ok(hdr_on()));
    assert_eq!(m.hdr_on, Some(true));
    assert_eq!(m.gamut, Some(Gamut::Rec2020));
    assert_eq!(m.wire_depth, Some(10));
    assert_eq!(m.peak_nits, Some(1000.0));
    assert_eq!(m.min_nits, Some(0.005));
    assert_eq!(m.sdr_white_nits, Some(240.0));
    assert!(
        m.description
            .starts_with("Monitor: HDR on, 10 bpc, peak 1000 cd/m2")
    );
}

/// D3D11 has no HDR swap chain with HDR off. The pattern is shown on the
/// SDR fallback with a warning.
#[test]
fn windows_hdr_off_warns_for_both_paths() {
    let mut off = hdr_on();
    off.mode.hdr_on = known(false);
    let caps = caps(Backend::D3d11, off.clone());
    for d in BitDepth::ALL {
        assert!(unknown_reason(&depth(&caps, d).available).starts_with("Windows HDR is off"));
        assert!(open_warning(&caps, d).contains("8-bit SDR swap chain"));
        assert!(no_reason(&depth(&caps, d).exact).contains("HDR is off"));
    }
    let m = mode(&Ok(off));
    assert_eq!(m.hdr_on, Some(false));
    assert!(m.description.contains("HDR off"));
}

#[test]
fn windows_sdr_wcg_is_not_hdr() {
    let caps = caps(Backend::D3d11, sdr_wcg());
    for d in BitDepth::ALL {
        assert!(unknown_reason(&depth(&caps, d).available).starts_with("Windows HDR is off"));
    }
    let m = mode(&Ok(sdr_wcg()));
    assert_eq!(m.hdr_on, Some(false));
    assert_eq!(m.gamut, None);
}

/// HDR and WCG support, a BT.2020 gamut and a peak luminance do not set
/// `hdr_on` without the active-mode report.
#[test]
fn support_and_gamut_alone_do_not_set_hdr_on() {
    let mut q = hdr_on();
    q.mode.hdr_on = unknown();
    let m = mode(&Ok(q.clone()));
    assert_eq!(m.hdr_on, None);
    assert!(m.description.contains("HDR state unknown"));
    let caps = caps(Backend::D3d11, q);
    assert_eq!(depth(&caps, BitDepth::Ten).available, Support::Yes);
    assert!(is_unknown(&depth(&caps, BitDepth::Ten).exact));
}

/// No backend reports calibration or dithering, so even the best case
/// makes no exact-code claim.
#[test]
fn unknown_calibration_and_dithering_give_no_exact_claim() {
    let mut q = hdr_on();
    q.mode.wire_depth = known(12);
    for backend in [Backend::D3d11, Backend::Metal, Backend::Vulkan] {
        let caps = caps(backend, q.clone());
        let exact = &depth(&caps, BitDepth::Ten).exact;
        assert!(is_unknown(exact), "{backend:?}: {exact:?}");
        assert_ne!(depth(&caps, BitDepth::Twelve).exact, Support::Yes);
    }
    q.mode.wire_depth = known(8);
    let caps = caps(Backend::D3d11, q);
    assert!(no_reason(&depth(&caps, BitDepth::Ten).exact).contains("8-bit wire"));
}

/// GNOME gives an HDR10 swap chain with HDR off. The path stays open, but
/// the codes are converted.
#[test]
fn linux_hdr_off_is_inexact() {
    let mut q = hdr_on();
    q.capabilities.hdr = Report::known(false, "EDID");
    q.mode.hdr_on = Report::known(false, "HDR_OUTPUT_METADATA");
    let caps = caps(Backend::Vulkan, q);
    let ten = depth(&caps, BitDepth::Ten);
    assert_eq!(ten.available, Support::Yes);
    let reason = no_reason(&ten.exact);
    assert!(reason.contains("no HDR support (EDID)"));
    assert!(reason.contains("HDR is off (HDR_OUTPUT_METADATA)"));
}

#[test]
fn unknown_fields_stay_unknown() {
    let unmapped: DeviceReport = Err("no device mapping for screen HDMI-1".into());
    let m = mode(&unmapped);
    assert_eq!(
        (m.hdr_on, m.gamut, m.wire_depth),
        (None, None, None),
        "{m:?}"
    );
    assert_eq!(
        (m.peak_nits, m.min_nits, m.sdr_white_nits),
        (None, None, None)
    );
    assert!(m.description.contains("no device mapping"));

    let caps = capabilities(Backend::Vulkan, &unmapped);
    let ten = depth(&caps, BitDepth::Ten);
    assert_eq!(ten.available, Support::Yes);
    assert!(matches!(&ten.exact, Support::Unknown(r) if r.contains("no device mapping")));
    // The Vulkan FP16 scale is a convention without a device test.
    assert!(is_unknown(&depth(&caps, BitDepth::Twelve).available));
    assert!(is_unknown(&caps.hdr_metadata));
    let metadata_request = None;
    let warnings = check_frame(
        &caps,
        Format::new(Encoding::HlgNarrow, BitDepth::Ten),
        metadata_request,
    )
    .unwrap();
    assert!(warnings[0].contains("scope use only"));

    let m = mode(&Ok(mac()));
    assert_eq!((m.hdr_on, m.gamut, m.wire_depth), (None, None, None));
    assert_eq!(
        (m.peak_nits, m.min_nits, m.sdr_white_nits),
        (None, None, None)
    );
    // EDR headroom is a ratio and fills no luminance field.
    assert!(
        m.description
            .contains("EDR headroom 1.20 (potential 16.00)")
    );
}

/// `QRhi` uses another swap chain format; the window stays open and warns.
#[test]
fn swap_chain_fallback_is_a_warning() {
    let fallback = ffi::PatternSurface {
        fallback: "D3D11 uses an 8-bit SDR swap chain".into(),
        width: 3840,
        height: 2160,
        description: "HDR10 swap chain (D3D11)".into(),
    };
    let s = surface(display_id(0), &fallback).unwrap();
    assert_eq!(s.size, (3840, 2160));
    let warning = surface_warning(BitDepth::Ten, &fallback).unwrap();
    assert!(warning.starts_with(BitDepth::Ten.label()));
    assert!(warning.ends_with("D3D11 uses an 8-bit SDR swap chain"));
    let opened = ffi::PatternSurface {
        fallback: String::new(),
        ..fallback
    };
    let s = surface(display_id(1), &opened).unwrap();
    assert_eq!(s.display, display_id(1));
    assert_eq!(surface_warning(BitDepth::Ten, &opened), None);
}

/// Runs the launcher's screen path for every Qt screen. Run with
/// `--ignored`.
#[test]
#[ignore = "needs Qt and the local displays"]
fn local_screen_report() {
    let _app = cxx_qt_lib::QGuiApplication::new();
    for screen in 0.. {
        let Ok(identity) = ffi::screen_identity(screen) else {
            break;
        };
        println!("screen {screen}: {} {}", identity.name, identity.gdi_name);
        let query = super::query(Backend::current(), &super::device_report(screen));
        println!("  {}", query.mode.description);
        for depth in BitDepth::ALL {
            println!("  {depth}: {:?}", check_open(&query.capabilities, depth));
        }
    }
}
