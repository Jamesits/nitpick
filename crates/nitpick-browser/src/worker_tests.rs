use std::sync::mpsc;
use std::time::Duration;

use super::*;

const WAIT: Duration = Duration::from_secs(10);

#[test]
fn runs_jobs_off_the_calling_thread() {
    let worker = EncodeWorker::default();
    let (tx, rx) = mpsc::channel();
    worker.spawn(Box::new(move || {
        tx.send(thread::current().name().map(str::to_owned))
            .unwrap();
    }));
    assert_eq!(
        rx.recv_timeout(WAIT).unwrap().as_deref(),
        Some("nitpick-png")
    );
}

#[test]
fn a_new_job_replaces_a_queued_job() {
    let worker = EncodeWorker::default();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    worker.spawn(Box::new(move || {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    }));
    started_rx.recv_timeout(WAIT).unwrap();

    // The first job runs; the second waits and is then replaced.
    let (done_tx, done_rx) = mpsc::channel();
    let replaced = done_tx.clone();
    worker.spawn(Box::new(move || replaced.send("replaced").unwrap()));
    worker.spawn(Box::new(move || done_tx.send("latest").unwrap()));
    release_tx.send(()).unwrap();
    assert_eq!(done_rx.recv_timeout(WAIT).unwrap(), "latest");
    // Both senders are gone: the replaced job was dropped, not run.
    assert!(done_rx.recv_timeout(WAIT).is_err());
}

#[test]
fn drop_discards_the_queued_job() {
    let worker = EncodeWorker::default();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    worker.spawn(Box::new(move || {
        started_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    }));
    started_rx.recv_timeout(WAIT).unwrap();
    let (tx, rx) = mpsc::channel::<()>();
    worker.spawn(Box::new(move || tx.send(()).unwrap()));
    drop(worker);
    release_tx.send(()).unwrap();
    assert!(rx.recv_timeout(WAIT).is_err());
}
