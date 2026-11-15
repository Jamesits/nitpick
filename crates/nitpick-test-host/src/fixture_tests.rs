use nitpick_core::signal::Encoding;
use nitpick_fixtures::{CASE_GROUPS, MetadataRequest};

use super::*;

fn cases() -> Vec<nitpick_fixtures::Case> {
    CASE_GROUPS
        .iter()
        .flat_map(|g| nitpick_fixtures::group(g).unwrap().cases)
        .collect()
}

#[test]
fn loads_every_case_by_id() {
    let fixtures = Fixtures::load().unwrap();
    for case in cases() {
        let frame = fixtures.get(&case.input.id).expect("loaded");
        assert_eq!(frame.size, (case.input.size[0], case.input.size[1]));
        assert_eq!(frame.format.encoding.id(), case.input.format.encoding);
        assert_eq!(frame.format.depth.bits(), case.input.format.depth);
        assert_eq!(frame.metadata, case.input.metadata);
    }
    assert!(fixtures.get("no-such-case").is_none());
}

#[test]
fn renders_the_palette_codes_at_their_own_size() {
    for case in cases() {
        let frame = FixtureFrame::from_case(&case.input).unwrap();
        let image = frame.render(frame.size).unwrap();
        let indices = nitpick_fixtures::raster(&case.input);
        for (pixel, index) in image.pixels().iter().zip(indices) {
            let codes = case.input.palette[index].codes;
            assert_eq!(
                [pixel.r, pixel.g, pixel.b].map(i64::from),
                codes,
                "{}",
                case.input.id
            );
        }
    }
}

#[test]
fn pads_with_nominal_black_at_the_top_left_origin() {
    let fixtures = Fixtures::load().unwrap();
    for (id, black) in [
        ("geom-corners-pq-full-10", 0),
        ("lum-neutral-pq-narrow-10", 64),
        ("lum-neutral-pq-narrow-12", 256),
        ("codes-endpoints-hlg-narrow-12", 256),
    ] {
        let frame = fixtures.get(id).unwrap();
        let own = frame.render(frame.size).unwrap();
        let (w, h) = frame.size;
        let image = frame.render((w + 3, h + 2)).unwrap();
        for y in 0..h + 2 {
            for x in 0..w + 3 {
                let want = if x < w && y < h {
                    own.get(x, y)
                } else {
                    Rgb::grey(black)
                };
                assert_eq!(image.get(x, y), want, "{id} ({x}, {y})");
            }
        }
    }
}

#[test]
fn rejects_a_surface_smaller_than_the_fixture() {
    let fixtures = Fixtures::load().unwrap();
    let frame = fixtures.get("state-frame-a-pq-full-10").unwrap();
    assert_eq!(frame.size, (32, 20));
    assert!(frame.render((31, 100)).is_err());
    assert!(frame.render((100, 19)).is_err());
    assert!(frame.render((32, 20)).is_ok());
}

#[test]
fn rejects_invalid_and_repeated_cases() {
    let invalid = nitpick_fixtures::invalid().unwrap();
    for case in &invalid.cases {
        assert!(FixtureFrame::from_case(&case.input).is_err(), "{}", case.id);
    }
    let case = cases().remove(0);
    let mut fixtures = Fixtures::default();
    fixtures.insert(&case.input).unwrap();
    assert!(fixtures.insert(&case.input).is_err());
}

#[test]
fn maps_the_metadata_request() {
    let md = MetadataRequest {
        primaries: [[0.68, 0.32], [0.265, 0.69], [0.15, 0.06]],
        white: [0.3127, 0.329],
        max_mastering_nits: 4000.0,
        min_mastering_nits: 0.005,
        light_level: LightLevelRequest::Explicit {
            max_cll: 800.0,
            max_fall: 200.0,
        },
    };
    assert_eq!(
        hdr10_request(&md),
        Hdr10Request {
            primaries: md.primaries,
            white: md.white,
            max_mastering_nits: 4000.0,
            min_mastering_nits: 0.005,
            light_level: ContentLightLevel::Explicit {
                max_cll: 800.0,
                max_fall: 200.0
            },
        }
    );
    let from_frame = MetadataRequest {
        light_level: LightLevelRequest::FromFrame,
        ..md
    };
    assert_eq!(
        hdr10_request(&from_frame).light_level,
        ContentLightLevel::FromFrame
    );
    // Narrow-range formats keep their encoding.
    let fixtures = Fixtures::load().unwrap();
    let frame = fixtures.get("lum-uniform-peak-pq-narrow-10").unwrap();
    assert_eq!(frame.format.encoding, Encoding::PqNarrow);
}
