use std::collections::BTreeSet;

use super::*;
use crate::fixture::Fixtures;
use crate::replay;

const FRAME_A: &str = "state-frame-a-pq-full-10";

fn fixtures() -> Fixtures {
    Fixtures::load().unwrap()
}

/// The definition, pixel by pixel.
fn brute_force(f: &FixtureFrame, inset: usize, exclude: &[Rect]) -> Vec<BTreeSet<(usize, usize)>> {
    let (w, h) = f.size;
    let mut out = vec![BTreeSet::new(); f.palette.len()];
    for y in 0..h {
        for x in 0..w {
            let i = f.index(x, y);
            let inside = x >= inset && y >= inset && x + inset < w && y + inset < h;
            let flat = inside
                && (y - inset..=y + inset)
                    .all(|q| (x - inset..=x + inset).all(|p| f.index(p, q) == i));
            if flat && !exclude.iter().any(|r| r.contains(x, y)) {
                out[i].insert((x, y));
            }
        }
    }
    out
}

fn as_sets(patches: &[Patch]) -> Vec<BTreeSet<(usize, usize)>> {
    patches
        .iter()
        .enumerate()
        .map(|(i, p)| {
            assert_eq!(p.index, i);
            p.pixels.iter().copied().collect()
        })
        .collect()
}

#[test]
fn flat_patch_samples_skip_edges() {
    let fx = fixtures();
    let f = fx.get(FRAME_A).unwrap();
    // The 8×8 patch of palette entry 2 at (12, 8).
    let p = patches(f, PATCH_INSET, &[]);
    let want: BTreeSet<_> = (9..15)
        .flat_map(|y| (13..19).map(move |x| (x, y)))
        .collect();
    assert_eq!(as_sets(&p)[2], want);
    // The 4×4 marker cells of entry 1 keep 2×2 each.
    assert_eq!(p[1].pixels.len(), 4 * 4);
    assert!(p[1].pixels.contains(&(1, 1)) && !p[1].pixels.contains(&(0, 1)));
    // Inset 2 leaves nothing of a 4×4 cell.
    let p2 = patches(f, 2, &[]);
    assert_eq!(p2[1].pixels.len(), 0);
    assert_eq!(p2[2].pixels.len(), 4 * 4);
}

#[test]
fn geometry_samples_every_pixel_once() {
    let fx = fixtures();
    let f = fx.get(FRAME_A).unwrap();
    let p = patches(f, 0, &[]);
    let total: usize = p.iter().map(|p| p.pixels.len()).sum();
    assert_eq!(total, f.size.0 * f.size.1);
    for patch in &p {
        for &(x, y) in &patch.pixels {
            assert_eq!(f.index(x, y), patch.index);
        }
    }
}

#[test]
fn excluded_areas_are_not_sampled() {
    let fx = fixtures();
    let f = fx.get(FRAME_A).unwrap();
    let cursor = Rect::new(14, 10, 3, 2);
    let p = patches(f, PATCH_INSET, &[cursor]);
    assert_eq!(p[2].pixels.len(), 36 - 6);
    assert!(p[2].pixels.iter().all(|&(x, y)| !cursor.contains(x, y)));
}

/// Rectangle and row layouts, overlapping rectangles and one-pixel lines.
#[test]
fn patches_follow_the_definition_for_the_fixtures() {
    let fx = fixtures();
    let ids = [
        FRAME_A,
        "geom-corners-pq-full-10",
        "lum-neutral-pq-narrow-10",
        "gamut-pq-full-10",
    ];
    let exclude = [Rect::new(3, 2, 5, 4)];
    for id in ids {
        let f = fx.get(id).unwrap();
        for inset in 0..3 {
            for ex in [&[][..], &exclude[..]] {
                assert_eq!(
                    as_sets(&patches(f, inset, ex)),
                    brute_force(f, inset, ex),
                    "{id} inset {inset}"
                );
            }
        }
    }
}

#[test]
fn surface_origin_needs_the_surface_size() {
    let image = nitpick_core::image::Image::new(64, 40, nitpick_core::signal::Rgb::grey(0));
    let whole = replay::frame(&image, replay::time(1));
    assert_eq!(surface_origin(&whole.descriptor, (64, 40)), Ok((0, 0)));
    let framed = replay::framed(&image, replay::time(1), (3, 5), (70, 50));
    assert_eq!(surface_origin(&framed.descriptor, (64, 40)), Ok((3, 5)));
    assert_eq!(
        surface_origin(&whole.descriptor, (64, 41)),
        Err("the capture content is 64×40 px; the surface is 64×41 px".into())
    );
    assert!(surface_origin(&framed.descriptor, (70, 50)).is_err());
}
