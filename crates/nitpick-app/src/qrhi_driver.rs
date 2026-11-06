//! `OutputDriver` for the `QRhi` pattern window. See
//! `doc/design/output_driver/qt.md`.
//!
//! Window and `QRhi` calls stay on the Qt GUI thread, where the host polls
//! the `present` future. Pixel conversion and automatic light levels run as
//! worker jobs with owned data. A oneshot channel from the worker and the
//! frame acknowledgement of `submission` wake the task.

use futures_channel::oneshot;
use nitpick_core::driver::{
    DisplayId, DisplayInfo, Frame, Hdr10Request, OutputDriver, OutputError, PresentFuture, Query,
    Surface, check_frame, check_open,
};
use nitpick_core::image::Image;
use nitpick_core::metadata::Hdr10Metadata;
use nitpick_core::output::{SwapChainFormat, swap_chain_bytes};
use nitpick_core::signal::{BitDepth, Format};

use crate::display::{self, Backend, DeviceStates, ffi};
use crate::hdr_metadata::{self, BridgeSink, MetadataSink, NativeMetadata, SwapChainMetadata};
use crate::submission::{self, ACK_TIMEOUT_MS, FrameId};

/// Screens and the pattern window. [`BridgeWindow`] calls the C++ bridge;
/// tests inject a fake. All calls are on the GUI thread.
pub trait PatternWindow: MetadataSink {
    fn screen_count(&self) -> i32;
    /// # Errors
    /// No screen with this index, or a platform failure.
    fn screen_identity(&self, screen: i32) -> Result<ffi::ScreenIdentity, String>;
    /// Pattern area size before the window opens.
    ///
    /// # Errors
    /// No screen with this index, or a platform failure.
    fn screen_size(&self, screen: i32) -> Result<(usize, usize), String>;
    /// Replaces any open window.
    ///
    /// # Errors
    /// The window or its swap chain could not be created.
    fn open(&mut self, screen: i32, fp16: bool) -> Result<ffi::PatternSurface, String>;
    fn close(&mut self);
    /// Queues `pixels` as `frame` and returns. The window reports the
    /// frame to `submission`.
    ///
    /// # Errors
    /// The window rejected the frame.
    fn queue(&mut self, pixels: &[u8], size: (i32, i32), frame: FrameId) -> Result<(), String>;
}

/// The C++ pattern window of `display::ffi`.
#[derive(Default)]
pub struct BridgeWindow;

impl MetadataSink for BridgeWindow {
    fn set(&mut self, metadata: &NativeMetadata) -> Result<(), String> {
        BridgeSink.set(metadata)
    }

    fn clear(&mut self) -> Result<(), String> {
        BridgeSink.clear()
    }
}

impl PatternWindow for BridgeWindow {
    fn screen_count(&self) -> i32 {
        ffi::screen_count()
    }

    fn screen_identity(&self, screen: i32) -> Result<ffi::ScreenIdentity, String> {
        ffi::screen_identity(screen).map_err(|e| e.to_string())
    }

    fn screen_size(&self, screen: i32) -> Result<(usize, usize), String> {
        let size = |v: i32| usize::try_from(v).map_err(|_| format!("invalid screen size {v}"));
        let width = ffi::screen_pixel_width(screen).map_err(|e| e.to_string())?;
        let height = ffi::screen_pixel_height(screen).map_err(|e| e.to_string())?;
        Ok((size(width)?, size(height)?))
    }

    fn open(&mut self, screen: i32, fp16: bool) -> Result<ffi::PatternSurface, String> {
        ffi::open_pattern_window(screen, fp16).map_err(|e| e.to_string())
    }

    fn close(&mut self) {
        ffi::close_pattern_window();
    }

    fn queue(&mut self, pixels: &[u8], size: (i32, i32), frame: FrameId) -> Result<(), String> {
        ffi::present_pattern(pixels, size.0, size.1, frame, ACK_TIMEOUT_MS)
            .map_err(|e| e.to_string())
    }
}

pub type Job = Box<dyn FnOnce() + Send>;

