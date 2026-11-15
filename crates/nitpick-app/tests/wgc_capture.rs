//! Windows Graphics Capture of the real pattern window through the capture
//! adapter of the test host: stored FP16 samples, row layout, timestamps,
//! window identity, resize and API failure.
//!
//! The test opens full-screen windows on the first screen, which must have
//! HDR on. It captures only its own pattern window and changes no display
//! setting. It is ignored by default; run it with
//! `cargo nextest run -p nitpick-app --test wgc_capture --run-ignored only`.
#![cfg(windows)]

use std::cmp::Ordering;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use cxx_qt_lib::QGuiApplication;
use futures_util::StreamExt as _;
use half::f16;
use nitpick_app::pattern_capture::{self, pattern_window_handle};
use nitpick_app::qrhi_driver::{BridgeWindow, QrhiDriver, Threads};
use nitpick_app::swap_chain_probe as probe;
use nitpick_app::{Backend, DeviceStates, device_reports};
use nitpick_capture::wgc::WgcAdapter;
use nitpick_core::driver::{DisplayId, Frame, OutputDriver, Surface};
use nitpick_core::image::Image;
use nitpick_core::output::{SwapChainFormat, swap_chain_bytes};
use nitpick_core::signal::{BitDepth, Encoding, Format, Rgb};
use nitpick_test_host::capture::{
    Capture, CaptureAdapter, CaptureFrames, CaptureTarget, Channel, Cursor, Rect, Sample, Source,
    SourceKind, Timestamp,
};
use serde_json::json;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
};

const WAIT: Duration = Duration::from_secs(20);

type Driver = QrhiDriver<BridgeWindow, Threads>;

/// Tiles of 61×59 colours, as in the swap-chain test. At 12 bits many
/// colours are outside BT.709 (negative scRGB components) or above
/// 80 cd/m² (components above 1).
fn coordinates(size: (usize, usize)) -> Image {
    let n = usize::from(BitDepth::Twelve.max_code()) + 1;
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
    image
}

fn open(driver: &mut Driver, display: &DisplayId) -> Surface {
    driver.query(display).expect("query");
    driver.open(display, BitDepth::Twelve).expect("open")
}

fn show(driver: &mut Driver, surface: &Surface, image: &Image) {
    let frame = Frame {
        image,
        format: Format::new(Encoding::PqFull, BitDepth::Twelve),
        metadata: None,
    };
    let mut future = driver.present(surface, &frame);
    let start = Instant::now();
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            result.expect("acknowledged");
            return;
        }
        assert!(start.elapsed() < WAIT, "no acknowledgement");
        probe::run_event_loop(2);
    }
}

/// The next item of the session; the Qt event loop runs while it waits.
/// Each `nudge_ms` it asks the window for a render, so that WGC gets a new
/// frame of the static window.
fn next(frames: &mut CaptureFrames, nudge_ms: Option<u64>) -> Result<Capture, String> {
    let start = Instant::now();
    let mut nudged = Instant::now();
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        match frames.poll_next_unpin(&mut cx) {
            Poll::Ready(Some(item)) => return item,
            Poll::Ready(None) => return Err("the session ended".into()),
            Poll::Pending => {}
        }
        assert!(start.elapsed() < WAIT, "no capture in time");
        if let Some(ms) = nudge_ms
            && nudged.elapsed() >= Duration::from_millis(ms)
        {
            probe::hold_renders(false);
            nudged = Instant::now();
        }
        probe::run_event_loop(2);
    }
}

fn target(display: &DisplayId) -> CaptureTarget<'_> {
    CaptureTarget {
        display,
        frame: 1,
        generation: 1,
    }
}

fn f16_at(bytes: &[u8], i: usize) -> f16 {
    f16::from_le_bytes([bytes[2 * i], bytes[2 * i + 1]])
}

