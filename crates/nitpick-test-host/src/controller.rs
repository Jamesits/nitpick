//! The test-host state machine: protocol requests, driver-side events, one
//! open surface and one pending presentation or measurement delay. Each
//! host polls [`TestHost::run`] with its own executor. See "Host boundary"
//! in `doc/design/output_driver/testing.md`.

use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use futures_core::Stream;
use futures_util::StreamExt as _;
use nitpick_core::driver::{DisplayId, Frame, Hdr10Request, OutputDriver, OutputError, Surface};
use nitpick_core::image::Image;
use nitpick_core::signal::{BitDepth, Format};

use crate::fixture::{Fixtures, hdr10_request};
use crate::protocol::{
    CaptureOutcome, Cause, DisplayEntry, DisplayState, ErrorKind, Event, Request, SurfaceState,
    VERSION, parse_request,
};

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
}

/// The acknowledged frame to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureTarget<'a> {
    pub display: &'a DisplayId,
    pub frame: u64,
    pub generation: u64,
}

/// Raw captures are not implemented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureResult {
    Unavailable { reason: String },
}

/// An OS capture adapter. It is not part of `OutputDriver`.
pub trait CaptureAdapter {
    fn capture(&mut self, target: &CaptureTarget<'_>) -> CaptureResult;
}

/// A host without a capture adapter.
#[derive(Debug, Clone)]
pub struct NoCapture(pub String);

impl CaptureAdapter for NoCapture {
    fn capture(&mut self, _: &CaptureTarget<'_>) -> CaptureResult {
        CaptureResult::Unavailable {
            reason: self.0.clone(),
        }
    }
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
    pub fixtures: Rc<Fixtures>,
    /// Measurement delay of a `show` without `delay_ms`.
    pub delay_ms: u32,
}

struct Open {
    surface: Surface,
    generation: u64,
    stale: bool,
    watch: Option<LocalFuture<()>>,
}

/// At most one; dropping it cancels the presentation or the timer.
enum Pending {
    Presenting {
        frame: u64,
        generation: u64,
        delay_ms: u32,
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
            Self::Presenting { frame, .. } | Self::Delay { frame, .. } => *frame,
        }
    }
}

enum Input {
    Line(Option<String>),
    Driver(Option<DriverEvent>),
    Refreshed,
    SurfaceClosed,
    Presented {
        frame: u64,
        generation: u64,
        delay_ms: u32,
        result: Result<String, OutputError>,
    },
    Elapsed {
        frame: u64,
        delay_ms: u32,
        result: Result<(), String>,
    },
}

struct Machine<D, O> {
    host: TestHost<D>,
    out: O,
    /// Invariant: `driver` has no other borrow while `Presenting`.
    pending: Option<Pending>,
    open: Option<Open>,
    refreshing: Option<LocalFuture<()>>,
    /// Of the last `open`.
    generation: u64,
    /// Of the last `show` that passed the order check.
    last_frame: Option<u64>,
    /// The acknowledged frame on the open surface, and its generation.
    current: Option<(u64, u64)>,
}

