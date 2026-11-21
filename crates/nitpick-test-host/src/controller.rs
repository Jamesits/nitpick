//! The test-host state machine: protocol requests, driver-side events, one
//! open surface, one pending presentation or measurement delay, and one
//! capture session. Each host polls [`TestHost::run`] with its own
//! executor. See "Host boundary" in `doc/design/output_driver/testing.md`.

use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use futures_core::Stream;
use futures_util::StreamExt as _;
use nitpick_core::device::DeviceId;
use nitpick_core::driver::{DisplayId, Frame, Hdr10Request, OutputDriver, OutputError, Surface};
use nitpick_core::image::Image;
use nitpick_core::signal::{BitDepth, Format};
use nitpick_fixtures::MetadataRequest;

use crate::artifact::{
    Acceptance, Artifact, Artifacts, Check, Environment, FormatRecord, RunRecord, Saved,
};
use crate::capture::{Capture, CaptureAdapter, CaptureFrames, CaptureTarget, Timestamp};
use crate::device::{DeviceCall, Devices};
use crate::fixture::{Fixtures, hdr10_request};
use crate::gate::{DEADLINE_MS, Expected, FrameGate, MATCH_LIMIT, Offer};
use crate::marker::Marker;
use crate::protocol::{
    CaptureOutcome, Cause, DisplayEntry, DisplayState, ErrorKind, Event, ModeReport, Request,
    SurfaceState, VERSION, parse_request,
};

/// Reason of a valid capture in a host without [`Artifacts`].
pub const NO_ARTIFACTS: &str = "the host has no artifact directory";

/// Reason of a device request in a host without [`Devices`].
pub const NO_DEVICES: &str = "this host has no device control";

pub type LocalFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Measurement delay timers of the host executor.
pub trait Clock {
    /// Resolves after `ms`; `Err` with a reason if the timer stopped.
    /// Dropping the future cancels the timer.
    fn sleep(&self, ms: u32) -> LocalFuture<Result<(), String>>;
}

/// Host-specific parts of the driver side.
pub trait Hooks {
    /// Reads the display state of `query` and `open` again.
    fn refresh(&mut self) -> LocalFuture<()>;
    /// Called after each successful `open`. Resolves when the surface
    /// closes outside the host, for example by Esc in the window. `None`:
    /// the host reports such changes as [`DriverEvent`]s.
    fn watch_surface(&mut self) -> Option<LocalFuture<()>>;
    /// The local display device of `display`, or the reason that it has
    /// none. A browser client is not a local device.
    fn device(&mut self, display: &DisplayId) -> LocalFuture<Result<DeviceId, String>>;
}

