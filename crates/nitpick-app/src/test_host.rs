//! `nitpick --test-host`: the test-host protocol of `nitpick-test-host`
//! with the `QRhi` driver on the Qt GUI thread, without QML. See "Host
//! boundary" in `doc/design/output_driver/testing.md`.

#[cxx_qt::bridge]
mod ffi {
    unsafe extern "C++" {
        include!("nitpick-app/cpp/test_host.h");

        /// Queues `runPostedJob(job)` in the event loop of the application
        /// object. Callable from any thread. `false` if no application
        /// object exists.
        #[namespace = "nitpick"]
        #[cxx_name = "postToGuiThread"]
        fn post_to_gui_thread(job: u64) -> bool;

        /// `QCoreApplication::exit(code)`.
        #[namespace = "nitpick"]
        #[cxx_name = "quitApplication"]
        fn quit_application(code: i32);
    }

    extern "Rust" {
        #[namespace = "nitpick"]
        #[cxx_name = "runPostedJob"]
        fn run_posted_job(job: u64);
    }
}

use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::thread;

use cxx_qt_lib::QGuiApplication;
use futures_util::stream;
use nitpick_core::driver::{DEFAULT_DELAY_MS, DisplayId};
use nitpick_test_host::{
    ArtifactWriter, Artifacts, Clock, Environment, EnvironmentSource, Event, Exit, Fixtures, Hooks,
    LocalFuture, QUEUE_CAPACITY, TestHost, host_os,
};

use crate::delay::{DelayTimer, QtTimer};
use crate::display::{self, Backend, DeviceStates};
use crate::gui_executor::{self, Post};
use crate::pattern_capture;
use crate::qrhi_driver::{BridgeWindow, QrhiDriver, Threads};
use crate::submission;

/// The command-line flag of this mode; the first argument.
pub const FLAG: &str = "--test-host";

type Job = Box<dyn FnOnce() + Send>;

/// Posts through the application object, without a `QObject` of its own.
struct AppPost;

impl Post for AppPost {
    fn post(&self, job: Job) {
        let addr = Box::into_raw(Box::new(job)).expose_provenance();
        let posted = u64::try_from(addr).is_ok_and(ffi::post_to_gui_thread);
        if !posted {
            // SAFETY: `addr` comes from `Box::into_raw` above and was not
            // queued, so this is its only owner.
            drop(unsafe { take_job(addr) });
        }
    }
}

/// # Safety
/// `addr` comes from `Box::into_raw` of a boxed [`Job`] in
/// [`AppPost::post`], and no other call takes it.
unsafe fn take_job(addr: usize) -> Job {
    // SAFETY: see the function contract.
    *unsafe { Box::from_raw(std::ptr::with_exposed_provenance_mut::<Job>(addr)) }
}

fn run_posted_job(job: u64) {
    let addr = usize::try_from(job).expect("from a usize address");
    // SAFETY: Qt calls each queued job once, with the address of
    // `AppPost::post`.
    let job = unsafe { take_job(addr) };
    job();
}

/// Qt timers on the GUI thread.
struct QtClock;

impl Clock for QtClock {
    fn sleep(&self, ms: u32) -> LocalFuture<Result<(), String>> {
        let elapsed = QtTimer.start(ms);
        Box::pin(async move {
            elapsed
                .await
                .map_err(|_| "the measurement delay timer stopped".into())
        })
    }
}

struct QtHooks {
    /// Lists the screen identities; it never opens a window.
    screens: QrhiDriver,
    states: DeviceStates,
}

impl Hooks for QtHooks {
    /// The device read of the launcher, on a worker.
    fn refresh(&mut self) -> LocalFuture<()> {
        let refresh = self.states.begin();
        let reports = display::read_device_reports(self.screens.screens());
        let states = self.states.clone();
        Box::pin(async move {
            if let Ok(reports) = reports.await {
                states.apply(refresh, reports);
            }
        })
    }

    /// Esc, Q, Space or a double click closes the pattern window. Called
    /// after `open`, which closes an earlier window.
    fn watch_surface(&mut self) -> Option<LocalFuture<()>> {
        let closed = submission::watch_close(submission::close_count());
        Some(Box::pin(async move {
            // `Canceled` occurs only at thread-local teardown.
            if closed.await.is_err() {
                std::future::pending::<()>().await;
            }
        }))
    }
}

/// The environment of the artifacts: the OS of the process and the
/// renderer of the open pattern window, which shows the captured frame.
struct QtEnvironment {
    os: Option<String>,
}

