use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use super::*;
use crate::signal::{D65, Rgb};

fn yes() -> Support {
    Support::Yes
}

fn no(reason: &str) -> Support {
    Support::No(reason.into())
}

fn unknown(reason: &str) -> Support {
    Support::Unknown(reason.into())
}

fn depth(available: Support, exact: Support) -> DepthSupport {
    DepthSupport {
        available,
        path: "mock path".into(),
        exact,
    }
}

fn encoding(input: Support, interpretation: Support, auto: Support) -> EncodingSupport {
    EncodingSupport {
        input,
        interpretation,
        auto_light_level: auto,
    }
}

/// All features `Yes`, both depths and all encodings reported.
fn full() -> Capabilities {
    Capabilities {
        depths: BitDepth::ALL
            .into_iter()
            .map(|d| (d, depth(yes(), yes())))
            .collect(),
        encodings: Encoding::ALL
            .into_iter()
            .map(|e| (e, encoding(yes(), yes(), yes())))
            .collect(),
        hdr_metadata: yes(),
        pixel_exact: yes(),
    }
}

fn set_depth(caps: &mut Capabilities, d: BitDepth, s: DepthSupport) {
    caps.depths.iter_mut().find(|(k, _)| *k == d).unwrap().1 = s;
}

fn set_encoding(caps: &mut Capabilities, e: Encoding, s: EncodingSupport) {
    caps.encodings.iter_mut().find(|(k, _)| *k == e).unwrap().1 = s;
}

