//! Host lifecycle of the launcher on the Qt GUI thread: the `QRhi` driver
//! with its device refresh on a worker, the browser driver with its
//! service events, the synchronous `query` and `open`, asynchronous
//! presentation, the measurement delay and cancellation. `launcher.rs`
//! connects it to QML. See "Asynchronous presentation" and "Measurement
//! delay" in `doc/design/output_driver/overview.md`.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::future::{self, Future};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;

use futures_channel::oneshot;
use nitpick_browser::{BrowserDriver, ClientState, ServiceConfig, ServiceEvent};
use nitpick_core::driver::{
    DisplayId, DisplayInfo, Frame, Hdr10Request, OutputDriver, OutputLimits, Surface,
};
use nitpick_core::image::Image;
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::Format;

use crate::browser::{self, NativeService, StartService};
use crate::delay::{DelayTimer, QtTimer};
use crate::display::{self, Backend, DeviceReports, DeviceStates, ffi};
use crate::gui_executor::{self, LocalTask, Post};
use crate::qrhi_driver::{BridgeWindow, PatternWindow, QrhiDriver, Threads, Workers};
use crate::submission;

/// Change for the host UI. Delivered on the GUI thread, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Status(String),
    /// New device reports or a new browser client state: the reference
    /// table can change.
    Devices,
    /// The browser client list changed: read [`Session::displays`] again.
    Displays,
    /// The URL, the pairing code or the client state of
    /// [`Session::browser_view`] changed.
    Browser,
    /// An event of the browser service with the given start count. The
    /// host gives it back to [`Session::on_service_event`] from its event
    /// loop, because the handling can replace the pending presentation.
    Service(u64, ServiceEvent),
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

/// Browser state for the launcher.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserView {
    /// `None` while the service does not run.
    pub url: Option<String>,
    /// The code of the connection that waits for authentication.
    pub pairing_code: Option<String>,
    /// The client state for the user.
    pub client: String,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Device read, `open`, render and `present`.
    Presenting,
    Delay,
}

/// The task of the last request. It stays after it finishes, until the
/// next request or a cancellation.
struct Pending {
    task: LocalTask,
    request: ShowRequest,
    phase: Rc<Cell<Phase>>,
}

impl Pending {
    fn is_browser(&self) -> bool {
        self.request.display.driver == browser::DRIVER
    }
}

/// A running browser service.
struct Browser<B> {
    /// Forwards the service events to the host.
    _forward: LocalTask,
    driver: Rc<RefCell<B>>,
    viewer: RefCell<B>,
    /// The display of the open surface. The presentation task sets it
    /// after `open`.
    opened: Rc<RefCell<Option<DisplayId>>>,
    /// The open surface became stale: show this request again at the next
    /// `Ready` of its display.
    reshow: Option<ShowRequest>,
    /// Dropped last: it stops the service.
    _service: Box<dyn Any>,
}

pub struct Session<W = BridgeWindow, J = Threads, B = BrowserDriver> {
    /// Presentation of the last request. Declared before the drivers: it
    /// holds a driver borrow until it completes, and dropping it cancels
    /// the presentation.
    pending: Option<Pending>,
    /// Device refresh for the display list and the reference table.
    refreshing: Option<LocalTask>,
    driver: Rc<RefCell<QrhiDriver<W, J>>>,
    browser: Option<Browser<B>>,
    service: Box<dyn StartService<B>>,
    /// Counts service starts. An event of an earlier service does not
    /// apply.
    service_starts: u64,
    view: BrowserView,
    states: DeviceStates,
    backend: Backend,
    reader: Box<dyn DeviceReader>,
    timer: Rc<dyn DelayTimer>,
    post: Arc<dyn Post>,
    shared: Rc<Shared>,
    /// `QRhi` displays of the last refresh.
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
            Box::new(NativeService::default()),
            Rc::new(QtTimer),
            post,
            events,
        )
    }
}

