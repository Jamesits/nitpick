//! The single client session: slot reservation, pairing code, bearer
//! token, the failed-attempt limit, and frame presentation. See
//! "Authentication" and "Image requests and presentation" in
//! `doc/design/output_driver/browser.md`.
//!
//! The session logic uses [`Transport`] and the Tokio clock only, so tests
//! drive it with a mock transport and a paused clock.
//!
//! The driver registers one pending frame in the hub and gives the
//! session a `show` command. The session task sends it, runs the
//! acknowledgement timeout on the server runtime and resolves the pending
//! frame from `applied`, `error`, a resize or the end of the session.

use std::fmt::Write as _;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::body::Bytes;
use nitpick_core::driver::{DisplayId, OutputError};
use tokio::sync::{oneshot, watch};
use tokio::time::Instant;

use crate::client::{ClientRecord, Hello, VIEWPORT_CHANGED};
use crate::driver::DRIVER;
use crate::protocol::{Applied, ClientMessage, ServerMessage, close};
use crate::service::{ClientState, EventSender, ServiceEvent};

/// From the start of the session to the `auth` message.
pub(crate) const AUTH_TIMEOUT: Duration = Duration::from_secs(60);

/// Lock after the first failed attempt. Each further consecutive failure
/// doubles it, up to [`LOCKOUT_MAX`]. With a new random code for each
/// connection, one guess succeeds with probability 1e-4; at the maximum
/// lock a guesser gets at most 1440 guesses a day.
const LOCKOUT_FIRST: Duration = Duration::from_secs(1);
const LOCKOUT_MAX: Duration = Duration::from_secs(60);

/// 256 bits from the OS generator.
const TOKEN_BYTES: usize = 32;

/// From sending `show` to the matching `applied`. The limit only ends a
/// wait for a client that stopped. **Unverified:** decode and layout of
/// the largest PNG on the slowest supported client take less time.
pub(crate) const ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// Result of a pending frame: the layout size in display pixels.
pub(crate) type Reply = Result<(f64, f64), OutputError>;

/// Command from the driver to the session task. Only the latest command
/// counts: a `show` that the session did not send yet is obsolete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    Idle,
    Show { id: u64, size: (usize, usize) },
    Close,
}

/// The frame that waits for `applied`.
#[derive(Debug)]
struct Pending {
    id: u64,
    size: (usize, usize),
    reply: oneshot::Sender<Reply>,
}

/// Source of the code that the user enters in the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pairing {
    /// A new random 4-digit code for each connection.
    Random,
    /// The same configured string for each connection; for headless use
    /// and tests.
    Fixed(String),
}

impl Pairing {
    fn code(&self) -> Result<String, getrandom::Error> {
        match self {
            Self::Random => random_code(),
            Self::Fixed(code) => Ok(code.clone()),
        }
    }
}

/// Uniform over `0000`–`9999`. Rejection sampling: 2^32 is not a multiple
/// of 10000, so a plain remainder favours low codes.
pub(crate) fn random_code() -> Result<String, getrandom::Error> {
    const LIMIT: u32 = u32::MAX / 10_000 * 10_000;
    loop {
        let value = getrandom::u32()?;
        if value < LIMIT {
            return Ok(format_code(value % 10_000));
        }
    }
}

pub(crate) fn format_code(value: u32) -> String {
    format!("{value:04}")
}

fn random_token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes)?;
    let mut token = String::with_capacity(2 * TOKEN_BYTES);
    for byte in bytes {
        let _ = write!(token, "{byte:02x}");
    }
    Ok(token)
}

/// Comparison time does not depend on the position of the first
/// difference. The length can leak; it is fixed for tokens and not secret
/// for codes.
fn secret_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReserveError {
    /// Another session holds the slot: HTTP 409.
    Busy,
    /// Recent failed attempts: HTTP 429. The value is the remaining lock.
    Locked(Duration),
}

