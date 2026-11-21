//! The swap chain of the real pattern window through `QrhiDriver`: the
//! native format and colour space, a readback of the swap-chain image after
//! the draw of each acknowledged frame, the native metadata structure, and
//! the render failure handling with injected faults.
//!
//! The tests open full-screen windows on the first screen, which must have
//! HDR on. They are ignored by default; run them with
//! `cargo nextest run -p nitpick-app --test qrhi_swap_chain --run-ignored only`.
//! Each test makes its own `QGuiApplication`, so run them with nextest,
//! which starts one process per test.

use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use cxx_qt_lib::QGuiApplication;
use nitpick_app::qrhi_driver::{BridgeWindow, QrhiDriver, Threads};
use nitpick_app::readback::{Placement, expected_target};
use nitpick_app::swap_chain_probe::{self as probe, FrameFault};
use nitpick_app::{Backend, DeviceStates, device_reports};
use nitpick_core::driver::{
    ContentLightLevel, DisplayId, Frame, Hdr10Request, OutputDriver, OutputError, PresentFuture,
    Surface,
};
use nitpick_core::image::Image;
use nitpick_core::output::{SwapChainFormat, swap_chain_bytes};
use nitpick_core::signal::{BT2020_PRIMARIES, BitDepth, D65, Encoding, Format, Rgb};
use nitpick_test_host::fixture::Fixtures;

/// Longer than the acknowledgement timeout of the pattern window (10 s).
const WAIT: Duration = Duration::from_secs(20);

type Driver = QrhiDriver<BridgeWindow, Threads>;
type Outcome = Result<String, OutputError>;

/// Polls `future` and runs the Qt event loop between the polls, until the
/// future ends or `limit` passes.
fn poll_for(future: &mut PresentFuture<'_>, limit: Duration) -> Option<Outcome> {
    let start = Instant::now();
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return Some(result);
        }
        if start.elapsed() >= limit {
            return None;
        }
        probe::run_event_loop(2);
    }
}

fn finish(future: &mut PresentFuture<'_>) -> Outcome {
    poll_for(future, WAIT).expect("the presentation ends in time")
}

/// Polls until the window queued one more frame; returns its frame ID.
fn poll_until_queued(future: &mut PresentFuture<'_>, before: usize) -> u64 {
    let start = Instant::now();
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            panic!("the presentation ended before its frame was queued: {result:?}");
        }
        let queued = probe::queued_frames();
        if queued.len() > before {
            return *queued.last().unwrap();
        }
        assert!(start.elapsed() < WAIT, "no frame queued in time");
        probe::run_event_loop(2);
    }
}

/// A frame and its expected swap-chain bytes.
struct Pattern {
    image: Image,
    format: Format,
    bytes: Vec<u8>,
}

impl Pattern {
    fn new(image: Image, depth: BitDepth) -> Self {
        let format = Format::new(Encoding::PqFull, depth);
        let bytes = swap_chain_bytes(&image, format).expect("valid codes");
        Self {
            image,
            format,
            bytes,
        }
    }

    /// Tiles of 61×59 different colours. The periods are prime and do not
    /// divide the screen sizes, so a flip, an offset, a scale or a channel
    /// swap changes the image. The FP16 conversion searches each colour
    /// once, so the number of colours sets its time.
    fn coordinates(size: (usize, usize), depth: BitDepth) -> Self {
        let n = usize::from(depth.max_code()) + 1;
        let mut image = Image::new(size.0, size.1, Rgb::grey(0));
        let code = |v: usize| u16::try_from(v % n).unwrap();
        for y in 0..size.1 {
            for x in 0..size.0 {
                let (i, j) = (x % 61, y % 59);
                image.set(
                    x,
                    y,
                    Rgb::new(code(i * 67), code(j * 69 + 5), code((i * 3 + j * 11) * 13)),
                );
            }
        }
        Self::new(image, depth)
    }

