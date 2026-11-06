use std::collections::HashSet;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::*;
use crate::service::ServiceEvents;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outgoing {
    Text(Value),
    Close(u16, &'static str),
}

struct MockTransport {
    from_client: mpsc::UnboundedReceiver<Incoming>,
    to_client: mpsc::UnboundedSender<Outgoing>,
}

impl Transport for MockTransport {
    async fn recv(&mut self) -> Incoming {
        self.from_client
            .recv()
            .await
            .unwrap_or_else(|| Incoming::Closed("client closed".into()))
    }

    fn send(&mut self, text: String) -> impl Future<Output = Result<(), String>> + Send {
        let value = serde_json::from_str(&text).expect("server sends JSON");
        let result = self
            .to_client
            .send(Outgoing::Text(value))
            .map_err(|_| "client gone".into());
        std::future::ready(result)
    }

    async fn close(&mut self, code: u16, reason: &'static str) {
        let _ = self.to_client.send(Outgoing::Close(code, reason));
    }
}

/// The client end of a mock connection.
struct Client {
    tx: mpsc::UnboundedSender<Incoming>,
    rx: mpsc::UnboundedReceiver<Outgoing>,
    task: JoinHandle<()>,
}

impl Client {
    fn send_text(&self, text: &str) {
        self.tx.send(Incoming::Text(text.into())).unwrap();
    }

    fn send_json(&self, value: &Value) {
        self.send_text(&value.to_string());
    }

    fn auth(&self, code: &str) {
        self.send_json(&json!({"type": "auth", "code": code}));
    }

    async fn next(&mut self) -> Option<Outgoing> {
        self.rx.recv().await
    }

    /// Waits for the session to end and the reservation to be released.
    async fn finished(self) -> Vec<Outgoing> {
        let Self { tx, mut rx, task } = self;
        drop(tx);
        task.await.unwrap();
        let mut rest = Vec::new();
        while let Ok(message) = rx.try_recv() {
            rest.push(message);
        }
        rest
    }
}

fn hub(pairing: Pairing) -> (Arc<SessionHub>, ServiceEvents) {
    let (sender, events) = EventSender::channel();
    (SessionHub::new(sender, pairing), events)
}

fn connect(hub: &Arc<SessionHub>) -> Client {
    let reservation = hub.reserve().expect("slot free");
    let (tx, from_client) = mpsc::unbounded_channel();
    let (to_client, rx) = mpsc::unbounded_channel();
    let transport = MockTransport {
        from_client,
        to_client,
    };
    Client {
        tx,
        rx,
        task: tokio::spawn(run_session(transport, reservation)),
    }
}

fn auth_required() -> Outgoing {
    Outgoing::Text(json!({"type": "auth_required"}))
}

async fn expect_token(client: &mut Client) -> String {
    match client.next().await {
        Some(Outgoing::Text(value)) if value["type"] == "authenticated" => {
            value["token"].as_str().expect("token string").to_owned()
        }
        other => panic!("expected authenticated, got {other:?}"),
    }
}

/// Authenticates a new connection with the fixed code.
async fn authenticated_client(hub: &Arc<SessionHub>, code: &str) -> (Client, String) {
    let mut client = connect(hub);
    assert_eq!(client.next().await, Some(auth_required()));
    client.auth(code);
    let token = expect_token(&mut client).await;
    (client, token)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn drain(events: &mut ServiceEvents) -> Vec<ServiceEvent> {
    std::iter::from_fn(|| events.try_next()).collect()
}

fn disconnected(reason: &str) -> ServiceEvent {
    ServiceEvent::Client(ClientState::Disconnected(reason.into()))
}

#[test]
fn codes_keep_leading_zeros() {
    assert_eq!(format_code(0), "0000");
    assert_eq!(format_code(42), "0042");
    assert_eq!(format_code(9999), "9999");
}

#[test]
fn random_codes_are_four_digits_and_vary() {
    let codes: HashSet<String> = (0..200).map(|_| random_code().unwrap()).collect();
    for code in &codes {
        assert_eq!(code.len(), 4, "{code}");
        assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code}");
    }
    // 200 draws from 10^4 codes: fewer than 150 distinct values has a
    // probability far below 1e-12.
    assert!(codes.len() > 150, "{}", codes.len());
}

#[test]
fn lockout_doubles_up_to_the_maximum() {
    assert_eq!(lockout(1), Duration::from_secs(1));
    assert_eq!(lockout(2), Duration::from_secs(2));
    assert_eq!(lockout(6), Duration::from_secs(32));
    assert_eq!(lockout(7), LOCKOUT_MAX);
    assert_eq!(lockout(u32::MAX), LOCKOUT_MAX);
}

#[tokio::test(start_paused = true)]
async fn random_code_is_announced_and_accepted() {
    let (hub, mut events) = hub(Pairing::Random);
    let mut client = connect(&hub);
    assert_eq!(client.next().await, Some(auth_required()));
    let events_now = drain(&mut events);
    assert_eq!(events_now[0], ServiceEvent::Client(ClientState::Connected));
    let ServiceEvent::PairingCode(code) = &events_now[1] else {
        panic!("{events_now:?}");
    };
    client.auth(code);
    let token = expect_token(&mut client).await;
    assert_eq!(token.len(), 64);
    assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(hub.authorize(Some(&bearer(&token))));
    assert_eq!(
        drain(&mut events),
        [ServiceEvent::Client(ClientState::Authenticated)]
    );
    assert_eq!(client.finished().await, []);
    assert_eq!(drain(&mut events), [disconnected("client closed")]);
}

#[tokio::test(start_paused = true)]
async fn fixed_code_with_leading_zeros_is_compared_as_a_string() {
    let (hub, _events) = hub(Pairing::Fixed("0042".into()));
    let mut client = connect(&hub);
    client.next().await;
    client.auth("42");
    assert_eq!(
        client.next().await,
        Some(Outgoing::Close(close::WRONG_CODE, "wrong code"))
    );
    client.finished().await;

    tokio::time::advance(lockout(1)).await;
    let (_client, token) = authenticated_client(&hub, "0042").await;
    assert!(hub.authorize(Some(&bearer(&token))));
}

#[tokio::test(start_paused = true)]
async fn configured_fixed_string_is_accepted() {
    let (hub, mut events) = hub(Pairing::Fixed("headless test".into()));
    let (_client, _token) = authenticated_client(&hub, "headless test").await;
    assert!(drain(&mut events).contains(&ServiceEvent::PairingCode("headless test".into())));
}

#[tokio::test(start_paused = true)]
async fn wrong_code_closes_and_releases_the_slot() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let mut client = connect(&hub);
    client.next().await;
    client.auth("1235");
    assert_eq!(
        client.next().await,
        Some(Outgoing::Close(close::WRONG_CODE, "wrong code"))
    );
    assert_eq!(client.finished().await, []);
    assert_eq!(drain(&mut events).last(), Some(&disconnected("wrong code")));
    assert!(!hub.state().busy);
}