/// Runs conversion jobs off the GUI thread. Tests inject controlled
/// workers.
pub trait Workers {
    fn spawn(&self, job: Job);
}

/// One thread per job. The host has at most one pending frame. A job of
/// a dropped future skips the conversion if it has not started; otherwise
/// it finishes and discards the result.
#[derive(Default)]
pub struct Threads;

impl Workers for Threads {
    fn spawn(&self, job: Job) {
        // A failed spawn drops the job and its sender; the future then
        // fails instead of waiting.
        let _ = std::thread::Builder::new()
            .name("nitpick-convert".into())
            .spawn(job);
    }
}

/// Output of a conversion job.
#[derive(Debug)]
pub struct Converted {
    /// Tightly packed rows in the swap chain format.
    pub pixels: Vec<u8>,
    pub metadata: Option<Hdr10Metadata>,
}

/// Swap chain bytes and resolved metadata of a frame. Runs on a worker.
///
/// # Errors
/// A code exceeds the maximum code of `format.depth`.
pub fn convert(
    image: &Image,
    format: Format,
    request: Option<&Hdr10Request>,
) -> Result<Converted, OutputError> {
    let pixels = swap_chain_bytes(image, format).map_err(|e| OutputError::Unsupported {
        feature: format.to_string(),
        reason: e.to_string(),
    })?;
    Ok(Converted {
        pixels,
        metadata: hdr_metadata::resolve(request, image, format.depth),
    })
}

/// The open pattern window.
struct Opened {
    display: DisplayId,
    depth: BitDepth,
    size: (usize, usize),
    query: Query,
    /// Warnings of `check_open` and the swap chain fallback.
    warnings: Vec<String>,
    description: String,
    metadata: SwapChainMetadata,
}

pub struct QrhiDriver<W = BridgeWindow, J = Threads> {
    backend: Backend,
    window: W,
    workers: J,
    /// Native device reports. The host refreshes them on a worker before
    /// it calls `query` and `open`, which do no native I/O.
    states: DeviceStates,
    /// Result of the last `query`. `open` uses it for the same display, so
    /// that it applies the capabilities the host checked.
    queried: Option<(DisplayId, Query)>,
    opened: Option<Opened>,
}

impl<W: PatternWindow, J: Workers> QrhiDriver<W, J> {
    pub fn new(backend: Backend, window: W, workers: J, states: DeviceStates) -> Self {
        Self {
            backend,
            window,
            workers,
            states,
            queried: None,
            opened: None,
        }
    }

    /// Identity of every screen that has one now, in Qt order. The host
    /// reads their device reports with it.
    pub fn screens(&self) -> Vec<ffi::ScreenIdentity> {
        // A screen that fails here disappeared during enumeration.
        (0..self.window.screen_count())
            .filter_map(|screen| self.window.screen_identity(screen).ok())
            .collect()
    }

    /// The display of the open window.
    pub fn opened(&self) -> Option<&DisplayId> {
        self.opened.as_ref().map(|o| &o.display)
    }

    /// The current screen index of `display`, if the screen exists.
    fn screen(&self, display: &DisplayId) -> Result<i32, OutputError> {
        if display.driver != display::DRIVER {
            return Err(OutputError::Platform(format!(
                "display {} belongs to driver {}",
                display.key, display.driver
            )));
        }
        (0..self.window.screen_count())
            .find(|&s| {
                self.window
                    .screen_identity(s)
                    .is_ok_and(|i| display::screen_key(&i) == display.key)
            })
            .ok_or(OutputError::Disconnected)
    }

    fn device_query(&self, display: &DisplayId) -> Query {
        display::query(self.backend, &self.states.report(&display.key))
    }
}

