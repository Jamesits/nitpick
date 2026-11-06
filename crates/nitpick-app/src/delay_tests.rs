use super::delay_from;

#[test]
fn negative_delays_are_rejected() {
    assert_eq!(delay_from(0), Ok(0));
    assert_eq!(delay_from(1000), Ok(1000));
    assert_eq!(delay_from(i32::MAX), Ok(i32::MAX.unsigned_abs()));
    let e = delay_from(-1).unwrap_err();
    assert!(e.contains(">= 0 ms"), "{e}");
}
