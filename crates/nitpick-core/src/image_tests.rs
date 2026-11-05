use super::*;

#[test]
fn fill_rect_is_half_open_and_clipped() {
    let mut img = Image::new(4, 3, Rgb10::grey(0));
    img.fill_rect(1, 1, 10, 2, Rgb10::grey(5));
    assert_eq!(img.get(0, 1), Rgb10::grey(0));
    assert_eq!(img.get(1, 1), Rgb10::grey(5));
    assert_eq!(img.get(3, 1), Rgb10::grey(5));
    assert_eq!(img.get(1, 2), Rgb10::grey(0));
    assert_eq!(img.get(1, 0), Rgb10::grey(0));
}

#[test]
fn fill_columns_uses_absolute_x() {
    let mut img = Image::new(4, 2, Rgb10::grey(0));
    #[allow(clippy::cast_possible_truncation)]
    img.fill_columns(1, 0, 3, 2, |x| Rgb10::grey(x as u16));
    assert_eq!(img.get(1, 1), Rgb10::grey(1));
    assert_eq!(img.get(2, 0), Rgb10::grey(2));
    assert_eq!(img.get(3, 0), Rgb10::grey(0));
}

#[test]
fn set_outside_is_ignored() {
    let mut img = Image::new(2, 2, Rgb10::grey(0));
    img.set(2, 0, Rgb10::grey(1));
    assert!(img.pixels().iter().all(|&p| p == Rgb10::grey(0).pack()));
}
