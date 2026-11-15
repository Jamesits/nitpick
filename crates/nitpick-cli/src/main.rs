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
use std::process::ExitCode;
use std::thread;

use clap::Parser;
use nitpick_browser::{BindHost, BrowserDriver, BrowserService, ServiceConfig};
use nitpick_core::driver::DEFAULT_DELAY_MS;
use nitpick_core::pattern::PatternKind;
use nitpick_core::signal::{BitDepth, Encoding, Format};
use nitpick_test_host::Fixtures;
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
}

fn main() -> ExitCode {
    let args = Args::parse();
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
