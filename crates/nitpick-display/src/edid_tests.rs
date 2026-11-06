//! EDID parser tests. The builders are shared with the Linux backend
//! tests.

use super::{Edid, HdrBlock, HdrStaticMetadata, max_luminance, md5_hex, min_luminance};

/// `DEL`: D = 4, E = 5, L = 12 in 5-bit letters.
pub(crate) const DELL: u16 = (0x04 << 10) | (0x05 << 5) | 0x0c;

pub(crate) struct Spec<'a> {
    pub manufacturer: u16,
    pub product: u16,
    pub serial: u32,
    pub name: Option<&'a str>,
    pub serial_text: Option<&'a str>,
    pub extensions: Vec<[u8; 128]>,
}

impl Default for Spec<'_> {
    fn default() -> Self {
        Self {
            manufacturer: DELL,
            product: 0x40e7,
            serial: 0x0101_0101,
            name: Some("DELL U2417H"),
            serial_text: Some("SN0001"),
            extensions: Vec::new(),
        }
    }
}

fn checksum(block: &mut [u8]) {
    let sum = block[..127].iter().fold(0u8, |s, &b| s.wrapping_add(b));
    block[127] = 0u8.wrapping_sub(sum);
}

fn descriptor(tag: u8, text: &str) -> [u8; 18] {
    let mut d = [0x20; 18];
    d[..5].copy_from_slice(&[0, 0, 0, tag, 0]);
    let n = text.len().min(13);
    d[5..5 + n].copy_from_slice(&text.as_bytes()[..n]);
    if n < 13 {
        d[5 + n] = b'\n';
    }
    d
}

