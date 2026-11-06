use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use nitpick_core::driver::DisplayId;

use super::*;

fn start_default() -> (BrowserService, ServiceEvents) {
    BrowserService::start(&ServiceConfig::default()).expect("start on loopback")
}

/// Plain HTTP/1.1 client: the test must not share the server's HTTP stack.
fn get(addr: SocketAddr, path: &str) -> io::Result<String> {
    let mut stream = TcpStream::connect(addr)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

#[test]
fn default_binds_loopback_only() {
    assert_eq!(ServiceConfig::default().host, BindHost::Loopback);
    let (service, _events) = start_default();
    let addr = service.local_addr();
    assert_eq!(addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_ne!(addr.port(), 0);
    assert_eq!(service.url(), format!("http://127.0.0.1:{}/", addr.port()));
}

#[test]
fn explicit_address_is_used_in_url() {
    let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let (service, _events) = BrowserService::start(&ServiceConfig {
        host: BindHost::Address(ip),
        port: 0,
    })
    .unwrap();
    assert_eq!(service.local_addr().ip(), ip);
}

#[test]
fn ipv6_url_has_brackets() {
    let config = ServiceConfig {
        host: BindHost::Address(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        port: 0,
    };
    // Hosts without IPv6 loopback cannot run this check.
    let Ok((service, _events)) = BrowserService::start(&config) else {
        return;
    };
    let port = service.local_addr().port();
    assert_eq!(service.url(), format!("http://[::1]:{port}/"));
}

#[test]
fn unspecified_address_is_rejected() {
    for ip in [
        IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        IpAddr::V6(Ipv6Addr::UNSPECIFIED),
    ] {
        let result = BrowserService::start(&ServiceConfig {
            host: BindHost::Address(ip),
            port: 0,
        });
        assert!(matches!(result, Err(StartError::Address(_))), "{ip}");
    }
}

#[test]
fn bind_failure_is_returned_by_start() {
    let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    let result = BrowserService::start(&ServiceConfig {
        host: BindHost::Loopback,
        port,
    });
    match result {
        Err(StartError::Bind(e)) => assert_eq!(e.kind(), ErrorKind::AddrInUse),
        other => panic!("expected a bind error, got {other:?}"),
    }
}

#[test]
fn bind_to_foreign_address_fails() {
    // TEST-NET-1 (RFC 5737): never assigned to a local interface.
    let result = BrowserService::start(&ServiceConfig {
        host: BindHost::Address(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
        port: 0,
    });
    assert!(matches!(result, Err(StartError::Bind(_))), "{result:?}");
}

#[test]
fn serves_embedded_client_page() {
    let (service, _events) = start_default();
    let response = get(service.local_addr(), "/").unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response
            .to_ascii_lowercase()
            .contains("content-type: text/html")
    );
    assert!(response.ends_with(INDEX_HTML), "{response}");
}

#[test]
fn unknown_path_is_not_found() {
    let (service, _events) = start_default();
    let response = get(service.local_addr(), "/nothing").unwrap();
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
}

#[tokio::test(flavor = "current_thread")]
async fn events_arrive_in_order_on_an_executor() {
    let (service, mut events) = start_default();
    let url = service.url().to_owned();
    assert_eq!(events.next().await, Some(ServiceEvent::Listening { url }));

    let sender = service.event_sender();
    let display = DisplayId {
        driver: "browser",
        key: "client".into(),
    };
    let sent = [
        ServiceEvent::Client(ClientState::Connected),
        ServiceEvent::PairingCode("0042".into()),
        ServiceEvent::Client(ClientState::Authenticated),
        ServiceEvent::Client(ClientState::Ready(display)),
        ServiceEvent::Client(ClientState::Disconnected("closed".into())),
    ];
    // Another thread, as the server runtime and workers do.
    let worker_events = sent.clone();
    std::thread::spawn(move || {
        for event in worker_events {
            sender.send(event);
        }
    })
    .join()
    .unwrap();
    for event in sent {
        assert_eq!(events.next().await, Some(event));
    }
    assert_eq!(events.try_next(), None);
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_stops_server_and_closes_events() {
    let (service, mut events) = start_default();
    let addr = service.local_addr();
    assert!(matches!(
        events.next().await,
        Some(ServiceEvent::Listening { .. })
    ));
    service.shutdown();
    assert_eq!(
        events.next().await,
        Some(ServiceEvent::Stopped { error: None })
    );
    assert_eq!(events.next().await, None);
    // The port is free again.
    drop(TcpListener::bind(addr).expect("port released"));
}

#[test]
fn drop_stops_server() {
    let (service, mut events) = start_default();
    drop(service);
    assert!(matches!(
        events.try_next(),
        Some(ServiceEvent::Listening { .. })
    ));
    assert_eq!(
        events.try_next(),
        Some(ServiceEvent::Stopped { error: None })
    );
    assert_eq!(events.try_next(), None);
}

#[test]
fn shutdown_with_open_request_is_bounded() {
    let (service, _events) = start_default();
    // Incomplete request headers: graceful shutdown alone waits for the
    // rest of the request.
    let mut open = TcpStream::connect(service.local_addr()).unwrap();
    write!(open, "GET / HTTP/1.1\r\nHost: x\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    service.shutdown();
    let elapsed = started.elapsed();
    assert!(elapsed >= SHUTDOWN_GRACE / 2, "{elapsed:?}");
    assert!(elapsed < SHUTDOWN_GRACE * 3, "{elapsed:?}");
}

#[test]
fn shutdown_without_connections_is_prompt() {
    let (service, _events) = start_default();
    let started = Instant::now();
    service.shutdown();
    assert!(
        started.elapsed() < SHUTDOWN_GRACE,
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn receiver_can_outlive_service() {
    let (service, events) = start_default();
    drop(events);
    // Sending to a dropped receiver must not panic.
    service
        .event_sender()
        .send(ServiceEvent::PairingCode("1".into()));
    service.shutdown();
}
