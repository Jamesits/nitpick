use std::cell::RefCell;
use std::rc::Rc;

use nitpick_core::driver::OutputError;

use super::{Completion, Fired, FrameId, Registration, Submissions};

/// Results by frame ID, in completion order.
type Log = Rc<RefCell<Vec<(FrameId, Result<(), OutputError>)>>>;

fn recorder(log: &Log, id: FrameId) -> Completion {
    let log = Rc::clone(log);
    Box::new(move |result| log.borrow_mut().push((id, result)))
}

fn run(fired: Option<Fired>) {
    if let Some((done, result)) = fired {
        done(result);
    }
}

/// Registers and queues a new frame, as `submission::present` does.
fn submit(s: &mut Submissions, log: &Log) -> FrameId {
    let id = s.next_frame_id();
    assert!(s.register(id, recorder(log, id)).is_none());
    s.queued(id);
    id
}

#[test]
fn frame_ids_are_unique_and_nonzero() {
    let mut s = Submissions::default();
    let a = s.next_frame_id();
    let b = s.next_frame_id();
    assert_ne!(a, 0);
    assert_ne!(a, b);
}

#[test]
fn success_only_after_upload_and_end_frame() {
    let log = Log::default();
    let mut s = Submissions::default();
    let id = submit(&mut s, &log);
    // Queueing alone, like requestUpdate(), is no acknowledgement.
    assert!(log.borrow().is_empty());
    assert_eq!(s.take_upload(), Some(id));
    assert!(log.borrow().is_empty());
    run(s.ended(id, ""));
    assert_eq!(*log.borrow(), vec![(id, Ok(()))]);
}

#[test]
fn render_without_queued_frame_uploads_nothing() {
    let log = Log::default();
    let mut s = Submissions::default();
    assert_eq!(s.take_upload(), None);
    let id = submit(&mut s, &log);
    assert_eq!(s.take_upload(), Some(id));
    // An expose redraw after the upload.
    assert_eq!(s.take_upload(), None);
}

#[test]
fn begin_frame_failure_fails_the_queued_frame() {
    let log = Log::default();
    let mut s = Submissions::default();
    let id = submit(&mut s, &log);
    run(s.failed("beginFrame failed: graphics device lost"));
    assert_eq!(
        *log.borrow(),
        vec![(
            id,
            Err(OutputError::Platform(
                "beginFrame failed: graphics device lost".into()
            ))
        )]
    );
    // The failed frame does not upload in a later render.
    assert_eq!(s.take_upload(), None);
}

#[test]
fn begin_frame_failure_without_queued_frame_is_ignored() {
    let log = Log::default();
    let mut s = Submissions::default();
    let id = submit(&mut s, &log);
    assert_eq!(s.take_upload(), Some(id));
    run(s.ended(id, ""));
    assert!(s.failed("beginFrame failed").is_none());
    assert_eq!(log.borrow().len(), 1);
}

#[test]
fn end_frame_failure_fails_the_uploaded_frame() {
    let log = Log::default();
    let mut s = Submissions::default();
    let id = submit(&mut s, &log);
    assert_eq!(s.take_upload(), Some(id));
    run(s.ended(id, "endFrame failed"));
    assert_eq!(
        *log.borrow(),
        vec![(id, Err(OutputError::Platform("endFrame failed".into())))]
    );
}

#[test]
fn timeout_fails_a_queued_frame_and_blocks_its_upload() {
    let log = Log::default();
    let mut s = Submissions::default();
    let id = submit(&mut s, &log);
    run(s.timed_out(id));
    assert_eq!(*log.borrow(), vec![(id, Err(OutputError::Timeout))]);
    assert_eq!(s.take_upload(), None);
    // A late render report does not complete the frame twice.
    assert!(s.ended(id, "").is_none());
}

#[test]
fn timeout_after_upload_is_ignored() {
    let log = Log::default();
    let mut s = Submissions::default();
    let id = submit(&mut s, &log);
    assert_eq!(s.take_upload(), Some(id));
    run(s.ended(id, ""));
    assert!(s.timed_out(id).is_none());
    assert_eq!(*log.borrow(), vec![(id, Ok(()))]);
}

#[test]
fn close_reports_disconnected() {
    let log = Log::default();
    let mut s = Submissions::default();
    let id = submit(&mut s, &log);
    run(s.closed());
    assert_eq!(*log.borrow(), vec![(id, Err(OutputError::Disconnected))]);
    assert_eq!(s.take_upload(), None);
    assert!(s.closed().is_none());
}

