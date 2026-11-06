use std::io::Cursor;

use nitpick_core::signal::{BitDepth, pq_eotf};
use zune_png::PngDecoder;
use zune_png::zune_core::result::DecodingResult;

use super::*;

const WIDTH: usize = 64;

fn formats() -> impl Iterator<Item = Format> {
    Encoding::ALL
        .into_iter()
        .flat_map(|e| BitDepth::ALL.into_iter().map(move |d| Format::new(e, d)))
}

/// Offset and range of the nominal signal, from the design table.
fn nominal(format: Format) -> (u64, u64) {
    if format.is_narrow() {
        let s = 1u64 << (format.depth.bits() - 8);
        (16 * s, 219 * s)
    } else {
        (0, u64::from(format.max_code()))
    }
}

/// Every code of the depth once per channel. G and B use other
/// permutations, so a channel swap changes the decoded values.
fn all_codes(format: Format) -> Image {
    let n = usize::from(format.max_code()) + 1;
    let mut img = Image::new(WIDTH, n / WIDTH, Rgb::default());
    for i in 0..n {
        let code = |j: usize| u16::try_from(j % n).unwrap();
        img.set(
            i % WIDTH,
            i / WIDTH,
            Rgb::new(code(i), code(n - 1 - i), code(i * 7 + 3)),
        );
    }
    img
}

/// Independent decoder: `zune-png` shares no code with the `png` encoder.
fn decode(bytes: &[u8]) -> ((usize, usize), Vec<u16>) {
    let mut decoder = PngDecoder::new(Cursor::new(bytes));
    let DecodingResult::U16(samples) = decoder.decode().expect("decode") else {
        panic!("expected 16-bit samples");
    };
    (decoder.dimensions().unwrap(), samples)
}

/// Checks the round-half-up definition without floating point:
/// $(2V - 1) \cdot range \le 2 \cdot 65535 \cdot (D - offset) < (2V + 1) \cdot range$.
fn assert_sample(code: u16, v: u16, format: Format) {
    let (offset, range) = nominal(format);
    let code = u64::from(code);
    let v = u64::from(v);
    if code <= offset {
        assert_eq!(v, 0, "{format} code {code}");
    } else if code >= offset + range {
        assert_eq!(v, 65535, "{format} code {code}");
    } else {
        let x = 2 * 65535 * (code - offset);
        assert!(
            (2 * v).saturating_sub(1) * range <= x,
            "{format} code {code} → {v}"
        );
        assert!(x < (2 * v + 1) * range, "{format} code {code} → {v}");
    }
}

#[test]
fn every_code_decodes_to_its_sample() {
    for format in formats() {
        let img = all_codes(format);
        let frame = encode_frame(&img, format, None).unwrap();
        assert_eq!(frame.size, (img.width(), img.height()));
        let (size, samples) = decode(&frame.bytes);
        assert_eq!(size, frame.size, "{format}");
        assert_eq!(samples.len(), img.pixels().len() * 3);
        for (px, s) in img.pixels().iter().zip(samples.as_chunks::<3>().0) {
            for (code, &v) in [px.r, px.g, px.b].into_iter().zip(s) {
                assert_sample(code, v, format);
            }
        }
    }
}

#[test]
fn inverse_mapping_recovers_nominal_codes() {
    for format in formats() {
        let (offset, range) = nominal(format);
        for code in 0..=format.max_code() {
            let (v, clip) = png_sample(code, format);
            if !format.is_nominal(code) {
                assert_ne!(clip, Clip::None, "{format} code {code}");
                continue;
            }
            assert_eq!(clip, Clip::None);
            let back = (2 * u64::from(v) * range + 65535) / (2 * 65535) + offset;
            assert_eq!(back, u64::from(code), "{format} code {code} → {v}");
        }
    }
}

