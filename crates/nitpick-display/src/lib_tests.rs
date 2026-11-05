fn assert_send<T: Send + ?Sized>(_: &T) {}

#[test]
fn native_backend_is_send() {
    assert_send(&*super::native());
}

#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn platform_without_backend_reports_why() {
    use nitpick_core::device::DeviceError;

    let err = super::native().devices().unwrap_err();
    assert!(matches!(
        err,
        DeviceError::Unsupported { reason, .. } if reason.contains(std::env::consts::OS)
    ));
}
