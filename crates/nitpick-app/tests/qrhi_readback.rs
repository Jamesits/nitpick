//! Offscreen draw and readback through the draw path of the pattern window:
//! the stored samples and their positions must equal the CPU output.
//!
//! The software D3D11 device (WARP) runs in every Windows test run; it
//! covers arithmetic only. The GPU tests are ignored by default; run them
//! on a GPU runner with
//! `cargo nextest run -p nitpick-app --test qrhi_readback --run-ignored all`.

use nitpick_app::readback::{Backend, OffscreenDevice, Placement, expected_target};
use nitpick_core::image::Image;
use nitpick_core::output::{SwapChainFormat, swap_chain_bytes};
use nitpick_core::signal::{BitDepth, Encoding, Format, Rgb};
use nitpick_fixtures::CASE_GROUPS;
use nitpick_test_host::fixture::FixtureFrame;

/// One draw to check.
struct Case {
    name: String,
    format: SwapChainFormat,
    size: (usize, usize),
    pixels: Vec<u8>,
}

/// The production CPU output of `image`.
fn cpu_case(name: String, image: &Image, format: Format) -> Case {
    Case {
        name,
        format: SwapChainFormat::for_depth(format.depth),
        size: (image.width(), image.height()),
        pixels: swap_chain_bytes(image, format).expect("valid codes"),
    }
}

/// Every output fixture case, at both depths.
fn fixture_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for group in CASE_GROUPS {
        for case in nitpick_fixtures::group(group).expect("fixture group").cases {
            let frame = FixtureFrame::from_case(&case.input).expect("valid fixture");
            let image = frame.render(frame.size).expect("fixture size");
            cases.push(cpu_case(case.input.id.clone(), &image, frame.format));
        }
    }
    cases
}

/// Every code of `depth` in R, G and B alone and in grey, one row each.
fn ramp_case(depth: BitDepth) -> Case {
    let count = usize::from(depth.max_code()) + 1;
    let mut image = Image::new(count, 4, Rgb::grey(0));
    for code in 0..=depth.max_code() {
        let x = usize::from(code);
        image.set(x, 0, Rgb::new(code, 0, 0));
        image.set(x, 1, Rgb::new(0, code, 0));
        image.set(x, 2, Rgb::new(0, 0, code));
        image.set(x, 3, Rgb::grey(code));
    }
    cpu_case(
        format!("ramps-{}", depth.id()),
        &image,
        Format::new(Encoding::PqFull, depth),
    )
}

/// `R10G10B10A2` words with every alpha value and asymmetric colours. The
/// CPU output is always opaque; this checks that the shader copies alpha.
fn hdr10_alpha_case() -> Case {
    let pixels = (0..7 * 5)
        .flat_map(|i: u32| {
            let red = (i * 37 + 1) % 1024;
            let green = (i * 101 + 512) % 1024;
            let blue = 1023 - (i * 29) % 1024;
            ((i % 4) << 30 | blue << 20 | green << 10 | red).to_le_bytes()
        })
        .collect();
    Case {
        name: "raw-hdr10-alpha".into(),
        format: SwapChainFormat::Hdr10,
        size: (7, 5),
        pixels,
    }
}

/// Binary16 values in every channel, also alpha: signed zeros, subnormals,
/// normal limits, negative and greater-than-one values. Not NaN or
/// infinity: the CPU output is finite.
fn fp16_raw_case() -> Case {
    const VALUES: [u16; 13] = [
        0x0000, 0x8000, 0x0001, 0x8001, 0x03ff, 0x0400, 0x3555, 0x3800, 0x3c00, 0xbc00, 0x5640,
        0x7bff, 0xfbff,
    ];
    let n = VALUES.len();
    let pixel = |i: usize| [i, i + 4, i + 8, i + 11].map(|k| VALUES[k % n]);
    let pixels = (0..n)
        .map(pixel)
        .chain((0..n).rev().map(|i| pixel(i + 1)))
        .flatten()
        .flat_map(u16::to_le_bytes)
        .collect();
    Case {
        name: "raw-fp16".into(),
        format: SwapChainFormat::ScRgb,
        size: (n, 2),
        pixels,
    }
}

