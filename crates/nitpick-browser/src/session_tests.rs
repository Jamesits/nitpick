use std::collections::HashSet;

use serde_json::{Value, json};

use super::*;
use crate::service::ServiceEvents;
use crate::test_support::{
    Client, Outgoing, PEER, auth_required, authenticated_client, connect, expect_token, hello_json,
    hello_with, hub,
};

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
        assert_eq!(hub.reserve(PEER).err(), Some(ReserveError::Locked(lock)));
        tokio::time::advance(lock.checked_sub(Duration::from_millis(1)).unwrap()).await;
        assert_eq!(
            hub.reserve(PEER).err(),
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
    assert_eq!(
        hub.reserve(PEER).err(),
        Some(ReserveError::Locked(lockout(1)))
    );
}

#[tokio::test(start_paused = true)]
async fn messages_other_than_auth_fail_before_authentication() {
    let hello = json!({"type": "hello", "pixelRatio": 2}).to_string();
    let cases: [(Incoming, &str); 6] = [
        (Incoming::Text(hello), "expected auth"),
        (
            Incoming::Text(applied(1, (4, 2)).to_string()),
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
            hub.reserve(PEER).err(),
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
    assert!(hub.reserve(PEER).is_ok());
}

#[tokio::test(start_paused = true)]
async fn client_close_before_auth_is_not_a_failure() {
    let (hub, _events) = hub(Pairing::Fixed("1234".into()));
    let mut client = connect(&hub);
    client.next().await;
    client.finished().await;
    assert!(hub.reserve(PEER).is_ok());
}

#[tokio::test(start_paused = true)]
async fn second_client_is_rejected_during_and_after_authentication() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let mut client = connect(&hub);
    client.next().await;
    assert_eq!(hub.reserve(PEER).err(), Some(ReserveError::Busy));

    client.auth("1234");
    let token = expect_token(&mut client).await;
    assert_eq!(hub.reserve(PEER).err(), Some(ReserveError::Busy));
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
    client.send_json(&hello_json());
    // No frame is pending: replies to unknown IDs change nothing.
    client.send_json(&applied(1, (2360, 1640)));
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
    assert!(hub.reserve(PEER).is_ok());
    assert!(drain(&mut events).contains(&disconnected("server stopped or upgrade failed")));
}

/// Waits for the next client event other than `Connected` and
/// `Authenticated`.
async fn next_client_event(events: &mut ServiceEvents) -> ClientState {
    loop {
        match events.next().await.expect("event") {
            ServiceEvent::Client(ClientState::Connected | ClientState::Authenticated)
            | ServiceEvent::PairingCode(_) => {}
            ServiceEvent::Client(state) => return state,
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn valid_hello_caches_the_client_state() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (client, _token) = authenticated_client(&hub, "1234").await;
    assert_eq!(hub.client(), None, "no hello yet");
    client.send_json(&hello_json());
    let ClientState::Ready(display) = next_client_event(&mut events).await else {
        panic!("expected Ready");
    };
    assert_eq!(display.driver, DRIVER);
    let record = hub.client().expect("cached");
    assert_eq!(record.display, display);
    assert_eq!(record.peer, PEER);
    assert_eq!(record.hello.surface_size(), (2360, 1640));
    let generation = record.generation;

    // Same geometry: `Ready` only, same generation.
    client.send_json(&hello_with(&[("dynamicRange", Some(json!("standard")))]));
    assert_eq!(
        next_client_event(&mut events).await,
        ClientState::Ready(display.clone())
    );
    assert_eq!(hub.client().unwrap().generation, generation);

    // Resize: `ViewportChanged` before `Ready`, new generation.
    client.send_json(&hello_with(&[("viewport.height", Some(json!(700)))]));
    assert_eq!(
        next_client_event(&mut events).await,
        ClientState::ViewportChanged(display.clone())
    );
    assert_eq!(
        next_client_event(&mut events).await,
        ClientState::Ready(display.clone())
    );
    let record = hub.client().unwrap();
    assert_ne!(record.generation, generation);
    assert_eq!(record.hello.surface_size(), (2360, 1400));

    client.finished().await;
    assert_eq!(hub.client(), None, "removed on disconnect");
}

#[tokio::test(start_paused = true)]
async fn invalid_hello_ends_the_session() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, token) = authenticated_client(&hub, "1234").await;
    client.send_json(&hello_json());
    next_client_event(&mut events).await;
    client.send_json(&hello_with(&[("pixelRatio", Some(json!(0)))]));
    assert_eq!(
        client.next().await,
        Some(Outgoing::Close(close::PROTOCOL, "invalid hello"))
    );
    client.finished().await;
    assert_eq!(hub.client(), None);
    assert!(!hub.authorize(Some(&bearer(&token))));
    assert_eq!(
        next_client_event(&mut events).await,
        ClientState::Disconnected("protocol violation: invalid hello".into())
    );
}

#[tokio::test(start_paused = true)]
async fn each_session_gets_a_new_display_id() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let mut displays = Vec::new();
    for _ in 0..2 {
        let (client, _token) = authenticated_client(&hub, "1234").await;
        client.send_json(&hello_json());
        let ClientState::Ready(display) = next_client_event(&mut events).await else {
            panic!("expected Ready");
        };
        displays.push(display);
        client.finished().await;
        assert!(matches!(
            next_client_event(&mut events).await,
            ClientState::Disconnected(_)
        ));
    }
    assert_ne!(displays[0], displays[1]);
}