#[test]
fn close_without_waiter_is_ignored() {
    let mut s = Submissions::default();
    let id = s.next_frame_id();
    s.queued(id);
    assert!(s.closed().is_none());
    assert_eq!(s.take_upload(), None);
}

#[test]
fn cancel_removes_the_registration() {
    let log = Log::default();
    let mut s = Submissions::default();
    let id = submit(&mut s, &log);
    assert!(s.cancel(id).is_some());
    // Already queued output can still show; no result reaches the host.
    assert_eq!(s.take_upload(), Some(id));
    assert!(s.ended(id, "").is_none());
    assert!(s.closed().is_none());
    assert!(log.borrow().is_empty());
}

#[test]
fn cancel_of_an_old_id_keeps_the_current_registration() {
    let log = Log::default();
    let mut s = Submissions::default();
    let old = submit(&mut s, &log);
    assert!(s.cancel(old).is_some());
    let new = submit(&mut s, &log);
    assert!(s.cancel(old).is_none());
    assert_eq!(s.take_upload(), Some(new));
    run(s.ended(new, ""));
    assert_eq!(*log.borrow(), vec![(new, Ok(()))]);
}

#[test]
fn rapid_replacements_upload_and_complete_only_the_last() {
    let log = Log::default();
    let mut s = Submissions::default();
    let first = submit(&mut s, &log);
    assert!(s.cancel(first).is_some());
    let second = submit(&mut s, &log);
    assert!(s.cancel(second).is_some());
    let third = submit(&mut s, &log);
    // One render after all three requests.
    assert_eq!(s.take_upload(), Some(third));
    // Late events of the replaced frames.
    assert!(s.timed_out(first).is_none());
    assert!(s.ended(second, "").is_none());
    assert!(s.timed_out(third).is_none());
    assert!(log.borrow().is_empty());
    run(s.ended(third, ""));
    assert_eq!(*log.borrow(), vec![(third, Ok(()))]);
}

#[test]
fn late_render_of_an_old_frame_does_not_complete_a_new_one() {
    let log = Log::default();
    let mut s = Submissions::default();
    let old = submit(&mut s, &log);
    assert_eq!(s.take_upload(), Some(old));
    assert!(s.cancel(old).is_some());
    let new = submit(&mut s, &log);
    assert!(s.ended(old, "").is_none());
    assert!(log.borrow().is_empty());
    assert_eq!(s.take_upload(), Some(new));
    run(s.ended(new, ""));
    assert_eq!(*log.borrow(), vec![(new, Ok(()))]);
}

#[test]
fn registration_drop_cancels_on_the_thread_state() {
    let log = Log::default();
    let id = super::with(Submissions::next_frame_id);
    drop(super::with(|s| s.register(id, recorder(&log, id))));
    super::with(|s| s.queued(id));
    drop(Registration {
        id,
        _gui_thread: std::marker::PhantomData,
    });
    assert_eq!(super::take_upload(), id);
    super::ended(id, "");
    assert!(log.borrow().is_empty());
}

#[test]
fn completion_can_register_again() {
    let log = Log::default();
    let id = super::with(Submissions::next_frame_id);
    let inner = Rc::clone(&log);
    let done: Completion = Box::new(move |result| {
        inner.borrow_mut().push((id, result));
        // A host that submits the next frame from the completion.
        let next = super::with(Submissions::next_frame_id);
        drop(super::with(|s| s.register(next, Box::new(|_| {}))));
    });
    drop(super::with(|s| s.register(id, done)));
    super::with(|s| s.queued(id));
    super::closed();
    assert_eq!(*log.borrow(), vec![(id, Err(OutputError::Disconnected))]);
}

#[test]
fn close_watch_resolves_at_the_next_close() {
    let mut s = Submissions::default();
    let since = s.close_count();
    let mut rx = s.watch_close(since);
    assert_eq!(rx.try_recv(), Ok(None));
    run(s.closed());
    assert_eq!(rx.try_recv(), Ok(Some(())));
    assert_eq!(s.close_count(), since + 1);
}

#[test]
fn close_watch_after_a_close_resolves_at_once() {
    let mut s = Submissions::default();
    let since = s.close_count();
    run(s.closed());
    assert_eq!(s.watch_close(since).try_recv(), Ok(Some(())));
    // A watch from the current count waits.
    assert_eq!(s.watch_close(s.close_count()).try_recv(), Ok(None));
}

#[test]
fn dropped_close_watches_are_released() {
    let mut s = Submissions::default();
    for _ in 0..3 {
        drop(s.watch_close(0));
    }
    let _kept = s.watch_close(0);
    assert_eq!(s.close_watchers.len(), 1);
}
