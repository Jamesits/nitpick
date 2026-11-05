use super::*;

#[test]
fn scale_is_exact_for_multiples() {
    assert_eq!(scale(206, 3840, 1920), 412);
    assert_eq!(scale(1062, 7680, 1920), 4248);
}