#[test]
fn endpoints_and_neighbours() {
    for (depth, black, peak) in [(BitDepth::Ten, 64, 940), (BitDepth::Twelve, 256, 3760)] {
        for e in [Encoding::PqNarrow, Encoding::HlgNarrow] {
            let f = Format::new(e, depth);
            assert_eq!(png_sample(black, f), (0, Clip::None));
            assert_eq!(png_sample(peak, f), (65535, Clip::None));
            assert_eq!(png_sample(black - 1, f), (0, Clip::Below));
            assert_eq!(png_sample(0, f), (0, Clip::Below));
            assert_eq!(png_sample(peak + 1, f), (65535, Clip::Above));
            assert_eq!(png_sample(f.max_code(), f), (65535, Clip::Above));
            let (above_black, _) = png_sample(black + 1, f);
            let (below_peak, _) = png_sample(peak - 1, f);
            assert!(above_black > 0 && below_peak < 65535, "{f}");
        }
        let full = Format::new(Encoding::PqFull, depth);
        assert_eq!(png_sample(0, full), (0, Clip::None));
        assert_eq!(png_sample(full.max_code(), full), (65535, Clip::None));
    }
}

#[test]
fn exact_half_rounds_up() {
    // 65535 · 584 / 3504 = 10922.5
    let f = Format::new(Encoding::PqNarrow, BitDepth::Twelve);
    assert_eq!(png_sample(840, f), (10923, Clip::None));
}

#[test]
fn clipping_counts_pixels() {
    let f = Format::new(Encoding::PqNarrow, BitDepth::Ten);
    let mut img = Image::new(4, 1, Rgb::grey(500));
    img.set(0, 0, Rgb::new(63, 500, 500));
    img.set(1, 0, Rgb::new(941, 941, 500));
    img.set(2, 0, Rgb::new(0, 1023, 500));
    let frame = encode_frame(&img, f, None).unwrap();
    let want = Clipping {
        below_black: 2,
        above_peak: 2,
    };
    assert_eq!(frame.clipping, want);
    assert!(frame.clipping.warning().unwrap().contains('2'));

    let full = Format::new(Encoding::PqFull, BitDepth::Ten);
    let frame = encode_frame(&img, full, None).unwrap();
    assert!(frame.clipping.is_empty());
    assert_eq!(frame.clipping.warning(), None);
}

#[test]
fn rejects_code_above_depth() {
    let f = Format::new(Encoding::PqFull, BitDepth::Ten);
    let mut img = Image::new(2, 2, Rgb::grey(0));
    img.set(1, 1, Rgb::new(0, 1024, 0));
    let err = encode_frame(&img, f, None).unwrap_err();
    let OutputError::Platform(msg) = err else {
        panic!("{err}");
    };
    assert!(msg.contains("(1, 1)"), "{msg}");
}

#[test]
fn rejects_empty_image() {
    let f = Format::new(Encoding::PqFull, BitDepth::Ten);
    assert!(encode_frame(&Image::new(0, 4, Rgb::default()), f, None).is_err());
}

struct Chunk {
    kind: [u8; 4],
    data: Vec<u8>,
}

/// Walks the chunk list and checks every CRC with `crc32fast`.
fn chunks(bytes: &[u8]) -> Vec<Chunk> {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    let mut out = Vec::new();
    let mut rest = &bytes[8..];
    while !rest.is_empty() {
        let len = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = rest[4..8].try_into().unwrap();
        let data = &rest[8..8 + len];
        let crc = u32::from_be_bytes(rest[8 + len..12 + len].try_into().unwrap());
        let mut h = crc32fast::Hasher::new();
        h.update(&kind);
        h.update(data);
        assert_eq!(
            h.finalize(),
            crc,
            "CRC of {}",
            String::from_utf8_lossy(&kind)
        );
        out.push(Chunk {
            kind,
            data: data.to_vec(),
        });
        rest = &rest[12 + len..];
    }
    out
}