impl<W: PatternWindow + 'static, J: Workers + 'static, B: OutputDriver + 'static> Session<W, J, B> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend: Backend,
        window: W,
        workers: J,
        reader: Box<dyn DeviceReader>,
        service: Box<dyn StartService<B>>,
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
            browser: None,
            service,
            service_starts: 0,
            view: BrowserView {
                client: "The browser service does not run.".into(),
                ..BrowserView::default()
            },
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

    /// The `QRhi` screens of the last refresh, then the browser clients.
    #[must_use]
    pub fn displays(&self) -> Vec<DisplayInfo> {
        let mut list = self.displays.clone();
        if let Some(b) = &self.browser {
            list.extend(b.viewer.borrow_mut().displays());
        }
        list
    }

    #[must_use]
    pub fn browser_view(&self) -> &BrowserView {
        &self.view
    }

    #[cfg(test)]
    pub fn driver(&self) -> &Rc<RefCell<QrhiDriver<W, J>>> {
        &self.driver
    }

    /// Drops the pending presentation or measurement delay. Its late
    /// results do not reach the status line.
    fn cancel(&mut self, reason: &str) {
        let generation = self.shared.next_generation();
        if let Some(p) = self.pending.take()
            && !p.task.is_finished()
        {
            drop(p);
            self.shared
                .status(generation, format!("Cancelled: {reason}"));
        }
    }

    /// A display change: the screens, the selected display or the output
    /// driver changed. Cancels the pending presentation, lists the
    /// displays and reads their device state on a worker. Closes the
    /// window if its screen is gone. A browser surface stays open, but a
    /// later viewport change does not show its pattern again.
    ///
    /// # Errors
    /// [`Busy`]; the pending presentation is cancelled already.
    pub fn refresh(&mut self) -> Result<(), Busy> {
        self.cancel("display changed");
        if let Some(b) = &mut self.browser {
            b.reshow = None;
        }
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

    /// Replaces the pending presentation. For a `QRhi` display, reads the
    /// device state on a worker first. Then queries, opens and renders at
    /// the surface size, presents and waits for the measurement delay. The
    /// status follows each step. A closed pattern window cancels the
    /// delay; for a browser display, the service events do.
    pub fn show(&mut self, request: ShowRequest) {
        // Dropping the earlier task cancels its presentation and releases
        // its driver borrow before a window closes, so that its
        // `Disconnected` does not reach the status line.
        self.pending = None;
        // A reshow must not replace a later request.
        if let Some(b) = &mut self.browser {
            b.reshow = None;
        }
        let generation = self.shared.next_generation();
        let phase = Rc::new(Cell::new(Phase::Presenting));
        let task = Steps {
            generation,
            shared: Rc::clone(&self.shared),
            timer: Rc::clone(&self.timer),
            phase: Rc::clone(&phase),
        };
        let future: Pin<Box<dyn Future<Output = ()>>> = if request.display.driver == browser::DRIVER
        {
            let Some(b) = &self.browser else {
                self.shared
                    .status(generation, "Error: the browser service does not run".into());
                return;
            };
            let (driver, opened, request) =
                (Rc::clone(&b.driver), Rc::clone(&b.opened), request.clone());
            Box::pin(async move {
                let opened = move |d: &DisplayId| {
                    *opened.borrow_mut() = Some(d.clone());
                    None
                };
                task.run(&driver, &request, opened, "").await;
            })
        } else {
            let Ok(screens) = self.driver.try_borrow().map(|d| d.screens()) else {
                self.shared
                    .status(generation, "Error: the display is busy".into());
                return;
            };
            let refresh = self.states.begin();
            let reports = self.reader.read(screens);
            self.shared
                .status(generation, "Reading display state".into());
            let (driver, states, request) = (
                Rc::clone(&self.driver),
                self.states.clone(),
                request.clone(),
            );
            Box::pin(async move {
                let Ok(reports) = reports.await else {
                    task.shared
                        .status(generation, "Error: the display state reader stopped".into());
                    return;
                };
                task.shared.apply(&states, refresh, reports);
                // After `open`, which can close an earlier window. A close
                // from here on ends the delay.
                let opened = |_: &DisplayId| Some(submission::close_count());
                task.run(&driver, &request, opened, " Esc closes the pattern.")
                    .await;
            })
        };
        self.pending = Some(Pending {
            task: gui_executor::spawn_local(Arc::clone(&self.post), future),
            request,
            phase,
        });
    }

    /// Reference table of `display` at its native size, with the limits of
    /// its last device report or browser client state.
    #[must_use]
    pub fn reference_table(
        &self,
        kind: PatternKind,
        format: Format,
        display: &DisplayId,
    ) -> String {
        if display.driver == browser::DRIVER {
            let Some(b) = &self.browser else {
                return "The browser service does not run.".into();
            };
            let mut viewer = b.viewer.borrow_mut();
            let Some(info) = viewer.displays().into_iter().find(|d| d.id == *display) else {
                return "No display selected.".into();
            };
            let limits = match viewer.query(display) {
                Ok(q) => OutputLimits::for_frame(&q.capabilities, format, None),
                Err(e) => OutputLimits::unavailable(e.to_string()),
            };
            return kind
                .reference_table(format, info.size, &limits)
                .unwrap_or_else(|e| e.to_string());
        }
        let Some(info) = self.displays.iter().find(|d| d.id == *display) else {
            return "No display selected.".into();
        };
        let capabilities =
            display::query(self.backend, &self.states.report(&display.key)).capabilities;
        let limits = OutputLimits::for_frame(&capabilities, format, None);
        kind.reference_table(format, info.size, &limits)
            .unwrap_or_else(|e| e.to_string())
    }

    /// Starts the browser service, or starts it again with a new
    /// configuration. A restart ends the current client session.
    ///
    /// # Errors
    /// The service cannot start; the old service is stopped already.
    pub fn start_browser(&mut self, config: &ServiceConfig) -> Result<(), String> {
        self.stop_browser("the browser service restarted");
        self.service_starts += 1;
        let started = match self.service.start(config) {
            Ok(s) => s,
            Err(e) => {
                self.view = BrowserView {
                    client: format!("The browser service does not run: {e}"),
                    ..BrowserView::default()
                };
                (self.shared.events)(Event::Browser);
                return Err(e);
            }
        };
        let service = self.service_starts;
        let (shared, mut events) = (Rc::clone(&self.shared), started.events);
        let forward = gui_executor::spawn_local(Arc::clone(&self.post), async move {
            while let Some(event) = events.next_event().await {
                (shared.events)(Event::Service(service, event));
            }
        });
        self.browser = Some(Browser {
            _forward: forward,
            driver: Rc::new(RefCell::new(started.driver)),
            viewer: RefCell::new(started.viewer),
            opened: Rc::default(),
            reshow: None,
            _service: started.service,
        });
        self.view = BrowserView {
            url: Some(started.url),
            pairing_code: None,
            client: "Waiting for a browser.".into(),
        };
        (self.shared.events)(Event::Browser);
        (self.shared.events)(Event::Displays);
        Ok(())
    }

    /// Ends the browser part of the pending presentation and the surface,
    /// then stops the service.
    fn stop_browser(&mut self, reason: &str) {
        if self.browser.is_none() {
            return;
        }
        self.end_browser_pending(None, reason);
        if let Some(b) = self.browser.take()
            && let Some(d) = b.opened.borrow_mut().take()
        {
            b.driver.borrow_mut().close(&d);
        }
        (self.shared.events)(Event::Displays);
        (self.shared.events)(Event::Devices);
    }

    /// Ends a pending browser presentation after a client change. A
    /// presentation fails with the error that the driver gives for the
    /// change, so the status does not depend on whether the driver result
    /// or the event comes first. A delay is cancelled with `reason`.
    /// Returns the request of the ended task.
    fn end_browser_pending(
        &mut self,
        change: Option<&ClientState>,
        reason: &str,
    ) -> Option<ShowRequest> {
        let p = self.pending.take_if(|p| p.is_browser())?;
        let generation = self.shared.next_generation();
        if !p.task.is_finished() {
            let text = match (
                p.phase.get(),
                change.and_then(ClientState::presentation_error),
            ) {
                (Phase::Presenting, Some(e)) => format!("Error: {e}"),
                _ => format!("Cancelled: {reason}"),
            };
            drop(p.task);
            self.shared.status(generation, text);
        }
        Some(p.request)
    }

    /// Handles an event that [`Event::Service`] gave. Events of an earlier
    /// service do nothing.
    pub fn on_service_event(&mut self, service: u64, event: ServiceEvent) {
        if service != self.service_starts || self.browser.is_none() {
            return;
        }
        match event {
            ServiceEvent::Listening { url } => self.view.url = Some(url),
            ServiceEvent::PairingCode(code) => {
                self.view.pairing_code = Some(code);
                self.view.client = "Enter the pairing code in the browser.".into();
            }
            ServiceEvent::Client(ClientState::Connected) => {
                self.view.client = "A browser connected.".into();
            }
            ServiceEvent::Client(ClientState::Authenticated) => {
                self.view.pairing_code = None;
                self.view.client = "The browser is authenticated; waiting for its state.".into();
            }
            ServiceEvent::Client(ClientState::Ready(display)) => self.on_ready(&display),
            ServiceEvent::Client(change @ ClientState::ViewportChanged(_)) => {
                let ClientState::ViewportChanged(display) = &change else {
                    unreachable!()
                };
                let b = self.browser.as_ref().expect("checked above");
                let shown = b.opened.borrow().as_ref() == Some(display)
                    && self
                        .pending
                        .as_ref()
                        .is_some_and(|p| p.request.display == *display);
                if shown {
                    let request =
                        self.end_browser_pending(Some(&change), "the browser viewport changed");
                    self.browser.as_mut().expect("checked above").reshow = request;
                }
            }
            ServiceEvent::Client(change @ ClientState::Disconnected(_)) => {
                let ClientState::Disconnected(reason) = &change else {
                    unreachable!()
                };
                self.view.pairing_code = None;
                self.view.client = format!("The browser disconnected: {reason}.");
                self.end_browser_pending(Some(&change), "the browser disconnected");
                let b = self.browser.as_mut().expect("checked above");
                b.reshow = None;
                if let Some(d) = b.opened.borrow_mut().take() {
                    b.driver.borrow_mut().close(&d);
                }
                (self.shared.events)(Event::Displays);
                (self.shared.events)(Event::Devices);
            }
            ServiceEvent::Stopped { error } => {
                self.stop_browser("the browser service stopped");
                self.view = BrowserView {
                    client: match error {
                        Some(e) => format!("The browser service stopped: {e}"),
                        None => "The browser service stopped.".into(),
                    },
                    ..BrowserView::default()
                };
            }
        }
        (self.shared.events)(Event::Browser);
    }

    /// Each `Ready` can change the client mode and size.
    fn on_ready(&mut self, display: &DisplayId) {
        let b = self.browser.as_mut().expect("checked by the caller");
        let name = b
            .viewer
            .borrow_mut()
            .displays()
            .into_iter()
            .find(|d| d.id == *display)
            .map_or_else(|| display.to_string(), |d| d.name);
        self.view.client = format!("Ready: {name}.");
        let reshow = b.reshow.take_if(|r| r.display == *display);
        (self.shared.events)(Event::Displays);
        (self.shared.events)(Event::Devices);
        if let Some(request) = reshow {
            self.show(request);
        }
    }
}

