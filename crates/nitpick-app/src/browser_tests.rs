use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener};

use nitpick_browser::{BindHost, ServiceConfig};

use super::{NativeService, StartService, config, remote_candidates};

#[test]
fn a_restart_on_the_same_fixed_port_binds() {
    let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = ServiceConfig {
        port,
        ..ServiceConfig::default()
    };
    let service = NativeService::default();
    let mut started = service.start(&config).unwrap();
    for _ in 0..5 {
        // The old service stops when the new one starts, as in the session.
        drop(started);
        started = service.start(&config).unwrap();
        assert_eq!(started.url, format!("http://127.0.0.1:{port}/"));
    }
}

#[test]
fn loopback_unless_remote() {
    let c = config(false, "not an address", 0).unwrap();
    assert_eq!(c.host, BindHost::Loopback);
    assert_eq!(c.port, 0);
    assert_eq!(c.fixed_code, None);

    let c = config(true, " 192.168.1.20 ", 8080).unwrap();
    assert_eq!(
        c.host,
        BindHost::Address(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)))
    );
    assert_eq!(c.port, 8080);
    assert_eq!(c.fixed_code, None);

    let e = config(true, "", 0).unwrap_err();
    assert!(e.contains("not an IP address"), "{e}");
}

#[test]
fn remote_candidates_leave_out_loopback_and_link_local() {
    let v4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
    let v6 = |s: &str| IpAddr::V6(s.parse::<Ipv6Addr>().unwrap());
    let ips = vec![
        v6("fe80::1"),
        v6("2001:db8::5"),
        v4(127, 0, 0, 1),
        v4(192, 168, 1, 20),
        v6("::1"),
        v4(10, 0, 0, 2),
        v4(192, 168, 1, 20),
    ];
    assert_eq!(
        remote_candidates(ips),
        vec![v4(192, 168, 1, 20), v4(10, 0, 0, 2), v6("2001:db8::5")]
    );
}
