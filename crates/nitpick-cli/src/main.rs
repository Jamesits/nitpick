//! `nitpick-cli`: command-line host of the browser output. It starts the
//! browser service, prints its events and reads console commands. It needs
//! no Qt, Tauri or window system. See "Host and dependency boundary" in
//! `doc/design/output_driver/overview.md`.

mod command;
mod host;
mod test_host;

use std::cell::RefCell;
use std::io::BufRead;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;

use clap::Parser;
use nitpick_browser::{BindHost, BrowserDriver, BrowserService, ServiceConfig};
use nitpick_core::driver::DEFAULT_DELAY_MS;
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::{BitDepth, Encoding, Format};
#[cfg(feature = "device")]
use nitpick_test_host::WorkerDevices;
use nitpick_test_host::{ArtifactWriter, Devices, Fixtures, QUEUE_CAPACITY};
use tokio::sync::mpsc;

use crate::host::{Exit, Settings, Stdout};

/// Shows nitpick test patterns in a local or remote browser.
///
/// Reads one command per stdin line; type `help` for the list.
#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    /// Interface address for a remote browser. Default: 127.0.0.1.
    #[arg(long)]
    host: Option<IpAddr>,
    /// 0: the OS selects a free port.
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Fixed pairing code for headless use and tests. Default: a new
    /// random 4-digit code for each connection.
    #[arg(long)]
    code: Option<String>,
    /// `bt2111` or `pq-grayscale`.
    #[arg(long, default_value = "bt2111")]
    pattern: PatternKind,
    /// `pq-narrow`, `pq-full` or `hlg-narrow`.
    #[arg(long, default_value = "pq-narrow")]
    encoding: Encoding,
    /// 10 or 12.
    #[arg(long, default_value = "10")]
    depth: BitDepth,
    /// Measurement delay after the driver acknowledgement, in ms.
    #[arg(long, default_value_t = DEFAULT_DELAY_MS, value_parser = command::parse_delay)]
    delay: u32,
    /// Read test-host protocol requests (one JSON object per line) in
    /// place of console commands, and print protocol events. `--delay` is
    /// the default delay of `show`.
    #[arg(long)]
    test_host: bool,
    /// With `--test-host`: the root directory of the capture artifacts. It
    /// must be empty or absent. Without it, an accepted capture is
    /// `unavailable`.
    #[arg(long, requires = "test_host")]
    artifacts: Option<PathBuf>,
    /// With `--test-host`: the directory of the device recovery records.
    /// Default: `nitpick/device-recovery` in the per-user state directory.
    #[cfg(feature = "device")]
    #[arg(long, requires = "test_host")]
    device_state: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let devices = match test_host_devices(&args) {
        Ok(devices) => devices,
        Err(e) => {
            eprintln!("nitpick-cli: {e}");
            return ExitCode::from(2);
        }
    };
    let config = ServiceConfig {
        host: args.host.map_or(BindHost::Loopback, BindHost::Address),
        port: args.port,
        fixed_code: args.code,
    };
    let settings = Settings {
        pattern: args.pattern,
        format: Format::new(args.encoding, args.depth),
        delay_ms: args.delay,
    };
    // Before the service starts, so that a missing file fails early.
    let fixtures = if args.test_host {
        match Fixtures::load() {
            Ok(fixtures) => Some(fixtures),
            Err(e) => {
                eprintln!("nitpick-cli: cannot read the output fixtures: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        None
    };
    let artifacts = match &args.artifacts {
        None => None,
        Some(dir) => match ArtifactWriter::start(dir.clone(), QUEUE_CAPACITY) {
            Ok(writer) => Some(writer),
            Err(e) => {
                eprintln!(
                    "nitpick-cli: cannot use the artifact directory {}: {e}",
                    dir.display()
                );
                return ExitCode::from(2);
            }
        },
    };
    let (service, mut events) = match BrowserService::start(&config) {
        Ok(started) => started,
        Err(e) => {
            eprintln!("nitpick-cli: {e}");
            return ExitCode::from(2);
        }
    };

    // The host executor; the service has its own runtime thread.
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("nitpick-cli: cannot start the executor: {e}");
            return ExitCode::from(2);
        }
    };
    let exit = if let Some(fixtures) = fixtures {
        let (lines_tx, lines) = futures_channel::mpsc::unbounded();
        read_stdin(move |line| lines_tx.unbounded_send(line).is_ok());
        runtime.block_on(test_host::run(
            &service,
            events,
            lines,
            fixtures,
            settings.delay_ms,
            artifacts,
            devices,
        ))
    } else {
        let (console_tx, mut console) = mpsc::unbounded_channel();
        read_stdin(move |line| console_tx.send(line).is_ok());
        let driver = RefCell::new(BrowserDriver::new(&service));
        runtime.block_on(host::run(
            &driver,
            BrowserDriver::new(&service),
            &mut events,
            &mut console,
            settings,
            Stdout,
        ))
    };
    service.shutdown();
    match exit {
        Exit::Quit | Exit::Stopped(None) => ExitCode::SUCCESS,
        Exit::Stopped(Some(_)) => ExitCode::FAILURE,
    }
}

/// The device worker of the test host, which starts at its first device
/// request. `None` without `--test-host` or without the feature `device`.
///
/// # Errors
/// No per-user state directory and no `--device-state`.
#[cfg(feature = "device")]
fn test_host_devices(args: &Args) -> Result<Option<Box<dyn Devices>>, String> {
    if !args.test_host {
        return Ok(None);
    }
    let dir = match &args.device_state {
        Some(dir) => dir.clone(),
        None => nitpick_display::store::user_dir().map_err(|e| e.to_string())?,
    };
    Ok(Some(Box::new(WorkerDevices::new(
        nitpick_display::native,
        dir,
    ))))
}

#[cfg(not(feature = "device"))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "the same signature as with the feature"
)]
fn test_host_devices(_: &Args) -> Result<Option<Box<dyn Devices>>, String> {
    Ok(None)
}

/// Passes each stdin line to `send` on a thread, until `send` fails. The
/// thread ends, and drops `send`, when stdin closes.
fn read_stdin(send: impl Fn(String) -> bool + Send + 'static) {
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if !send(line) {
                return;
            }
        }
    });
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