#[derive(Debug, Default)]
struct HubState {
    busy: bool,
    /// Set only between successful authentication and the end of the
    /// session.
    token: Option<String>,
    consecutive_failures: u32,
    locked_until: Option<Instant>,
    /// The PNG of the latest frame. Only this frame is served; a new
    /// frame, `close` or the end of the session releases it.
    frame: Option<(u64, Bytes)>,
    /// Last frame ID. IDs are never reused, also across sessions, so a
    /// late reply cannot match a new frame.
    frames: u64,
    pending: Option<Pending>,
    /// Set only between successful authentication and the end of the
    /// session.
    commands: Option<watch::Sender<Command>>,
    /// Authenticated sessions so far; numbers the display IDs.
    sessions: u64,
    /// Last [`ClientRecord::generation`].
    generation: u64,
    /// Set only between a valid `hello` and the end of the session.
    client: Option<ClientRecord>,
}

/// State shared by the HTTP handlers and the session task.
#[derive(Debug)]
pub(crate) struct SessionHub {
    events: EventSender,
    pairing: Pairing,
    state: Mutex<HubState>,
}

impl SessionHub {
    pub(crate) fn new(events: EventSender, pairing: Pairing) -> Arc<Self> {
        Arc::new(Self {
            events,
            pairing,
            state: Mutex::default(),
        })
    }