impl<W: PatternWindow, J: Workers> OutputDriver for QrhiDriver<W, J> {
    fn id(&self) -> &'static str {
        display::DRIVER
    }

    fn displays(&mut self) -> Vec<DisplayInfo> {
        // A screen that fails here disappeared during enumeration.
        (0..self.window.screen_count())
            .filter_map(|screen| {
                let identity = self.window.screen_identity(screen).ok()?;
                Some(DisplayInfo {
                    id: display::display_id(&identity),
                    name: display::screen_label(&identity),
                    size: self.window.screen_size(screen).ok()?,
                })
            })
            .collect()
    }

    fn query(&mut self, id: &DisplayId) -> Result<Query, OutputError> {
        self.screen(id)?;
        let query = self.device_query(id);
        self.queried = Some((id.clone(), query.clone()));
        Ok(query)
    }

    fn open(&mut self, id: &DisplayId, depth: BitDepth) -> Result<Surface, OutputError> {
        let screen = self.screen(id)?;
        let query = match self.queried.take() {
            Some((d, q)) if d == *id => q,
            _ => self.device_query(id),
        };
        let mut warnings = check_open(&query.capabilities, depth)?;
        let fp16 = SwapChainFormat::for_depth(depth) == SwapChainFormat::ScRgb;
        // The C++ open replaces the old window, so the old state goes first.
        self.opened = None;
        let native = self
            .window
            .open(screen, fp16)
            .map_err(OutputError::Platform)?;
        let surface = display::surface(id.clone(), &native)?;
        warnings.extend(display::surface_warning(depth, &native));
        self.opened = Some(Opened {
            display: id.clone(),
            depth,
            size: surface.size,
            query,
            warnings,
            description: surface.description.clone(),
            metadata: SwapChainMetadata::default(),
        });
        Ok(surface)
    }

    fn present<'a>(&'a mut self, surface: &'a Surface, frame: &'a Frame<'a>) -> PresentFuture<'a> {
        Box::pin(async move {
            let Self {
                backend,
                window,
                workers,
                opened,
                ..
            } = self;
            let open = opened
                .as_mut()
                .filter(|o| o.display == surface.display && o.size == surface.size)
                .ok_or(OutputError::Disconnected)?;
            if frame.format.depth != open.depth {
                return Err(OutputError::Unsupported {
                    feature: frame.format.depth.label().into(),
                    reason: format!("the surface was opened for {}", open.depth.label()),
                });
            }
            if (frame.image.width(), frame.image.height()) != open.size {
                return Err(OutputError::Platform(
                    "image size differs from the surface size".into(),
                ));
            }
            let size = i32::try_from(open.size.0)
                .and_then(|w| Ok((w, i32::try_from(open.size.1)?)))
                .map_err(|e| OutputError::Platform(e.to_string()))?;
            let mut warnings = open.warnings.clone();
            warnings.extend(check_frame(
                &open.query.capabilities,
                frame.format,
                frame.metadata,
            )?);

            // The worker owns copies. Dropping the future drops the
            // receiver; the worker then skips or discards its result.
            let (tx, rx) = oneshot::channel();
            let (image, format, request) =
                (frame.image.clone(), frame.format, frame.metadata.copied());
            workers.spawn(Box::new(move || {
                if !tx.is_canceled() {
                    let _ = tx.send(convert(&image, format, request.as_ref()));
                }
            }));
            let converted = rx
                .await
                .map_err(|_| OutputError::Platform("the conversion worker stopped".into()))??;

            // No await from here to the queue: the metadata applies to the
            // frame queued next.
            let md = open
                .metadata
                .apply(window, *backend, converted.metadata.as_ref());
            let (done_tx, done_rx) = oneshot::channel();
            let _registration = submission::present(
                |id| window.queue(&converted.pixels, size, id),
                move |result| {
                    let _ = done_tx.send(result);
                },
            )
            .map_err(OutputError::Platform)?;
            // A sender dropped without a result: another registration
            // replaced this one.
            done_rx.await.map_err(|_| OutputError::Disconnected)??;

            let warnings = if warnings.is_empty() {
                String::new()
            } else {
                format!(" Warnings: {}.", warnings.join("; "))
            };
            Ok(format!(
                "{}. {}. {md}{warnings}",
                open.description, open.query.mode.description
            ))
        })
    }

    fn close(&mut self, id: &DisplayId) {
        if self.opened.as_ref().is_some_and(|o| o.display == *id) {
            self.opened = None;
            self.window.close();
        }
    }
}

#[cfg(test)]
#[path = "qrhi_driver_tests.rs"]
pub(crate) mod tests;
