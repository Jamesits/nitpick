use std::cmp::Ordering;

use nitpick_core::driver::DisplayId;

use super::*;

fn target(display: &DisplayId) -> CaptureTarget<'_> {
    CaptureTarget {
        display,
        frame: 1,
        generation: 1,
    }
}

fn display() -> DisplayId {
    DisplayId {
        driver: "qrhi",
        key: "test".into(),
    }
}

#[test]
fn clock_is_qpc() {
    let adapter = WgcAdapter::new(Box::new(|_| Err("unused".into())));
    let a = adapter.now().expect("QPC");
    let b = adapter.now().expect("QPC");
    assert_eq!(a.base, TimeBase::Qpc);
    assert!(a.ticks_per_second > 0);
    assert_ne!(a.cmp_time(&b), Some(Ordering::Greater));
}

#[test]
fn resolver_failure_is_the_reason() {
    let mut adapter = WgcAdapter::new(Box::new(|_| Err("no pattern window is open".into())));
    let display = display();
    assert_eq!(
        adapter.start(&target(&display)).err(),
        Some("no pattern window is open".into())
    );
}

#[test]
fn window_without_capture_item_is_unavailable() {
    let mut adapter = WgcAdapter::new(Box::new(|_| Ok(0)));
    let display = display();
    let reason = adapter
        .start(&target(&display))
        .err()
        .expect("no capture of a null window");
    assert!(
        reason.starts_with("CreateForWindow(0x0) failed"),
        "{reason}"
    );
}
