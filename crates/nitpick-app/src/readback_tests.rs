use nitpick_core::output::SwapChainFormat;

use super::{Placement, expected_target, normalize, outside_pixel};

/// 3×2 pixels of 2 bytes: each byte names its row and column.
fn rows() -> Vec<u8> {
    vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, //
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15,
    ]
}

#[test]
fn packed_top_down_data_is_unchanged() {
    assert_eq!(normalize(rows(), (3, 2), 2, false), Ok(rows()));
}

#[test]
fn row_padding_is_removed() {
    let padded = vec![
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0xee, 0xee, //
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0xee, 0xee,
    ];
    assert_eq!(normalize(padded, (3, 2), 2, false), Ok(rows()));
}

#[test]
fn y_up_rows_are_reversed() {
    let bottom_up = vec![
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0xee, 0xee, //
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0xee, 0xee,
    ];
    assert_eq!(normalize(bottom_up, (3, 2), 2, true), Ok(rows()));
}

#[test]
fn byte_counts_without_whole_rows_fail() {
    // Not a multiple of the height.
    assert!(normalize(vec![0; 13], (3, 2), 2, false).is_err());
    // Rows shorter than the image row.
    assert!(normalize(vec![0; 10], (3, 2), 2, false).is_err());
    assert!(normalize(vec![], (0, 2), 2, false).is_err());
    assert!(normalize(vec![], (3, 0), 2, false).is_err());
}

#[test]
fn outside_pixels_are_opaque_black() {
    let word = u32::from_le_bytes(outside_pixel(SwapChainFormat::Hdr10).try_into().unwrap());
    assert_eq!(word, 0b11 << 30);
    let halves: Vec<u16> = outside_pixel(SwapChainFormat::ScRgb)
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    assert_eq!(halves, [0, 0, 0, 0x3c00]);
}

#[test]
fn expected_target_places_the_pattern_at_the_origin() {
    let pixels: Vec<u8> = (1..=8).flat_map(|v| [v, v, v, v]).collect();
    let placement = Placement {
        size: (4, 2),
        target: (5, 4),
        origin: (1, 2),
    };
    let out = expected_target(SwapChainFormat::Hdr10, &pixels, placement).unwrap();
    let black = outside_pixel(SwapChainFormat::Hdr10);
    let at = |x: usize, y: usize| &out[(y * 5 + x) * 4..(y * 5 + x + 1) * 4];
    for y in 0..4 {
        for x in 0..5 {
            let inside = (1..5).contains(&x) && (2..4).contains(&y);
            if inside {
                let v = u8::try_from((y - 2) * 4 + (x - 1) + 1).unwrap();
                assert_eq!(at(x, y), [v; 4], "({x}, {y})");
            } else {
                assert_eq!(at(x, y), black, "({x}, {y})");
            }
        }
    }
}

#[test]
fn expected_target_rejects_bad_input() {
    let pixels = vec![0; 4 * 4 * 2];
    let outside = Placement {
        size: (4, 2),
        target: (4, 2),
        origin: (1, 0),
    };
    assert!(expected_target(SwapChainFormat::Hdr10, &pixels, outside).is_err());
    let whole = Placement::whole((4, 2));
    assert!(expected_target(SwapChainFormat::Hdr10, &pixels[1..], whole).is_err());
    assert_eq!(
        expected_target(SwapChainFormat::Hdr10, &pixels, whole),
        Ok(pixels)
    );
}
