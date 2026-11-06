use nitpick_core::driver::{ContentLightLevel, Hdr10Request};
use nitpick_core::image::Image;
use nitpick_core::metadata::{Hdr10Metadata, MasteringPrimaries};
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::{BT2020_PRIMARIES, BitDepth, D65, Format, Rgb, pq_eotf};

use super::{
    METAL_OPTICAL_OUTPUT_SCALE, MetadataSink, NativeMetadata, SwapChainMetadata, pack, resolve,
};
use crate::display::{Backend, ffi};

fn request(light_level: ContentLightLevel) -> Hdr10Request {
    Hdr10Request {
        primaries: BT2020_PRIMARIES,
        white: D65,
        max_mastering_nits: 1000.0,
        min_mastering_nits: 0.0001,
        light_level,
    }
}

fn metadata(max_cll: f64, max_fall: f64) -> Hdr10Metadata {
    request(ContentLightLevel::Explicit { max_cll, max_fall }).resolve(|| unreachable!())
}

fn grey(code: u16) -> Rgb {
    Rgb {
        r: code,
        g: code,
        b: code,
    }
}

/// Independent full-range PQ light level: per-pixel maximum, then the
/// maximum and the mean over the frame.
fn expected_light_level(image: &Image, depth: BitDepth) -> (f64, f64) {
    let max = f64::from(depth.max_code());
    let l: Vec<f64> = image
        .pixels()
        .iter()
        .map(|p| pq_eotf(f64::from(p.r.max(p.g).max(p.b)) / max))
        .collect();
    #[allow(clippy::cast_precision_loss)]
    let mean = l.iter().sum::<f64>() / l.len() as f64;
    (l.iter().copied().fold(0.0, f64::max), mean)
}

#[test]
fn resolve_without_request_is_none() {
    let image = Image::new(4, 4, grey(940));
    assert_eq!(resolve(None, &image, BitDepth::Ten), None);
}

#[test]
fn resolve_reads_codes_as_full_range_pq_for_every_encoding() {
    let req = request(ContentLightLevel::FromFrame);
    for kind in PatternKind::ALL {
        for &encoding in kind.encodings() {
            for depth in BitDepth::ALL {
                let image = kind
                    .render(Format::new(encoding, depth), 1920, 1080)
                    .expect("render");
                let md = resolve(Some(&req), &image, depth).expect("metadata");
                let (max_cll, max_fall) = expected_light_level(&image, depth);
                let what = format!("{} {encoding:?} {depth:?}", kind.id());
                assert!((md.max_cll - max_cll).abs() < 1e-9, "{what}");
                assert!((md.max_fall - max_fall).abs() < 1e-9, "{what}");
            }
        }
    }
}

#[test]
fn resolve_hlg_peak_code_is_not_hlg_luminance() {
    // HLG 100 % (10-bit narrow code 940) read as full-range PQ: about
    // 5000 cd/m², not the HLG nominal peak.
    let image = Image::new(2, 2, grey(940));
    let md = resolve(
        Some(&request(ContentLightLevel::FromFrame)),
        &image,
        BitDepth::Ten,
    )
    .expect("metadata");
    let expected = pq_eotf(940.0 / 1023.0);
    assert!((md.max_cll - expected).abs() < 1e-9);
    assert!((md.max_fall - expected).abs() < 1e-9);
    assert!(md.max_cll > 4000.0);
}

#[test]
fn resolve_explicit_values_bypass_the_frame() {
    let image = Image::new(2, 2, grey(1023));
    let req = request(ContentLightLevel::Explicit {
        max_cll: 123.0,
        max_fall: 45.0,
    });
    let md = resolve(Some(&req), &image, BitDepth::Ten).expect("metadata");
    assert_eq!((md.max_cll, md.max_fall), (123.0, 45.0));
    assert_eq!(md.max_mastering_nits, 1000.0);
    assert_eq!(md.min_mastering_nits, 0.0001);
}

#[test]
fn pack_dxgi_field_order_and_units() {
    let NativeMetadata::Dxgi(d) = pack(Backend::D3d11, &metadata(1000.4, 400.6)) else {
        panic!("not DXGI");
    };
    assert_eq!(
        d,
        ffi::DxgiHdrMetadata {
            red_primary: [35_400, 14_600],
            green_primary: [8_500, 39_850],
            blue_primary: [6_550, 2_300],
            white_point: [15_635, 16_450],
            // Whole cd/m²; the minimum stays in 0.0001 cd/m².
            max_mastering_luminance: 1000,
            min_mastering_luminance: 1,
            max_content_light_level: 1000,
            max_frame_average_light_level: 401,
        }
    );
}

#[test]
fn pack_dxgi_rounds_maximum_mastering_luminance() {
    let max = |nits: f64| {
        let mut md = metadata(0.0, 0.0);
        md.max_mastering_nits = nits;
        match pack(Backend::D3d11, &md) {
            NativeMetadata::Dxgi(d) => d.max_mastering_luminance,
            other => panic!("not DXGI: {other:?}"),
        }
    };
    assert_eq!(max(999.5), 1000);
    assert_eq!(max(999.4999), 999);
    assert_eq!(max(0.0), 0);
}

