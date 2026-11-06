//! `OutputDriver` for the browser client. See
//! `doc/design/output_driver/browser.md`.
//!
//! `displays`, `query` and `open` read the client state that the session
//! cached from `hello`; they never wait for the client.

use std::sync::Arc;

use nitpick_core::driver::{
    DisplayId, DisplayInfo, Frame, OutputDriver, OutputError, PresentFuture, Query, Surface,
    check_open,
};
use nitpick_core::signal::BitDepth;

use crate::client::ClientRecord;
use crate::service::BrowserService;
use crate::session::SessionHub;

/// [`DisplayId::driver`] of browser clients.
pub const DRIVER: &str = "browser";

/// Stale-surface reason for the status line.
const VIEWPORT_CHANGED: &str = "the browser viewport changed; open the surface again";

#[derive(Debug)]
struct Opened {
    display: DisplayId,
    depth: BitDepth,
    size: (usize, usize),
    /// [`ClientRecord::generation`] at `open`.
    generation: u64,
    #[allow(dead_code)] // For the presentation status.
    warnings: Vec<String>,
}

/// Browser output driver. It shares the session state with the service;
/// the host keeps the [`BrowserService`] and its event receiver separately.
#[derive(Debug)]
pub struct BrowserDriver {
    hub: Arc<SessionHub>,
    opened: Option<Opened>,
}

impl BrowserDriver {
    #[must_use]
    pub fn new(service: &BrowserService) -> Self {
        Self::with_hub(Arc::clone(service.hub()))
    }

    pub(crate) fn with_hub(hub: Arc<SessionHub>) -> Self {
        Self { hub, opened: None }
    }

    /// The cached state of `display`.
    ///
    /// # Errors
    /// `Disconnected` if no client with a valid `hello` has this ID.
    fn client(&self, display: &DisplayId) -> Result<ClientRecord, OutputError> {
        self.hub
            .client()
            .filter(|c| c.display == *display)
            .ok_or(OutputError::Disconnected)
    }
}

impl OutputDriver for BrowserDriver {
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
        Ok(self.client(display)?.hello.query())
    }

    fn open(&mut self, display: &DisplayId, depth: BitDepth) -> Result<Surface, OutputError> {
        let client = self.client(display)?;
        let mut warnings = check_open(&client.hello.query().capabilities, depth)?;
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
            warnings,
        });
        Ok(Surface {
            display: display.clone(),
            size,
            description,
        })
    }

    fn present<'a>(&'a mut self, surface: &'a Surface, frame: &'a Frame<'a>) -> PresentFuture<'a> {
        Box::pin(async move {
            let open = self
                .opened
                .as_ref()
                .filter(|o| o.display == surface.display && o.size == surface.size)
                .ok_or(OutputError::Disconnected)?;
            // From the cached state only: a stale surface fails without a
            // network wait.
            let client = self.client(&open.display)?;
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
            Err(OutputError::Platform(
                "browser presentation is not implemented".into(),
            ))
        })
    }

    fn close(&mut self, display: &DisplayId) {
        if self.opened.as_ref().is_some_and(|o| o.display == *display) {
            self.opened = None;
        }
    }
}

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;
