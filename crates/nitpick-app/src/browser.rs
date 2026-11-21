//! Browser output of the launcher: the service of `nitpick-browser` with
//! its two drivers, its event source, and the local interface addresses
//! for remote displays. The lifecycle is in `session.rs`.

use std::any::Any;
use std::cell::RefCell;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::rc::Rc;
use std::thread::JoinHandle;

use nitpick_browser::{
    BindHost, BrowserDriver, BrowserService, ServiceConfig, ServiceEvent, ServiceEvents,
};

pub use nitpick_browser::driver::DRIVER;

/// Service events in order. Tests inject a channel.
pub trait EventSource {
    /// `None`: the service is gone.
    fn next_event(&mut self) -> Pin<Box<dyn Future<Output = Option<ServiceEvent>> + '_>>;
}

impl EventSource for ServiceEvents {
    fn next_event(&mut self) -> Pin<Box<dyn Future<Output = Option<ServiceEvent>> + '_>> {
        Box::pin(self.next())
    }
}

/// A started service.
pub struct Started<B> {
    pub url: String,
    /// Opens, presents and closes.
    pub driver: B,
    /// A second driver on the same client state. It answers `displays` and
    /// `query` while a presentation borrows `driver`.
    pub viewer: B,
    pub events: Box<dyn EventSource>,
    /// Keeps the service. Dropping it stops the service.
    pub service: Box<dyn Any>,
}

/// Starts the browser service. Tests inject a mock driver.
pub trait StartService<B> {
    /// # Errors
    /// The service cannot start, for example because the address is not
    /// local or the port is in use.
    fn start(&self, config: &ServiceConfig) -> Result<Started<B>, String>;
}

/// The service of `nitpick-browser` on its own thread.
#[derive(Default)]
pub struct NativeService {
    /// The stop of the last service.
    stopping: Rc<RefCell<Option<Stopping>>>,
}

struct Stopping {
    /// The fixed address of the stopped service; `None` for a port that
    /// the OS selected.
    address: Option<(IpAddr, u16)>,
    thread: JoinHandle<()>,
}

/// The address that a later service with a fixed port can also bind.
fn fixed_address(config: &ServiceConfig) -> Option<(IpAddr, u16)> {
    let ip = match config.host {
        BindHost::Loopback => IpAddr::V4(Ipv4Addr::LOCALHOST),
        BindHost::Address(ip) => ip,
    };
    (config.port != 0).then_some((ip, config.port))
}

impl StartService<BrowserDriver> for NativeService {
    fn start(&self, config: &ServiceConfig) -> Result<Started<BrowserDriver>, String> {
        // The old service closes its listener on its own thread after the
        // stop signal; a bind before that fails (measured on Windows: each
        // second restart on a fixed port). Thus a start on the same fixed
        // address waits for the stop. Counter-intuitive: this blocks the
        // Qt GUI thread for up to about 1 s, the grace time of open
        // connections of the old service, but only at this restart.
        let address = fixed_address(config);
        let previous = self.stopping.borrow_mut().take();
        if let Some(previous) = previous
            && address.is_some()
            && previous.address == address
        {
            let _ = previous.thread.join();
        }
        let (service, events) = BrowserService::start(config).map_err(|e| e.to_string())?;
        Ok(Started {
            url: service.url().to_owned(),
            driver: BrowserDriver::new(&service),
            viewer: BrowserDriver::new(&service),
            events: Box::new(events),
            service: Box::new(StopInBackground {
                service: Some(service),
                address,
                stopping: Rc::clone(&self.stopping),
            }),
        })
    }
}

/// Stops the service on a helper thread. The stop waits up to about 1 s
/// for open connections; the Qt GUI thread must not wait for it, except
/// at a restart on the same fixed address (see [`NativeService`]).
struct StopInBackground {
    service: Option<BrowserService>,
    address: Option<(IpAddr, u16)>,
    stopping: Rc<RefCell<Option<Stopping>>>,
}

impl Drop for StopInBackground {
    fn drop(&mut self) {
        let Some(service) = self.service.take() else {
            return;
        };
        // A failed spawn drops the closure here, which stops the service
        // on this thread.
        if let Ok(thread) = std::thread::Builder::new()
            .name("nitpick-browser-stop".into())
            .spawn(move || service.shutdown())
        {
            *self.stopping.borrow_mut() = Some(Stopping {
                address: self.address,
                thread,
            });
        }
    }
}

/// The service configuration of the launcher settings. Without `remote`,
/// the service binds the loopback interface and ignores `address`. The
/// launcher uses a random pairing code for each connection.
///
/// # Errors
/// `remote` with an address that is not an IP address.
pub fn config(remote: bool, address: &str, port: u16) -> Result<ServiceConfig, String> {
    let host = if remote {
        let ip: IpAddr = address
            .trim()
            .parse()
            .map_err(|_| format!("`{address}` is not an IP address"))?;
        BindHost::Address(ip)
    } else {
        BindHost::Loopback
    };
    Ok(ServiceConfig {
        host,
        port,
        fixed_code: None,
    })
}

/// Addresses for a remote display: IPv4 first, then IPv6, in interface
/// order. Without loopback, and without IPv6 link-local addresses: a URL
/// with a zone index does not work in browsers, and the bind needs the
/// zone.
#[must_use]
pub fn interface_addresses() -> Vec<IpAddr> {
    let ips: Vec<IpAddr> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.ip())
        .collect();
    remote_candidates(ips)
}

fn remote_candidates(ips: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for ip in ips {
        let usable = match ip {
            IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_unspecified(),
            IpAddr::V6(v6) => {
                !v6.is_loopback() && !v6.is_unspecified() && !v6.is_unicast_link_local()
            }
        };
        if usable && !out.contains(&ip) {
            out.push(ip);
        }
    }
    // Stable: keeps the interface order within each family.
    out.sort_by_key(IpAddr::is_ipv6);
    out
}

#[cfg(test)]
#[path = "browser_tests.rs"]
mod tests;