#[test]
fn chunk_layout_and_colour_tag() {
    let want = [
        (Encoding::PqNarrow, [9, 16, 0, 1]),
        (Encoding::PqFull, [9, 16, 0, 1]),
        (Encoding::HlgNarrow, [9, 18, 0, 1]),
    ];
    for depth in BitDepth::ALL {
        for (e, tag) in want {
            let f = Format::new(e, depth);
            let img = Image::new(3, 2, Rgb::grey(f.white()));
            let list = chunks(&encode_frame(&img, f, None).unwrap().bytes);
            let kinds: Vec<&[u8; 4]> = list.iter().map(|c| &c.kind).collect();

            assert_eq!(kinds[0], b"IHDR");
            let ihdr = &list[0].data;
            assert_eq!(&ihdr[..8], &[0, 0, 0, 3, 0, 0, 0, 2]);
            // Bit depth 16, colour type 2 (RGB), no interlace.
            assert_eq!(&ihdr[8..], &[16, 2, 0, 0, 0]);

            let cicp_at: Vec<usize> = (0..list.len()).filter(|&i| kinds[i] == b"cICP").collect();
            assert_eq!(cicp_at.len(), 1, "{f}");
            assert_eq!(list[cicp_at[0]].data, tag, "{f}");
            let first_idat = kinds.iter().position(|k| *k == b"IDAT").unwrap();
            assert!(cicp_at[0] < first_idat, "{f}");

            // Metadata is off: no mDCV or cLLI.
            for other in [
                b"sRGB", b"gAMA", b"cHRM", b"iCCP", b"PLTE", b"mDCV", b"cLLI",
            ] {
                assert!(!kinds.contains(&other), "{f}: unexpected chunk");
            }
            assert_eq!(kinds.last().unwrap(), &b"IEND");
            assert_eq!(list.last().unwrap().data, Vec::<u8>::new());
        }
    }
}

fn request(light_level: ContentLightLevel) -> Hdr10Request {
    Hdr10Request {
        primaries: [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
        white: [0.3127, 0.3290],
        max_mastering_nits: 1000.0,
        min_mastering_nits: 0.0001,
        light_level,
    }
}

const EXPLICIT: ContentLightLevel = ContentLightLevel::Explicit {
    max_cll: 1234.5678,
    max_fall: 400.25,
};

fn be16(b: &[u8]) -> Vec<u16> {
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect()
}

fn be32(b: &[u8]) -> Vec<u32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_be_bytes(*c))
        .collect()
}

/// `cLLI` values in 0.0001 cd/m².
fn clli_of(bytes: &[u8]) -> [u32; 2] {
    let list = chunks(bytes);
    let c = list.iter().find(|c| &c.kind == b"cLLI").expect("cLLI");
    assert_eq!(c.data.len(), 8);
    be32(&c.data).try_into().unwrap()
}

#[test]
fn metadata_chunk_widths_order_and_units() {
    for format in formats() {
        let img = Image::new(3, 2, Rgb::grey(format.white()));
        let frame = encode_frame(&img, format, Some(&request(EXPLICIT))).unwrap();
        let list = chunks(&frame.bytes);
        let kinds: Vec<&[u8; 4]> = list.iter().map(|c| &c.kind).collect();
        let at = |k: &[u8; 4]| {
            let found: Vec<usize> = (0..list.len()).filter(|&i| kinds[i] == k).collect();
            assert_eq!(found.len(), 1, "{format}: {}", String::from_utf8_lossy(k));
            found[0]
        };
        let first_idat = kinds.iter().position(|k| *k == b"IDAT").unwrap();
        assert!(at(b"cICP") < first_idat);
        assert!(at(b"mDCV") < first_idat, "{format}");
        assert!(at(b"cLLI") < first_idat, "{format}");

        // PNG 3rd edition 11.3.2.7: R, G, B, white as 16-bit x, y in
        // 0.00002 units, then maximum and minimum luminance as 32-bit
        // values in 0.0001 cd/m².
        let mdcv = &list[at(b"mDCV")].data;
        assert_eq!(mdcv.len(), 24);
        assert_eq!(
            be16(&mdcv[..16]),
            [35400, 14600, 8500, 39850, 6550, 2300, 15635, 16450]
        );
        assert_eq!(be32(&mdcv[16..]), [10_000_000, 1]);

        // Two 32-bit values in 0.0001 cd/m², not 16-bit whole cd/m².
        assert_eq!(clli_of(&frame.bytes), [12_345_678, 4_002_500], "{format}");
        assert_eq!(frame.metadata.unwrap().max_cll, 1234.5678);
    }
}

