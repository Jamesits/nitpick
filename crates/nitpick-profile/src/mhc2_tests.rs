use super::*;
use crate::native::Mhc2Values;

const MATRIX_AT: u32 = 36;

fn put(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_be_bytes());
}

/// A valid tag: matrix entries `k` (as stored integers, row order), then the
/// red, green and blue LUTs with `count` entries each; LUT entry `k` of
/// channel `c` is `100 * c + k`.
fn tag(count: u32) -> Vec<u8> {
    let n = count as usize;
    let mut b = vec![0u8; 36];
    b[..4].copy_from_slice(b"MHC2");
    put(&mut b, 8, count);
    put(&mut b, 12, 3277); // ~0.05
    put(&mut b, 16, 1000 << 16);
    put(&mut b, 20, MATRIX_AT);
    for k in 0..12u32 {
        b.extend_from_slice(&k.to_be_bytes());
    }
    for c in 0..3u32 {
        let at = u32::try_from(b.len()).unwrap();
        put(&mut b, 24 + 4 * c as usize, at);
        b.extend_from_slice(b"sf32\0\0\0\0");
        for k in 0..n {
            b.extend_from_slice(&(100 * c + u32::try_from(k).unwrap()).to_be_bytes());
        }
    }
    b
}

fn malformed(d: Defect) -> Result<Mhc2, ProfileError> {
    Err(ProfileError::MalformedTag(Tag::Mhc2, d))
}

fn unsupported(u: Unsupported) -> Result<Mhc2, ProfileError> {
    Err(ProfileError::Unsupported(Tag::Mhc2, u))
}

fn q(n: i32) -> f64 {
    f64::from(n) / 65536.0
}

#[test]
fn decodes_fields_at_their_offsets() {
    let m = parse(&tag(2)).expect("valid");
    assert_eq!((m.min_nits, m.peak_nits), (q(3277), 1000.0));
    let rows = [0, 1, 2].map(|r| [0, 1, 2, 3].map(|c| q(4 * r + c)));
    assert_eq!(m.matrix, Some(rows));
    let lut = [0, 1, 2].map(|c| vec![q(100 * c), q(100 * c + 1)]);
    assert_eq!(m.lut, Some(lut));
}

#[test]
fn negative_values_are_exact() {
    let mut b = tag(2);
    put(&mut b, 36, (-1i32).cast_unsigned());
    put(&mut b, 40, i32::MIN.cast_unsigned());
    let m = parse(&b).expect("valid").matrix.expect("matrix");
    assert_eq!((m[0][0], m[0][1]), (-1.0 / 65536.0, -32768.0));
}

#[test]
fn element_may_end_at_tag_end() {
    let b = tag(4);
    assert!(parse(&b).is_ok());
    // The blue LUT is last.
    assert_eq!(
        parse(&b[..b.len() - 1]),
        malformed(Defect::OutOfBounds(Element::Lut(Channel::Blue)))
    );
}

#[test]
fn element_inside_header_is_out_of_bounds() {
    let mut b = tag(2);
    put(&mut b, 20, 4);
    assert_eq!(parse(&b), malformed(Defect::OutOfBounds(Element::Matrix)));
    let mut b = tag(2);
    put(&mut b, 28, 35);
    assert_eq!(
        parse(&b),
        malformed(Defect::OutOfBounds(Element::Lut(Channel::Green)))
    );
}

#[test]
fn large_offsets_do_not_wrap() {
    for off in [u32::MAX, u32::MAX - 7, u32::MAX - 47] {
        let mut b = tag(2);
        put(&mut b, 20, off);
        assert_eq!(parse(&b), malformed(Defect::OutOfBounds(Element::Matrix)));
        let mut b = tag(2);
        put(&mut b, 24, off);
        assert_eq!(
            parse(&b),
            malformed(Defect::OutOfBounds(Element::Lut(Channel::Red)))
        );
    }
}

#[test]
fn header_length_boundary() {
    let mut b = tag(0);
    b.truncate(36);
    put(&mut b, 20, 0);
    assert!(parse(&b).is_ok());
    assert_eq!(parse(&b[..35]), malformed(Defect::Truncated));
}

#[test]
fn lut_count_boundary() {
    assert_eq!(
        parse(&tag(4096)).expect("4096").lut.expect("lut")[0].len(),
        4096
    );
    let mut b = tag(4096);
    put(&mut b, 8, 4097);
    assert_eq!(parse(&b), malformed(Defect::LutCount));
}

#[test]
fn count_zero_ignores_lut_offsets() {
    let mut b = tag(2);
    put(&mut b, 8, 0);
    put(&mut b, 24, u32::MAX);
    let m = parse(&b).expect("valid");
    assert_eq!(m.lut, None);
    assert!(m.matrix.is_some());
}

#[test]
fn null_lut_offset_with_entries_is_unsupported() {
    let mut b = tag(2);
    put(&mut b, 28, 0);
    assert_eq!(
        parse(&b),
        unsupported(Unsupported::NullLutOffset(Channel::Green))
    );
}

#[test]
fn structure_is_checked_before_support() {
    let mut b = tag(1);
    assert_eq!(parse(&b), unsupported(Unsupported::SingleEntryLut));
    let at = be_u32(&b, 32) as usize;
    b[at..at + 4].copy_from_slice(b"curv");
    assert_eq!(parse(&b), malformed(Defect::LutSignature(Channel::Blue)));
    let mut b = tag(2);
    put(&mut b, 24, 0);
    put(&mut b, 32, u32::MAX);
    assert_eq!(
        parse(&b),
        malformed(Defect::OutOfBounds(Element::Lut(Channel::Blue)))
    );
}

#[test]
fn agreement_is_exact() {
    let raw = parse(&tag(2)).expect("valid");
    let lib = Mhc2Values {
        min_nits: raw.min_nits,
        peak_nits: raw.peak_nits,
        matrix: raw.matrix.expect("matrix"),
        lut: raw.lut.clone().expect("lut"),
    };
    assert!(agrees(&raw, &lib));
    let mut off = lib.clone();
    off.lut[2][1] += 1.0 / 65536.0;
    assert!(!agrees(&raw, &off));
    let mut short = lib.clone();
    short.lut[0].pop();
    assert!(!agrees(&raw, &short));

    // Little CMS gives identity for matrix offset 0 and empty curves for
    // count 0.
    let none = Mhc2 {
        matrix: None,
        lut: None,
        ..raw
    };
    let identity = Mhc2Values {
        matrix: IDENTITY,
        lut: Default::default(),
        ..lib
    };
    assert!(agrees(&none, &identity));
}