    /// No code panics while it holds the lock; a poisoned lock still has
    /// consistent state.
    fn state(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reserves the slot before the WebSocket upgrade completes. The slot
    /// stays reserved until the returned value is dropped.
    pub(crate) fn reserve(self: &Arc<Self>, peer: SocketAddr) -> Result<Reservation, ReserveError> {
        {
            let mut state = self.state();
            if state.busy {
                return Err(ReserveError::Busy);
            }
            if let Some(until) = state.locked_until {
                let now = Instant::now();
                if until > now {
                    return Err(ReserveError::Locked(until - now));
                }
            }
            state.busy = true;
        }
        self.events
            .send(ServiceEvent::Client(ClientState::Connected));
        Ok(Reservation {
            hub: Arc::clone(self),
            peer,
            // Kept only if the session task never ends normally.
            reason: "server stopped or upgrade failed".into(),
        })
    }

    /// `header` is the value of `Authorization`. The scheme is
    /// case-insensitive (RFC 9110, section 11.1).
    pub(crate) fn authorize(&self, header: Option<&str>) -> bool {
        let Some(token) = header.and_then(|h| {
            let (scheme, token) = h.split_once(' ')?;
            scheme.eq_ignore_ascii_case("Bearer").then_some(token)
        }) else {
            return false;
        };
        self.state()
            .token
            .as_deref()
            .is_some_and(|current| secret_eq(current, token))
    }

    #[cfg(test)]
    pub(crate) fn set_frame(&self, id: u64, png: Bytes) {
        self.state().frame = Some((id, png));
    }

    pub(crate) fn frame(&self, id: u64) -> Option<Bytes> {
        match &self.state().frame {
            Some((current, png)) if *current == id => Some(png.clone()),
            _ => None,
        }
    }

    fn record_failure(&self) {
        let mut state = self.state();
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.locked_until = Some(Instant::now() + lockout(state.consecutive_failures));
    }

    /// Returns the display ID of the new session and its command receiver.
    /// A new ID for each session keeps a host from using a surface of an
    /// earlier client.
    pub(crate) fn accept(&self, token: String) -> (DisplayId, watch::Receiver<Command>) {
        let (tx, rx) = watch::channel(Command::Idle);
        let mut state = self.state();
        state.token = Some(token);
        state.consecutive_failures = 0;
        state.locked_until = None;
        state.commands = Some(tx);
        state.sessions += 1;
        let display = DisplayId {
            driver: DRIVER,
            key: format!("client-{}", state.sessions),
        };
        (display, rx)
    }

    /// Stores `png` as the current frame, registers it as pending and
    /// tells the session to send `show`. Replaces an earlier pending frame
    /// and releases its PNG. Uses the cached state only.
    ///
    /// # Errors
    /// `Disconnected` if `display` has no session; `Platform` if the
    /// geometry changed after `generation`.
    pub(crate) fn submit(
        &self,
        display: &DisplayId,
        generation: u64,
        size: (usize, usize),
        png: Bytes,
    ) -> Result<(u64, oneshot::Receiver<Reply>), OutputError> {
        let mut state = self.state();
        let client = state
            .client
            .as_ref()
            .filter(|c| c.display == *display)
            .ok_or(OutputError::Disconnected)?;
        if client.generation != generation {
            return Err(OutputError::Platform(VIEWPORT_CHANGED.into()));
        }
        let commands = state.commands.clone().ok_or(OutputError::Disconnected)?;
        state.frames += 1;
        let id = state.frames;
        let (reply, rx) = oneshot::channel();
        state.frame = Some((id, png));
        state.pending = Some(Pending { id, size, reply });
        commands.send_replace(Command::Show { id, size });
        Ok((id, rx))
    }

    /// Removes the pending registration of frame `id`, if it is still
    /// pending. The PNG stays: the client can still fetch it, and the next
    /// frame or `close` releases it.
    pub(crate) fn cancel(&self, id: u64) {
        let mut state = self.state();
        if state.pending.as_ref().is_some_and(|p| p.id == id) {
            state.pending = None;
        }
    }

    /// Takes the pending registration if it is frame `id`. Other IDs are
    /// late and change nothing.
    fn take_pending(&self, id: u64) -> Option<Pending> {
        let mut state = self.state();
        if state.pending.as_ref().is_some_and(|p| p.id == id) {
            state.pending.take()
        } else {
            None
        }
    }

    pub(crate) fn resolve(&self, id: u64, reply: Reply) {
        if let Some(p) = self.take_pending(id) {
            let _ = p.reply.send(reply);
        }
    }

    /// Resolves the pending frame from `applied` and keeps the layout size
    /// for `query`. A wrong natural size fails the frame.
    pub(crate) fn applied(&self, applied: &Applied) {
        let Some(p) = self.take_pending(applied.id) else {
            return;
        };
        let natural = (applied.natural_width, applied.natural_height);
        let reply = if natural == (p.size.0 as u64, p.size.1 as u64) {
            let layout = (applied.layout_width, applied.layout_height);
            // A pending frame exists only for the current generation: a
            // geometry change resolves it first.
            if let Some(client) = self.state().client.as_mut() {
                client.layout = Some(layout);
            }
            Ok(layout)
        } else {
            Err(OutputError::Platform(format!(
                "the client decoded frame {} as {}×{} px, not {}×{}",
                applied.id, natural.0, natural.1, p.size.0, p.size.1
            )))
        };
        let _ = p.reply.send(reply);
    }

    /// Tells the session of `display` to show a black page, and releases
    /// the frame and its pending registration.
    pub(crate) fn close(&self, display: &DisplayId) {
        let mut state = self.state();
        if !state.client.as_ref().is_some_and(|c| c.display == *display) {
            return;
        }
        state.frame = None;
        state.pending = None;
        if let Some(commands) = &state.commands {
            commands.send_replace(Command::Close);
        }
    }

    /// Caches a valid `hello` and notifies the host: `ViewportChanged`
    /// first if the surface geometry changed, then `Ready`.
    ///
    /// A geometry change also fails the pending frame: it has the size of
    /// the old surface.
    pub(crate) fn update_client(&self, display: &DisplayId, peer: SocketAddr, hello: Hello) {
        let (changed, stale) = {
            let mut state = self.state();
            let old = state.client.take();
            let changed = old
                .as_ref()
                .map(|old| old.hello.geometry() != hello.geometry());
            // The layout report holds while the geometry holds.
            let layout = if let (Some(false), Some(old)) = (changed, old) {
                old.layout
            } else {
                state.generation += 1;
                None
            };
            let generation = state.generation;
            state.client = Some(ClientRecord {
                display: display.clone(),
                peer,
                hello,
                generation,
                layout,
            });
            let changed = changed == Some(true);
            (changed, changed.then(|| state.pending.take()).flatten())
        };
        if let Some(p) = stale {
            let _ = p
                .reply
                .send(Err(OutputError::Platform(VIEWPORT_CHANGED.into())));
        }
        if changed {
            self.events
                .send(ServiceEvent::Client(ClientState::ViewportChanged(
                    display.clone(),
                )));
        }
        self.events
            .send(ServiceEvent::Client(ClientState::Ready(display.clone())));
    }

    /// The cached client state; `None` before the first valid `hello` and
    /// after the session ends.
    pub(crate) fn client(&self) -> Option<ClientRecord> {
        self.state().client.clone()
    }
}

/// Lock duration after `failures` consecutive failed attempts, `failures`
/// ≥ 1.
pub(crate) fn lockout(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(31);
    LOCKOUT_FIRST
        .saturating_mul(1 << doublings)
        .min(LOCKOUT_MAX)
}

/// The reserved slot. Dropping it revokes the token, frees the slot and
/// sends [`ClientState::Disconnected`]. A failed upgrade or a server
/// shutdown also drops it.
#[derive(Debug)]
pub(crate) struct Reservation {
    hub: Arc<SessionHub>,
    peer: SocketAddr,
    reason: String,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let pending = {
            let mut state = self.hub.state();
            state.token = None;
            state.client = None;
            state.frame = None;
            state.commands = None;
            state.busy = false;
            state.pending.take()
        };
        if let Some(p) = pending {
            let _ = p.reply.send(Err(OutputError::Disconnected));
        }
        self.hub
            .events
            .send(ServiceEvent::Client(ClientState::Disconnected(
                std::mem::take(&mut self.reason),
            )));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Incoming {
    Text(String),
    Binary,
    /// The client closed the connection, or the connection failed. The
    /// value is the reason for the status line.
    Closed(String),
}

/// A WebSocket after the upgrade. Implementations answer pings and skip
/// pongs.
pub(crate) trait Transport: Send {
    fn recv(&mut self) -> impl Future<Output = Incoming> + Send;
    fn send(&mut self, text: String) -> impl Future<Output = Result<(), String>> + Send;
    fn close(&mut self, code: u16, reason: &'static str) -> impl Future<Output = ()> + Send;
}

/// How a session ended.
struct End {
    /// Close frame to send; `None` if the connection is already closed.
    close: Option<(u16, &'static str)>,
    reason: String,
}

impl End {
    fn client(reason: String) -> Self {
        Self {
            close: None,
            reason,
        }
    }

