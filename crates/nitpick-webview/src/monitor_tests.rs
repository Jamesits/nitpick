use super::*;

fn monitor(position: (i32, i32), size: (u32, u32), scale: f64) -> Monitor {
    Monitor {
        name: String::new(),
        position,
        size,
        scale,
    }
}

/// Windows: a 150 % monitor at the left of a 100 % primary monitor, in
/// native pixels.
fn windows_layout() -> Vec<Monitor> {
    vec![
        monitor((0, 0), (2560, 1440), 1.0),
        monitor((-3840, -720), (3840, 2160), 1.5),
    ]
}

/// macOS: a Retina monitor (2x) at the right of a 1x monitor. The library
/// multiplies the point position by each monitor's scale, so the
/// physical rectangles overlap.
fn macos_layout() -> Vec<Monitor> {
    vec![
        monitor((0, 0), (1920, 1080), 1.0),
        monitor((3840, 0), (3024, 1964), 2.0),
    ]
}

#[test]
fn physical_rect_is_unchanged() {
    let m = monitor((-3840, -720), (3840, 2160), 1.5);
    let rect = m.desktop_rect(Coordinates::Physical);
    assert_eq!(
        (rect.x, rect.y, rect.width, rect.height),
        (-3840, -720, 3840, 2160)
    );
}

#[test]
fn logical_rect_divides_by_the_monitor_scale() {
    let rect = macos_layout()[1].desktop_rect(Coordinates::Logical);
    assert_eq!(
        (rect.x, rect.y, rect.width, rect.height),
        (1920, 0, 1512, 982)
    );
    // Fractional GDK or macOS scales round to the nearest unit.
    let rect = monitor((-1, 0), (2561, 1441), 1.25).desktop_rect(Coordinates::Logical);
    assert_eq!(
        (rect.x, rect.y, rect.width, rect.height),
        (-1, 0, 2049, 1153)
    );
}

#[test]
fn select_uses_half_open_rectangles() {
    let monitors = windows_layout();
    let at = |x, y| select(&monitors, Coordinates::Physical, (x, y));
    assert_eq!(at(0, 0), Ok(0));
    assert_eq!(at(2559, 1439), Ok(0));
    assert_eq!(at(-1, 0), Ok(1));
    assert_eq!(at(-3840, -720), Ok(1));
    assert_eq!(at(2560, 0), Err(SelectError::None));
    assert_eq!(at(-1, 1440), Err(SelectError::None));
}

#[test]
fn select_in_logical_units_separates_mixed_scales() {
    let monitors = macos_layout();
    // Physical values: the 1x monitor would not contain x = 2000, and
    // x = 1920 to 3839 would be a gap.
    assert_eq!(select(&monitors, Coordinates::Logical, (2000, 10)), Ok(1));
    assert_eq!(select(&monitors, Coordinates::Logical, (1919, 10)), Ok(0));
    assert_eq!(
        select(&monitors, Coordinates::Physical, (2000, 10)),
        Err(SelectError::None)
    );
}

#[test]
fn select_rejects_overlapping_monitors() {
    // Mirrored displays report the same rectangle.
    let monitors = vec![
        monitor((0, 0), (1920, 1080), 1.0),
        monitor((0, 0), (1920, 1080), 1.0),
    ];
    assert_eq!(
        select(&monitors, Coordinates::Physical, (5, 5)),
        Err(SelectError::Ambiguous(vec![0, 1]))
    );
}

#[test]
fn physical_centre_is_inside_the_physical_rect() {
    assert_eq!(windows_layout()[1].physical_centre(), (-1920.0, 360.0));
}