fn applied(id: u64, size: (u64, u64)) -> Value {
    json!({
        "type": "applied",
        "id": id,
        "naturalWidth": size.0,
        "naturalHeight": size.1,
        "layoutWidth": size.0,
        "layoutHeight": size.1,
    })
}

/// An authenticated client after a valid `hello`.
async fn ready_client(hub: &Arc<SessionHub>, events: &mut ServiceEvents) -> (Client, DisplayId) {
    let (client, _token) = authenticated_client(hub, "1234").await;
    client.send_json(&hello_json());
    let ClientState::Ready(display) = next_client_event(events).await else {
        panic!("expected Ready");
    };
    (client, display)
}

const SIZE: (usize, usize) = (2360, 1640);

fn submit(
    hub: &SessionHub,
    display: &DisplayId,
    png: &'static [u8],
) -> (u64, oneshot::Receiver<Reply>) {
    let generation = hub.client().expect("client").generation;
    hub.submit(display, generation, SIZE, Bytes::from_static(png))
        .expect("submitted")
}

async fn expect_show(client: &mut Client, id: u64) {
    assert_eq!(
        client.next().await,
        Some(Outgoing::Text(
            json!({"type": "show", "id": id, "width": SIZE.0, "height": SIZE.1})
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn submit_stores_the_frame_and_sends_show() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, display) = ready_client(&hub, &mut events).await;
    let (first, _reply) = submit(&hub, &display, b"one");
    expect_show(&mut client, first).await;
    assert_eq!(hub.frame(first), Some(Bytes::from_static(b"one")));
    assert_eq!(hub.frame(first + 1), None);

    // A new frame releases the old PNG.
    let (second, _reply) = submit(&hub, &display, b"two");
    assert!(second > first);
    expect_show(&mut client, second).await;
    assert_eq!(hub.frame(first), None);
    assert_eq!(hub.frame(second), Some(Bytes::from_static(b"two")));

    client.finished().await;
    assert_eq!(hub.frame(second), None, "released on disconnect");
}

#[tokio::test(start_paused = true)]
async fn only_the_latest_unsent_show_is_sent() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, display) = ready_client(&hub, &mut events).await;
    // No await between the submits: the session task cannot run.
    let (first, first_reply) = submit(&hub, &display, b"one");
    let (second, _reply) = submit(&hub, &display, b"two");
    expect_show(&mut client, second).await;
    assert!(client.rx.try_recv().is_err());
    // The replaced registration has no reply.
    assert!(first_reply.await.is_err());
    assert_eq!(hub.frame(first), None);
}

#[tokio::test(start_paused = true)]
async fn applied_resolves_the_matching_frame_only() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, display) = ready_client(&hub, &mut events).await;
    let (id, mut reply) = submit(&hub, &display, b"png");
    expect_show(&mut client, id).await;
    client.send_json(&applied(id - 1, (2360, 1640)));
    client.send_json(&applied(id + 1, (2360, 1640)));
    tokio::task::yield_now().await;
    assert!(reply.try_recv().is_err(), "other IDs");
    client.send_json(&applied(id, (2360, 1640)));
    assert_eq!(reply.await.unwrap(), Ok((2360.0, 1640.0)));
    assert_eq!(hub.client().unwrap().layout, Some((2360.0, 1640.0)));
}

#[tokio::test(start_paused = true)]
async fn wrong_natural_size_or_client_error_fails_the_frame() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, display) = ready_client(&hub, &mut events).await;
    let (id, reply) = submit(&hub, &display, b"png");
    expect_show(&mut client, id).await;
    client.send_json(&applied(id, (2360, 1639)));
    assert!(matches!(
        reply.await.unwrap(),
        Err(OutputError::Platform(m)) if m.contains("2360×1639")
    ));
    assert_eq!(hub.client().unwrap().layout, None);

    let (id, reply) = submit(&hub, &display, b"png");
    expect_show(&mut client, id).await;
    client.send_json(&json!({"type": "error", "id": id, "message": "decode failed"}));
    assert!(matches!(
        reply.await.unwrap(),
        Err(OutputError::Platform(m)) if m.contains("decode failed")
    ));
}