/// The capture holds the submitted binary16 words at each pixel. Prints the
/// first difference and returns the number of different pixels.
fn compare(capture: &Capture, want: &[u8], size: (usize, usize)) -> usize {
    let view = capture.validate().expect("valid descriptor");
    let pitch = capture.descriptor.planes[0].row_pitch;
    let plane = &capture.planes[0];
    let row = size.0 * 8;
    let mut differ = 0;
    for y in 0..size.1 {
        let got = &plane[y * pitch..y * pitch + row];
        let want = &want[y * row..(y + 1) * row];
        for x in (0..size.0).filter(|&x| got[x * 8..x * 8 + 8] != want[x * 8..x * 8 + 8]) {
            if differ == 0 {
                eprintln!(
                    "first difference at ({x}, {y}): got {:02x?}, want {:02x?}",
                    &got[x * 8..x * 8 + 8],
                    &want[x * 8..x * 8 + 8]
                );
            }
            differ += 1;
        }
    }
    // The descriptor reads the same samples as the byte comparison, also
    // negative values and values above 1.
    let channels = [Channel::R, Channel::G, Channel::B];
    let mut negative = None;
    let mut above_one = None;
    for i in 0..size.0 * size.1 {
        for (c, &channel) in channels.iter().enumerate() {
            let v = f16_at(want, i * 4 + c).to_f32();
            if v < 0.0 && negative.is_none() {
                negative = Some((i, channel, v));
            }
            if v > 1.0 && above_one.is_none() {
                above_one = Some((i, channel, v));
            }
        }
    }
    for (i, channel, v) in [negative, above_one].map(|s| s.expect("the pattern has such a value")) {
        let (x, y) = (i % size.0, i / size.0);
        let got = view.sample(channel, x, y).expect("in the image");
        eprintln!("({x}, {y}) {channel:?}: submitted {v}, captured {got:?}");
        if differ == 0 {
            assert_eq!(got, Sample::Float(v));
        }
    }
    differ
}

fn check_identity(capture: &Capture, handle: isize) {
    assert_eq!(
        capture.descriptor.source,
        Source {
            kind: SourceKind::Window,
            native: format!("hwnd:{handle:#x}"),
        }
    );
    assert_eq!(capture.raw["display_name"], json!("nitpick pattern"));
    assert_eq!(capture.raw["window_process"], json!(std::process::id()));
    assert_eq!(capture.config["pixel_format"], json!("R16G16B16A16Float"));
    assert_eq!(capture.descriptor.cursor, Cursor::Hidden);
    assert!(
        capture.descriptor.colour.transfer.is_none()
            && capture.descriptor.colour.primaries.is_none()
            && capture.descriptor.colour.luminance.is_none(),
        "no colour contract from FP16 storage"
    );
}

fn check_times(capture: &Capture, before: &Timestamp, after: &Timestamp) {
    let t = &capture.descriptor.timestamps;
    let frame = t.frame.expect("a frame time");
    let arrival = t.arrival.expect("an arrival time");
    eprintln!("QPC before {before:?}; frame {frame:?}; arrival {arrival:?}; after {after:?}");
    assert_eq!(frame.cmp_time(before), Some(Ordering::Greater));
    assert_ne!(arrival.cmp_time(after), Some(Ordering::Greater));
    // The frame time can be after the arrival: it was up to 11 ms later
    // on a 60 Hz screen, as for the target present time of the
    // composition. A time in another base would be far away.
    let seconds = |t: &Timestamp| {
        #[allow(clippy::cast_precision_loss)]
        let s = t.ticks as f64 / t.ticks_per_second as f64;
        s
    };
    let lead = seconds(&frame) - seconds(&arrival);
    eprintln!("frame time - arrival: {:.3} ms", lead * 1e3);
    assert!(lead.abs() < 0.1, "the frame time is near the QPC time");
}