#[test]
fn pack_dxgi_p3_primaries() {
    let mut md = metadata(0.0, 0.0);
    md.primaries = MasteringPrimaries::P3D65.primaries();
    let NativeMetadata::Dxgi(d) = pack(Backend::D3d11, &md) else {
        panic!("not DXGI");
    };
    assert_eq!(d.red_primary, [34_000, 16_000]);
    assert_eq!(d.green_primary, [13_250, 34_500]);
    assert_eq!(d.blue_primary, [7_500, 3_000]);
}

#[test]
fn pack_vulkan_field_order_and_units() {
    let NativeMetadata::Vulkan(v) = pack(Backend::Vulkan, &metadata(1000.4, 400.6)) else {
        panic!("not Vulkan");
    };
    assert_eq!(
        v,
        ffi::VulkanHdrMetadata {
            display_primary_red: [0.708, 0.292],
            display_primary_green: [0.170, 0.797],
            display_primary_blue: [0.131, 0.046],
            white_point: [0.3127, 0.3290],
            max_luminance: 1000.0,
            min_luminance: 0.0001,
            // CTA-861.3 whole cd/m², as on the other backends.
            max_content_light_level: 1000.0,
            max_frame_average_light_level: 401.0,
        }
    );
}

#[test]
fn pack_metal_sei_payloads() {
    let md = metadata(1000.4, 400.6);
    let NativeMetadata::Metal(m) = pack(Backend::Metal, &md) else {
        panic!("not Metal");
    };
    assert_eq!(m.display_info, md.mastering_display_bytes());
    // Green x first, big-endian; luminance in 0.0001 cd/m².
    assert_eq!(m.display_info[..2], 8_500u16.to_be_bytes());
    assert_eq!(m.display_info[16..20], 10_000_000u32.to_be_bytes());
    assert_eq!(m.display_info[20..], 1u32.to_be_bytes());
    assert_eq!(m.content_info, [0x03, 0xE8, 0x01, 0x91]);
    assert_eq!(m.optical_output_scale, METAL_OPTICAL_OUTPUT_SCALE);
}

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Set(NativeMetadata),
    Clear,
}

/// Records the calls. `fail_set` and `fail_clear` make the next calls fail.
#[derive(Default)]
struct Recorder {
    calls: Vec<Call>,
    fail_set: bool,
    fail_clear: bool,
}

impl MetadataSink for Recorder {
    fn set(&mut self, metadata: &NativeMetadata) -> Result<(), String> {
        self.calls.push(Call::Set(*metadata));
        if self.fail_set {
            Err("set failed".into())
        } else {
            Ok(())
        }
    }

    fn clear(&mut self) -> Result<(), String> {
        self.calls.push(Call::Clear);
        if self.fail_clear {
            Err("no clear".into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn disabled_metadata_on_a_new_swap_chain_makes_no_call() {
    let mut sink = Recorder::default();
    let mut state = SwapChainMetadata::default();
    for _ in 0..2 {
        let status = state.apply(&mut sink, Backend::D3d11, None);
        assert_eq!(status, "No HDR metadata.");
    }
    assert_eq!(sink.calls, []);
}

#[test]
fn replacement_sends_the_new_values_then_removes_them() {
    let mut sink = Recorder::default();
    let mut state = SwapChainMetadata::default();
    let first = metadata(1000.0, 400.0);
    let second = metadata(203.0, 100.0);
    let status = state.apply(&mut sink, Backend::D3d11, Some(&first));
    assert_eq!(
        status,
        "HDR10 metadata: MaxCLL 1000, MaxFALL 400, mastering 0.0001 to 1000 cd/m2."
    );
    state.apply(&mut sink, Backend::D3d11, Some(&second));
    assert_eq!(
        state.apply(&mut sink, Backend::D3d11, None),
        "HDR metadata removed."
    );
    assert_eq!(
        state.apply(&mut sink, Backend::D3d11, None),
        "No HDR metadata."
    );
    assert_eq!(
        sink.calls,
        [
            Call::Set(pack(Backend::D3d11, &first)),
            Call::Set(pack(Backend::D3d11, &second)),
            Call::Clear,
        ]
    );
}

#[test]
fn metal_status_names_edr() {
    let mut sink = Recorder::default();
    let status = SwapChainMetadata::default().apply(
        &mut sink,
        Backend::Metal,
        Some(&metadata(1000.0, 400.0)),
    );
    assert!(status.starts_with("EDR HDR10 metadata: "), "{status}");
}

#[test]
fn failed_set_is_reported_and_leaves_nothing_to_remove() {
    let mut sink = Recorder {
        fail_set: true,
        ..Recorder::default()
    };
    let mut state = SwapChainMetadata::default();
    let status = state.apply(&mut sink, Backend::D3d11, Some(&metadata(1.0, 1.0)));
    assert_eq!(status, "HDR metadata NOT sent: set failed.");
    assert_eq!(
        state.apply(&mut sink, Backend::D3d11, None),
        "No HDR metadata."
    );
    assert_eq!(sink.calls.len(), 1);
}

#[test]
fn failed_removal_is_reported_and_retried() {
    let mut sink = Recorder {
        fail_clear: true,
        ..Recorder::default()
    };
    let mut state = SwapChainMetadata::default();
    state.apply(&mut sink, Backend::Vulkan, Some(&metadata(1.0, 1.0)));
    for _ in 0..2 {
        let status = state.apply(&mut sink, Backend::Vulkan, None);
        assert_eq!(status, "HDR metadata NOT removed: no clear.");
    }
    assert_eq!(&sink.calls[1..], [Call::Clear, Call::Clear]);
}