#[tokio::test(start_paused = true)]
async fn acknowledgement_times_out_10_s_after_show() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, display) = ready_client(&hub, &mut events).await;
    let (id, mut reply) = submit(&hub, &display, b"png");
    expect_show(&mut client, id).await;
    let almost = ACK_TIMEOUT.checked_sub(Duration::from_millis(1)).unwrap();
    tokio::time::advance(almost).await;
    assert!(reply.try_recv().is_err());
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(reply.try_recv(), Ok(Err(OutputError::Timeout)));

    // A late reply after the timeout does not complete the next frame.
    let (next, mut next_reply) = submit(&hub, &display, b"png");
    expect_show(&mut client, next).await;
    client.send_json(&applied(id, (2360, 1640)));
    tokio::task::yield_now().await;
    assert!(next_reply.try_recv().is_err());
    // Each `show` starts its own timer.
    tokio::time::advance(ACK_TIMEOUT).await;
    tokio::task::yield_now().await;
    assert_eq!(next_reply.try_recv(), Ok(Err(OutputError::Timeout)));
}

#[tokio::test(start_paused = true)]
async fn resize_and_disconnect_fail_the_pending_frame() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, display) = ready_client(&hub, &mut events).await;
    let (id, reply) = submit(&hub, &display, b"png");
    expect_show(&mut client, id).await;
    // A mode change keeps the frame; a resize fails it.
    client.send_json(&hello_with(&[("dynamicRange", Some(json!("standard")))]));
    client.send_json(&hello_with(&[("viewport.width", Some(json!(1000)))]));
    assert_eq!(
        reply.await.unwrap(),
        Err(OutputError::Platform(VIEWPORT_CHANGED.into()))
    );
    let generation = hub.client().unwrap().generation;
    let (id, reply) = hub
        .submit(
            &display,
            generation,
            (2000, 1640),
            Bytes::from_static(b"png"),
        )
        .unwrap();
    assert!(matches!(client.next().await, Some(Outgoing::Text(v)) if v["id"] == id));
    client.finished().await;
    assert_eq!(reply.await.unwrap(), Err(OutputError::Disconnected));
    assert_eq!(
        hub.submit(&display, generation, (2000, 1640), Bytes::new())
            .err(),
        Some(OutputError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn stale_generation_or_other_display_is_rejected_at_submit() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (_client, display) = ready_client(&hub, &mut events).await;
    let generation = hub.client().unwrap().generation;
    assert_eq!(
        hub.submit(&display, generation + 1, SIZE, Bytes::new())
            .err(),
        Some(OutputError::Platform(VIEWPORT_CHANGED.into()))
    );
    let other = DisplayId {
        driver: DRIVER,
        key: "other".into(),
    };
    assert_eq!(
        hub.submit(&other, generation, SIZE, Bytes::new()).err(),
        Some(OutputError::Disconnected)
    );
}

#[tokio::test(start_paused = true)]
async fn cancel_and_close_release_the_registration() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, display) = ready_client(&hub, &mut events).await;
    let (id, reply) = submit(&hub, &display, b"png");
    hub.cancel(id + 1);
    expect_show(&mut client, id).await;
    hub.cancel(id);
    assert!(reply.await.is_err(), "registration removed");
    // The client can still fetch the cancelled frame.
    assert!(hub.frame(id).is_some());

    let (id, reply) = submit(&hub, &display, b"png");
    expect_show(&mut client, id).await;
    hub.close(&display);
    assert!(reply.await.is_err());
    assert_eq!(hub.frame(id), None);
    assert_eq!(
        client.next().await,
        Some(Outgoing::Text(json!({"type": "close"})))
    );
    // No timer after `close`.
    tokio::time::advance(ACK_TIMEOUT * 2).await;
    assert!(client.rx.try_recv().is_err());
}

#[tokio::test(start_paused = true)]
async fn frame_ids_are_not_reused_across_sessions() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, display) = ready_client(&hub, &mut events).await;
    let (first, _reply) = submit(&hub, &display, b"png");
    expect_show(&mut client, first).await;
    client.finished().await;
    next_client_event(&mut events).await;
    let (_client, display) = ready_client(&hub, &mut events).await;
    let (second, _reply) = submit(&hub, &display, b"png");
    assert!(second > first);
}

#[tokio::test(start_paused = true)]
async fn invalid_applied_ends_the_session() {
    let (hub, mut events) = hub(Pairing::Fixed("1234".into()));
    let (mut client, _display) = ready_client(&hub, &mut events).await;
    let mut message = applied(1, (2360, 1640));
    message["layoutWidth"] = json!(-1);
    client.send_json(&message);
    assert_eq!(
        client.next().await,
        Some(Outgoing::Close(close::PROTOCOL, "invalid applied"))
    );
    client.finished().await;
}
