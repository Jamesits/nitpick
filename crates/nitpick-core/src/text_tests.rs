use super::*;

#[test]
fn width_scales_with_length() {
    assert_eq!(text_width("abc", 2), 48);
    assert_eq!(line_height(3), 24);
}

#[test]
fn draws_only_two_levels() {
    let bg = Rgb10::grey(64);
    let fg = Rgb10::grey(500);
    let mut img = Image::new(40, 16, bg);
    draw_text(&mut img, 0, 0, 2, "A1", fg);
    let fg_count = img.pixels().iter().filter(|&&p| p == fg.pack()).count();
    assert!(fg_count > 0);
    assert!(
        img.pixels()
            .iter()
            .all(|&p| p == fg.pack() || p == bg.pack())
    );
}