/// A change on the driver side, outside the requests.
#[derive(Debug, Clone, PartialEq)]
pub enum DriverEvent {
    /// Goes to the output unchanged.
    Notice(Event),
    Ready(DisplayId),
    /// The surface of `display` is stale. A pending presentation fails with
    /// `error`, the error that the driver gives for this change, so the
    /// result does not depend on which of the two the host sees first.
    Changed {
        display: DisplayId,
        error: OutputError,
    },
    /// `None`: the display of the open surface. A pending presentation
    /// fails with `Disconnected`, as the driver does.
    Disconnected {
        display: Option<DisplayId>,
        reason: String,
    },
    Stopped {
        error: Option<String>,
    },
    /// An HDR, topology or resume event of the OS. The display setup of
    /// the device run is read again; a change fails the run.
    SystemChanged {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// `quit` or the end of the input.
    Quit,
    /// The driver side stopped; `Some` with its error.
    Stopped(Option<String>),
}

/// The parts of a test host. [`Self::run`] consumes them.
pub struct TestHost<D> {
    /// Opens, presents and closes.
    pub driver: Rc<RefCell<D>>,
    /// Answers `displays` and `query` while a presentation borrows
    /// `driver`; a second driver on the same state.
    pub viewer: D,
    pub clock: Box<dyn Clock>,
    pub hooks: Box<dyn Hooks>,
    pub capture: Box<dyn CaptureAdapter>,
    /// `None`: a valid capture is `unavailable`, with [`NO_ARTIFACTS`].
    pub artifacts: Option<Artifacts>,
    /// `None`: each device request fails with [`NO_DEVICES`].
    pub devices: Option<Box<dyn Devices>>,
    pub fixtures: Rc<Fixtures>,
    /// Measurement delay of a `show` without `delay_ms`.
    pub delay_ms: u32,
}

struct Open {
    surface: Surface,
    generation: u64,
    /// Of the query before `open`.
    mode: ModeReport,
    stale: bool,
    watch: Option<LocalFuture<()>>,
}

/// A `show` request in presentation, and then acknowledged.
#[derive(Clone)]
struct Shown {
    frame: u64,
    generation: u64,
    delay_ms: u32,
    fixture: String,
    fixture_size: (usize, usize),
    format: Format,
    metadata: Option<MetadataRequest>,
    /// With `marker`: the marker and the presented image, for the
    /// heartbeat. Without it the host keeps no copy of the image.
    marked: Option<(Marker, Rc<Image>)>,
    /// [`CaptureAdapter::now`] before the presentation.
    submitted: Option<Timestamp>,
}

/// At most one; dropping it cancels the presentation or the timer.
enum Pending {
    Presenting {
        shown: Box<Shown>,
        future: LocalFuture<Result<String, OutputError>>,
    },
    Delay {
        frame: u64,
        delay_ms: u32,
        timer: LocalFuture<Result<(), String>>,
    },
}

impl Pending {
    fn frame(&self) -> u64 {
        match self {
            Self::Presenting { shown, .. } => shown.frame,
            Self::Delay { frame, .. } => *frame,
        }
    }
}

/// A `capture` in progress on the acknowledged frame. Dropping it ends the
/// session, the deadline timer and the heartbeat.
struct Capturing {
    frame: u64,
    /// Identifies the session in a device reply.
    serial: u64,
    /// The gate accepted a frame; the display setup of the device run is
    /// read before the capture passes. The session and the heartbeat have
    /// ended.
    verifying: bool,
    frames: CaptureFrames,
    gate: FrameGate,
    deadline_ms: u32,
    deadline: LocalFuture<Result<(), String>>,
    heartbeat: Option<Heartbeat>,
}

struct Heartbeat {
    interval_ms: u32,
    /// The heartbeat cell of the last presentation.
    set: bool,
    beat: Beat,
}

enum Beat {
    Wait(LocalFuture<Result<(), String>>),
    /// Holds the driver, as `Pending::Presenting` does. A capture exists
    /// only while no presentation is in progress.
    Present(LocalFuture<Result<String, OutputError>>),
}

enum Input {
    Line(Option<String>),
    Driver(Option<DriverEvent>),
    Refreshed,
    SurfaceClosed,
    Saved(Saved),
    Presented {
        shown: Box<Shown>,
        result: Result<String, OutputError>,
    },
    Elapsed {
        frame: u64,
        delay_ms: u32,
        result: Result<(), String>,
    },
    /// `None`: the session ended.
    CaptureFrame(Option<Result<Box<Capture>, String>>),
    CaptureDeadline(Result<(), String>),
    BeatDue(Result<(), String>),
    BeatPresented(Result<String, OutputError>),
    Device(devices::Done),
}

struct Machine<D, O> {
    host: TestHost<D>,
    out: O,
    /// Invariant: `driver` has no other borrow while `Presenting`.
    pending: Option<Pending>,
    /// Invariant: ends before `pending` gets a new frame and before the
    /// surface closes.
    capturing: Option<Capturing>,
    /// `show` requests that started a presentation; gives the marker value.
    shows: u64,
    open: Option<Open>,
    refreshing: Option<LocalFuture<()>>,
    /// Of the last `open`.
    generation: u64,
    /// Of the last `show` that passed the order check.
    last_frame: Option<u64>,
    /// The acknowledged frame on the open surface.
    current: Option<Shown>,
    /// Capture sessions started; gives [`Capturing::serial`].
    sessions: u64,
    dev: devices::DeviceSide,
}

mod devices;

impl<D: OutputDriver + 'static> TestHost<D> {
    /// Runs until `quit`, the end of `lines` or [`DriverEvent::Stopped`].
    /// Then it ends the pending frame, closes the surface, waits for the
    /// queued device calls, restores an active profile change, waits for
    /// the queued artifacts and sends [`Event::Stopped`].
    pub async fn run(
        self,
        mut lines: impl Stream<Item = String> + Unpin,
        mut events: impl Stream<Item = DriverEvent> + Unpin,
        out: impl FnMut(&Event),
    ) -> Exit {
        let mut m = Machine {
            host: self,
            out,
            pending: None,
            capturing: None,
            shows: 0,
            open: None,
            refreshing: None,
            generation: 0,
            last_frame: None,
            current: None,
            sessions: 0,
            dev: devices::DeviceSide::default(),
        };
        let driver = m.host.driver.borrow().id().to_owned();
        m.emit(&Event::Hello {
            protocol: VERSION,
            driver,
        });
        let exit = loop {
            let input = poll_fn(|cx| m.poll_input(&mut lines, &mut events, cx)).await;
            if let Some(exit) = m.on_input(input) {
                break exit;
            }
        };
        m.end_capture(Cause::Stopped);
        m.cancel(Cause::Stopped);
        m.close_surface(Cause::Stopped);
        m.stop_devices().await;
        while let Some(saved) = poll_fn(|cx| m.poll_saved(cx)).await {
            m.saved(saved);
        }
        let error = match &exit {
            Exit::Quit => None,
            Exit::Stopped(error) => error.clone(),
        };
        m.emit(&Event::Stopped { error });
        exit
    }
}