impl<D: OutputDriver + 'static> TestHost<D> {
    /// Runs until `quit`, the end of `lines` or [`DriverEvent::Stopped`].
    /// Then it ends the pending frame, closes the surface and sends
    /// [`Event::Stopped`].
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
            open: None,
            refreshing: None,
            generation: 0,
            last_frame: None,
            current: None,
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
        m.cancel(Cause::Stopped);
        m.close_surface(Cause::Stopped);
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
        let ready = match &mut self.pending {
            Some(Pending::Presenting { future, .. }) => future.as_mut().poll(cx).map(Ok),
            Some(Pending::Delay { timer, .. }) => timer.as_mut().poll(cx).map(Err),
            None => Poll::Pending,
        };
        let Poll::Ready(result) = ready else {
            return Poll::Pending;
        };
        Poll::Ready(match (self.pending.take(), result) {
            (
                Some(Pending::Presenting {
                    frame,
                    generation,
                    delay_ms,
                    ..
                }),
                Ok(result),
            ) => Input::Presented {
                frame,
                generation,
                delay_ms,
                result,
            },
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
            Input::Refreshed => self.displays(),
            Input::SurfaceClosed => self.surface_lost(),
            Input::Presented {
                frame,
                generation,
                delay_ms,
                result,
            } => self.presented(frame, generation, delay_ms, result),
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
            } => self.show(frame, &fixture, delay_ms, metadata),
            Request::Capture { frame } => self.capture(frame),
            Request::Close {} => {
                if self.open.is_some() {
                    self.cancel(Cause::Closed);
                    self.close_surface(Cause::Closed);
                } else {
                    self.emit(&Event::error(None, ErrorKind::NoSurface, "no open surface"));
                }
            }
            Request::Quit {} => return Some(Exit::Quit),
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
            Some(Pending::Presenting { frame, .. }) => {
                self.emit(&Event::output_error(Some(frame), error));
            }
            Some(Pending::Delay { frame, .. }) => self.emit(&Event::Cancelled { frame, cause }),
            None => {}
        }
    }

    /// The display disconnected or the surface closed outside the host.
    fn surface_lost(&mut self) {
        self.fail_pending(&OutputError::Disconnected, Cause::Disconnected);
        self.close_surface(Cause::Disconnected);
    }

    /// Call without a pending frame.
    fn close_surface(&mut self, cause: Cause) {
        debug_assert!(self.pending.is_none(), "the presentation holds the driver");
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
        let opened = driver
            .query(&display)
            .and_then(|_| driver.open(&display, depth));
        if opened.is_err() && earlier.as_ref() == Some(&display) {
            // The event above reported the earlier surface closed.
            driver.close(&display);
        }
        drop(driver);
        match opened {
            Ok(surface) => {
                self.generation += 1;
                let open = Open {
                    surface,
                    generation: self.generation,
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
        let (kind, message) = match &self.open {
            None => (ErrorKind::NoSurface, "no open surface".into()),
            Some(open) if open.stale => (
                ErrorKind::StaleSurface,
                "the display changed; open the surface again".into(),
            ),
            Some(open) => match fx.render(open.surface.size) {
                Ok(image) => {
                    let (surface, generation) = (open.surface.clone(), open.generation);
                    self.cancel(Cause::Replaced);
                    self.current = None;
                    let future = present(
                        Rc::clone(&self.host.driver),
                        surface,
                        image,
                        format,
                        metadata.as_ref().map(hdr10_request),
                    );
                    self.pending = Some(Pending::Presenting {
                        frame,
                        generation,
                        delay_ms: delay_ms.unwrap_or(self.host.delay_ms),
                        future: Box::pin(future),
                    });
                    return;
                }
                Err(e) => (ErrorKind::FixtureTooLarge, e),
            },
        };
        self.emit(&error(kind, message));
    }

    /// A failed presentation starts no timer. A zero delay adds no wait.
    fn presented(
        &mut self,
        frame: u64,
        generation: u64,
        delay_ms: u32,
        result: Result<String, OutputError>,
    ) {
        let status = match result {
            Ok(status) => status,
            Err(e) => return self.emit(&Event::output_error(Some(frame), &e)),
        };
        self.current = Some((frame, generation));
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

    fn capture(&mut self, frame: u64) {
        let open = self
            .open
            .as_ref()
            .filter(|o| !o.stale && self.current == Some((frame, o.generation)));
        let Some(open) = open else {
            let e = Event::error(
                Some(frame),
                ErrorKind::StaleFrame,
                format!("frame {frame} is not the acknowledged frame of the open surface"),
            );
            return self.emit(&e);
        };
        let target = CaptureTarget {
            display: &open.surface.display,
            frame,
            generation: open.generation,
        };
        let CaptureResult::Unavailable { reason } = self.host.capture.capture(&target);
        self.emit(&Event::Captured {
            frame,
            result: CaptureOutcome::Unavailable,
            reason,
        });
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
    image: Image,
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