fn all_cases() -> Vec<Case> {
    let mut cases = fixture_cases();
    cases.push(ramp_case(BitDepth::Ten));
    cases.push(ramp_case(BitDepth::Twelve));
    cases.push(hdr10_alpha_case());
    cases.push(fp16_raw_case());
    cases
}

/// The first different pixel and the number of different pixels.
fn difference(want: &[u8], got: &[u8], bpp: usize, width: usize) -> Option<String> {
    if want.len() != got.len() {
        return Some(format!("{} bytes, want {}", got.len(), want.len()));
    }
    let mut diffs = want
        .chunks_exact(bpp)
        .zip(got.chunks_exact(bpp))
        .enumerate()
        .filter(|(_, (a, b))| a != b);
    let (i, (a, b)) = diffs.next()?;
    Some(format!(
        "{} pixels differ; first at ({}, {}): got {b:02x?}, want {a:02x?}",
        diffs.count() + 1,
        i % width,
        i / width
    ))
}

/// Draws every case whole and inside a larger target, and reports all
/// differences.
fn check_device(backend: Backend, software: bool) {
    let mut device = OffscreenDevice::new(backend, software).expect("QRhi device");
    let info = device.info();
    eprintln!("QRhi {} device: {}", info.backend, info.name);
    assert_eq!(
        info.software, software,
        "QRhi selected {:?}, software device: {}",
        info.name, info.software
    );
    let mut failures = Vec::new();
    let mut draws = 0;
    for case in all_cases() {
        let (w, h) = case.size;
        // Odd changes of the size, so that both parities of the target
        // width occur, and an origin like a safe-area margin.
        let placements = [
            Placement::whole(case.size),
            Placement {
                size: case.size,
                target: (w + 5, h + 4),
                origin: (3, 2),
            },
        ];
        for placement in placements {
            draws += 1;
            let want = expected_target(case.format, &case.pixels, placement).expect("placement");
            let result = device
                .draw(case.format, &case.pixels, placement)
                .map(|got| {
                    let bpp = case.format.bytes_per_pixel();
                    difference(&want, &got, bpp, placement.target.0)
                });
            match result {
                Ok(None) => {}
                Ok(Some(diff)) => failures.push(format!("{} {placement:?}: {diff}", case.name)),
                Err(e) => failures.push(format!("{} {placement:?}: {e}", case.name)),
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {draws} draws differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
    eprintln!("{draws} draws equal the CPU output");
}

#[cfg(windows)]
#[test]
fn d3d11_software_draw_matches_cpu_output() {
    check_device(Backend::D3d11, true);
}

#[cfg(windows)]
#[test]
#[ignore = "needs a D3D11 GPU"]
fn d3d11_draw_matches_cpu_output() {
    check_device(Backend::D3d11, false);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs a Metal GPU"]
fn metal_draw_matches_cpu_output() {
    check_device(Backend::Metal, false);
}

/// The Vulkan instance needs a Qt platform with Vulkan, so this needs a
/// window system on Linux.
#[cfg(not(target_os = "macos"))]
fn check_vulkan(software: bool) {
    let _app = cxx_qt_lib::QGuiApplication::new();
    check_device(Backend::Vulkan, software);
}

#[cfg(not(target_os = "macos"))]
#[test]
#[ignore = "needs a Vulkan GPU, a window system and the Vulkan headers at build time"]
fn vulkan_draw_matches_cpu_output() {
    check_vulkan(false);
}

#[cfg(not(target_os = "macos"))]
#[test]
#[ignore = "needs a Vulkan CPU device (lavapipe), a window system and the Vulkan headers at build time"]
fn vulkan_software_draw_matches_cpu_output() {
    check_vulkan(true);
}
