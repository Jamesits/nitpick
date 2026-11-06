//! Frame-ID acknowledgement of `QRhi` submissions. The pattern window
//! reports render events by frame ID; this module resolves the one
//! registered completion. See "Output report" in
//! `doc/design/output_driver/qt.md`.
//!
//! All state lives on the Qt GUI thread. The window queues a frame and
//! returns to the event loop; a later render uploads it and calls
//! `endFrame`. Only a successful `endFrame` after the upload of that frame
//! is an acknowledgement.

use std::cell::RefCell;
use std::marker::PhantomData;

use nitpick_core::driver::OutputError;

/// Unique per process; 0 means "no frame" at the C++ boundary.
pub type FrameId = u64;

/// Time from queueing a frame to its acknowledgement, in ms. Covers a
/// window that is not exposed, so that the window never renders.
pub const ACK_TIMEOUT_MS: i32 = 10_000;

pub type Completion = Box<dyn FnOnce(Result<(), OutputError>)>;

/// A completion with its result, called after the state borrow ends.
pub type Fired = (Completion, Result<(), OutputError>);

/// Submission state of the pattern window.
#[derive(Default)]
pub struct Submissions {
    next_id: FrameId,
    /// The frame whose pixels wait in the window for the next render. The
    /// window keeps one pixel buffer, so a new frame replaces the old one
    /// before upload.
    queued: Option<FrameId>,
    /// One completion: the driver has at most one pending frame.
    waiter: Option<(FrameId, Completion)>,
}

impl Submissions {
    pub fn next_frame_id(&mut self) -> FrameId {
        self.next_id += 1;
        self.next_id
    }

    /// Replaces an earlier waiter without calling it; the host cancels
    /// the old request before a new one.
    pub fn register(&mut self, id: FrameId, done: Completion) -> Option<Completion> {
        self.waiter.replace((id, done)).map(|(_, old)| old)
    }

    /// Removes the completion of `id`. The queued frame stays: the window
    /// can still show it, but no result reaches the host.
    pub fn cancel(&mut self, id: FrameId) -> Option<Completion> {
        if self.waiter.as_ref().is_some_and(|(w, _)| *w == id) {
            self.waiter.take().map(|(_, done)| done)
        } else {
            None
        }
    }

    /// The window holds the pixels of `id`; an older queued frame will
    /// not upload.
    pub fn queued(&mut self, id: FrameId) {
        self.queued = Some(id);
    }

    /// The frame to upload in the current render, if any. After this, the
    /// render reports [`Self::ended`] for the returned ID.
    pub fn take_upload(&mut self) -> Option<FrameId> {
        self.queued.take()
    }

    /// `endFrame` of the render that uploaded `id`. `error` is empty on
    /// success.
    pub fn ended(&mut self, id: FrameId, error: &str) -> Option<Fired> {
        let result = if error.is_empty() {
            Ok(())
        } else {
            Err(OutputError::Platform(error.to_owned()))
        };
        self.resolve(id, result)
    }

    /// Unrecoverable render error before the upload. The queued frame
    /// fails and is not uploaded later.
    pub fn failed(&mut self, error: &str) -> Option<Fired> {
        let id = self.queued.take()?;
        self.resolve(id, Err(OutputError::Platform(error.to_owned())))
    }

    /// The acknowledgement timer of `id` expired. A frame that was already
    /// uploaded or replaced is not affected.
    pub fn timed_out(&mut self, id: FrameId) -> Option<Fired> {
        if self.queued != Some(id) {
            return None;
        }
        self.queued = None;
        self.resolve(id, Err(OutputError::Timeout))
    }

    /// The window closed or was replaced. A pending waiter gets
    /// `Disconnected`, also if its frame was not yet queued.
    pub fn closed(&mut self) -> Option<Fired> {
        self.queued = None;
        self.waiter
            .take()
            .map(|(_, done)| (done, Err(OutputError::Disconnected)))
    }

    fn resolve(&mut self, id: FrameId, result: Result<(), OutputError>) -> Option<Fired> {
        if self.waiter.as_ref().is_some_and(|(w, _)| *w == id) {
            self.waiter.take().map(|(_, done)| (done, result))
        } else {
            None
        }
    }
}

thread_local! {
    static STATE: RefCell<Submissions> = RefCell::default();
}

/// Runs `f` on the GUI-thread state. A no-op during thread-local
/// teardown, when a window destructor can still report its close.
fn with<R: Default>(f: impl FnOnce(&mut Submissions) -> R) -> R {
    STATE
        .try_with(|s| f(&mut s.borrow_mut()))
        .unwrap_or_default()
}

/// Completions run after the borrow ends, so that they can register or
/// cancel again.
fn fire(f: impl FnOnce(&mut Submissions) -> Option<Fired>) {
    if let Some((done, result)) = with(f) {
        done(result);
    }
}

/// Registration of one completion. Dropping it cancels the completion.
/// Not `Send`: the state is on the Qt GUI thread.
pub struct Registration {
    id: FrameId,
    _gui_thread: PhantomData<*const ()>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // The removed completion is dropped outside the borrow.
        drop(with(|s| s.cancel(self.id)));
    }
}

/// Queues `pixels` in the open window under a new frame ID. `done` runs
/// once on the GUI thread with the acknowledgement, a failure, a timeout
/// or `Disconnected`; it does not run after the registration is dropped.
///
/// # Errors
/// The window rejected the frame; `done` is not called.
pub fn present(
    pixels: &[u8],
    width: i32,
    height: i32,
    done: impl FnOnce(Result<(), OutputError>) + 'static,
) -> Result<Registration, String> {
    let id = with(Submissions::next_frame_id);
    drop(with(|s| s.register(id, Box::new(done))));
    let registration = Registration {
        id,
        _gui_thread: PhantomData,
    };
    // The window only posts an update request here, so no render event
    // can run before `queued`.
    crate::display::ffi::present_pattern(pixels, width, height, id, ACK_TIMEOUT_MS)
        .map_err(|e| e.to_string())?;
    with(|s| s.queued(id));
    Ok(registration)
}

// Render events from `pattern_window.cpp`, through the `display` bridge.

pub fn take_upload() -> FrameId {
    with(Submissions::take_upload).unwrap_or(0)
}

pub fn ended(id: FrameId, error: &str) {
    fire(|s| s.ended(id, error));
}

pub fn failed(error: &str) {
    fire(|s| s.failed(error));
}

pub fn timed_out(id: FrameId) {
    fire(|s| s.timed_out(id));
}

pub fn closed() {
    fire(Submissions::closed);
}

#[cfg(test)]
#[path = "submission_tests.rs"]
mod tests;
