//! Host lifecycle of the `QRhi` driver on the Qt GUI thread: device
//! refresh on a worker, the synchronous `query` and `open`, asynchronous
//! presentation, the measurement delay and cancellation. `launcher.rs`
//! connects it to QML. See "Asynchronous presentation" and "Measurement
//! delay" in `doc/design/output_driver/overview.md`.

use std::cell::{Cell, RefCell};
use std::future::{self, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;

use futures_channel::oneshot;
use nitpick_core::driver::{
    DisplayId, DisplayInfo, Frame, Hdr10Request, OutputDriver, OutputLimits, Surface,
};
use nitpick_core::image::Image;
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::Format;

use crate::delay::{DelayTimer, QtTimer};
use crate::display::{self, Backend, DeviceReports, DeviceStates, ffi};
use crate::gui_executor::{self, LocalTask, Post};
use crate::qrhi_driver::{BridgeWindow, PatternWindow, QrhiDriver, Threads, Workers};
use crate::submission;

/// Change for the host UI. Delivered on the GUI thread, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Status(String),
    /// New device reports: the reference table can change.
    Devices,
}

/// Reads device reports off the GUI thread. Tests inject a held reader.
pub trait DeviceReader {
    fn read(&self, screens: Vec<ffi::ScreenIdentity>) -> oneshot::Receiver<DeviceReports>;
}

/// The native device backend, on a new thread per refresh.
pub struct ThreadReader;

impl DeviceReader for ThreadReader {
    fn read(&self, screens: Vec<ffi::ScreenIdentity>) -> oneshot::Receiver<DeviceReports> {
        display::read_device_reports(screens)
    }
}

/// One "Show pattern" request.
#[derive(Debug, Clone)]
pub struct ShowRequest {
    pub kind: PatternKind,
    pub format: Format,
    pub metadata: Option<Hdr10Request>,
    pub display: DisplayId,
    /// Measurement delay after the acknowledgement, in ms.
    pub delay_ms: u32,
}

/// The driver is borrowed. This occurs when the nested event loop of
/// `open` runs a host event; call again from a later event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy;

struct Shared {
    /// Counts presentation requests and cancellations. A status of an
    /// older request does not apply.
    generation: Cell<u64>,
    events: Box<dyn Fn(Event)>,
}

impl Shared {
    fn next_generation(&self) -> u64 {
        self.generation.set(self.generation.get() + 1);
        self.generation.get()
    }

    fn status(&self, generation: u64, text: String) {
        if self.generation.get() == generation {
            (self.events)(Event::Status(text));
        }
    }

    fn apply(&self, states: &DeviceStates, refresh: u64, reports: DeviceReports) {
        if states.apply(refresh, reports) {
            (self.events)(Event::Devices);
        }
    }
}

pub struct Session<W = BridgeWindow, J = Threads> {
    /// Presentation of the last request. Declared before `driver`: it
    /// holds the driver borrow until it completes, and dropping it cancels
    /// the presentation.
    pending: Option<LocalTask>,
    /// Device refresh for the display list and the reference table.
    refreshing: Option<LocalTask>,
    driver: Rc<RefCell<QrhiDriver<W, J>>>,
    states: DeviceStates,
    backend: Backend,
    reader: Box<dyn DeviceReader>,
    timer: Rc<dyn DelayTimer>,
    post: Arc<dyn Post>,
    shared: Rc<Shared>,
    /// `displays` of the last refresh.
    displays: Vec<DisplayInfo>,
}

impl Session {
    #[must_use]
    pub fn native(post: Arc<dyn Post>, events: impl Fn(Event) + 'static) -> Self {
        Self::new(
            Backend::current(),
            BridgeWindow,
            Threads,
            Box::new(ThreadReader),
            Rc::new(QtTimer),
            post,
            events,
        )
    }
}

impl<W: PatternWindow + 'static, J: Workers + 'static> Session<W, J> {
    pub fn new(
        backend: Backend,
        window: W,
        workers: J,
        reader: Box<dyn DeviceReader>,
        timer: Rc<dyn DelayTimer>,
        post: Arc<dyn Post>,
        events: impl Fn(Event) + 'static,
    ) -> Self {
        let states = DeviceStates::default();
        Self {
            pending: None,
            refreshing: None,
            driver: Rc::new(RefCell::new(QrhiDriver::new(
                backend,
                window,
                workers,
                states.clone(),
            ))),
            states,
            backend,
            reader,
            timer,
            post,
            shared: Rc::new(Shared {
                generation: Cell::new(0),
                events: Box::new(events),
            }),
            displays: Vec::new(),
        }
    }

    #[must_use]
    pub fn displays(&self) -> &[DisplayInfo] {
        &self.displays
    }

    #[cfg(test)]
    pub fn driver(&self) -> &Rc<RefCell<QrhiDriver<W, J>>> {
        &self.driver
    }

    /// Drops the pending presentation or measurement delay. Its late
    /// results do not reach the status line.
    fn cancel(&mut self, reason: &str) {
        let generation = self.shared.next_generation();
        if let Some(task) = self.pending.take()
            && !task.is_finished()
        {
            drop(task);
            self.shared
                .status(generation, format!("Cancelled: {reason}"));
        }
    }

