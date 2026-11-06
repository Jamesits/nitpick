//! Measurement delay of the Qt launcher: a Qt timer after the driver
//! acknowledgement. See "Measurement delay" in
//! `doc/design/output_driver/overview.md`.

#[cxx_qt::bridge]
pub mod ffi {
    unsafe extern "C++" {
        include!("nitpick-app/cpp/delay_timer.h");

        /// Single-shot precise Qt timer on the GUI thread. Calls
        /// `delay_elapsed(timer)` after `ms`.
        #[namespace = "nitpick"]
        #[cxx_name = "startDelayTimer"]
        fn start_delay_timer(timer: u64, ms: i32);
    }

    extern "Rust" {
        #[namespace = "nitpick"]
        #[cxx_name = "delayElapsed"]
        fn delay_elapsed(timer: u64);
    }
}

use std::cell::RefCell;
use std::collections::HashMap;

use futures_channel::oneshot;

/// **Unverified:** sufficient for a particular display or probe.
pub const DEFAULT_DELAY_MS: u32 = 1000;

/// Starts measurement delay timers. Tests inject a controlled clock.
pub trait DelayTimer {
    /// Resolves after `ms`. Dropping the receiver cancels the timer.
    fn start(&self, ms: u32) -> oneshot::Receiver<()>;
}

/// Checks a delay from the launcher. The delay is in ms and must fit a Qt
/// timer interval.
///
/// # Errors
/// `ms` is negative.
pub fn delay_from(ms: i32) -> Result<u32, String> {
    u32::try_from(ms).map_err(|_| format!("the measurement delay must be >= 0 ms, not {ms}"))
}

/// Qt timers on the GUI thread.
pub struct QtTimer;

#[derive(Default)]
struct Timers {
    next: u64,
    /// A cancelled timer still fires; its entry stays until then.
    waiting: HashMap<u64, oneshot::Sender<()>>,
}

thread_local! {
    static TIMERS: RefCell<Timers> = RefCell::default();
}

impl DelayTimer for QtTimer {
    fn start(&self, ms: u32) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        let id = TIMERS.with_borrow_mut(|t| {
            t.next += 1;
            t.waiting.insert(t.next, tx);
            t.next
        });
        ffi::start_delay_timer(id, i32::try_from(ms).unwrap_or(i32::MAX));
        rx
    }
}

fn delay_elapsed(timer: u64) {
    // `try_with`: a timer can fire during thread-local teardown.
    let tx = TIMERS
        .try_with(|t| t.borrow_mut().waiting.remove(&timer))
        .ok()
        .flatten();
    if let Some(tx) = tx {
        let _ = tx.send(());
    }
}

#[cfg(test)]
#[path = "delay_tests.rs"]
mod tests;
