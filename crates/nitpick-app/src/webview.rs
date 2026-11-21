//! The optional local webview of the browser output: starts
//! `nitpick-webview` from the directory of the launcher executable and
//! reports its exit. The lifecycle is in `session.rs`; the process does not
//! get screen geometry from the browser service.

use std::any::Any;
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use futures_channel::oneshot;
use shared_child::SharedChild;

/// Time from the authentication of the webview page to its first valid
/// `hello`. The page sends `hello` directly after authentication.
pub const HELLO_TIMEOUT_MS: u32 = 10_000;

/// File name of the process without the OS suffix.
const EXECUTABLE: &str = "nitpick-webview";

/// Time to read the error lines after the exit. A process that the webview
/// started can keep the pipe open, so the read does not wait for its end.
const STDERR_GRACE: Duration = Duration::from_millis(200);

/// Error lines kept for the exit description.
const STDERR_LINES: usize = 32;

/// A started process.
pub struct Launched {
    /// Resolves once with a description of the exit.
    pub exit: oneshot::Receiver<String>,
    /// Dropping it ends the process.
    pub process: Box<dyn Any>,
}

/// Starts the webview process. Tests inject a fake.
pub trait LaunchWebview {
    /// Starts `nitpick-webview --url <url> --at <x>,<y>`.
    ///
    /// # Errors
    /// The executable is missing or does not start.
    fn launch(&self, url: &str, at: (i64, i64)) -> Result<Launched, String>;
}

/// The arguments of `nitpick-webview` for a page and a point on the target
/// monitor. Each value is in its flag argument: a separate value with a
/// leading `-`, such as the point of a monitor left of the primary one,
/// is a flag for `clap`.
#[must_use]
pub fn arguments(url: &str, (x, y): (i64, i64)) -> [String; 2] {
    [format!("--url={url}"), format!("--at={x},{y}")]
}

/// `exit` (for example `exit status 1`) with the first error line of the
/// process, or its last line if no line starts with `error`.
#[must_use]
pub fn describe_exit(exit: &str, stderr: &[String]) -> String {
    let lines = || stderr.iter().map(|l| l.trim()).filter(|l| !l.is_empty());
    match lines()
        .find(|l| l.starts_with("error"))
        .or_else(|| lines().next_back())
    {
        Some(line) => format!("{exit}: {line}"),
        None => exit.to_owned(),
    }
}

/// `nitpick-webview` in the directory of the launcher executable, without a
/// console window.
#[derive(Default)]
pub struct NativeWebview;

impl LaunchWebview for NativeWebview {
    fn launch(&self, url: &str, at: (i64, i64)) -> Result<Launched, String> {
        let path = executable()?;
        let mut command = Command::new(&path);
        command
            .args(arguments(url, at))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // `nitpick-webview` is a console program, so that its errors
            // and `--list-monitors` reach a terminal. Without this flag,
            // Windows opens a console window for it.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = Arc::new(
            SharedChild::spawn(&mut command)
                .map_err(|e| format!("cannot start {}: {e}", path.display()))?,
        );
        // Ends the process if the rest of the start fails.
        let process = Kill(Arc::clone(&child));
        allow_foreground(child.id());
        let lines = read_lines(child.take_stderr());
        let (tx, exit) = oneshot::channel();
        std::thread::Builder::new()
            .name("nitpick-webview-wait".into())
            .spawn(move || {
                let status = match child.wait() {
                    Ok(s) => s
                        .code()
                        .map_or_else(|| s.to_string(), |c| format!("exit status {c}")),
                    Err(e) => format!("the wait failed: {e}"),
                };
                let _ = tx.send(describe_exit(&status, &collect(&lines)));
            })
            .map_err(|e| format!("cannot start the wait thread: {e}"))?;
        Ok(Launched {
            exit,
            process: Box::new(process),
        })
    }
}

fn executable() -> Result<PathBuf, String> {
    let launcher =
        std::env::current_exe().map_err(|e| format!("cannot find the launcher path: {e}"))?;
    let path = launcher.with_file_name(format!("{EXECUTABLE}{}", std::env::consts::EXE_SUFFIX));
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "{} not found; build it with `cargo build -p {EXECUTABLE}`",
            path.display()
        ))
    }
}

/// Ends the process when dropped. A process that ended already is not
/// affected.
struct Kill(Arc<SharedChild>);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// Windows gives the foreground only to some processes, for example one
/// that the foreground process permits. The launcher is the foreground
/// process when the user selects "Show pattern"; this lets the webview
/// window take the keyboard focus.
#[cfg(windows)]
fn allow_foreground(pid: u32) {
    use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
    // SAFETY: no pointers; a failure only leaves the focus unchanged.
    let _ = unsafe { AllowSetForegroundWindow(pid) };
}

#[cfg(not(windows))]
fn allow_foreground(_: u32) {}

/// The lines of `pipe`, read on a thread until the end of the pipe.
fn read_lines(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    if let Some(pipe) = pipe {
        // A failed spawn drops the sender: no lines.
        let _ = std::thread::Builder::new()
            .name("nitpick-webview-stderr".into())
            .spawn(move || {
                for line in BufReader::new(pipe).lines() {
                    let Ok(line) = line else { return };
                    if tx.send(line).is_err() {
                        return;
                    }
                }
            });
    }
    rx
}

/// The first [`STDERR_LINES`] lines that arrive before the pipe ends or
/// [`STDERR_GRACE`] elapses.
fn collect(lines: &mpsc::Receiver<String>) -> Vec<String> {
    let deadline = Instant::now() + STDERR_GRACE;
    let mut out = Vec::new();
    while let Ok(line) = lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        if out.len() < STDERR_LINES {
            out.push(line);
        }
    }
    out
}

#[cfg(test)]
#[path = "webview_tests.rs"]
mod tests;
