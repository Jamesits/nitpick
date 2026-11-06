//! The single client session: slot reservation, pairing code, bearer
//! token and the failed-attempt limit. See "Authentication" in
//! `doc/design/output_driver/browser.md`.
//!
//! The session logic uses [`Transport`] and the Tokio clock only, so tests
//! drive it with a mock transport and a paused clock.

use std::fmt::Write as _;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use axum::body::Bytes;
use tokio::time::Instant;

use crate::protocol::{ClientMessage, ServerMessage, close};
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
    frame: Option<(u64, Bytes)>,
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
    pub(crate) fn reserve(self: &Arc<Self>) -> Result<Reservation, ReserveError> {
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

    #[allow(dead_code)] // For the driver's frame storage.
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

    fn accept(&self, token: String) {
        let mut state = self.state();
        state.token = Some(token);
        state.consecutive_failures = 0;
        state.locked_until = None;
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
    reason: String,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        {
            let mut state = self.hub.state();
            state.token = None;
            state.busy = false;
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
        Ok(()) => authenticated(&mut transport).await,
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

async fn authenticate<T: Transport>(transport: &mut T, hub: &SessionHub) -> Result<(), End> {
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

async fn accept<T: Transport>(transport: &mut T, hub: &SessionHub) -> Result<(), End> {
    let token = random_token()
        .map_err(|_| End::closing(close::INTERNAL, "no random data for the token"))?;
    // Before the reply: the client can fetch as soon as it has the token.
    hub.accept(token.clone());
    send(transport, &ServerMessage::Authenticated { token }).await?;
    hub.events
        .send(ServiceEvent::Client(ClientState::Authenticated));
    Ok(())
}

async fn authenticated<T: Transport>(transport: &mut T) -> End {
    loop {
        match transport.recv().await {
            Incoming::Closed(reason) => return End::client(reason),
            Incoming::Binary => return End::violation("binary message"),
            Incoming::Text(text) => match ClientMessage::parse(&text) {
                Ok(ClientMessage::Auth { .. }) => return End::violation("already authenticated"),
                // Client state and acknowledgements are not used by the
                // service yet; the session only enforces message order.
                Ok(_) => {}
                Err(_) => return End::violation("malformed message"),
            },
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
