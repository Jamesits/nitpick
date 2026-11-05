use super::*;

fn model(sections: Vec<Section>) -> PatternModel {
    PatternModel {
        title: "test".into(),
        encoding: Encoding::PqFull,
        reference: (100, 50),
        background: Rgb10::grey(0),
        sections,
    }
}

#[test]
fn adjacent_regions_tile_at_any_size() {
    let mut s = Section::new("row");
    s.push(Region::solid("a", Rect::new(0, 0, 33, 50), Rgb10::grey(1)))
        .push(Region::solid(
            "b",
            Rect::new(33, 0, 100, 50),
            Rgb10::grey(2),
        ));
    let m = model(vec![s]);
    for w in [100, 137, 200, 251] {
        let img = m.render(w, 20);
        assert!(
            img.pixels().iter().all(|&p| p != Rgb10::grey(0).pack()),
            "gap at {w}"
        );
    }
    let img = m.render(200, 100);
    assert_eq!(img.get(65, 0), Rgb10::grey(1));
    assert_eq!(img.get(66, 0), Rgb10::grey(2));
}

#[test]
fn later_regions_cover_earlier_ones() {
    let mut s = Section::new("row");
    s.push(Region::solid(
        "under",
        Rect::new(0, 0, 100, 50),
        Rgb10::grey(1),
    ))
    .push(Region::solid(
        "over",
        Rect::new(10, 10, 20, 20),
        Rgb10::grey(2),
    ));
    let img = model(vec![s]).render(100, 50);
    assert_eq!(img.get(15, 15), Rgb10::grey(2));
    assert_eq!(img.get(5, 5), Rgb10::grey(1));
}

#[test]
fn ramp_spreads_codes_evenly() {
    assert_eq!(Fill::ramp_value(5, 1018, 0, 1014), 5);
    assert_eq!(Fill::ramp_value(5, 1018, 1013, 1014), 1018);
    assert_eq!(Fill::ramp_value(5, 1018, 1, 2028), 5);
    assert_eq!(Fill::ramp_value(5, 1018, 2, 2028), 6);
    assert_eq!(Fill::ramp_value(5, 1018, 2027, 2028), 1018);
    // Fewer pixels than codes: end points stay exact.
    assert_eq!(Fill::ramp_value(0, 1023, 0, 100), 0);
    assert_eq!(Fill::ramp_value(0, 1023, 99, 100), 1013);
}

#[test]
fn centred_text_is_centred() {
    let mut s = Section::new("labels");
    s.push(Region {
        name: "label".into(),
        rect: Rect::new(0, 0, 100, 16),
        fill: Fill::Text {
            text: "ab".into(),
            colour: Rgb10::grey(9),
            height: 16,
            align: Align::Centre,
        },
    });
    // Reference 100×50 at 2× output: glyph scale 4, text 64 px wide, centre x = 100.
    let img = model(vec![s]).render(200, 100);
    let xs: Vec<usize> = (0..200)
        .filter(|&x| (0..32).any(|y| img.get(x, y) == Rgb10::grey(9)))
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
        Rgb10::grey(0),
    ))
    .push(Region::solid(
        "black",
        Rect::new(10, 0, 20, 10),
        Rgb10::grey(0),
    ))
    .push(Region {
        name: "ramp".into(),
        rect: Rect::new(20, 0, 30, 10),
        fill: Fill::Ramp { first: 1, last: 9 },
    });
    let mut b = Section::new("b");
    b.push(Region::solid(
        "black",
        Rect::new(0, 10, 10, 20),
        Rgb10::grey(0),
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