    fn closing(code: u16, text: &'static str) -> Self {
        Self {
            close: Some((code, text)),
            reason: text.into(),
        }
    }

    fn violation(text: &'static str) -> Self {
        Self {
            close: Some((close::PROTOCOL, text)),
            reason: format!("protocol violation: {text}"),
        }
    }
}

/// Runs one session to its end and then releases the reservation.
pub(crate) async fn run_session<T: Transport>(mut transport: T, mut reservation: Reservation) {
    let end = match authenticate(&mut transport, &reservation.hub).await {
        Ok((display, commands)) => {
            let session = Session {
                hub: &reservation.hub,
                display: &display,
                peer: reservation.peer,
            };
            session.run(&mut transport, commands).await
        }
        Err(end) => end,
    };
    if let Some((code, text)) = end.close {
        transport.close(code, text).await;
    }
    reservation.reason = end.reason;
}

async fn send<T: Transport>(transport: &mut T, message: &ServerMessage) -> Result<(), End> {
    transport.send(message.to_json()).await.map_err(End::client)
}

type Accepted = (DisplayId, watch::Receiver<Command>);

async fn authenticate<T: Transport>(transport: &mut T, hub: &SessionHub) -> Result<Accepted, End> {
    let deadline = Instant::now() + AUTH_TIMEOUT;
    let code = hub
        .pairing
        .code()
        .map_err(|_| End::closing(close::INTERNAL, "no random data for the code"))?;
    hub.events.send(ServiceEvent::PairingCode(code.clone()));
    send(transport, &ServerMessage::AuthRequired).await?;

    let Ok(incoming) = tokio::time::timeout_at(deadline, transport.recv()).await else {
        // Not a failed attempt: no code was tried.
        return Err(End::closing(close::AUTH_TIMEOUT, "authentication timeout"));
    };
    let failure = match incoming {
        Incoming::Closed(reason) => return Err(End::client(reason)),
        Incoming::Binary => End::violation("binary message"),
        Incoming::Text(text) => match ClientMessage::parse(&text) {
            Ok(ClientMessage::Auth { code: given }) if secret_eq(&given, &code) => {
                return accept(transport, hub).await;
            }
            Ok(ClientMessage::Auth { .. }) => End::closing(close::WRONG_CODE, "wrong code"),
            Ok(_) => End::violation("expected auth"),
            Err(_) => End::violation("malformed message"),
        },
    };
    // Any other first message counts: otherwise a guesser could skip the
    // lock with malformed `auth` messages.
    hub.record_failure();
    Err(failure)
}

async fn accept<T: Transport>(transport: &mut T, hub: &SessionHub) -> Result<Accepted, End> {
    let token = random_token()
        .map_err(|_| End::closing(close::INTERNAL, "no random data for the token"))?;
    // Before the reply: the client can fetch as soon as it has the token.
    let accepted = hub.accept(token.clone());
    send(transport, &ServerMessage::Authenticated { token }).await?;
    hub.events
        .send(ServiceEvent::Client(ClientState::Authenticated));
    Ok(accepted)
}

/// The `show` that was sent and its acknowledgement deadline.
type Awaiting = Option<(u64, Instant)>;

struct Session<'a> {
    hub: &'a SessionHub,
    display: &'a DisplayId,
    peer: SocketAddr,
}

impl Session<'_> {
    async fn run<T: Transport>(
        &self,
        transport: &mut T,
        mut commands: watch::Receiver<Command>,
    ) -> End {
        let mut awaiting: Awaiting = None;
        loop {
            let deadline = awaiting.map(|(_, at)| at);
            let timeout = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                incoming = transport.recv() => {
                    if let Err(end) = self.receive(incoming, &mut awaiting) {
                        return end;
                    }
                }
                // The hub drops the sender only after this task ends.
                Ok(()) = commands.changed() => {
                    let command = *commands.borrow_and_update();
                    let message = match command {
                        Command::Idle => continue,
                        Command::Show { id, size: (width, height) } => {
                            ServerMessage::Show { id, width, height }
                        }
                        Command::Close => ServerMessage::Close,
                    };
                    if let Err(end) = send(transport, &message).await {
                        return end;
                    }
                    // The timeout starts when `show` is sent.
                    awaiting = match command {
                        Command::Show { id, .. } => Some((id, Instant::now() + ACK_TIMEOUT)),
                        Command::Idle | Command::Close => None,
                    };
                }
                () = timeout => {
                    if let Some((id, _)) = awaiting.take() {
                        self.hub.resolve(id, Err(OutputError::Timeout));
                    }
                }
            }
        }
    }