    fn fixture(fixtures: &Fixtures, id: &str, size: (usize, usize)) -> Self {
        let frame = fixtures.get(id).expect("fixture");
        Self::new(
            frame.render(size).expect("fixture size"),
            frame.format.depth,
        )
    }

    fn swap_chain_format(&self) -> SwapChainFormat {
        SwapChainFormat::for_depth(self.format.depth)
    }
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

/// The native swap chain that `QRhi` makes for each request.
fn expected_native(backend: Backend, depth: BitDepth) -> (&'static str, &'static str) {
    match (backend, depth) {
        (Backend::D3d11, BitDepth::Ten) => (
            "DXGI_FORMAT_R10G10B10A2_UNORM",
            "DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020",
        ),
        (Backend::D3d11, BitDepth::Twelve) => (
            "DXGI_FORMAT_R16G16B16A16_FLOAT",
            "DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709",
        ),
        (Backend::Vulkan, BitDepth::Ten) => (
            "VK_FORMAT_A2B10G10R10_UNORM_PACK32",
            "VK_COLOR_SPACE_HDR10_ST2084_EXT",
        ),
        (Backend::Vulkan, BitDepth::Twelve) => (
            "VK_FORMAT_R16G16B16A16_SFLOAT",
            "VK_COLOR_SPACE_EXTENDED_SRGB_LINEAR_EXT",
        ),
        (Backend::Metal, BitDepth::Ten) => {
            ("MTLPixelFormatRGB10A2Unorm", "kCGColorSpaceITUR_2100_PQ")
        }
        (Backend::Metal, BitDepth::Twelve) => (
            "MTLPixelFormatRGBA16Float",
            "kCGColorSpaceExtendedLinearSRGB",
        ),
    }
}

/// Metadata request with a different value in each field.
fn request() -> Hdr10Request {
    Hdr10Request {
        primaries: BT2020_PRIMARIES,
        white: D65,
        max_mastering_nits: 4000.0,
        min_mastering_nits: 0.005,
        light_level: ContentLightLevel::Explicit {
            max_cll: 800.0,
            max_fall: 200.0,
        },
    }
}

/// The request in ST 2086 and CTA-861.3 code units: chromaticity in
/// 0.00002, mastering luminance in 0.0001 cd/m², light levels in cd/m².
const CHROMATICITY: [u16; 8] = [35400, 14600, 8500, 39850, 6550, 2300, 15635, 16450];
const MAX_MASTERING: u32 = 40_000_000;
const MIN_MASTERING: u32 = 50;
const MAX_CLL: u16 = 800;
const MAX_FALL: u16 = 200;

/// The native structure of `request` for `backend`, as passed to the call.
fn expected_metadata(backend: Backend) -> Vec<u8> {
    match backend {
        // DXGI_HDR_METADATA_HDR10; MaxMasteringLuminance in whole cd/m².
        Backend::D3d11 => {
            let mut out: Vec<u8> = CHROMATICITY.iter().flat_map(|v| v.to_le_bytes()).collect();
            out.extend((MAX_MASTERING / 10_000).to_le_bytes());
            out.extend(MIN_MASTERING.to_le_bytes());
            out.extend(MAX_CLL.to_le_bytes());
            out.extend(MAX_FALL.to_le_bytes());
            out
        }
        // VkHdrMetadataEXT from displayPrimaryRed, f32 in natural units.
        Backend::Vulkan => {
            #[allow(clippy::cast_possible_truncation)]
            let f = |code: f64, unit: f64| ((code / unit) as f32).to_le_bytes();
            let mut out: Vec<u8> = CHROMATICITY
                .iter()
                .flat_map(|&v| f(f64::from(v), 50_000.0))
                .collect();
            out.extend(f(f64::from(MAX_MASTERING), 10_000.0));
            out.extend(f(f64::from(MIN_MASTERING), 10_000.0));
            out.extend(f(f64::from(MAX_CLL), 1.0));
            out.extend(f(f64::from(MAX_FALL), 1.0));
            out
        }
        // Big-endian SEI payloads, then the scale of linear 1.0.
        Backend::Metal => {
            // G, B, R, white, as in ST 2086 SEI.
            let mut out: Vec<u8> = [1, 2, 0, 3]
                .iter()
                .flat_map(|&i| {
                    [CHROMATICITY[2 * i], CHROMATICITY[2 * i + 1]]
                        .into_iter()
                        .flat_map(u16::to_be_bytes)
                })
                .collect();
            out.extend(MAX_MASTERING.to_be_bytes());
            out.extend(MIN_MASTERING.to_be_bytes());
            out.extend(MAX_CLL.to_be_bytes());
            out.extend(MAX_FALL.to_be_bytes());
            out.extend(100.0_f32.to_le_bytes());
            out
        }
    }
}

struct Run {
    backend: Backend,
    driver: Driver,
    display: DisplayId,
    fixtures: Fixtures,
}

impl Run {
    fn start(backend: Backend) -> Self {
        probe::set_swap_chain_readback(true);
        let states = DeviceStates::default();
        let mut driver = QrhiDriver::new(backend, BridgeWindow, Threads, states.clone());
        let refresh = states.begin();
        states.apply(refresh, device_reports(&driver.screens()));
        let display = driver.displays().first().expect("a screen").id.clone();
        let mode = driver.query(&display).expect("query").mode;
        eprintln!("display: {}", mode.description);
        assert_eq!(mode.hdr_on, Some(true), "the first screen must have HDR on");
        Self {
            backend,
            driver,
            display,
            fixtures: Fixtures::load().expect("fixtures"),
        }
    }

