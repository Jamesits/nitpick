//! Browser service: the server on a dedicated Tokio thread, and the
//! host handle and event receiver. See "Host and dependency boundary" in
//! `doc/design/output_driver/overview.md`.
//!
//! The handle and the receiver are separate from the driver, so the host
//! receives events while `present` holds the mutable driver borrow.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use nitpick_core::driver::DisplayId;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::MAX_CLIENT_MESSAGE;
use crate::session::{Incoming, Pairing, ReserveError, SessionHub, Transport, run_session};

const INDEX_HTML: &str = include_str!("../client/index.html");

/// Wait for open HTTP requests after the shutdown signal. Hyper's graceful
/// shutdown has no limit of its own: an idle connection that has not sent
/// a request keeps it waiting. After this time the runtime is dropped,
/// which aborts the remaining connections.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// Longest configured fixed code. It must fit in a client message.
const MAX_FIXED_CODE: usize = 256;

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
    /// `None`: a new random 4-digit code for each connection. `Some`: this
    /// string for each connection, for headless use and tests. Each session
    /// still gets a new random bearer token.
    pub fixed_code: Option<String>,
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
    /// The configured fixed code cannot be used.
    PairingCode(String),
    Bind(io::Error),
    /// Thread creation failed.
    Thread(io::Error),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Address(reason) => write!(f, "invalid bind address: {reason}"),
            Self::PairingCode(reason) => write!(f, "invalid pairing code: {reason}"),
            Self::Bind(e) => write!(f, "cannot bind the browser service: {e}"),
            Self::Thread(e) => write!(f, "cannot start the browser service: {e}"),
        }
    }
}

impl std::error::Error for StartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Address(_) | Self::PairingCode(_) => None,
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
    pub(crate) fn channel() -> (Self, ServiceEvents) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(tx), ServiceEvents { rx })
    }

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
    hub: Arc<SessionHub>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl BrowserService {
    /// Binds before return, so bind errors come from this call. The
    /// returned receiver first gives [`ServiceEvent::Listening`].
    ///
    /// # Errors
    /// An unspecified bind address, an empty or long fixed code, a bind
    /// failure or a thread failure.
    pub fn start(config: &ServiceConfig) -> Result<(Self, ServiceEvents), StartError> {
        let pairing = match &config.fixed_code {
            None => Pairing::Random,
            Some(code) if code.is_empty() || code.len() > MAX_FIXED_CODE => {
                return Err(StartError::PairingCode(format!(
                    "length {} is outside 1..={MAX_FIXED_CODE} bytes",
                    code.len()
                )));
            }
            Some(code) => Pairing::Fixed(code.clone()),
        };
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

        let (events, receiver) = EventSender::channel();
        let url = format!("http://{addr}/");
        events.send(ServiceEvent::Listening { url: url.clone() });
        let hub = SessionHub::new(events.clone(), pairing);

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let thread_events = events.clone();
        let thread_hub = Arc::clone(&hub);
        let thread = thread::Builder::new()
            .name("nitpick-browser".into())
            .spawn(move || {
                let error = serve(listener, thread_hub, shutdown_rx)
                    .err()
                    .map(|e| e.to_string());
                thread_events.send(ServiceEvent::Stopped { error });
            })
            .map_err(StartError::Thread)?;

        Ok((
            Self {
                addr,
                url,
                events,
                hub,
                shutdown: Some(shutdown_tx),
                thread: Some(thread),
            },
            receiver,
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

    #[allow(dead_code)] // For the driver's frame storage.
    pub(crate) fn hub(&self) -> &Arc<SessionHub> {
        &self.hub
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

fn router(hub: Arc<SessionHub>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX_HTML) }))
        .route("/api/v1/ws", get(websocket))
        .route("/api/v1/frame/{file}", get(frame))
        .with_state(hub)
}

async fn websocket(State(hub): State<Arc<SessionHub>>, upgrade: WebSocketUpgrade) -> Response {
    // Reserve before the upgrade: a second client gets an HTTP status, not
    // an open WebSocket that closes at once.
    let reservation = match hub.reserve() {
        Ok(reservation) => reservation,
        Err(ReserveError::Busy) => {
            return (StatusCode::CONFLICT, "another client holds the session").into_response();
        }
        Err(ReserveError::Locked(remaining)) => {
            let seconds = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, seconds.to_string())],
                "too many failed attempts",
            )
                .into_response();
        }
    };
    upgrade
        .max_message_size(MAX_CLIENT_MESSAGE)
        .max_frame_size(MAX_CLIENT_MESSAGE)
        // A failed upgrade drops the closure and with it the reservation.
        .on_upgrade(move |socket| run_session(WsTransport(socket), reservation))
}

/// Every response, including errors, is `no-store`: a cache must not keep
/// a frame or an authorization result.
async fn frame(
    State(hub): State<Arc<SessionHub>>,
    Path(file): Path<String>,
    headers: HeaderMap,
) -> Response {
    const NO_STORE: (header::HeaderName, HeaderValue) =
        (header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Before the frame lookup, so that a client without the token cannot
    // probe frame IDs.
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if !hub.authorize(authorization) {
        return (
            StatusCode::UNAUTHORIZED,
            [
                NO_STORE,
                (header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer")),
            ],
        )
            .into_response();
    }
    let png = file
        .strip_suffix(".png")
        .and_then(|id| id.parse::<u64>().ok())
        .and_then(|id| hub.frame(id));
    match png {
        Some(png) => (
            [
                NO_STORE,
                (header::CONTENT_TYPE, HeaderValue::from_static("image/png")),
            ],
            png,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, [NO_STORE]).into_response(),
    }
}

struct WsTransport(WebSocket);

impl Transport for WsTransport {
    async fn recv(&mut self) -> Incoming {
        loop {
            return match self.0.recv().await {
                None => Incoming::Closed("connection closed".into()),
                Some(Err(e)) => Incoming::Closed(format!("connection failed: {e}")),
                Some(Ok(Message::Text(text))) => Incoming::Text(text.as_str().to_owned()),
                Some(Ok(Message::Binary(_))) => Incoming::Binary,
                Some(Ok(Message::Close(_))) => Incoming::Closed("client closed".into()),
                // The WebSocket library answers pings itself.
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            };
        }
    }

    async fn send(&mut self, text: String) -> Result<(), String> {
        self.0
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| format!("connection failed: {e}"))
    }

    async fn close(&mut self, code: u16, reason: &'static str) {
        let _ = self
            .0
            .send(Message::Close(Some(CloseFrame {
                code,
                reason: reason.into(),
            })))
            .await;
    }
}

fn serve(
    listener: TcpListener,
    hub: Arc<SessionHub>,
    shutdown: oneshot::Receiver<()>,
) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let (begun_tx, begun_rx) = oneshot::channel();
        let server = axum::serve(listener, router(hub)).with_graceful_shutdown(async move {
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
