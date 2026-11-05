use super::*;
use crate::signal::{BitDepth, Encoding};

fn model(sections: Vec<Section>) -> PatternModel {
    PatternModel {
        title: "test".into(),
        format: Format::new(Encoding::PqFull, BitDepth::Ten),
        reference: (100, 50),
        background: Rgb::grey(0),
        sections,
    }
}

#[test]
fn adjacent_regions_tile_at_any_size() {
    let mut s = Section::new("row");
    s.push(Region::solid("a", Rect::new(0, 0, 33, 50), Rgb::grey(1)))
        .push(Region::solid("b", Rect::new(33, 0, 100, 50), Rgb::grey(2)));
    let m = model(vec![s]);
    for w in [100, 137, 200, 251] {
        let img = m.render(w, 20);
        assert!(
            img.pixels().iter().all(|&p| p != Rgb::grey(0)),
            "gap at {w}"
        );
    }
    let img = m.render(200, 100);
    assert_eq!(img.get(65, 0), Rgb::grey(1));
    assert_eq!(img.get(66, 0), Rgb::grey(2));
}

#[test]
fn later_regions_cover_earlier_ones() {
    let mut s = Section::new("row");
    s.push(Region::solid(
        "under",
        Rect::new(0, 0, 100, 50),
        Rgb::grey(1),
    ))
    .push(Region::solid(
        "over",
        Rect::new(10, 10, 20, 20),
        Rgb::grey(2),
    ));
    let img = model(vec![s]).render(100, 50);
    assert_eq!(img.get(15, 15), Rgb::grey(2));
    assert_eq!(img.get(5, 5), Rgb::grey(1));
}

#[test]
fn ramp_spreads_codes_evenly() {
    assert_eq!(Fill::ramp_value(5, 1018, 1, 0, 1014), 5);
    assert_eq!(Fill::ramp_value(5, 1018, 1, 1013, 1014), 1018);
    assert_eq!(Fill::ramp_value(5, 1018, 1, 1, 2028), 5);
    assert_eq!(Fill::ramp_value(5, 1018, 1, 2, 2028), 6);
    assert_eq!(Fill::ramp_value(5, 1018, 1, 2027, 2028), 1018);
    // Fewer pixels than codes: end points stay exact.
    assert_eq!(Fill::ramp_value(0, 1023, 1, 0, 100), 0);
    assert_eq!(Fill::ramp_value(0, 1023, 1, 50, 100), 517);
    assert_eq!(Fill::ramp_value(0, 1023, 1, 99, 100), 1023);
}

#[test]
fn stepped_ramp_keeps_the_step() {
    // BT.2111-3 Table 5, 2K 12-bit: 1015 px hold 20, 24, …, 4076.
    assert_eq!(Fill::ramp_value(20, 4076, 4, 0, 1015), 20);
    assert_eq!(Fill::ramp_value(20, 4076, 4, 1, 1015), 24);
    assert_eq!(Fill::ramp_value(20, 4076, 4, 1014, 1015), 4076);
    // Scaled to 2030 px, each code covers 2 px.
    assert_eq!(Fill::ramp_value(20, 4076, 4, 1, 2030), 20);
    assert_eq!(Fill::ramp_value(20, 4076, 4, 2, 2030), 24);
    assert!((0..1500).all(|i| (Fill::ramp_value(20, 4076, 4, i, 1500) - 20).is_multiple_of(4)));
}

#[test]
fn centred_text_is_centred() {
    let mut s = Section::new("labels");
    s.push(Region {
        name: "label".into(),
        rect: Rect::new(0, 0, 100, 16),
        fill: Fill::Text {
            text: "ab".into(),
            colour: Rgb::grey(9),
            height: 16,
            align: Align::Centre,
        },
    });
    // Reference 100×50 at 2× output: glyph scale 4, text 64 px wide, centre x = 100.
    let img = model(vec![s]).render(200, 100);
    let xs: Vec<usize> = (0..200)
        .filter(|&x| (0..32).any(|y| img.get(x, y) == Rgb::grey(9)))
        .collect();
    assert!(
        *xs.first().unwrap() >= 68 && *xs.last().unwrap() < 132,
        "{xs:?}"
    );
}

#[test]
fn levels_are_unique_per_section_and_name() {
    let mut a = Section::new("a");
    a.push(Region::solid(
        "black",
        Rect::new(0, 0, 10, 10),
        Rgb::grey(0),
    ))
    .push(Region::solid(
        "black",
        Rect::new(10, 0, 20, 10),
        Rgb::grey(0),
    ))
    .push(Region {
        name: "ramp".into(),
        rect: Rect::new(20, 0, 30, 10),
        fill: Fill::Ramp {
            first: 1,
            last: 9,
            step: 1,
        },
    });
    let mut b = Section::new("b");
    b.push(Region::solid(
        "black",
        Rect::new(0, 10, 10, 20),
        Rgb::grey(0),
    ));
    let m = model(vec![a, b]);
    let names: Vec<(&str, &str)> = m.levels().iter().map(|l| (l.section, l.name)).collect();
    assert_eq!(
        names,
        [
            ("a", "black"),
            ("a", "ramp first"),
            ("a", "ramp last"),
            ("b", "black")
        ]
    );
}