    fn receive(&self, incoming: Incoming, awaiting: &mut Awaiting) -> Result<(), End> {
        let mut answered = |id: u64| {
            if awaiting.is_some_and(|(a, _)| a == id) {
                *awaiting = None;
            }
        };
        match incoming {
            Incoming::Closed(reason) => Err(End::client(reason)),
            Incoming::Binary => Err(End::violation("binary message")),
            Incoming::Text(text) => match ClientMessage::parse(&text) {
                Ok(ClientMessage::Auth { .. }) => Err(End::violation("already authenticated")),
                Ok(ClientMessage::Hello(fields)) => match Hello::parse(fields) {
                    Ok(hello) => {
                        self.hub.update_client(self.display, self.peer, hello);
                        Ok(())
                    }
                    Err(_) => Err(End::violation("invalid hello")),
                },
                Ok(ClientMessage::Applied(applied)) if applied.is_valid() => {
                    answered(applied.id);
                    self.hub.applied(&applied);
                    Ok(())
                }
                Ok(ClientMessage::Applied(_)) => Err(End::violation("invalid applied")),
                Ok(ClientMessage::Error { id, message }) => {
                    answered(id);
                    self.hub.resolve(
                        id,
                        Err(OutputError::Platform(format!(
                            "the client failed to show frame {id}: {message}"
                        ))),
                    );
                    Ok(())
                }
                Err(_) => Err(End::violation("malformed message")),
            },
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