/// The presentation steps after the device read.
struct Steps {
    generation: u64,
    shared: Rc<Shared>,
    timer: Rc<dyn DelayTimer>,
    phase: Rc<Cell<Phase>>,
}

impl Steps {
    /// `opened` runs after `open` and gives the `submission` close count
    /// from which a window close ends the delay. `suffix` ends the ready
    /// status.
    // The task holds the driver borrow across the await. Each request
    // drops the task before it borrows the driver again.
    #[allow(clippy::await_holding_refcell_ref)]
    async fn run<D: OutputDriver>(
        self,
        driver: &RefCell<D>,
        request: &ShowRequest,
        opened: impl FnOnce(&DisplayId) -> Option<u64>,
        suffix: &str,
    ) {
        let Self {
            generation,
            shared,
            timer,
            phase,
        } = self;
        let (shown, close_count) = {
            let mut driver = driver.borrow_mut();
            let Prepared {
                surface,
                image,
                label,
            } = match prepare(&mut *driver, request) {
                Ok(p) => p,
                Err(e) => return shared.status(generation, format!("Error: {e}")),
            };
            let close_count = opened(&surface.display);
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
        let ready = format!("Ready after delay: {delay} ms. {shown}{suffix}");
        if delay == 0 {
            return shared.status(generation, ready);
        }
        let closed = close_count.map(submission::watch_close);
        let elapsed = timer.start(delay);
        phase.set(Phase::Delay);
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
    }
}

enum Wait {
    Elapsed,
    Closed,
    TimerStopped,
}

/// The first of the timer and the window close, if any.
async fn wait(
    mut elapsed: oneshot::Receiver<()>,
    mut closed: Option<oneshot::Receiver<()>>,
) -> Wait {
    future::poll_fn(|cx| {
        // A dropped close sender occurs only at thread-local teardown.
        if let Some(closed) = &mut closed
            && Pin::new(closed).poll(cx).is_ready()
        {
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
