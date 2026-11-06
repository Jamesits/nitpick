use std::io::Cursor;

use nitpick_core::signal::BitDepth;
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
        let frame = encode_frame(&img, format).unwrap();
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
    let frame = encode_frame(&img, f).unwrap();
    let want = Clipping {
        below_black: 2,
        above_peak: 2,
    };
    assert_eq!(frame.clipping, want);
    assert!(frame.clipping.warning().unwrap().contains('2'));

    let full = Format::new(Encoding::PqFull, BitDepth::Ten);
    let frame = encode_frame(&img, full).unwrap();
    assert!(frame.clipping.is_empty());
    assert_eq!(frame.clipping.warning(), None);
}

#[test]
fn rejects_code_above_depth() {
    let f = Format::new(Encoding::PqFull, BitDepth::Ten);
    let mut img = Image::new(2, 2, Rgb::grey(0));
    img.set(1, 1, Rgb::new(0, 1024, 0));
    let err = encode_frame(&img, f).unwrap_err();
    assert!(err.0.contains("(1, 1)"), "{err}");
}

#[test]
fn rejects_empty_image() {
    let f = Format::new(Encoding::PqFull, BitDepth::Ten);
    assert!(encode_frame(&Image::new(0, 4, Rgb::default()), f).is_err());
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
            let list = chunks(&encode_frame(&img, f).unwrap().bytes);
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

            for other in [b"sRGB", b"gAMA", b"cHRM", b"iCCP", b"PLTE"] {
                assert!(!kinds.contains(&other), "{f}: competing chunk");
            }
            assert_eq!(kinds.last().unwrap(), &b"IEND");
            assert_eq!(list.last().unwrap().data, Vec::<u8>::new());
        }
    }
}