#[tokio::test(start_paused = true)]
async fn failed_attempts_lock_new_connections() {
    let (hub, _events) = hub(Pairing::Fixed("1234".into()));
    for failures in 1..=3 {
        let mut client = connect(&hub);
        client.next().await;
        client.auth("0000");
        client.finished().await;

        let lock = lockout(failures);
        assert_eq!(hub.reserve().err(), Some(ReserveError::Locked(lock)));
        tokio::time::advance(lock.checked_sub(Duration::from_millis(1)).unwrap()).await;
        assert_eq!(
            hub.reserve().err(),
            Some(ReserveError::Locked(Duration::from_millis(1)))
        );
        tokio::time::advance(Duration::from_millis(1)).await;
    }
    // Success resets the count: the next failure locks for the first step.
    let (client, _token) = authenticated_client(&hub, "1234").await;
    client.finished().await;
    let mut client = connect(&hub);
    client.next().await;
    client.auth("0000");
    client.finished().await;
    assert_eq!(hub.reserve().err(), Some(ReserveError::Locked(lockout(1))));
}

#[tokio::test(start_paused = true)]
async fn messages_other_than_auth_fail_before_authentication() {
    let hello = json!({"type": "hello", "pixelRatio": 2}).to_string();
    let cases: [(Incoming, &str); 6] = [
        (Incoming::Text(hello), "expected auth"),
        (
            Incoming::Text(r#"{"type":"applied","id":1}"#.into()),
            "expected auth",
        ),
        (Incoming::Text("not json".into()), "malformed message"),
        (
            Incoming::Text(r#"{"type":"auth","code":1234}"#.into()),
            "malformed message",
        ),
        (
            Incoming::Text(r#"{"type":"show","id":1}"#.into()),
            "malformed message",
        ),
        (Incoming::Binary, "binary message"),
    ];
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    for (failures, (message, text)) in (1..).zip(cases) {
        let mut client = connect(&hub);
        client.next().await;
        client.tx.send(message).unwrap();
        assert_eq!(
            client.next().await,
            Some(Outgoing::Close(close::PROTOCOL, text))
        );
        client.finished().await;
        assert_eq!(
            drain(&mut events).last(),
            Some(&disconnected(&format!("protocol violation: {text}")))
        );
        // Each counts as a failed attempt.
        assert_eq!(
            hub.reserve().err(),
            Some(ReserveError::Locked(lockout(failures)))
        );
        tokio::time::advance(LOCKOUT_MAX).await;
    }
}

#[tokio::test(start_paused = true)]
async fn authentication_times_out_after_60_s() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let mut client = connect(&hub);
    client.next().await;
    tokio::time::advance(AUTH_TIMEOUT.checked_sub(Duration::from_millis(1)).unwrap()).await;
    assert!(client.rx.try_recv().is_err(), "closed early");
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        client.next().await,
        Some(Outgoing::Close(
            close::AUTH_TIMEOUT,
            "authentication timeout"
        ))
    );
    client.finished().await;
    assert_eq!(
        drain(&mut events).last(),
        Some(&disconnected("authentication timeout"))
    );
    // No code was tried, so no lock.
    assert!(hub.reserve().is_ok());
}

#[tokio::test(start_paused = true)]
async fn client_close_before_auth_is_not_a_failure() {
    let (hub, _events) = hub(Pairing::Fixed("1234".into()));
    let mut client = connect(&hub);
    client.next().await;
    client.finished().await;
    assert!(hub.reserve().is_ok());
}

#[tokio::test(start_paused = true)]
async fn second_client_is_rejected_during_and_after_authentication() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let mut client = connect(&hub);
    client.next().await;
    assert_eq!(hub.reserve().err(), Some(ReserveError::Busy));

    client.auth("1234");
    let token = expect_token(&mut client).await;
    assert_eq!(hub.reserve().err(), Some(ReserveError::Busy));
    // The rejected attempts did not disturb the session.
    assert!(hub.authorize(Some(&bearer(&token))));
    assert_eq!(
        drain(&mut events)
            .iter()
            .filter(|e| matches!(e, ServiceEvent::Client(ClientState::Connected)))
            .count(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn authenticated_session_accepts_client_messages_in_order() {
    let (hub, _events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, token) = authenticated_client(&hub, "1234").await;
    client.send_json(&json!({"type": "hello", "pixelRatio": 2}));
    client.send_json(&json!({"type": "applied", "id": 1}));
    client.send_json(&json!({"type": "error", "id": 1, "message": "decode"}));
    // Long after the authentication timeout: it no longer applies.
    tokio::time::advance(AUTH_TIMEOUT * 2).await;
    assert!(client.rx.try_recv().is_err());
    assert!(hub.authorize(Some(&bearer(&token))));

    client.auth("1234");
    assert_eq!(
        client.next().await,
        Some(Outgoing::Close(close::PROTOCOL, "already authenticated"))
    );
    client.finished().await;
    assert!(!hub.authorize(Some(&bearer(&token))));
}

#[tokio::test(start_paused = true)]
async fn token_checks() {
    let (hub, _events) = hub(Pairing::Fixed("1234".into()));
    assert!(!hub.authorize(None), "no session");
    let (_client, token) = authenticated_client(&hub, "1234").await;
    let mut wrong = token.clone();
    wrong.replace_range(63.., if token.ends_with('0') { "1" } else { "0" });
    for header in [
        String::new(),
        token.clone(),
        format!("Basic {token}"),
        format!("Bearer  {token}"),
        format!("Bearer {}", &token[..63]),
        format!("Bearer {token}0"),
        format!("Bearer {wrong}"),
        "Bearer ".into(),
    ] {
        assert!(!hub.authorize(Some(&header)), "{header:?}");
    }
    assert!(hub.authorize(Some(&format!("bearer {token}"))));
    assert!(hub.authorize(Some(&format!("BEARER {token}"))));
}

#[tokio::test(start_paused = true)]
async fn disconnect_revokes_the_token_and_reconnect_needs_a_new_code() {
    let (hub, mut events) = hub(Pairing::Random);
    let mut tokens: Vec<String> = Vec::new();
    for _ in 0..3 {
        let mut client = connect(&hub);
        client.next().await;
        let code = drain(&mut events)
            .into_iter()
            .find_map(|e| match e {
                ServiceEvent::PairingCode(code) => Some(code),
                _ => None,
            })
            .expect("pairing code");
        client.auth(&code);
        let token = expect_token(&mut client).await;
        for old in &tokens {
            assert!(!hub.authorize(Some(&bearer(old))));
        }
        assert!(hub.authorize(Some(&bearer(&token))));
        client.finished().await;
        assert!(!hub.authorize(Some(&bearer(&token))));
        tokens.push(token);
    }
    let distinct: HashSet<_> = tokens.iter().collect();
    assert_eq!(distinct.len(), tokens.len());
}

#[tokio::test(start_paused = true)]
async fn aborted_session_releases_the_slot() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (client, token) = authenticated_client(&hub, "1234").await;
    // As on server shutdown: the runtime drops the session task.
    client.task.abort();
    let _ = client.task.await;
    assert!(!hub.authorize(Some(&bearer(&token))));
    assert!(hub.reserve().is_ok());
    assert!(drain(&mut events).contains(&disconnected("server stopped or upgrade failed")));
}

#[test]
fn frame_lookup_matches_the_current_id_only() {
    let (sender, _events) = EventSender::channel();
    let hub = SessionHub::new(sender, Pairing::Random);
    assert_eq!(hub.frame(1), None);
    hub.set_frame(7, Bytes::from_static(b"png"));
    assert_eq!(hub.frame(7), Some(Bytes::from_static(b"png")));
    assert_eq!(hub.frame(6), None);
}