    /// Opens at `depth` and checks the native swap chain.
    fn open(&mut self, depth: BitDepth) -> Surface {
        self.driver.query(&self.display).expect("query");
        let surface = self.driver.open(&self.display, depth).expect("open");
        let record = probe::swap_chain_record().expect("record");
        eprintln!("{} {record:?}", depth.label());
        let (format, colour_space) = expected_native(self.backend, depth);
        assert_eq!(
            (record.native_format.as_str(), record.colour_space.as_str()),
            (format, colour_space)
        );
        assert!(record.matches_request && record.fallback.is_empty());
        assert!(record.readback, "the swap chain is a transfer source");
        assert_eq!(
            (
                usize::try_from(record.area_width).unwrap(),
                usize::try_from(record.area_height).unwrap()
            ),
            surface.size
        );
        assert!(
            surface.description.contains(format) && surface.description.contains(colour_space),
            "{}",
            surface.description
        );
        surface
    }

    fn present<'a>(&'a mut self, surface: &'a Surface, frame: &'a Frame<'a>) -> PresentFuture<'a> {
        self.driver.present(surface, frame)
    }

    /// Presents `pattern` and checks that it is acknowledged and in the
    /// swap chain. Returns the status and the frame ID.
    fn show(
        &mut self,
        surface: &Surface,
        pattern: &Pattern,
        metadata: Option<&Hdr10Request>,
    ) -> (String, u64) {
        let before = probe::queued_frames().len();
        let frame = Frame {
            image: &pattern.image,
            format: pattern.format,
            metadata,
        };
        let mut future = self.present(surface, &frame);
        let result = finish(&mut future);
        drop(future);
        let status = result.expect("acknowledged");
        let queued = probe::queued_frames();
        assert_eq!(queued.len(), before + 1, "one queued frame");
        let id = *queued.last().unwrap();
        check_swap_chain(id, pattern);
        (status, id)
    }
}

/// The swap-chain readback after the draw of frame `id` holds `pattern` in
/// the pattern area and opaque black elsewhere.
fn check_swap_chain(id: u64, pattern: &Pattern) {
    assert!(
        probe::uploaded_frames().contains(&id),
        "frame {id} was uploaded"
    );
    let format = pattern.swap_chain_format();
    let bpp = format.bytes_per_pixel();
    let image = probe::take_swap_chain_image(bpp).expect("swap-chain readback");
    assert_eq!(image.frame, id, "the readback names the submitted frame");
    let texture = match format {
        SwapChainFormat::Hdr10 => "RGB10A2",
        SwapChainFormat::ScRgb => "RGBA16F",
    };
    assert_eq!(image.format, texture);
    let record = probe::swap_chain_record().expect("record");
    let size = |v: i32| usize::try_from(v).unwrap();
    let placement = Placement {
        size: (size(record.area_width), size(record.area_height)),
        target: (size(record.width), size(record.height)),
        origin: (size(record.area_x), size(record.area_y)),
    };
    assert_eq!((image.width, image.height), placement.target);
    let want = expected_target(format, &pattern.bytes, placement).expect("placement");
    if let Some(diff) = difference(&want, &image.data, bpp, placement.target.0) {
        panic!("swap chain of frame {id}: {diff}");
    }
}

fn metadata(run: &mut Run, surface: &Surface, pattern: &Pattern) {
    let req = request();
    let (status, _) = run.show(surface, pattern, Some(&req));
    let call = probe::last_metadata_call().expect("a metadata call");
    eprintln!("metadata: {status}; {call:?}");
    if run.backend == Backend::Vulkan && !call.error.is_empty() {
        // Without VK_EXT_hdr_metadata the frame shows without metadata.
        assert!(status.contains("HDR metadata NOT sent"), "{status}");
        eprintln!("UNAVAILABLE: Vulkan HDR metadata: {}", call.error);
        return;
    }
    assert_eq!(call.error, "");
    assert_eq!(call.data, expected_metadata(run.backend));
    assert!(
        status.contains("metadata: MaxCLL 800, MaxFALL 200, mastering 0.0050 to 4000 cd/m2"),
        "{status}"
    );

    let (status, _) = run.show(surface, pattern, None);
    let call = probe::last_metadata_call().expect("a metadata call");
    if run.backend == Backend::Vulkan {
        // Vulkan has no call that removes metadata.
        assert!(status.contains("HDR metadata NOT removed"), "{status}");
    } else {
        assert!(status.contains("HDR metadata removed."), "{status}");
        assert!(call.data.is_empty() && call.error.is_empty(), "{call:?}");
    }
}

/// No acknowledgement while the window does not render; the frame
/// uploads and completes at the next render.
fn delayed_submission(run: &mut Run, surface: &Surface, pattern: &Pattern) {
    probe::hold_renders(true);
    let before = probe::queued_frames().len();
    let frame = Frame {
        image: &pattern.image,
        format: pattern.format,
        metadata: None,
    };
    let mut future = run.present(surface, &frame);
    let id = poll_until_queued(&mut future, before);
    assert!(
        poll_for(&mut future, Duration::from_millis(500)).is_none(),
        "no acknowledgement before the render"
    );
    assert!(!probe::uploaded_frames().contains(&id));
    probe::hold_renders(false);
    finish(&mut future).expect("acknowledged after the render");
    drop(future);
    check_swap_chain(id, pattern);
}

/// Of two frames queued before a render, only the second uploads and
/// completes.
fn rapid_replacement(run: &mut Run, surface: &Surface, first: &Pattern, second: &Pattern) {
    probe::hold_renders(true);
    let before = probe::queued_frames().len();
    let frame = Frame {
        image: &first.image,
        format: first.format,
        metadata: None,
    };
    let mut future = run.present(surface, &frame);
    let replaced = poll_until_queued(&mut future, before);
    // The host drops the earlier presentation before the next one.
    drop(future);
    let frame = Frame {
        image: &second.image,
        format: second.format,
        metadata: None,
    };
    let mut future = run.present(surface, &frame);
    let id = poll_until_queued(&mut future, before + 1);
    probe::hold_renders(false);
    finish(&mut future).expect("the replacement is acknowledged");
    drop(future);
    assert!(!probe::uploaded_frames().contains(&replaced));
    check_swap_chain(id, second);
}

/// Presents `pattern` with `fault` injected `count` times in the render of
/// that frame. Returns the outcome and the frame ID.
fn with_fault(
    run: &mut Run,
    surface: &Surface,
    pattern: &Pattern,
    fault: FrameFault,
    count: u32,
) -> (Outcome, u64) {
    probe::hold_renders(true);
    let before = probe::queued_frames().len();
    let frame = Frame {
        image: &pattern.image,
        format: pattern.format,
        metadata: None,
    };
    let mut future = run.present(surface, &frame);
    let id = poll_until_queued(&mut future, before);
    probe::inject_frame_fault(fault, count);
    probe::hold_renders(false);
    let outcome = finish(&mut future);
    drop(future);
    probe::inject_frame_fault(FrameFault::None, 0);
    (outcome, id)
}

fn out_of_date(run: &mut Run, surface: &Surface, a: &Pattern, b: &Pattern) {
    // One: the render resizes the swap chain and begins again. Two: the
    // window retries after a short interval.
    for (count, pattern) in [(1, a), (2, b)] {
        let (outcome, id) = with_fault(run, surface, pattern, FrameFault::BeginOutOfDate, count);
        outcome.unwrap_or_else(|e| panic!("out of date {count} times: {e}"));
        check_swap_chain(id, pattern);
    }
    // Always out of date: the frame times out and does not upload later.
    let (outcome, id) = with_fault(run, surface, a, FrameFault::BeginOutOfDate, u32::MAX);
    assert_eq!(outcome, Err(OutputError::Timeout));
    probe::run_event_loop(100);
    assert!(!probe::uploaded_frames().contains(&id));
    assert!(
        probe::take_swap_chain_image(4).is_err(),
        "no render read back the timed-out frame"
    );
    run.show(surface, b, None);
}

fn render_failures(run: &mut Run, surface: &Surface, shown: &Pattern, next: &Pattern) {
    let cases = [
        (
            FrameFault::BeginDeviceLost,
            "beginFrame failed: graphics device lost",
        ),
        (FrameFault::BeginError, "beginFrame failed"),
        (
            FrameFault::EndDeviceLost,
            "endFrame failed: graphics device lost",
        ),
        (FrameFault::EndError, "endFrame failed"),
        (FrameFault::Upload, "injected upload failure"),
    ];
    for (fault, message) in cases {
        let (outcome, id) = with_fault(run, surface, next, fault, 1);
        assert_eq!(
            outcome,
            Err(OutputError::Platform(message.into())),
            "{fault:?}"
        );
        let uploaded = probe::uploaded_frames().contains(&id);
        match fault {
            FrameFault::EndDeviceLost | FrameFault::EndError => {
                assert!(uploaded, "{fault:?}: the upload ran before endFrame");
                // The swap chain holds the frame, but the host got an error.
                check_swap_chain(id, next);
            }
            FrameFault::Upload => {
                assert!(!uploaded);
                // The previous pattern stays.
                check_swap_chain_after_failed_upload(id, shown);
            }
            _ => {
                assert!(!uploaded, "{fault:?}: the frame does not upload later");
                assert!(probe::take_swap_chain_image(4).is_err());
            }
        }
        // The fault is not permanent: the next frame shows.
        run.show(surface, shown, None);
    }
}

/// After a failed upload, the readback of that render holds the pattern
/// of the previous frame.
fn check_swap_chain_after_failed_upload(id: u64, previous: &Pattern) {
    let format = previous.swap_chain_format();
    let image = probe::take_swap_chain_image(format.bytes_per_pixel()).expect("readback");
    assert_eq!(image.frame, id);
    let record = probe::swap_chain_record().expect("record");
    let size = |v: i32| usize::try_from(v).unwrap();
    let placement = Placement {
        size: (size(record.area_width), size(record.area_height)),
        target: (size(record.width), size(record.height)),
        origin: (size(record.area_x), size(record.area_y)),
    };
    let want = expected_target(format, &previous.bytes, placement).expect("placement");
    if let Some(diff) = difference(
        &want,
        &image.data,
        format.bytes_per_pixel(),
        placement.target.0,
    ) {
        panic!("swap chain after the failed upload of frame {id}: {diff}");
    }
}

/// Esc while the frame waits for its render.
fn close_while_queued(run: &mut Run, surface: &Surface, pattern: &Pattern) {
    probe::hold_renders(true);
    let before = probe::queued_frames().len();
    let frame = Frame {
        image: &pattern.image,
        format: pattern.format,
        metadata: None,
    };
    let mut future = run.present(surface, &frame);
    poll_until_queued(&mut future, before);
    probe::close_pattern_window_later().expect("a window");
    assert_eq!(finish(&mut future), Err(OutputError::Disconnected));
    drop(future);
    probe::hold_renders(false);
}

/// Esc while the worker converts the frame.
fn close_during_conversion(run: &mut Run, surface: &Surface, pattern: &Pattern) {
    let before = probe::queued_frames().len();
    let frame = Frame {
        image: &pattern.image,
        format: pattern.format,
        metadata: None,
    };
    let mut future = run.present(surface, &frame);
    assert!(
        poll_for(&mut future, Duration::ZERO).is_none(),
        "the conversion runs on a worker"
    );
    probe::close_pattern_window_later().expect("a window");
    assert_eq!(finish(&mut future), Err(OutputError::Disconnected));
    drop(future);
    assert_eq!(
        probe::queued_frames().len(),
        before,
        "no frame queued in a closed window"
    );
}

fn check_backend(backend: Backend) {
    let _app = QGuiApplication::new();
    let mut run = Run::start(backend);

    let surface = run.open(BitDepth::Ten);
    let geometry = Pattern::coordinates(surface.size, BitDepth::Ten);
    run.show(&surface, &geometry, None);
    let a = Pattern::fixture(&run.fixtures, "state-frame-a-pq-full-10", surface.size);
    let b = Pattern::fixture(&run.fixtures, "state-frame-b-pq-full-10", surface.size);
    metadata(&mut run, &surface, &a);
    delayed_submission(&mut run, &surface, &b);
    rapid_replacement(&mut run, &surface, &a, &geometry);
    out_of_date(&mut run, &surface, &a, &b);
    run.show(&surface, &a, None);
    render_failures(&mut run, &surface, &a, &b);
    close_while_queued(&mut run, &surface, &b);

    let surface = run.open(BitDepth::Twelve);
    let geometry = Pattern::coordinates(surface.size, BitDepth::Twelve);
    run.show(&surface, &geometry, None);
    let a = Pattern::fixture(&run.fixtures, "state-frame-a-pq-full-12", surface.size);
    let (status, _) = run.show(&surface, &a, Some(&request()));
    eprintln!("12-bit metadata: {status}");
    close_during_conversion(&mut run, &surface, &geometry);
}

#[cfg(windows)]
#[test]
#[ignore = "opens full-screen windows on the first screen, which must have HDR on"]
fn d3d11_swap_chain() {
    check_backend(Backend::D3d11);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "opens full-screen windows on the first screen, which must have HDR on"]
fn metal_swap_chain() {
    check_backend(Backend::Metal);
}

/// On Windows, needs the Vulkan headers at build time (`VULKAN_SDK`).
#[cfg(not(target_os = "macos"))]
#[test]
#[ignore = "opens full-screen windows on the first screen, which must have HDR on"]
fn vulkan_swap_chain() {
    check_backend(Backend::Vulkan);
}
