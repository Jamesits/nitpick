//! Browser service: the server on a dedicated Tokio thread, and the
//! host handle and event receiver. See "Host and dependency boundary" in
//! `doc/design/output_driver/overview.md`.
//!
//! The handle and the receiver are separate from the driver, so the host
//! receives events while `present` holds the mutable driver borrow.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::thread;
use std::time::Duration;

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use nitpick_core::driver::DisplayId;
use tokio::sync::{mpsc, oneshot};

const INDEX_HTML: &str = include_str!("../client/index.html");

/// Wait for open HTTP requests after the shutdown signal. Hyper's graceful
/// shutdown has no limit of its own: an idle connection that has not sent
/// a request keeps it waiting. After this time the runtime is dropped,
/// which aborts the remaining connections.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// Interface for the listening socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BindHost {
    /// `127.0.0.1`: a browser on the same computer only.
    #[default]
    Loopback,
    /// An explicit interface address, for a remote display. It must be a
    /// specific address, because it is also the host part of the URL.
    Address(IpAddr),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceConfig {
    pub host: BindHost,
    /// 0: the OS selects a free port. The URL gives the result.
    pub port: u16,
}

/// Change of the single client session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientState {
    /// A WebSocket session reserved the slot and waits for authentication.
    Connected,
    Authenticated,
    /// A valid `hello` arrived. Each new `hello` gives this event again;
    /// the host queries the display again and reopens an open surface.
    Ready(DisplayId),
    /// The session closed; the reason is for the status line.
    Disconnected(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceEvent {
    /// Always the first event.
    Listening {
        url: String,
    },
    /// Code for the user to enter in the client.
    PairingCode(String),
    Client(ClientState),
    /// Always the last event. `error` is set if the server stopped
    /// without a shutdown request.
    Stopped {
        error: Option<String>,
    },
}

#[derive(Debug)]
pub enum StartError {
    /// The configured address cannot be used.
    Address(String),
    Bind(io::Error),
    /// Thread creation failed.
    Thread(io::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Address(reason) => write!(f, "invalid bind address: {reason}"),
            Self::Bind(e) => write!(f, "cannot bind the browser service: {e}"),
            Self::Thread(e) => write!(f, "cannot start the browser service: {e}"),
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Address(_) => None,
            Self::Bind(e) | Self::Thread(e) => Some(e),
        }
    }
}

/// Service events in order. Independent of the executor: any Rust
/// executor can await [`ServiceEvents::next`].
#[derive(Debug)]
pub struct ServiceEvents {
    rx: mpsc::UnboundedReceiver<ServiceEvent>,
}

impl ServiceEvents {
    /// `None` after [`ServiceEvent::Stopped`] once the service handle is
    /// gone.
    pub async fn next(&mut self) -> Option<ServiceEvent> {
        self.rx.recv().await
    }

    /// Does not wait. `None` if no event is queued.
    pub fn try_next(&mut self) -> Option<ServiceEvent> {
        self.rx.try_recv().ok()
    }
}

/// Unbounded: events are rare and small, and a dropped pairing code or
/// disconnect would desynchronise the host. Senders ignore a dropped
/// receiver.
#[derive(Debug, Clone)]
pub(crate) struct EventSender(mpsc::UnboundedSender<ServiceEvent>);

impl EventSender {
    pub(crate) fn send(&self, event: ServiceEvent) {
        let _ = self.0.send(event);
    }
}

/// Running service. Dropping it also stops the server.
#[derive(Debug)]
pub struct BrowserService {
    addr: SocketAddr,
    url: String,
    events: EventSender,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl BrowserService {
    /// Binds before return, so bind errors come from this call. The
    /// returned receiver first gives [`ServiceEvent::Listening`].
    ///
    /// # Errors
    /// An unspecified bind address, a bind failure or a thread failure.
    pub fn start(config: &ServiceConfig) -> Result<(Self, ServiceEvents), StartError> {
        let ip = match config.host {
            BindHost::Loopback => IpAddr::V4(Ipv4Addr::LOCALHOST),
            BindHost::Address(ip) if ip.is_unspecified() => {
                return Err(StartError::Address(format!(
                    "{ip} is not a specific interface; the URL needs one"
                )));
            }
            BindHost::Address(ip) => ip,
        };
        let listener = TcpListener::bind((ip, config.port)).map_err(StartError::Bind)?;
        let addr = listener.local_addr().map_err(StartError::Bind)?;
        // Required by `tokio::net::TcpListener::from_std`.
        listener.set_nonblocking(true).map_err(StartError::Bind)?;

        let (tx, rx) = mpsc::unbounded_channel();
        let events = EventSender(tx);
        let url = format!("http://{addr}/");
        events.send(ServiceEvent::Listening { url: url.clone() });

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let thread_events = events.clone();
        let thread = thread::Builder::new()
            .name("nitpick-browser".into())
            .spawn(move || {
                let error = serve(listener, shutdown_rx).err().map(|e| e.to_string());
                thread_events.send(ServiceEvent::Stopped { error });
            })
            .map_err(StartError::Thread)?;

        Ok((
            Self {
                addr,
                url,
                events,
                shutdown: Some(shutdown_tx),
                thread: Some(thread),
            },
            ServiceEvents { rx },
        ))
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Stops the server and waits for its thread, at most about
    /// [`SHUTDOWN_GRACE`]. The port is free on return.
    pub fn shutdown(mut self) {
        self.stop();
    }

    #[allow(dead_code)] // For the session and driver code.
    pub(crate) fn event_sender(&self) -> EventSender {
        self.events.clone()
    }

    fn stop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for BrowserService {
    fn drop(&mut self) {
        self.stop();
    }
}

fn router() -> Router {
    Router::new().route("/", get(|| async { Html(INDEX_HTML) }))
}

fn serve(listener: TcpListener, shutdown: oneshot::Receiver<()>) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let (begun_tx, begun_rx) = oneshot::channel();
        let server = axum::serve(listener, router()).with_graceful_shutdown(async move {
            // A dropped sender also stops the server.
            let _ = shutdown.await;
            let _ = begun_tx.send(());
        });
        tokio::select! {
            result = server => result,
            () = async {
                if begun_rx.await.is_ok() {
                    tokio::time::sleep(SHUTDOWN_GRACE).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => Ok(()),
        }
    })
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