impl EnvironmentSource for QtEnvironment {
    fn read(&mut self, _: &DisplayId) -> Environment {
        let renderer = display::ffi::pattern_window_renderer().map_err(|e| e.what().to_owned());
        if let Err(e) = &renderer {
            // The capture holds the surface, so the window exists.
            eprintln!("nitpick: cannot read the renderer of the pattern window: {e}");
        }
        environment(self.os.clone(), renderer.ok().as_ref())
    }
}

/// `renderer`: `None` if it cannot be read.
fn environment(os: Option<String>, renderer: Option<&display::ffi::RendererInfo>) -> Environment {
    let Some(r) = renderer else {
        return Environment {
            os,
            ..Environment::default()
        };
    };
    let ids = if (r.vendor_id, r.device_id) == (0, 0) {
        String::new()
    } else {
        format!(
            " (vendor 0x{:04x}, device 0x{:04x})",
            r.vendor_id, r.device_id
        )
    };
    let driver = if r.driver_version.is_empty() {
        String::new()
    } else {
        format!(", driver {}", r.driver_version)
    };
    Environment {
        os,
        gpu: Some(format!("{}{ids}{driver}", r.device_name)),
        renderer: Some(format!("Qt {}, QRhi {}", r.qt_version, r.backend)),
        ..Environment::default()
    }
}

/// The options after [`FLAG`].
#[derive(Debug, Default, PartialEq, Eq)]
struct Options {
    /// The root directory of the capture artifacts.
    artifacts: Option<PathBuf>,
}

/// # Errors
/// An unknown or repeated option, or an option without its value.
fn parse_options(args: impl IntoIterator<Item = OsString>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg != "--artifacts" {
            return Err(format!("unknown argument `{}`", arg.to_string_lossy()));
        }
        if options.artifacts.is_some() {
            return Err("`--artifacts` is given twice".into());
        }
        let dir = args.next().ok_or("`--artifacts` needs a directory")?;
        options.artifacts = Some(dir.into());
    }
    Ok(options)
}

fn exit_code(exit: &Exit) -> i32 {
    match exit {
        Exit::Quit | Exit::Stopped(None) => 0,
        Exit::Stopped(Some(_)) => 1,
    }
}

/// Runs the protocol on stdin and stdout until `quit` or the end of stdin.
/// The options follow [`FLAG`]: `--artifacts <directory>`.
pub fn main() -> ExitCode {
    let options = match parse_options(std::env::args_os().skip(2)) {
        Ok(options) => options,
        Err(e) => {
            eprintln!("nitpick: {e}; usage: nitpick {FLAG} [--artifacts <directory>]");
            return ExitCode::from(2);
        }
    };
    let fixtures = match Fixtures::load() {
        Ok(fixtures) => fixtures,
        Err(e) => {
            eprintln!("nitpick: cannot read the output fixtures: {e}");
            return ExitCode::from(2);
        }
    };
    let artifacts = match options.artifacts {
        None => None,
        Some(dir) => match ArtifactWriter::start(dir.clone(), QUEUE_CAPACITY) {
            Ok(writer) => Some(Artifacts {
                sink: Box::new(writer),
                environment: Box::new(QtEnvironment {
                    os: Some(host_os()),
                }),
            }),
            Err(e) => {
                eprintln!(
                    "nitpick: cannot use the artifact directory {}: {e}",
                    dir.display()
                );
                return ExitCode::from(2);
            }
        },
    };
    let mut app = QGuiApplication::new();
    let backend = Backend::current();
    let states = DeviceStates::default();
    let qrhi = || QrhiDriver::new(backend, BridgeWindow, Threads, states.clone());
    let host = TestHost {
        driver: Rc::new(RefCell::new(qrhi())),
        viewer: qrhi(),
        clock: Box::new(QtClock),
        hooks: Box::new(QtHooks {
            screens: qrhi(),
            states: states.clone(),
        }),
        capture: pattern_capture::adapter(),
        artifacts,
        fixtures: Rc::new(fixtures),
        delay_ms: DEFAULT_DELAY_MS,
    };
    let (lines_tx, lines) = futures_channel::mpsc::unbounded();
    // The thread ends, and drops the sender, when stdin closes.
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if lines_tx.unbounded_send(line).is_err() {
                return;
            }
        }
    });
    let post: Arc<dyn Post> = Arc::new(AppPost);
    // The first poll is posted, so it runs in `exec`.
    let task = gui_executor::spawn_local(post, async move {
        let exit = host
            .run(lines, stream::pending(), |event: &Event| {
                let mut out = std::io::stdout().lock();
                let _ = writeln!(out, "{}", event.to_line());
                let _ = out.flush();
            })
            .await;
        ffi::quit_application(exit_code(&exit));
    });
    let code = app.as_mut().map_or(2, QGuiApplication::exec);
    drop(task);
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

#[cfg(test)]
#[path = "test_host_tests.rs"]
mod tests;