/// Makes the window `by` pixels smaller in each direction, as a user
/// resize does; the swap chain follows at the next render.
fn shrink(handle: isize, size: (usize, usize), by: usize) -> (usize, usize) {
    let new = (size.0 - by, size.1 - by);
    // SAFETY: `handle` is the live pattern window of this process.
    unsafe {
        SetWindowPos(
            HWND(handle as *mut _),
            None,
            0,
            0,
            i32::try_from(new.0).unwrap(),
            i32::try_from(new.1).unwrap(),
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    }
    .expect("SetWindowPos");
    new
}

/// After a resize, the frames keep the pool size with the new content size
/// until the adapter recreates the pool; then the texture has the new size.
fn check_resize(frames: &mut CaptureFrames, handle: isize, size: (usize, usize)) {
    let new = shrink(handle, size, 64);
    let mut cut = false;
    let resized = loop {
        let c = next(frames, Some(100)).expect("a frame after the resize");
        let d = &c.descriptor;
        eprintln!(
            "after resize: texture {}×{}, content {:?}, raw content {}",
            d.width, d.height, d.content, c.raw["content_size"]
        );
        if (d.width, d.height) == new {
            break c;
        }
        if c.raw["content_size"] == json!([new.0, new.1]) {
            assert_eq!((d.width, d.height), size);
            assert_eq!(d.content, Some(Rect::new(0, 0, new.0, new.1)));
            c.validate().expect("valid descriptor");
            cut = true;
        }
    };
    assert!(
        cut,
        "a frame with the old pool size and the new content size"
    );
    assert_eq!(
        resized.descriptor.content,
        Some(Rect::new(0, 0, new.0, new.1))
    );
    resized.validate().expect("valid descriptor");
    check_identity(&resized, handle);
}

#[test]
#[ignore = "opens full-screen windows on the first screen, which must have HDR on"]
fn captures_the_pattern_window() {
    nitpick_app::test_host::use_timer_update_requests();
    let _app = QGuiApplication::new();
    let states = DeviceStates::default();
    let mut driver = QrhiDriver::new(Backend::D3d11, BridgeWindow, Threads, states.clone());
    let refresh = states.begin();
    states.apply(refresh, device_reports(&driver.screens()));
    let display = driver.displays().first().expect("a screen").id.clone();
    let mode = driver.query(&display).expect("query").mode;
    eprintln!("display: {}", mode.description);
    assert_eq!(mode.hdr_on, Some(true), "the first screen must have HDR on");

    let surface = open(&mut driver, &display);
    eprintln!("surface: {}", surface.description);
    let size = surface.size;
    let image = coordinates(size);
    let format = Format::new(Encoding::PqFull, BitDepth::Twelve);
    assert_eq!(
        SwapChainFormat::for_depth(format.depth),
        SwapChainFormat::ScRgb
    );
    let want = swap_chain_bytes(&image, format).expect("valid codes");

    let mut adapter = pattern_capture::adapter();
    let before = adapter.now().expect("QPC");
    show(&mut driver, &surface, &image);
    let handle = pattern_window_handle().expect("a window");
    let mut frames = adapter.start(&target(&display)).expect("a session");
    let capture = next(&mut frames, None).expect("a frame");
    let after = adapter.now().expect("QPC");
    eprintln!(
        "descriptor: {:?}\nraw: {:?}\nconfig: {:?}",
        capture.descriptor, capture.raw, capture.config
    );
    assert_eq!(
        (capture.descriptor.width, capture.descriptor.height),
        size,
        "the frame pool has the window size"
    );
    assert_eq!(
        capture.descriptor.content,
        Some(Rect::new(0, 0, size.0, size.1))
    );
    check_identity(&capture, handle);
    check_times(&capture, &before, &after);
    let differ = compare(&capture, &want, size);
    eprintln!(
        "{differ} of {} pixels differ from the submitted words",
        size.0 * size.1
    );
    assert_eq!(differ, 0, "the capture keeps the submitted binary16 words");

    check_resize(&mut frames, handle, size);

    // A new window ends the session of the old one, after the frames that
    // are still queued.
    let surface = open(&mut driver, &display);
    let end = loop {
        match next(&mut frames, None) {
            Ok(c) => assert_eq!(c.descriptor.source.native, format!("hwnd:{handle:#x}")),
            Err(e) => break e,
        }
    };
    assert_eq!(end, "the captured window closed");
    drop(frames);
    let new_handle = pattern_window_handle().expect("a window");
    show(&mut driver, &surface, &image);
    let mut frames = adapter.start(&target(&display)).expect("a session");
    let capture = next(&mut frames, None).expect("a frame");
    check_identity(&capture, new_handle);
    drop(frames);

    // The closed window has no capture item.
    if new_handle != handle {
        let mut stale = WgcAdapter::new(Box::new(move |_| Ok(handle)));
        let reason = stale
            .start(&target(&display))
            .err()
            .expect("no capture of a closed window");
        eprintln!("closed window: {reason}");
        assert!(reason.starts_with("CreateForWindow("), "{reason}");
    }
    driver.close(&display);
}