fn request(light_level: ContentLightLevel) -> Hdr10Request {
    Hdr10Request {
        primaries: [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
        white: D65,
        max_mastering_nits: 1000.0,
        min_mastering_nits: 0.0001,
        light_level,
    }
}

const EXPLICIT: ContentLightLevel = ContentLightLevel::Explicit {
    max_cll: 1000.0,
    max_fall: 400.0,
};

const PQ10: Format = Format::new(Encoding::PqFull, BitDepth::Ten);

fn assert_unsupported(r: &Result<Vec<String>, OutputError>, feature: &str, reason: &str) {
    assert_eq!(
        r,
        &Err(OutputError::Unsupported {
            feature: feature.into(),
            reason: reason.into()
        })
    );
}

#[test]
fn open_all_yes_has_no_warning() {
    for d in BitDepth::ALL {
        assert_eq!(check_open(&full(), d), Ok(vec![]));
    }
}

#[test]
fn open_rejects_unavailable_depth() {
    let mut caps = full();
    set_depth(
        &mut caps,
        BitDepth::Twelve,
        depth(no("scale unknown"), yes()),
    );
    assert_unsupported(
        &check_open(&caps, BitDepth::Twelve),
        "12-bit",
        "scale unknown",
    );
    assert_eq!(check_open(&caps, BitDepth::Ten), Ok(vec![]));
}

#[test]
fn open_rejects_unreported_depth() {
    let mut caps = full();
    caps.depths.retain(|(d, _)| *d == BitDepth::Ten);
    assert_unsupported(
        &check_open(&caps, BitDepth::Twelve),
        "12-bit",
        "not reported by the driver",
    );
}

#[test]
fn open_warns_on_unknown_availability() {
    let mut caps = full();
    set_depth(&mut caps, BitDepth::Ten, depth(unknown("no report"), yes()));
    assert_eq!(
        check_open(&caps, BitDepth::Ten),
        Ok(vec!["10-bit (mock path): unknown (no report)".into()])
    );
}

#[test]
fn open_warns_on_precision_limits() {
    for (exact, text) in [
        (no("FP16 rounding"), "no (FP16 rounding)"),
        (unknown("compositor"), "unknown (compositor)"),
    ] {
        let mut caps = full();
        set_depth(&mut caps, BitDepth::Twelve, depth(yes(), exact));
        assert_eq!(
            check_open(&caps, BitDepth::Twelve),
            Ok(vec![format!("12-bit (mock path) exact codes: {text}")])
        );
    }
}

#[test]
fn open_warns_on_pixel_mapping() {
    for (s, text) in [
        (no("ratio 1.5"), "no (ratio 1.5)"),
        (unknown("zoom"), "unknown (zoom)"),
    ] {
        let mut caps = full();
        caps.pixel_exact = s;
        assert_eq!(
            check_open(&caps, BitDepth::Ten),
            Ok(vec![format!("pixel-exact layout: {text}")])
        );
    }
}

#[test]
fn frame_all_yes_has_no_warning() {
    for e in Encoding::ALL {
        let f = Format::new(e, BitDepth::Ten);
        assert_eq!(check_frame(&full(), f, None), Ok(vec![]));
        let r = request(ContentLightLevel::FromFrame);
        assert_eq!(check_frame(&full(), f, Some(&r)), Ok(vec![]));
    }
}

#[test]
fn frame_rejects_unsupported_input() {
    let mut caps = full();
    set_encoding(
        &mut caps,
        Encoding::HlgNarrow,
        encoding(no("PQ only"), yes(), yes()),
    );
    let f = Format::new(Encoding::HlgNarrow, BitDepth::Ten);
    assert_unsupported(&check_frame(&caps, f, None), "HLG narrow range", "PQ only");
}

#[test]
fn frame_rejects_unreported_encoding() {
    let mut caps = full();
    caps.encodings.retain(|(e, _)| *e == Encoding::PqFull);
    let f = Format::new(Encoding::PqNarrow, BitDepth::Ten);
    assert_unsupported(
        &check_frame(&caps, f, None),
        "PQ narrow range",
        "not reported by the driver",
    );
}

#[test]
fn frame_warns_on_unknown_input() {
    let mut caps = full();
    set_encoding(
        &mut caps,
        Encoding::PqFull,
        encoding(unknown("no report"), yes(), yes()),
    );
    assert_eq!(
        check_frame(&caps, PQ10, None),
        Ok(vec!["PQ full range input: unknown (no report)".into()])
    );
}

#[test]
fn frame_accepts_interpretation_limits() {
    for (s, text) in [
        (no("PQ signalling"), "no (PQ signalling)"),
        (unknown("browser"), "unknown (browser)"),
    ] {
        let mut caps = full();
        set_encoding(&mut caps, Encoding::HlgNarrow, encoding(yes(), s, yes()));
        let f = Format::new(Encoding::HlgNarrow, BitDepth::Twelve);
        assert_eq!(
            check_frame(&caps, f, None),
            Ok(vec![format!("HLG narrow range interpretation: {text}")])
        );
    }
}

#[test]
fn frame_rejects_unsupported_metadata() {
    let mut caps = full();
    caps.hdr_metadata = no("no API");
    for l in [ContentLightLevel::FromFrame, EXPLICIT] {
        let r = request(l);
        assert_unsupported(
            &check_frame(&caps, PQ10, Some(&r)),
            "HDR10 metadata",
            "no API",
        );
    }
    // Without a request, the metadata report has no effect.
    assert_eq!(check_frame(&caps, PQ10, None), Ok(vec![]));
}

#[test]
fn frame_warns_on_unknown_metadata() {
    let mut caps = full();
    caps.hdr_metadata = unknown("effect");
    let r = request(EXPLICIT);
    assert_eq!(
        check_frame(&caps, PQ10, Some(&r)),
        Ok(vec!["HDR10 metadata: unknown (effect)".into()])
    );
    assert_eq!(check_frame(&caps, PQ10, None), Ok(vec![]));
}

#[test]
fn auto_light_level_rules() {
    let mut caps = full();
    set_encoding(
        &mut caps,
        Encoding::HlgNarrow,
        encoding(yes(), yes(), no("HLG is relative")),
    );
    set_encoding(
        &mut caps,
        Encoding::PqNarrow,
        encoding(yes(), yes(), unknown("rounding")),
    );
    let hlg = Format::new(Encoding::HlgNarrow, BitDepth::Ten);
    let pq = Format::new(Encoding::PqNarrow, BitDepth::Ten);
    let auto = request(ContentLightLevel::FromFrame);
    let explicit = request(EXPLICIT);

    assert_unsupported(
        &check_frame(&caps, hlg, Some(&auto)),
        "HLG narrow range automatic light level",
        "HLG is relative",
    );
    // Explicit values bypass the calculation.
    assert_eq!(check_frame(&caps, hlg, Some(&explicit)), Ok(vec![]));
    assert_eq!(
        check_frame(&caps, pq, Some(&auto)),
        Ok(vec![
            "PQ narrow range automatic light level: unknown (rounding)".into()
        ])
    );
    assert_eq!(check_frame(&caps, pq, Some(&explicit)), Ok(vec![]));
}

#[test]
fn warnings_combine_in_order() {
    let mut caps = full();
    caps.hdr_metadata = unknown("m");
    set_encoding(
        &mut caps,
        Encoding::PqFull,
        encoding(unknown("i"), no("t"), unknown("a")),
    );
    let r = request(ContentLightLevel::FromFrame);
    assert_eq!(
        check_frame(&caps, PQ10, Some(&r)),
        Ok(vec![
            "PQ full range input: unknown (i)".into(),
            "PQ full range interpretation: no (t)".into(),
            "HDR10 metadata: unknown (m)".into(),
            "PQ full range automatic light level: unknown (a)".into(),
        ])
    );
}

#[test]
fn error_display() {
    assert_eq!(
        unsupported("12-bit", "scale unknown").to_string(),
        "12-bit not supported: scale unknown"
    );
    assert_eq!(
        OutputError::Disconnected.to_string(),
        "display disconnected"
    );
}

/// Mock driver. Its present future waits for one simulated callback, then
/// resolves. The `Rc` state makes the future non-`Send`.
struct Mock {
    caps: Capabilities,
    open: Option<DisplayId>,
    presented: Rc<Cell<usize>>,
}

const MOCK: &str = "mock";

fn mock_id() -> DisplayId {
    DisplayId {
        driver: MOCK,
        key: "0".into(),
    }
}

impl Mock {
    fn new(caps: Capabilities) -> Self {
        Self {
            caps,
            open: None,
            presented: Rc::new(Cell::new(0)),
        }
    }

    fn known(display: &DisplayId) -> Result<(), OutputError> {
        if *display == mock_id() {
            Ok(())
        } else {
            Err(OutputError::Disconnected)
        }
    }
}

impl OutputDriver for Mock {
    fn id(&self) -> &'static str {
        MOCK
    }

    fn displays(&mut self) -> Vec<DisplayInfo> {
        vec![DisplayInfo {
            id: mock_id(),
            name: "Mock display".into(),
            size: (4, 2),
        }]
    }

    fn query(&mut self, display: &DisplayId) -> Result<Query, OutputError> {
        Self::known(display)?;
        Ok(Query {
            capabilities: self.caps.clone(),
            mode: ModeSettings {
                hdr_on: None,
                gamut: Some(Gamut::Rec2020),
                wire_depth: None,
                peak_nits: None,
                min_nits: None,
                sdr_white_nits: None,
                pixel_ratio: 1.0,
                description: "mock".into(),
            },
        })
    }

    fn open(&mut self, display: &DisplayId, depth: BitDepth) -> Result<Surface, OutputError> {
        Self::known(display)?;
        let warnings = check_open(&self.caps, depth)?;
        self.open = Some(display.clone());
        Ok(Surface {
            display: display.clone(),
            size: (4, 2),
            description: warnings.join("; "),
        })
    }

    fn present<'a>(&'a mut self, surface: &'a Surface, frame: &'a Frame<'a>) -> PresentFuture<'a> {
        let presented = Rc::clone(&self.presented);
        let checked = if self.open.as_ref() == Some(&surface.display) {
            check_frame(&self.caps, frame.format, frame.metadata)
        } else {
            Err(OutputError::Disconnected)
        };
        let mut called_back = false;
        Box::pin(std::future::poll_fn(move |cx| {
            let warnings = match &checked {
                Ok(w) => w,
                Err(e) => return Poll::Ready(Err(e.clone())),
            };
            if !called_back {
                // A real driver stores the waker and calls it from a callback.
                called_back = true;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            presented.set(presented.get() + 1);
            let head = format!("{}x{} submitted", frame.image.width(), frame.image.height());
            Poll::Ready(Ok(std::iter::once(&head)
                .chain(warnings)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("; ")))
        }))
    }

    fn close(&mut self, display: &DisplayId) {
        if self.open.as_ref() == Some(display) {
            self.open = None;
        }
    }
}

#[derive(Default)]
struct CountWaker(AtomicUsize);

impl Wake for CountWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Minimal local executor: polls on this thread only when woken.
/// Returns the output and the number of polls.
fn run_local<F: Future>(fut: F) -> (F::Output, usize) {
    let counter = Arc::new(CountWaker::default());
    let waker = Waker::from(Arc::clone(&counter));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    let mut polls = 0;
    loop {
        polls += 1;
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return (v, polls);
        }
        assert_eq!(
            counter.0.swap(0, Ordering::SeqCst),
            1,
            "pending without a wake"
        );
    }
}