/// A digital-input base block with a detailed timing in the first
/// descriptor, then the name and serial descriptors, then extensions.
pub(crate) fn build(spec: &Spec<'_>) -> Vec<u8> {
    let mut b = [0u8; 128];
    b[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
    b[8..10].copy_from_slice(&spec.manufacturer.to_be_bytes());
    b[10..12].copy_from_slice(&spec.product.to_le_bytes());
    b[12..16].copy_from_slice(&spec.serial.to_le_bytes());
    b[18] = 1;
    b[19] = 4;
    // 148.5 MHz pixel clock: a detailed timing, not a descriptor.
    b[54..56].copy_from_slice(&14850u16.to_le_bytes());
    let mut slot = 72;
    for (tag, text) in [(0xfc, spec.name), (0xff, spec.serial_text)] {
        if let Some(text) = text {
            b[slot..slot + 18].copy_from_slice(&descriptor(tag, text));
            slot += 18;
        }
    }
    // Dummy descriptors fill the rest.
    while slot < 126 {
        b[slot..slot + 18]
            .copy_from_slice(&[0, 0, 0, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        slot += 18;
    }
    b[126] = u8::try_from(spec.extensions.len()).unwrap();
    checksum(&mut b);
    let mut out = b.to_vec();
    for ext in &spec.extensions {
        out.extend_from_slice(ext);
    }
    out
}

/// A CTA-861 revision 3 extension with these data blocks.
pub(crate) fn cta(data_blocks: &[&[u8]]) -> [u8; 128] {
    let mut b = [0u8; 128];
    b[0] = 0x02;
    b[1] = 3;
    let mut i = 4;
    for block in data_blocks {
        b[i..i + block.len()].copy_from_slice(block);
        i += block.len();
    }
    b[2] = u8::try_from(i).unwrap();
    checksum(&mut b);
    b
}

/// An HDR Static Metadata Data Block. `payload`: EOTFs, metadata
/// descriptors, then up to three luminance codes.
pub(crate) fn hdr(payload: &[u8]) -> Vec<u8> {
    let len = u8::try_from(payload.len() + 1).unwrap();
    let mut v = vec![(7 << 5) | len, 6];
    v.extend_from_slice(payload);
    v
}

/// A Colorimetry Data Block (extended tag 5); never the HDR block.
const COLORIMETRY: [u8; 4] = [(7 << 5) | 3, 5, 0xe0, 0x01];
/// A Video Data Block with two VICs.
const VIDEO: [u8; 3] = [(2 << 5) | 2, 16, 4];

fn parse_hdr(extensions: Vec<[u8; 128]>) -> HdrBlock {
    let spec = Spec {
        extensions,
        ..Spec::default()
    };
    Edid::parse(&build(&spec)).unwrap().hdr
}

#[test]
fn identity_and_descriptors() {
    let edid = Edid::parse(&build(&Spec::default())).unwrap();
    assert_eq!(edid.manufacturer, 0x10ac);
    assert_eq!(edid.pnp_id(), "DEL");
    assert_eq!(edid.product, 0x40e7);
    assert_eq!(edid.serial, 0x0101_0101);
    assert_eq!(edid.name.as_deref(), Some("DELL U2417H"));
    assert_eq!(edid.serial_text.as_deref(), Some("SN0001"));
    assert_eq!(edid.hdr, HdrBlock::Absent { displayid: false });
}

#[test]
fn descriptor_text_is_trimmed_and_ascii() {
    // Trailing padding and a byte outside printable ASCII.
    let edid = Edid::parse(&build(&Spec {
        name: Some("Panel\u{7f}      "),
        serial_text: Some("   "),
        ..Spec::default()
    }))
    .unwrap();
    assert_eq!(edid.name.as_deref(), Some("Panel?"));
    assert_eq!(edid.serial_text, None);
}

#[test]
fn missing_descriptors_and_bad_letters() {
    let edid = Edid::parse(&build(&Spec {
        manufacturer: 0,
        name: None,
        serial_text: None,
        ..Spec::default()
    }))
    .unwrap();
    assert_eq!(edid.pnp_id(), "???");
    assert_eq!(edid.name, None);
    assert_eq!(edid.serial_text, None);
}

#[test]
fn rejects_short_or_headerless_blobs() {
    assert!(Edid::parse(&[0; 127]).unwrap_err().contains("127 bytes"));
    let mut bytes = build(&Spec::default());
    bytes[0] = 1;
    assert_eq!(Edid::parse(&bytes).unwrap_err(), "no EDID header");
}

#[test]
fn hdr_static_metadata_with_all_codes() {
    let block = hdr(&[0x0d, 0x01, 96, 64, 51]);
    let found = parse_hdr(vec![cta(&[&VIDEO, &COLORIMETRY, &block])]);
    let HdrBlock::Found(h) = found else {
        panic!("{found:?}")
    };
    assert_eq!(
        h,
        HdrStaticMetadata {
            eotfs: 0x0d,
            max_luminance: Some(96),
            max_frame_average: Some(64),
            min_luminance: Some(51),
        }
    );
    assert!(h.pq());
}

/// The luminance bytes are optional; the block length decides.
#[test]
fn hdr_static_metadata_without_luminance() {
    let found = parse_hdr(vec![cta(&[&hdr(&[0x0f, 0x01])])]);
    let HdrBlock::Found(h) = found else {
        panic!("{found:?}")
    };
    assert_eq!(h.eotfs, 0x0f);
    assert_eq!(
        (h.max_luminance, h.max_frame_average, h.min_luminance),
        (None, None, None)
    );

    let found = parse_hdr(vec![cta(&[&hdr(&[0x03])])]);
    let HdrBlock::Found(h) = found else {
        panic!("{found:?}")
    };
    assert!(!h.pq());
}

#[test]
fn malformed_or_old_cta_blocks_are_not_read() {
    // A data block that runs past the end of the collection.
    let mut overrun = cta(&[&hdr(&[0x04, 0x01, 96])]);
    overrun[2] -= 1;
    assert_eq!(
        parse_hdr(vec![overrun]),
        HdrBlock::Absent { displayid: false }
    );
    // Revision 1 has no data block collection.
    let mut old = cta(&[&hdr(&[0x04])]);
    old[1] = 1;
    assert_eq!(parse_hdr(vec![old]), HdrBlock::Absent { displayid: false });
    // An HDR block with only the extended tag has no EOTF byte.
    assert_eq!(
        parse_hdr(vec![cta(&[&[(7 << 5) | 1, 6]])]),
        HdrBlock::Absent { displayid: false }
    );
}

/// The blob length gives the block count, so a block after the count in
/// byte 126 is still read.
#[test]
fn later_extension_blocks_are_read() {
    let spec = Spec {
        extensions: vec![cta(&[&VIDEO]), cta(&[&hdr(&[0x04, 0x01])])],
        ..Spec::default()
    };
    let mut bytes = build(&spec);
    bytes[126] = 1;
    assert!(matches!(
        Edid::parse(&bytes).unwrap().hdr,
        HdrBlock::Found(_)
    ));
}

#[test]
fn displayid_extension_is_reported() {
    let mut displayid = [0u8; 128];
    displayid[0] = 0x70;
    assert_eq!(
        parse_hdr(vec![cta(&[&VIDEO]), displayid]),
        HdrBlock::Absent { displayid: true }
    );
}

/// 50 × 2^(CV/32) and max × (CV/255)² / 100. Codes that are multiples
/// of 32 give exact powers of two.
#[test]
fn luminance_codes() {
    assert_eq!(max_luminance(0), 50.0);
    assert_eq!(max_luminance(96), 400.0);
    assert_eq!(max_luminance(160), 1600.0);
    assert_eq!(min_luminance(255, 400.0), 4.0);
    assert_eq!(min_luminance(0, 400.0), 0.0);
    assert!((min_luminance(51, 400.0) - 0.16).abs() < 1e-12);
}

/// RFC 1321 test vectors.
#[test]
fn md5_vectors() {
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
}
