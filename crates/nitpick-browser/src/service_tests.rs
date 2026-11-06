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
        ..ServiceConfig::default()
    })
    .unwrap();
    assert_eq!(service.local_addr().ip(), ip);
}

#[test]
fn ipv6_url_has_brackets() {
    let config = ServiceConfig {
        host: BindHost::Address(IpAddr::V6(Ipv6Addr::LOCALHOST)),
        port: 0,
        ..ServiceConfig::default()
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
            ..ServiceConfig::default()
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
        ..ServiceConfig::default()
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
        ..ServiceConfig::default()
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

/// WebSocket and frame requests over real HTTP. The session logic itself
/// is tested with a mock transport in `session_tests.rs`.
mod session {
    use axum::body::Bytes;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::net::TcpStream as AsyncTcpStream;
    use tokio_tungstenite::tungstenite::protocol::CloseFrame;
    use tokio_tungstenite::tungstenite::{Error as WsError, Message};
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

    use super::*;
    use crate::protocol::{MAX_CLIENT_MESSAGE, close};

    type Ws = WebSocketStream<MaybeTlsStream<AsyncTcpStream>>;

    const CODE: &str = "0042";
    const FRAME_PATH: &str = "/api/v1/frame/1.png";

    fn start_fixed() -> (BrowserService, ServiceEvents) {
        BrowserService::start(&ServiceConfig {
            fixed_code: Some(CODE.into()),
            ..ServiceConfig::default()
        })
        .unwrap()
    }

    /// Plain HTTP/1.1 GET with extra header lines.
    fn request(addr: SocketAddr, path: &str, headers: &[String]) -> String {
        String::from_utf8(request_bytes(addr, path, headers)).unwrap()
    }

    fn request_bytes(addr: SocketAddr, path: &str, headers: &[String]) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut head = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        for line in headers {
            head.push_str(line);
            head.push_str("\r\n");
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    fn status(response: &str) -> u16 {
        response[9..12].parse().unwrap()
    }

    fn has_header(response: &str, line: &str) -> bool {
        let head = response.split("\r\n\r\n").next().unwrap_or_default();
        head.lines().any(|l| l.eq_ignore_ascii_case(line))
    }

    async fn connect(addr: SocketAddr) -> Result<Ws, WsError> {
        connect_async(format!("ws://{addr}/api/v1/ws"))
            .await
            .map(|(ws, _)| ws)
    }

    fn rejected_status(result: Result<Ws, WsError>) -> u16 {
        match result {
            Err(WsError::Http(response)) => response.status().as_u16(),
            other => panic!("expected an HTTP rejection, got {other:?}"),
        }
    }

    async fn next_json(ws: &mut Ws) -> Value {
        let message = ws.next().await.expect("message").expect("no error");
        serde_json::from_str(message.to_text().unwrap()).unwrap()
    }

    async fn send_json(ws: &mut Ws, value: &Value) {
        ws.send(Message::text(value.to_string())).await.unwrap();
    }

    async fn close_frame(ws: &mut Ws) -> Option<CloseFrame> {
        match ws.next().await {
            Some(Ok(Message::Close(frame))) => frame,
            other => panic!("expected close, got {other:?}"),
        }
    }

    async fn authenticate(addr: SocketAddr) -> (Ws, String) {
        let mut ws = connect(addr).await.unwrap();
        assert_eq!(next_json(&mut ws).await, json!({"type": "auth_required"}));
        send_json(&mut ws, &json!({"type": "auth", "code": CODE})).await;
        let reply = next_json(&mut ws).await;
        assert_eq!(reply["type"], "authenticated");
        (ws, reply["token"].as_str().unwrap().to_owned())
    }

    /// The server releases the slot after its session task ends, which is
    /// after the client sees the close.
    async fn connect_when_free(addr: SocketAddr) -> Ws {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match connect(addr).await {
                Ok(ws) => return ws,
                Err(e) => assert!(Instant::now() < deadline, "{e}"),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn invalid_fixed_code_is_rejected() {
        for code in [String::new(), "0".repeat(257)] {
            let result = BrowserService::start(&ServiceConfig {
                fixed_code: Some(code),
                ..ServiceConfig::default()
            });
            assert!(matches!(result, Err(StartError::PairingCode(_))));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn frame_requests_need_the_session_token() {
        let (service, _events) = start_fixed();
        let addr = service.local_addr();
        service.hub().set_frame(1, Bytes::from_static(b"PNG test"));

        let response = request(addr, FRAME_PATH, &[]);
        assert_eq!(status(&response), 401, "{response}");
        assert!(has_header(&response, "cache-control: no-store"));
        assert!(has_header(&response, "www-authenticate: Bearer"));

        let (ws, token) = authenticate(addr).await;
        let auth = vec![format!("Authorization: Bearer {token}")];
        for query in ["token", "access_token"] {
            let response = request(addr, &format!("{FRAME_PATH}?{query}={token}"), &[]);
            assert_eq!(status(&response), 401, "URL token: {response}");
        }
        let response = request(addr, FRAME_PATH, &["Authorization: Bearer 00".into()]);
        assert_eq!(status(&response), 401, "{response}");

        let response = request(addr, FRAME_PATH, &auth);
        assert_eq!(status(&response), 200, "{response}");
        assert!(has_header(&response, "cache-control: no-store"));
        assert!(has_header(&response, "content-type: image/png"));
        assert!(response.ends_with("\r\n\r\nPNG test"), "{response}");

        for missing in [
            "/api/v1/frame/2.png",
            "/api/v1/frame/1",
            "/api/v1/frame/x.png",
        ] {
            let response = request(addr, missing, &auth);
            assert_eq!(status(&response), 404, "{missing}: {response}");
            assert!(has_header(&response, "cache-control: no-store"));
        }

        // Closing the WebSocket revokes the token; a new session gets a
        // new one.
        drop(ws);
        let (_ws, new_token) = {
            let mut ws = connect_when_free(addr).await;
            assert_eq!(status(&request(addr, FRAME_PATH, &auth)), 401);
            next_json(&mut ws).await;
            send_json(&mut ws, &json!({"type": "auth", "code": CODE})).await;
            let reply = next_json(&mut ws).await;
            (ws, reply["token"].as_str().unwrap().to_owned())
        };
        assert_ne!(new_token, token);
        // The end of a session releases the frame.
        assert_eq!(service.hub().frame(1), None);
        service.hub().set_frame(1, Bytes::from_static(b"PNG test"));
        assert_eq!(status(&request(addr, FRAME_PATH, &auth)), 401);
        let new_auth = [format!("Authorization: Bearer {new_token}")];
        assert_eq!(status(&request(addr, FRAME_PATH, &new_auth)), 200);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn second_websocket_gets_conflict() {
        let (service, mut events) = start_fixed();
        let addr = service.local_addr();
        let mut first = connect(addr).await.unwrap();
        next_json(&mut first).await;
        assert_eq!(rejected_status(connect(addr).await), 409);

        send_json(&mut first, &json!({"type": "auth", "code": CODE})).await;
        assert_eq!(next_json(&mut first).await["type"], "authenticated");
        assert_eq!(rejected_status(connect(addr).await), 409);

        first.close(None).await.unwrap();
        let mut seen = Vec::new();
        while !matches!(
            seen.last(),
            Some(ServiceEvent::Client(ClientState::Disconnected(_)))
        ) {
            seen.push(events.next().await.unwrap());
        }
        assert_eq!(
            seen,
            [
                ServiceEvent::Listening {
                    url: service.url().into()
                },
                ServiceEvent::Client(ClientState::Connected),
                ServiceEvent::PairingCode(CODE.into()),
                ServiceEvent::Client(ClientState::Authenticated),
                ServiceEvent::Client(ClientState::Disconnected("client closed".into())),
            ]
        );
        connect(addr).await.expect("slot free after the event");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn wrong_code_closes_and_locks_upgrades() {
        let (service, _events) = start_fixed();
        let addr = service.local_addr();
        let mut ws = connect(addr).await.unwrap();
        next_json(&mut ws).await;
        send_json(&mut ws, &json!({"type": "auth", "code": "42"})).await;
        let frame = close_frame(&mut ws).await.expect("close frame");
        assert_eq!(u16::from(frame.code), close::WRONG_CODE);
        assert_eq!(frame.reason.as_str(), "wrong code");

        // The lock is set before the close frame is sent.
        let Err(WsError::Http(response)) = connect(addr).await else {
            panic!("upgrade accepted during the lock");
        };
        assert_eq!(response.status().as_u16(), 429);
        assert_eq!(response.headers()["retry-after"], "1");
        tokio::time::sleep(Duration::from_millis(1100)).await;
        authenticate(addr).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn oversized_message_ends_the_session() {
        let (service, _events) = start_fixed();
        let addr = service.local_addr();
        let mut ws = connect(addr).await.unwrap();
        next_json(&mut ws).await;
        let code = "0".repeat(MAX_CLIENT_MESSAGE);
        let _ = ws
            .send(Message::text(
                json!({"type": "auth", "code": code}).to_string(),
            ))
            .await;
        // The server ends the connection, with or without a close frame.
        while let Some(Ok(message)) = ws.next().await {
            if message.is_close() {
                break;
            }
        }
        connect_when_free(addr).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hello_over_http_reaches_the_driver() {
        use nitpick_core::driver::OutputDriver;

        use crate::driver::BrowserDriver;
        use crate::test_support::hello_json;

        let (service, mut events) = start_fixed();
        let addr = service.local_addr();
        let mut driver = BrowserDriver::new(&service);
        let (mut ws, _token) = authenticate(addr).await;
        assert_eq!(driver.displays(), []);
        send_json(&mut ws, &hello_json()).await;
        let display = loop {
            if let ServiceEvent::Client(ClientState::Ready(d)) = events.next().await.unwrap() {
                break d;
            }
        };
        let displays = driver.displays();
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].id, display);
        assert_eq!(displays[0].name, "Safari at 127.0.0.1");
        let surface = driver
            .open(&display, nitpick_core::signal::BitDepth::Ten)
            .unwrap();
        assert_eq!(surface.size, (2360, 1640));

        ws.close(None).await.unwrap();
        while !matches!(
            events.next().await,
            Some(ServiceEvent::Client(ClientState::Disconnected(_)))
        ) {}
        assert_eq!(driver.displays(), []);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn frame_is_presented_over_http() {
        use std::io::Cursor;

        use nitpick_core::driver::{Frame, OutputDriver};
        use nitpick_core::image::Image;
        use nitpick_core::signal::{BitDepth, Encoding, Format, Rgb};
        use zune_png::PngDecoder;
        use zune_png::zune_core::result::DecodingResult;

        use crate::driver::BrowserDriver;
        use crate::test_support::hello_with;

        let (service, mut events) = start_fixed();
        let addr = service.local_addr();
        let mut driver = BrowserDriver::new(&service);
        let (mut ws, token) = authenticate(addr).await;
        send_json(
            &mut ws,
            &hello_with(&[
                ("viewport.width", Some(json!(8))),
                ("viewport.height", Some(json!(4))),
                ("pixelRatio", Some(json!(1))),
            ]),
        )
        .await;
        let display = loop {
            if let ServiceEvent::Client(ClientState::Ready(d)) = events.next().await.unwrap() {
                break d;
            }
        };
        let surface = driver.open(&display, BitDepth::Ten).unwrap();
        assert_eq!(surface.size, (8, 4));
        let image = Image::new(
            8,
            4,
            Rgb {
                r: 940,
                g: 64,
                b: 502,
            },
        );
        let frame = Frame {
            image: &image,
            format: Format::new(Encoding::PqNarrow, BitDepth::Ten),
            metadata: None,
        };

        let client = async {
            let show = next_json(&mut ws).await;
            assert_eq!(show["type"], "show");
            assert_eq!(
                (show["width"].clone(), show["height"].clone()),
                (json!(8), json!(4))
            );
            let id = show["id"].as_u64().unwrap();
            let response = request_bytes(
                addr,
                &format!("/api/v1/frame/{id}.png"),
                &[format!("Authorization: Bearer {token}")],
            );
            let split = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let head = String::from_utf8_lossy(&response[..split]).into_owned();
            assert_eq!(status(&head), 200, "{head}");
            let mut decoder = PngDecoder::new(Cursor::new(&response[split + 4..]));
            let DecodingResult::U16(samples) = decoder.decode().unwrap() else {
                panic!("expected 16-bit samples");
            };
            assert_eq!(decoder.dimensions(), Some((8, 4)));
            // Narrow peak, narrow black and a mid code (see `png_sample`).
            assert_eq!(&samples[..3], &[65535, 0, 32768]);
            send_json(
                &mut ws,
                &json!({"type": "applied", "id": id, "naturalWidth": 8, "naturalHeight": 4,
                        "layoutWidth": 8, "layoutHeight": 4}),
            )
            .await;
        };
        let (status_line, ()) = tokio::join!(driver.present(&surface, &frame), client);
        let status_line = status_line.unwrap();
        assert!(status_line.contains("Pixel-exact layout"), "{status_line}");
        assert_eq!(
            driver.query(&display).unwrap().capabilities.pixel_exact,
            nitpick_core::driver::Support::Yes
        );

        driver.close(&display);
        assert_eq!(next_json(&mut ws).await, json!({"type": "close"}));
    }
}
