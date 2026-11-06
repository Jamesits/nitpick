//! The EDID fields that the Linux backend reads: identity, product name
//! and the CTA-861 HDR Static Metadata Data Block.
//!
//! Layouts are from VESA E-EDID 1.4 and CTA-861. Other blocks are not
//! parsed. A `DisplayID` extension is detected but not read.

use std::fmt::Write as _;

/// Bytes 0..8 of the base block.
const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
const BLOCK: usize = 128;
/// First of the four 18-byte descriptors in the base block.
const DESCRIPTORS: usize = 54;
const DESCRIPTOR_LEN: usize = 18;
const TAG_PRODUCT_NAME: u8 = 0xfc;
const TAG_SERIAL: u8 = 0xff;
/// Extension block tags.
const EXT_CTA: u8 = 0x02;
const EXT_DISPLAYID: u8 = 0x70;
/// CTA data block tag "use extended tag" and the extended tag of the HDR
/// Static Metadata Data Block.
const CTA_EXTENDED: u8 = 7;
const CTA_EXT_HDR_STATIC: u8 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edid {
    /// Bytes 8–9, big-endian: three 5-bit letters. The same value as the
    /// Windows `edidManufactureId`.
    pub manufacturer: u16,
    /// Bytes 10–11, little-endian.
    pub product: u16,
    /// Bytes 12–15, little-endian. 0 if not given. Many displays put a
    /// fixed value here and the unit serial in [`Self::serial_text`].
    pub serial: u32,
    /// Display product name descriptor.
    pub name: Option<String>,
    /// Display product serial number descriptor.
    pub serial_text: Option<String>,
    pub hdr: HdrBlock,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HdrBlock {
    Found(HdrStaticMetadata),
    /// No CTA-861 extension has the block. `displayid`: a `DisplayID`
    /// extension exists, which can report HDR in a form this parser does
    /// not read.
    Absent {
        displayid: bool,
    },
}

/// Raw code values of the block. The luminance bytes are optional: the
/// block length decides which are present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HdrStaticMetadata {
    /// Supported EOTFs; see [`Self::PQ`].
    pub eotfs: u8,
    /// Desired content max luminance.
    pub max_luminance: Option<u8>,
    /// Desired content max frame-average luminance.
    pub max_frame_average: Option<u8>,
    /// Desired content min luminance.
    pub min_luminance: Option<u8>,
}

impl HdrStaticMetadata {
    /// EOTF bit of SMPTE ST 2084. Bit 0 is traditional SDR gamma, bit 1
    /// traditional HDR gamma, bit 3 HLG.
    pub const PQ: u8 = 1 << 2;

    #[must_use]
    pub fn pq(&self) -> bool {
        self.eotfs & Self::PQ != 0
    }
}

/// cd/m² of a max or max frame-average luminance code: 50 × 2^(CV/32).
#[must_use]
pub fn max_luminance(cv: u8) -> f64 {
    50.0 * (f64::from(cv) / 32.0).exp2()
}

/// cd/m² of a min luminance code: max × (CV/255)² / 100.
#[must_use]
pub fn min_luminance(cv: u8, max: f64) -> f64 {
    let ratio = f64::from(cv) / 255.0;
    max * ratio * ratio / 100.0
}

impl Edid {
    /// Checksums are not checked: the kernel drops blocks with a bad
    /// checksum before it exposes the EDID.
    ///
    /// # Errors
    /// The reason, for a blob shorter than one block or without the
    /// header.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let base = bytes
            .get(..BLOCK)
            .ok_or_else(|| format!("{} bytes, shorter than one EDID block", bytes.len()))?;
        if base[..8] != HEADER {
            return Err("no EDID header".into());
        }
        let mut name = None;
        let mut serial_text = None;
        for d in base[DESCRIPTORS..DESCRIPTORS + 4 * DESCRIPTOR_LEN]
            .as_chunks::<DESCRIPTOR_LEN>()
            .0
        {
            // A pixel clock of 0 marks a display descriptor instead of a
            // detailed timing.
            if d[..3] != [0, 0, 0] {
                continue;
            }
            match d[3] {
                TAG_PRODUCT_NAME => name = name.or_else(|| text(&d[5..])),
                TAG_SERIAL => serial_text = serial_text.or_else(|| text(&d[5..])),
                _ => {}
            }
        }
        Ok(Self {
            manufacturer: u16::from_be_bytes([base[8], base[9]]),
            product: u16::from_le_bytes([base[10], base[11]]),
            serial: u32::from_le_bytes([base[12], base[13], base[14], base[15]]),
            name,
            serial_text,
            // The blob length, not byte 126, gives the block count: with
            // an HDMI EDID Extension Override, byte 126 understates it.
            hdr: hdr_block(&bytes[BLOCK..]),
        })
    }

    /// Three-letter PNP ID, for example `DEL`. `?` for a letter out of
    /// range.
    #[must_use]
    pub fn pnp_id(&self) -> String {
        [10, 5, 0]
            .iter()
            .map(|shift| match (self.manufacturer >> shift) & 0x1f {
                // 1 is 'A'.
                n @ 1..=26 => char::from(b'A' + u8::try_from(n - 1).unwrap_or(0)),
                _ => '?',
            })
            .collect()
    }
}

/// Descriptor text: up to 13 bytes, ended by a line feed and padded with
/// spaces. The code page is 437; bytes outside printable ASCII become `?`.
fn text(bytes: &[u8]) -> Option<String> {
    let end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let s: String = bytes[..end]
        .iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect();
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

fn hdr_block(extensions: &[u8]) -> HdrBlock {
    let mut displayid = false;
    for block in extensions.as_chunks::<BLOCK>().0 {
        match block[0] {
            EXT_CTA => {
                if let Some(hdr) = cta_hdr_static(block) {
                    return HdrBlock::Found(hdr);
                }
            }
            EXT_DISPLAYID => displayid = true,
            _ => {}
        }
    }
    HdrBlock::Absent { displayid }
}

/// Data blocks are at bytes 4..d of revision 3 and later, where byte 2
/// is d. Byte 127 is the checksum.
fn cta_hdr_static(block: &[u8]) -> Option<HdrStaticMetadata> {
    if block[1] < 3 {
        return None;
    }
    let end = usize::from(block[2]).min(BLOCK - 1);
    let mut i = 4;
    while i < end {
        let tag = block[i] >> 5;
        let len = usize::from(block[i] & 0x1f);
        let data = block
            .get(i + 1..i + 1 + len)
            .filter(|_| i + 1 + len <= end)?;
        // data[0]: extended tag; [1] EOTFs; [2] metadata descriptors;
        // [3..6] luminance codes.
        if tag == CTA_EXTENDED && len >= 2 && data[0] == CTA_EXT_HDR_STATIC {
            return Some(HdrStaticMetadata {
                eotfs: data[1],
                max_luminance: data.get(3).copied(),
                max_frame_average: data.get(4).copied(),
                min_luminance: data.get(5).copied(),
            });
        }
        i += 1 + len;
    }
    None
}

/// Lower-case hex MD5 of the EDID bytes. colord stores this value in the
/// `OutputEdidMd5` metadata of a display device.
#[must_use]
pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    Md5::digest(bytes)
        .iter()
        .fold(String::with_capacity(32), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
#[path = "edid_tests.rs"]
pub(crate) mod tests;