#[test]
fn dyn_driver_on_local_executor() {
    let mut mock = Mock::new(full());
    let presented = Rc::clone(&mock.presented);
    let driver: &mut dyn OutputDriver = &mut mock;
    assert_eq!(driver.id(), MOCK);
    let info = driver.displays().remove(0);
    let q = driver.query(&info.id).unwrap();
    assert_eq!(q.mode.gamut, Some(Gamut::Rec2020));
    let surface = driver.open(&info.id, BitDepth::Ten).unwrap();
    let image = Image::new(surface.size.0, surface.size.1, Rgb::default());
    let r = request(ContentLightLevel::FromFrame);
    let frame = Frame {
        image: &image,
        format: PQ10,
        metadata: Some(&r),
    };
    let (status, polls) = run_local(driver.present(&surface, &frame));
    assert_eq!(status, Ok("4x2 submitted".into()));
    assert_eq!(polls, 2);
    assert_eq!(presented.get(), 1);
    driver.close(&info.id);
    let (status, _) = run_local(driver.present(&surface, &frame));
    assert_eq!(status, Err(OutputError::Disconnected));
}

#[test]
fn dyn_driver_rejects_and_warns() {
    let mut caps = full();
    set_depth(&mut caps, BitDepth::Twelve, depth(no("scale"), yes()));
    set_encoding(
        &mut caps,
        Encoding::HlgNarrow,
        encoding(yes(), no("PQ signalling"), no("relative")),
    );
    caps.hdr_metadata = no("no API");
    let mut mock = Mock::new(caps);
    let driver: &mut dyn OutputDriver = &mut mock;
    let id = mock_id();
    assert!(matches!(
        driver.open(&id, BitDepth::Twelve),
        Err(OutputError::Unsupported { .. })
    ));
    let other = DisplayId {
        driver: MOCK,
        key: "gone".into(),
    };
    assert_eq!(driver.query(&other), Err(OutputError::Disconnected));

    let surface = driver.open(&id, BitDepth::Ten).unwrap();
    let image = Image::new(4, 2, Rgb::default());
    let hlg = Format::new(Encoding::HlgNarrow, BitDepth::Ten);
    let frame = Frame {
        image: &image,
        format: hlg,
        metadata: None,
    };
    let (status, _) = run_local(driver.present(&surface, &frame));
    assert_eq!(
        status,
        Ok("4x2 submitted; HLG narrow range interpretation: no (PQ signalling)".into())
    );
    let r = request(EXPLICIT);
    let frame = Frame {
        metadata: Some(&r),
        ..frame
    };
    let (status, _) = run_local(driver.present(&surface, &frame));
    assert!(matches!(status, Err(OutputError::Unsupported { .. })));
}

#[test]
fn dropped_future_does_not_complete() {
    let mut mock = Mock::new(full());
    let presented = Rc::clone(&mock.presented);
    let surface = mock.open(&mock_id(), BitDepth::Ten).unwrap();
    let image = Image::new(4, 2, Rgb::default());
    let frame = Frame {
        image: &image,
        format: PQ10,
        metadata: None,
    };
    {
        let mut fut = mock.present(&surface, &frame);
        let waker = Waker::from(Arc::new(CountWaker::default()));
        let mut cx = Context::from_waker(&waker);
        assert!(fut.as_mut().poll(&mut cx).is_pending());
    }
    assert_eq!(presented.get(), 0);
    // The borrow ends with the dropped future, so the driver can close.
    mock.close(&mock_id());
    assert_eq!(mock.open, None);
}
