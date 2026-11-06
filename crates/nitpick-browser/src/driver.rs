//! `OutputDriver` for the browser client. See
//! `doc/design/output_driver/browser.md`.
//!
//! `displays`, `query` and `open` read the client state that the session
//! cached from `hello`; they never wait for the client. `present` encodes
//! on a worker, stores the PNG in the session hub and awaits the matching
//! `applied` through a oneshot channel. The session task on the server
//! runtime runs the acknowledgement timeout and wakes the host task.

use std::sync::Arc;

use axum::body::Bytes;
use nitpick_core::driver::{
    DisplayId, DisplayInfo, Frame, OutputDriver, OutputError, PresentFuture, Query, Support,
    Surface, check_frame, check_open,
};
use nitpick_core::signal::BitDepth;
use tokio::sync::oneshot;

use crate::client::{ClientRecord, VIEWPORT_CHANGED};
use crate::encode::encode_frame;
use crate::service::BrowserService;
use crate::session::SessionHub;
use crate::worker::{EncodeWorker, Workers};

/// [`DisplayId::driver`] of browser clients.
pub const DRIVER: &str = "browser";

#[derive(Debug)]
struct Opened {
    display: DisplayId,
    depth: BitDepth,
    size: (usize, usize),
    /// [`ClientRecord::generation`] at `open`.
    generation: u64,
}

/// Browser output driver. It shares the session state with the service;
/// the host keeps the [`BrowserService`] and its event receiver separately.
#[derive(Debug)]
pub struct BrowserDriver<J = EncodeWorker> {
    hub: Arc<SessionHub>,
    workers: J,
    opened: Option<Opened>,
}

impl BrowserDriver {
    #[must_use]
    pub fn new(service: &BrowserService) -> Self {
        Self::with_hub(Arc::clone(service.hub()), EncodeWorker::default())
    }
}

impl<J: Workers> BrowserDriver<J> {
    pub(crate) fn with_hub(hub: Arc<SessionHub>, workers: J) -> Self {
        Self {
            hub,
            workers,
            opened: None,
        }
    }
}

/// The cached state of `display`.
///
/// # Errors
/// `Disconnected` if no client with a valid `hello` has this ID.
fn client(hub: &SessionHub, display: &DisplayId) -> Result<ClientRecord, OutputError> {
    hub.client()
        .filter(|c| c.display == *display)
        .ok_or(OutputError::Disconnected)
}

/// Removes the pending registration when the `present` future completes
/// or is dropped.
struct Registration<'a> {
    hub: &'a SessionHub,
    id: u64,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.hub.cancel(self.id);
    }
}

impl<J: Workers> OutputDriver for BrowserDriver<J> {
    fn id(&self) -> &'static str {
        DRIVER
    }

    fn displays(&mut self) -> Vec<DisplayInfo> {
        self.hub
            .client()
            .map(|c| DisplayInfo {
                name: c.hello.display_name(c.peer),
                size: c.hello.surface_size(),
                id: c.display,
            })
            .into_iter()
            .collect()
    }

    fn query(&mut self, display: &DisplayId) -> Result<Query, OutputError> {
        Ok(client(&self.hub, display)?.query())
    }

    fn open(&mut self, display: &DisplayId, depth: BitDepth) -> Result<Surface, OutputError> {
        let client = client(&self.hub, display)?;
        let mut warnings = check_open(&client.query().capabilities, depth)?;
        let size = client.hello.surface_size();
        let mut description = format!("16-bit PNG, {}×{} display px", size.0, size.1);
        if let Some(sdr) = client.hello.sdr_reason() {
            warnings.push(format!("SDR preview: {sdr}"));
        }
        if let Some(layout) = client.hello.layout_warning() {
            warnings.push(layout.into());
        }
        for warning in &warnings {
            description.push_str("; ");
            description.push_str(warning);
        }
        self.opened = Some(Opened {
            display: display.clone(),
            depth,
            size,
            generation: client.generation,
        });
        Ok(Surface {
            display: display.clone(),
            size,
            description,
        })
    }

    fn present<'a>(&'a mut self, surface: &'a Surface, frame: &'a Frame<'a>) -> PresentFuture<'a> {
        Box::pin(async move {
            let Self {
                hub,
                workers,
                opened,
            } = self;
            let open = opened
                .as_ref()
                .filter(|o| o.display == surface.display && o.size == surface.size)
                .ok_or(OutputError::Disconnected)?;
            // From the cached state only: a stale surface fails without a
            // network wait.
            let client = client(hub, &open.display)?;
            if client.generation != open.generation {
                return Err(OutputError::Platform(VIEWPORT_CHANGED.into()));
            }
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
            // Rejects the frame before the encoding; the status repeats the
            // check with the state after `applied`.
            check_frame(&client.query().capabilities, frame.format, frame.metadata)?;

            // The job owns copies. Dropping the future drops the receiver;
            // the job then skips or discards its result.
            let (tx, rx) = oneshot::channel();
            let (image, format, request) =
                (frame.image.clone(), frame.format, frame.metadata.copied());
            workers.spawn(Box::new(move || {
                if !tx.is_closed() {
                    let _ = tx.send(encode_frame(&image, format, request.as_ref()));
                }
            }));
            let encoded = rx
                .await
                .map_err(|_| OutputError::Platform("the PNG encoding worker stopped".into()))??;

            // `submit` checks the generation again: the client can resize
            // during encoding.
            let (id, reply) = hub.submit(
                &open.display,
                open.generation,
                open.size,
                Bytes::from(encoded.bytes),
            )?;
            let _registration = Registration { hub, id };
            // A dropped sender: a newer frame replaced this one.
            let layout = reply.await.map_err(|_| OutputError::Disconnected)??;
            // The mode can change during the wait without a new surface.
            let client = hub
                .client()
                .filter(|c| c.display == open.display)
                .unwrap_or(client);

            let mut caps = client.query().capabilities;
            caps.pixel_exact = client.hello.pixel_exact(Some(layout));
            let mut warnings = check_open(&caps, open.depth)?;
            warnings.extend(check_frame(&caps, frame.format, frame.metadata)?);
            warnings.extend(encoded.clipping.warning());
            let pixel = if caps.pixel_exact == Support::Yes {
                " Pixel-exact layout."
            } else {
                ""
            };
            let warnings = if warnings.is_empty() {
                String::new()
            } else {
                format!(" Warnings: {}.", warnings.join("; "))
            };
            // The mode description has the SDR preview and full-screen
            // warnings; the open description is not reused, because its
            // pixel-exact state predates `applied`.
            Ok(format!(
                "Frame {id} applied by the client: {}, 16-bit PNG, {}×{} display px. {}.{pixel}{warnings}",
                frame.format,
                open.size.0,
                open.size.1,
                client.query().mode.description
            ))
        })
    }

    fn close(&mut self, display: &DisplayId) {
        if self.opened.as_ref().is_some_and(|o| o.display == *display) {
            self.opened = None;
            self.hub.close(display);
        }
    }
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