impl<D: OutputDriver + 'static, O: FnMut(&Event)> Machine<D, O> {
    fn emit(&mut self, event: &Event) {
        (self.out)(event);
    }

    /// Driver-side events first, so that a request after a change sees it
    /// when both are ready.
    fn poll_input(
        &mut self,
        lines: &mut (impl Stream<Item = String> + Unpin),
        events: &mut (impl Stream<Item = DriverEvent> + Unpin),
        cx: &mut Context<'_>,
    ) -> Poll<Input> {
        if let Poll::Ready(event) = events.poll_next_unpin(cx) {
            return Poll::Ready(Input::Driver(event));
        }
        if let Poll::Ready(line) = lines.poll_next_unpin(cx) {
            return Poll::Ready(Input::Line(line));
        }
        if let Some(f) = &mut self.refreshing
            && f.as_mut().poll(cx).is_ready()
        {
            self.refreshing = None;
            return Poll::Ready(Input::Refreshed);
        }
        if let Some(watch) = self.open.as_mut().and_then(|o| o.watch.as_mut())
            && watch.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(Input::SurfaceClosed);
        }
        if let Poll::Ready(Some(saved)) = self.poll_saved(cx) {
            return Poll::Ready(Input::Saved(saved));
        }
        if let Poll::Ready(done) = self.dev.poll(cx) {
            return Poll::Ready(Input::Device(done));
        }
        // The handler of each capture input replaces or drops the future
        // that completed.
        if let Some(c) = &mut self.capturing {
            if let Poll::Ready(frame) = c.frames.as_mut().poll_next(cx) {
                return Poll::Ready(Input::CaptureFrame(frame.map(|f| f.map(Box::new))));
            }
            match c.heartbeat.as_mut().map(|h| &mut h.beat) {
                Some(Beat::Wait(timer)) => {
                    if let Poll::Ready(r) = timer.as_mut().poll(cx) {
                        return Poll::Ready(Input::BeatDue(r));
                    }
                }
                Some(Beat::Present(future)) => {
                    if let Poll::Ready(r) = future.as_mut().poll(cx) {
                        return Poll::Ready(Input::BeatPresented(r));
                    }
                }
                None => {}
            }
            if let Poll::Ready(r) = c.deadline.as_mut().poll(cx) {
                return Poll::Ready(Input::CaptureDeadline(r));
            }
        }
        let ready = match &mut self.pending {
            Some(Pending::Presenting { future, .. }) => future.as_mut().poll(cx).map(Ok),
            Some(Pending::Delay { timer, .. }) => timer.as_mut().poll(cx).map(Err),
            None => Poll::Pending,
        };
        let Poll::Ready(result) = ready else {
            return Poll::Pending;
        };
        Poll::Ready(match (self.pending.take(), result) {
            (Some(Pending::Presenting { shown, .. }), Ok(result)) => {
                Input::Presented { shown, result }
            }
            (
                Some(Pending::Delay {
                    frame, delay_ms, ..
                }),
                Err(result),
            ) => Input::Elapsed {
                frame,
                delay_ms,
                result,
            },
            _ => unreachable!("the result matches the pending state"),
        })
    }

    /// `Ready(None)`: no artifact is queued.
    fn poll_saved(&mut self, cx: &mut Context<'_>) -> Poll<Option<Saved>> {
        match &mut self.host.artifacts {
            Some(a) => a.sink.poll_saved(cx),
            None => Poll::Ready(None),
        }
    }

    fn saved(&mut self, saved: Saved) {
        let event = match saved.result {
            Ok(()) => Event::Saved {
                frame: saved.frame,
                artifact: saved.name,
            },
            Err(e) => Event::error(
                Some(saved.frame),
                ErrorKind::Artifact,
                format!("{}: {e}", saved.name),
            ),
        };
        self.emit(&event);
    }

    fn on_input(&mut self, input: Input) -> Option<Exit> {
        match input {
            Input::Line(None) => return Some(Exit::Quit),
            Input::Line(Some(line)) => match parse_request(&line) {
                Ok(Some(request)) => return self.on_request(request),
                Ok(None) => {}
                Err(e) => self.emit(&Event::error(None, ErrorKind::Malformed, e)),
            },
            Input::Driver(None) => {
                return Some(Exit::Stopped(Some("the driver event source ended".into())));
            }
            Input::Driver(Some(event)) => return self.on_event(event),
            Input::Refreshed => {
                self.displays();
                self.refreshed();
            }
            Input::SurfaceClosed => self.surface_lost(),
            Input::Saved(saved) => self.saved(saved),
            Input::Presented { shown, result } => self.presented(shown, result),
            Input::Elapsed {
                frame,
                delay_ms,
                result: Ok(()),
            } => self.emit(&Event::Ready { frame, delay_ms }),
            Input::Elapsed {
                frame,
                result: Err(e),
                ..
            } => self.emit(&Event::error(Some(frame), ErrorKind::Timer, e)),
            Input::CaptureFrame(frame) => self.capture_frame(frame),
            Input::CaptureDeadline(result) => self.capture_deadline(result),
            Input::BeatDue(result) => self.beat_due(result),
            Input::BeatPresented(result) => self.beat_presented(result),
            Input::Device(done) => self.device_done(done),
        }
        None
    }

    fn on_request(&mut self, request: Request) -> Option<Exit> {
        match request {
            Request::Displays {} => self.displays(),
            Request::Refresh {} => self.refreshing = Some(self.host.hooks.refresh()),
            Request::Query { display } => self.query(&display),
            Request::Open { display, depth } => self.open(&display, depth),
            Request::Show {
                frame,
                fixture,
                delay_ms,
                metadata,
                marker,
            } => self.show(frame, &fixture, delay_ms, metadata, marker),
            Request::Capture {
                frame,
                heartbeat_ms,
                deadline_ms,
            } => self.capture(frame, heartbeat_ms, deadline_ms),
            Request::Close {} => {
                if self.open.is_some() {
                    self.end_capture(Cause::Closed);
                    self.cancel(Cause::Closed);
                    self.close_surface(Cause::Closed);
                } else {
                    self.emit(&Event::error(None, ErrorKind::NoSurface, "no open surface"));
                }
            }
            Request::Quit {} => return Some(Exit::Quit),
            Request::DeviceList {} => self.device_call(DeviceCall::List, devices::Purpose::Reply),
            Request::DeviceMap { display } => self.device_map(&display),
            Request::DeviceQuery { device } => {
                self.device_call(DeviceCall::Query(device), devices::Purpose::Reply);
            }
            Request::DeviceSetup { device } => {
                self.device_call(DeviceCall::Setup(device), devices::Purpose::Reply);
            }
            Request::DeviceRun { device } => self.device_run(device),
            Request::ProfileTargets { device } => {
                self.device_call(DeviceCall::Targets(device), devices::Purpose::Reply);
            }
            Request::ProfileState { target } => {
                self.device_call(DeviceCall::State(target), devices::Purpose::Reply);
            }
            Request::ProfileAssign { target, profile } => self.profile_assign(target, &profile),
            Request::ProfileRestore {} => self.profile_restore(),
        }
        None
    }

    fn on_event(&mut self, event: DriverEvent) -> Option<Exit> {
        match event {
            DriverEvent::Notice(event) => self.emit(&event),
            DriverEvent::Ready(display) => self.emit(&Event::Display {
                state: DisplayState::Ready,
                display: Some(display.to_string()),
                reason: None,
            }),
            DriverEvent::Changed { display, error } => {
                self.emit(&Event::Display {
                    state: DisplayState::Changed,
                    display: Some(display.to_string()),
                    reason: None,
                });
                if self
                    .open
                    .as_ref()
                    .is_some_and(|o| o.surface.display == display && !o.stale)
                {
                    self.end_capture(Cause::Changed);
                    self.fail_pending(&error, Cause::Changed);
                    self.current = None;
                    let open = self.open.as_mut().expect("checked above");
                    open.stale = true;
                    let event = surface_event(open, SurfaceState::Stale, Some(Cause::Changed));
                    self.emit(&event);
                }
            }
            DriverEvent::Disconnected { display, reason } => {
                self.emit(&Event::Display {
                    state: DisplayState::Disconnected,
                    display: display.as_ref().map(ToString::to_string),
                    reason: Some(reason),
                });
                if self
                    .open
                    .as_ref()
                    .is_some_and(|o| display.as_ref().is_none_or(|d| *d == o.surface.display))
                {
                    self.surface_lost();
                }
            }
            DriverEvent::Stopped { error } => return Some(Exit::Stopped(error)),
            DriverEvent::SystemChanged { reason } => self.check_run(format!("after {reason}")),
        }
        None
    }

    /// Ends the pending frame with `cause`. Drops its future first, so the
    /// driver is free afterwards.
    fn cancel(&mut self, cause: Cause) {
        if let Some(pending) = self.pending.take() {
            self.emit(&Event::Cancelled {
                frame: pending.frame(),
                cause,
            });
        }
    }

    /// A presentation gets the driver's `error`; a delay is cancelled.
    fn fail_pending(&mut self, error: &OutputError, cause: Cause) {
        match self.pending.take() {
            Some(Pending::Presenting { shown, .. }) => {
                self.emit(&Event::output_error(Some(shown.frame), error));
            }
            Some(Pending::Delay { frame, .. }) => self.emit(&Event::Cancelled { frame, cause }),
            None => {}
        }
    }

    /// The display disconnected or the surface closed outside the host.
    fn surface_lost(&mut self) {
        self.end_capture(Cause::Disconnected);
        self.fail_pending(&OutputError::Disconnected, Cause::Disconnected);
        self.close_surface(Cause::Disconnected);
    }

    /// Call without a pending frame or capture.
    fn close_surface(&mut self, cause: Cause) {
        debug_assert!(self.pending.is_none(), "the presentation holds the driver");
        debug_assert!(self.capturing.is_none(), "the heartbeat holds the driver");
        self.current = None;
        if let Some(open) = self.open.take() {
            self.host.driver.borrow_mut().close(&open.surface.display);
            self.emit(&surface_event(&open, SurfaceState::Closed, Some(cause)));
        }
    }

    fn displays(&mut self) {
        let displays = self
            .host
            .viewer
            .displays()
            .into_iter()
            .map(|d| DisplayEntry {
                id: d.id.to_string(),
                name: d.name,
                size: [d.size.0, d.size.1],
            })
            .collect();
        self.emit(&Event::Displays { displays });
    }

    fn find(&mut self, text: &str) -> Option<DisplayId> {
        let found = self
            .host
            .viewer
            .displays()
            .into_iter()
            .find(|d| d.id.to_string() == text)
            .map(|d| d.id);
        if found.is_none() {
            self.emit(&Event::error(
                None,
                ErrorKind::UnknownDisplay,
                format!("no display {text}"),
            ));
        }
        found
    }

    fn query(&mut self, text: &str) {
        let Some(display) = self.find(text) else {
            return;
        };
        let event = match self.host.viewer.query(&display) {
            Ok(query) => Event::query(display.to_string(), &query),
            Err(e) => Event::output_error(None, &e),
        };
        self.emit(&event);
    }

    /// Ends the earlier generation, then queries and opens, as the
    /// launcher does before each `open`.
    fn open(&mut self, text: &str, depth: u32) {
        if self.dev.changing() {
            return self.emit(&devices::pending(None));
        }
        let Some(display) = self.find(text) else {
            return;
        };
        let Ok(depth) = depth.to_string().parse::<BitDepth>() else {
            self.emit(&Event::error(
                None,
                ErrorKind::Malformed,
                format!("no bit depth {depth}"),
            ));
            return;
        };
        self.end_capture(Cause::Opened);
        self.cancel(Cause::Opened);
        self.current = None;
        let earlier = self.open.take().map(|old| {
            self.emit(&surface_event(
                &old,
                SurfaceState::Closed,
                Some(Cause::Opened),
            ));
            old.surface.display
        });
        let mut driver = self.host.driver.borrow_mut();
        if let Some(old) = earlier.as_ref().filter(|old| **old != display) {
            driver.close(old);
        }
        let opened = driver.query(&display).and_then(|query| {
            let surface = driver.open(&display, depth)?;
            Ok((surface, ModeReport::from(&query.mode)))
        });
        if opened.is_err() && earlier.as_ref() == Some(&display) {
            // The event above reported the earlier surface closed.
            driver.close(&display);
        }
        drop(driver);
        match opened {
            Ok((surface, mode)) => {
                self.generation += 1;
                let open = Open {
                    surface,
                    generation: self.generation,
                    mode,
                    stale: false,
                    watch: self.host.hooks.watch_surface(),
                };
                self.emit(&surface_event(&open, SurfaceState::Open, None));
                self.open = Some(open);
            }
            Err(e) => self.emit(&Event::output_error(None, &e)),
        }
    }

    /// A rejected request keeps the pending frame.
    // `metadata` as in `Request::Show`: absent and `null` differ.
    #[allow(clippy::option_option)]
    fn show(
        &mut self,
        frame: u64,
        fixture: &str,
        delay_ms: Option<u32>,
        metadata: Option<Option<nitpick_fixtures::MetadataRequest>>,
        marker: bool,
    ) {
        let error = |kind, message: String| Event::error(Some(frame), kind, message);
        if let Some(last) = self.last_frame
            && frame <= last
        {
            let e = error(ErrorKind::FrameOrder, format!("frame {frame} after {last}"));
            return self.emit(&e);
        }
        self.last_frame = Some(frame);
        let fixtures = Rc::clone(&self.host.fixtures);
        let Some(fx) = fixtures.get(fixture) else {
            let e = error(ErrorKind::UnknownFixture, format!("no fixture {fixture}"));
            return self.emit(&e);
        };
        let format = fx.format;
        let metadata = metadata.unwrap_or_else(|| fx.metadata.clone());
        if let Some(Err(e)) = metadata.as_ref().map(nitpick_fixtures::validate_metadata) {
            return self.emit(&error(ErrorKind::InvalidMetadata, e.to_string()));
        }
        if self.dev.changing() {
            return self.emit(&devices::pending(Some(frame)));
        }
        let Some(open) = &self.open else {
            return self.emit(&error(ErrorKind::NoSurface, "no open surface".into()));
        };
        if open.stale {
            let e = error(
                ErrorKind::StaleSurface,
                "the display changed; open the surface again".into(),
            );
            return self.emit(&e);
        }
        let size = open.surface.size;
        // Truncation is the marker value: the sequence modulo 256.
        #[allow(clippy::cast_possible_truncation)]
        let value = (self.shows + 1) as u8;
        let drawn = fx.render(size).and_then(|mut image| {
            let marker = marker
                .then(|| Marker::place(size, fx.size, value))
                .transpose()?;
            if let Some(m) = &marker {
                m.draw(&mut image, format, false);
            }
            Ok((image, marker))
        });
        let (image, marker) = match drawn {
            Ok(drawn) => drawn,
            Err(e) => return self.emit(&error(ErrorKind::FixtureTooLarge, e)),
        };
        let (surface, generation) = (open.surface.clone(), open.generation);
        self.end_capture(Cause::Replaced);
        self.cancel(Cause::Replaced);
        self.current = None;
        self.shows += 1;
        let image = Rc::new(image);
        let shown = Shown {
            frame,
            generation,
            delay_ms: delay_ms.unwrap_or(self.host.delay_ms),
            fixture: fixture.into(),
            fixture_size: fx.size,
            format,
            marked: marker.map(|m| (m, Rc::clone(&image))),
            submitted: self.host.capture.now(),
            metadata,
        };
        let future = present(
            Rc::clone(&self.host.driver),
            surface,
            image,
            format,
            shown.metadata.as_ref().map(hdr10_request),
        );
        self.pending = Some(Pending::Presenting {
            shown: Box::new(shown),
            future: Box::pin(future),
        });
    }

    /// A failed presentation starts no timer. A zero delay adds no wait.
    fn presented(&mut self, shown: Box<Shown>, result: Result<String, OutputError>) {
        let (frame, generation, delay_ms) = (shown.frame, shown.generation, shown.delay_ms);
        let status = match result {
            Ok(status) => status,
            Err(e) => return self.emit(&Event::output_error(Some(frame), &e)),
        };
        self.current = Some(*shown);
        self.emit(&Event::Submitted {
            frame,
            generation,
            delay_ms,
            status,
        });
        if delay_ms == 0 {
            self.emit(&Event::Ready { frame, delay_ms });
        } else {
            self.pending = Some(Pending::Delay {
                frame,
                delay_ms,
                timer: self.host.clock.sleep(delay_ms),
            });
        }
    }

    /// Starts a capture session of the acknowledged frame. The session
    /// ends with one `captured` event.
    fn capture(&mut self, frame: u64, heartbeat_ms: Option<u32>, deadline_ms: Option<u32>) {
        let error = |kind, message: String| Event::error(Some(frame), kind, message);
        if heartbeat_ms == Some(0) || deadline_ms == Some(0) {
            let e = error(
                ErrorKind::Malformed,
                "`heartbeat_ms` and `deadline_ms` must be above 0".into(),
            );
            return self.emit(&e);
        }
        let current = self.current.as_ref().filter(|c| c.frame == frame);
        let found = self
            .open
            .as_ref()
            .filter(|o| !o.stale)
            .zip(current)
            .filter(|(o, c)| c.generation == o.generation);
        let Some((open, shown)) = found else {
            let e = error(
                ErrorKind::StaleFrame,
                format!("frame {frame} is not the acknowledged frame of the open surface"),
            );
            return self.emit(&e);
        };
        if let Some(c) = &self.capturing {
            let e = error(
                ErrorKind::CapturePending,
                format!("the capture of frame {} is in progress", c.frame),
            );
            return self.emit(&e);
        }
        let Some((marker, _)) = shown.marked else {
            let e = error(
                ErrorKind::NoMarker,
                format!("frame {frame} was shown without `marker`"),
            );
            return self.emit(&e);
        };
        let target = CaptureTarget {
            display: &open.surface.display,
            frame,
            generation: open.generation,
        };
        let expected = shown.submitted.map(|submitted| Expected {
            surface: open.surface.size,
            fixture: shown.fixture_size,
            marker,
            submitted,
        });
        let frames = match self.host.capture.start(&target) {
            Ok(frames) => frames,
            Err(reason) => return self.emit(&captured(frame, CaptureOutcome::Unavailable, reason)),
        };
        let Some(expected) = expected else {
            let reason = "the capture adapter gave no time before the presentation".into();
            return self.emit(&captured(frame, CaptureOutcome::Fail, reason));
        };
        let deadline_ms = deadline_ms.unwrap_or(DEADLINE_MS);
        let clock = &self.host.clock;
        self.sessions += 1;
        self.capturing = Some(Capturing {
            frame,
            serial: self.sessions,
            verifying: false,
            frames,
            gate: FrameGate::new(expected, MATCH_LIMIT),
            deadline_ms,
            deadline: clock.sleep(deadline_ms),
            heartbeat: heartbeat_ms.map(|interval_ms| Heartbeat {
                interval_ms,
                set: false,
                beat: Beat::Wait(clock.sleep(interval_ms)),
            }),
        });
    }

    /// Ends the capture session with `cancelled`. Drops it first, so the
    /// driver is free afterwards.
    fn end_capture(&mut self, cause: Cause) {
        if let Some(c) = self.capturing.take() {
            let frame = c.frame;
            drop(c);
            self.emit(&Event::Captured {
                frame,
                result: CaptureOutcome::Cancelled,
                reason: None,
                cause: Some(cause),
                artifact: None,
            });
        }
    }

    fn capture_frame(&mut self, frame: Option<Result<Box<Capture>, String>>) {
        let c = self.capturing.as_mut().expect("polled while capturing");
        match frame {
            None => self.finish_capture(
                CaptureOutcome::Unavailable,
                "the capture session ended".into(),
                false,
            ),
            Some(Err(reason)) => self.finish_capture(CaptureOutcome::Unavailable, reason, false),
            Some(Ok(capture)) => match c.gate.offer(*capture) {
                Offer::Accepted => self.accepted(),
                Offer::Counted | Offer::Skipped(_) => {}
                Offer::Rejected(reason) => self.finish_capture(CaptureOutcome::Fail, reason, true),
            },
        }
    }

    fn capture_deadline(&mut self, result: Result<(), String>) {
        let c = self.capturing.as_ref().expect("polled while capturing");
        let reason = match result {
            Ok(()) => {
                let last = c.gate.reason().map(|r| format!("; last: {r}"));
                format!(
                    "no accepted frame within {} ms; {} frames received{}",
                    c.deadline_ms,
                    c.gate.frames(),
                    last.unwrap_or_default()
                )
            }
            Err(e) => format!("the capture deadline timer stopped: {e}"),
        };
        self.finish_capture(CaptureOutcome::Fail, reason, true);
    }

    /// Presents the acknowledged frame again with the heartbeat cell
    /// changed. The marker and the fixture stay the same.
    fn beat_due(&mut self, result: Result<(), String>) {
        if let Err(e) = result {
            let reason = format!("the heartbeat timer stopped: {e}");
            return self.finish_capture(CaptureOutcome::Fail, reason, true);
        }
        let (Some(open), Some(shown), Some(c)) = (&self.open, &self.current, &mut self.capturing)
        else {
            unreachable!("a capture ends before its frame or surface");
        };
        let h = c.heartbeat.as_mut().expect("polled with a heartbeat");
        h.set = !h.set;
        let (marker, shown_image) = shown.marked.as_ref().expect("captures need a marker");
        let mut image = (**shown_image).clone();
        marker.draw_heartbeat(&mut image, shown.format, h.set);
        h.beat = Beat::Present(Box::pin(present(
            Rc::clone(&self.host.driver),
            open.surface.clone(),
            Rc::new(image),
            shown.format,
            shown.metadata.as_ref().map(hdr10_request),
        )));
    }

    fn beat_presented(&mut self, result: Result<String, OutputError>) {
        match result {
            Ok(_) => {
                let c = self.capturing.as_mut().expect("polled while capturing");
                let h = c.heartbeat.as_mut().expect("polled with a heartbeat");
                h.beat = Beat::Wait(self.host.clock.sleep(h.interval_ms));
            }
            Err(e) => {
                let reason = format!("the heartbeat presentation failed: {e}");
                self.finish_capture(CaptureOutcome::Fail, reason, true);
            }
        }
    }

    /// Ends the session with `result`. With `save`, the last frame of the
    /// session goes to the artifact: the accepted frame, or a frame that
    /// failed, for its raw data.
    fn finish_capture(&mut self, result: CaptureOutcome, reason: String, save: bool) {
        let c = self.capturing.take().expect("a capture in progress");
        let Capturing {
            frame,
            gate,
            deadline_ms,
            heartbeat,
            ..
        } = c;
        let expected = *gate.expected();
        let acceptance = Acceptance {
            submitted: expected.submitted,
            marker: expected.marker.value,
            marker_origin: [expected.marker.origin.0, expected.marker.origin.1],
            frames: gate.accepted().to_vec(),
            received: gate.frames(),
            deadline_ms,
            heartbeat_ms: heartbeat.map(|h| h.interval_ms),
        };
        let reason = (result != CaptureOutcome::Pass).then_some(reason);
        let last = if save { gate.into_last() } else { None };
        let Some(capture) = last else {
            return self.emit(&Event::Captured {
                frame,
                result,
                reason,
                cause: None,
                artifact: None,
            });
        };
        self.save(frame, result, reason, acceptance, capture);
    }

    /// Queues the artifact of a finished capture.
    fn save(
        &mut self,
        frame: u64,
        result: CaptureOutcome,
        reason: Option<String>,
        acceptance: Acceptance,
        capture: Capture,
    ) {
        let event = |result, reason, artifact| Event::Captured {
            frame,
            result,
            reason,
            cause: None,
            artifact,
        };
        let open = self
            .open
            .as_ref()
            .expect("a capture ends before its surface");
        let environment = self.host.artifacts.as_mut().map(|a| {
            let mut e = a.environment.read(&open.surface.display);
            e.display_setup = self.dev.setup().cloned();
            e.profile_sha256 = self.dev.selected().map(|h| h.to_string());
            e
        });
        let Some(environment) = environment else {
            let e = match result {
                CaptureOutcome::Pass => {
                    event(CaptureOutcome::Unavailable, Some(NO_ARTIFACTS.into()), None)
                }
                _ => event(result, reason, None),
            };
            return self.emit(&e);
        };
        let artifact = Artifact {
            check: Check {
                result,
                reason: reason.clone(),
            },
            run: self.run_record(environment),
            acceptance,
            capture,
        };
        let sink = &mut self.host.artifacts.as_mut().expect("checked above").sink;
        let e = match sink.submit(artifact) {
            Ok(name) => event(result, reason, Some(name)),
            Err(e) => {
                let reason = match reason {
                    Some(r) => format!("{r}; not saved: {e}"),
                    None => e,
                };
                event(CaptureOutcome::Fail, Some(reason), None)
            }
        };
        self.emit(&e);
    }

    /// Of the acknowledged frame on the open surface.
    fn run_record(&self, environment: Environment) -> RunRecord {
        let (Some(open), Some(shown)) = (&self.open, &self.current) else {
            unreachable!("a capture ends before its frame or surface");
        };
        RunRecord {
            driver: self.host.viewer.id().into(),
            display: open.surface.display.to_string(),
            generation: open.generation,
            surface_size: [open.surface.size.0, open.surface.size.1],
            surface: open.surface.description.clone(),
            mode: open.mode.clone(),
            frame: shown.frame,
            fixture: shown.fixture.clone(),
            format: FormatRecord {
                encoding: shown.format.encoding.id().into(),
                depth: shown.format.depth.bits(),
            },
            metadata: shown.metadata.clone(),
            delay_ms: shown.delay_ms,
            environment,
        }
    }
}

/// `captured` without an artifact.
fn captured(frame: u64, result: CaptureOutcome, reason: String) -> Event {
    Event::Captured {
        frame,
        result,
        reason: Some(reason),
        cause: None,
        artifact: None,
    }
}

fn surface_event(open: &Open, state: SurfaceState, cause: Option<Cause>) -> Event {
    let is_open = state == SurfaceState::Open;
    Event::Surface {
        generation: open.generation,
        state,
        display: open.surface.display.to_string(),
        size: is_open.then_some([open.surface.size.0, open.surface.size.1]),
        description: is_open.then(|| open.surface.description.clone()),
        cause,
    }
}

/// Owns its inputs, so the pending frame needs no host borrow.
#[allow(clippy::await_holding_refcell_ref)] // See `Machine::pending`.
async fn present<D: OutputDriver>(
    driver: Rc<RefCell<D>>,
    surface: Surface,
    image: Rc<Image>,
    format: Format,
    metadata: Option<Hdr10Request>,
) -> Result<String, OutputError> {
    let frame = Frame {
        image: &image,
        format,
        metadata: metadata.as_ref(),
    };
    driver.borrow_mut().present(&surface, &frame).await
}

#[cfg(test)]
#[path = "controller_tests.rs"]
mod tests;