#[test]
fn mdcv_is_not_the_sei_payload() {
    let md = request(EXPLICIT).resolve(|| unreachable!());
    // The SEI payload starts with green; mDCV starts with red.
    assert_ne!(mdcv(&md), md.mastering_display_bytes());
    assert_eq!(be16(&mdcv(&md)[..4]), [35400, 14600]);
    assert_eq!(be16(&md.mastering_display_bytes()[..4]), [8500, 39850]);
    assert_ne!(clli(&md)[..4], md.content_light_level_bytes());
}

#[test]
fn mdcv_matches_spec_p3_example() {
    let md = Hdr10Request {
        primaries: [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
        ..request(EXPLICIT)
    }
    .resolve(|| unreachable!());
    // The spec's example 9 gives 13520 for green x; 0.265 / 0.00002 is
    // 13250.
    assert_eq!(
        be16(&mdcv(&md)[..12]),
        [34000, 16000, 13250, 34500, 7500, 3000]
    );
}

#[test]
fn uniform_10bit_peak_is_10000_nits_in_the_browser() {
    let narrow = Format::new(Encoding::PqNarrow, BitDepth::Ten);
    let img = Image::new(4, 4, Rgb::grey(940));
    let auto = request(ContentLightLevel::FromFrame);
    let frame = encode_frame(&img, narrow, Some(&auto)).unwrap();
    // Narrow 940 stores 65535: E' = 1.
    assert_eq!(clli_of(&frame.bytes), [100_000_000, 100_000_000]);

    // QRhi reads the same codes as full-range PQ.
    let qrhi = metadata::light_level([[940.0 / 1023.0; 3]]);
    assert!((qrhi.max_cll - 4654.397).abs() < 1e-3, "{}", qrhi.max_cll);

    // Browser full-range PQ stores round(65535 · 940 / 1023).
    let full = Format::new(Encoding::PqFull, BitDepth::Ten);
    let frame = encode_frame(&img, full, Some(&auto)).unwrap();
    let md = frame.metadata.unwrap();
    assert!((md.max_cll - 4654.4).abs() < 0.1, "{}", md.max_cll);
}

#[test]
fn automatic_levels_use_rounded_samples() {
    // 12-bit narrow 840 stores 10923, not E' = 10922.5 / 65535.
    let f = Format::new(Encoding::PqNarrow, BitDepth::Twelve);
    let mut img = Image::new(2, 1, Rgb::grey(840));
    // Below black: stored as 0 and counted in the average.
    img.set(1, 0, Rgb::new(10, 840, 3));
    let samples = PngSamples::from_image(&img, f).unwrap();
    let level = samples.light_level().unwrap();

    let stored = pq_eotf(10923.0 / 65535.0);
    let unrounded = pq_eotf(10922.5 / 65535.0);
    assert!(stored > unrounded);
    assert_eq!(level.max_cll, stored);
    assert_eq!(level.max_fall, stored);

    // Above peak clips to 65535 → 10000 cd/m².
    img.set(1, 0, Rgb::new(4000, 256, 256));
    let level = PngSamples::from_image(&img, f)
        .unwrap()
        .light_level()
        .unwrap();
    assert_eq!(level.max_cll, pq_eotf(1.0));
    assert!((level.max_fall - f64::midpoint(10_000.0, stored)).abs() < 1e-9);
}

#[test]
fn hlg_needs_explicit_light_levels() {
    for depth in BitDepth::ALL {
        let f = Format::new(Encoding::HlgNarrow, depth);
        let img = Image::new(2, 2, Rgb::grey(f.white()));
        let err = encode_frame(&img, f, Some(&request(ContentLightLevel::FromFrame))).unwrap_err();
        assert!(matches!(err, OutputError::Unsupported { .. }), "{err}");

        let frame = encode_frame(&img, f, Some(&request(EXPLICIT))).unwrap();
        assert_eq!(clli_of(&frame.bytes), [12_345_678, 4_002_500]);
        let tag = chunks(&frame.bytes)
            .into_iter()
            .find(|c| &c.kind == b"cICP")
            .unwrap();
        assert_eq!(tag.data, [9, 18, 0, 1]);
    }
}