    /// A display change: the screens or the selected display changed.
    /// Cancels the pending presentation, lists the displays and reads
    /// their device state on a worker. Closes the window if its screen is
    /// gone.
    ///
    /// # Errors
    /// [`Busy`]; the pending presentation is cancelled already.
    pub fn refresh(&mut self) -> Result<(), Busy> {
        self.cancel("display changed");
        let mut driver = self.driver.try_borrow_mut().map_err(|_| Busy)?;
        self.displays = driver.displays();
        let screens = driver.screens();
        if let Some(id) = driver.opened().cloned()
            && !self.displays.iter().any(|d| d.id == id)
        {
            driver.close(&id);
        }
        drop(driver);
        let refresh = self.states.begin();
        let reports = self.reader.read(screens);
        let (states, shared) = (self.states.clone(), Rc::clone(&self.shared));
        // A newer refresh replaces this one.
        self.refreshing = Some(gui_executor::spawn_local(
            Arc::clone(&self.post),
            async move {
                if let Ok(reports) = reports.await {
                    shared.apply(&states, refresh, reports);
                }
            },
        ));
        Ok(())
    }

    /// Replaces the pending presentation. Reads the device state on a
    /// worker, then queries, opens and renders at the surface size,
    /// presents and waits for the measurement delay. The status follows
    /// each step. A closed pattern window cancels the delay.
    // The task holds the driver borrow across the await. Each request
    // drops the task before it borrows the driver again.
    #[allow(clippy::await_holding_refcell_ref)]
    pub fn show(&mut self, request: ShowRequest) {
        // Dropping the earlier task cancels its presentation and releases
        // its driver borrow before the window closes, so that its
        // `Disconnected` does not reach the status line.
        self.pending = None;
        let generation = self.shared.next_generation();
        let Ok(screens) = self.driver.try_borrow().map(|d| d.screens()) else {
            self.shared
                .status(generation, "Error: the display is busy".into());
            return;
        };
        let refresh = self.states.begin();
        let reports = self.reader.read(screens);
        self.shared
            .status(generation, "Reading display state".into());
        let (driver, states, shared, timer) = (
            Rc::clone(&self.driver),
            self.states.clone(),
            Rc::clone(&self.shared),
            Rc::clone(&self.timer),
        );
        self.pending = Some(gui_executor::spawn_local(
            Arc::clone(&self.post),
            async move {
                let Ok(reports) = reports.await else {
                    shared.status(generation, "Error: the display state reader stopped".into());
                    return;
                };
                shared.apply(&states, refresh, reports);
                let (shown, close_count) = {
                    let mut driver = driver.borrow_mut();
                    let Prepared {
                        surface,
                        image,
                        label,
                    } = match prepare(&mut *driver, &request) {
                        Ok(p) => p,
                        Err(e) => return shared.status(generation, format!("Error: {e}")),
                    };
                    // After `open`, which can close an earlier window. A
                    // close from here on ends the delay.
                    let close_count = submission::close_count();
                    shared.status(generation, "Submitting pattern".into());
                    let frame = Frame {
                        image: &image,
                        format: request.format,
                        metadata: request.metadata.as_ref(),
                    };
                    match driver.present(&surface, &frame).await {
                        Ok(s) => (format!("{label}. {s}"), close_count),
                        Err(e) => return shared.status(generation, format!("Error: {e}")),
                    }
                };
                // The driver borrow ends before the delay.
                let delay = request.delay_ms;
                let ready =
                    format!("Ready after delay: {delay} ms. {shown} Esc closes the pattern.");
                if delay == 0 {
                    return shared.status(generation, ready);
                }
                let closed = submission::watch_close(close_count);
                let elapsed = timer.start(delay);
                shared.status(
                    generation,
                    format!("Waiting for measurement delay: {delay} ms. {shown}"),
                );
                let text = match wait(elapsed, closed).await {
                    Wait::Elapsed => ready,
                    Wait::Closed => "Cancelled: the pattern window closed".into(),
                    Wait::TimerStopped => "Error: the measurement delay timer stopped".into(),
                };
                shared.status(generation, text);
            },
        ));
    }

    /// Reference table of the display `key` at its native size, with the
    /// limits of its last device report.
    #[must_use]
    pub fn reference_table(&self, kind: PatternKind, format: Format, key: &str) -> String {
        let Some(info) = self.displays.iter().find(|d| d.id.key == key) else {
            return "No display selected.".into();
        };
        let capabilities = display::query(self.backend, &self.states.report(key)).capabilities;
        let limits = OutputLimits::for_frame(&capabilities, format, None);
        kind.reference_table(format, info.size, &limits)
            .unwrap_or_else(|e| e.to_string())
    }
}

enum Wait {
    Elapsed,
    Closed,
    TimerStopped,
}

/// The first of the timer and the window close.
async fn wait(mut elapsed: oneshot::Receiver<()>, mut closed: oneshot::Receiver<()>) -> Wait {
    future::poll_fn(|cx| {
        // A dropped close sender occurs only at thread-local teardown.
        if Pin::new(&mut closed).poll(cx).is_ready() {
            return Poll::Ready(Wait::Closed);
        }
        match Pin::new(&mut elapsed).poll(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Wait::Elapsed),
            Poll::Ready(Err(_)) => Poll::Ready(Wait::TimerStopped),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

/// A rendered pattern for the open surface.
struct Prepared {
    surface: Surface,
    image: Image,
    /// Status line text before the driver status.
    label: String,
}

/// Opens the surface and renders the pattern at its size. The driver
/// rejects an unavailable path before the window opens; remapped paths,
/// such as Windows with HDR off, only warn.
fn prepare(driver: &mut impl OutputDriver, request: &ShowRequest) -> Result<Prepared, String> {
    let format = request.format;
    driver.query(&request.display).map_err(|e| e.to_string())?;
    let surface = driver
        .open(&request.display, format.depth)
        .map_err(|e| e.to_string())?;
    let (width, height) = surface.size;
    let image = request
        .kind
        .render(format, width, height)
        .map_err(|e| e.to_string())?;
    Ok(Prepared {
        surface,
        image,
        label: format!("{} ({format}) at {width}x{height}", request.kind.label()),
    })
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
