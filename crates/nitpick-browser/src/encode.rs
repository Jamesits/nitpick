//! Conversion of pattern codes to 16-bit PNG samples and the PNG encoding
//! with its cICP tag and HDR10 metadata chunks. See "Code to PNG sample" in
//! `doc/design/output_driver/browser.md`.

use std::fmt;

use nitpick_core::driver::{ContentLightLevel, Hdr10Request, OutputError};
use nitpick_core::image::Image;
use nitpick_core::metadata::{self, Hdr10Metadata, LightLevel};
use nitpick_core::output::{CodeRangeError, check_codes};
use nitpick_core::signal::{Encoding, Format, Rgb};
use png::chunk;

/// cICP values (ITU-T H.273): colour primaries, transfer characteristics,
/// matrix coefficients and the video full-range flag. All encodings store
/// full-range RGB samples, because Skia ignores cICP when the full-range
/// flag is zero.
#[must_use]
pub fn cicp(encoding: Encoding) -> [u8; 4] {
    // 9: BT.2020 primaries. 16: PQ. 18: HLG. 0: RGB (identity matrix).
    let transfer = if encoding.is_pq() { 16 } else { 18 };
    [9, transfer, 0, 1]
}

/// PNG `mDCV` chunk data (PNG 3rd edition, 11.3.2.7): primaries and white
/// point as x, y pairs of 16-bit values in 0.00002 units, then maximum and
/// minimum luminance as 32-bit values in 0.0001 cd/m².
///
/// The primary order differs from the SEI payload in
/// [`Hdr10Metadata::mastering_display_bytes`] (green, blue, red). PNG orders
/// by the largest x, then the largest y, which is red, green, blue for both
/// mastering presets.
#[must_use]
pub fn mdcv(md: &Hdr10Metadata) -> [u8; 24] {
    let [r, g, b] = md.primaries;
    let xy = [r, g, b, md.white]
        .into_iter()
        .flatten()
        .flat_map(|v| Hdr10Metadata::chromaticity_code(v).to_be_bytes());
    let luminance = [md.max_mastering_nits, md.min_mastering_nits]
        .into_iter()
        .flat_map(|v| Hdr10Metadata::luminance_code(v).to_be_bytes());
    let mut out = [0; 24];
    for (o, v) in out.iter_mut().zip(xy.chain(luminance)) {
        *o = v;
    }
    out
}

/// PNG `cLLI` chunk data (PNG 3rd edition, 11.3.2.8): `MaxCLL`, then
/// `MaxFALL`, as 32-bit values in 0.0001 cd/m². Not the two 16-bit whole
/// cd/m² fields of the CTA-861.3 SEI payload.
///
/// PNG reads 0 as "unknown". An all-black frame gives 0 from the
/// calculation, so its chunk states "unknown".
#[must_use]
pub fn clli(md: &Hdr10Metadata) -> [u8; 8] {
    let mut out = [0; 8];
    let (cll, fall) = out.split_at_mut(4);
    cll.copy_from_slice(&Hdr10Metadata::luminance_code(md.max_cll).to_be_bytes());
    fall.copy_from_slice(&Hdr10Metadata::luminance_code(md.max_fall).to_be_bytes());
    out
}

/// Clip state of one component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Clip {
    None,
    /// Below narrow-range black.
    Below,
    /// Above narrow-range nominal peak.
    Above,
}

/// PNG sample $V = \mathrm{round}(65535 \cdot \min(1, \max(0, E')))$ of a
/// code. The caller checks that `code` does not exceed `format.max_code()`.
///
/// Integer arithmetic with round half up: f64 can round exact halves down,
/// for example 12-bit narrow code 840 gives 10922.5 and must store 10923.
#[must_use]
pub fn png_sample(code: u16, format: Format) -> (u16, Clip) {
    let (offset, range) = if format.is_narrow() {
        let s = 1u64 << (format.depth.bits() - 8);
        (16 * s, 219 * s)
    } else {
        (0, u64::from(format.max_code()))
    };
    let code = u64::from(code);
    if code < offset {
        return (0, Clip::Below);
    }
    if code > offset + range {
        return (u16::MAX, Clip::Above);
    }
    let v = (2 * u64::from(u16::MAX) * (code - offset) + range) / (2 * range);
    // `code - offset <= range`, so `v <= 65535`.
    #[allow(clippy::cast_possible_truncation)]
    (v as u16, Clip::None)
}

/// Pixels with at least one clipped component. A pixel with components
/// below black and above peak counts in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clipping {
    pub below_black: usize,
    pub above_peak: usize,
}

impl Clipping {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.below_black == 0 && self.above_peak == 0
    }

    /// Status text; `None` if no sample clipped.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        (!self.is_empty()).then(|| {
            format!(
                "PNG clipped {} pixels below black and {} pixels above nominal peak",
                self.below_black, self.above_peak
            )
        })
    }
}

/// Stored PNG samples of one frame, before encoding. Automatic light levels
/// use these samples, not the input codes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PngSamples {
    width: usize,
    height: usize,
    format: Format,
    /// Row-major, interleaved R, G, B.
    samples: Vec<u16>,
    clipping: Clipping,
}

impl PngSamples {
    /// # Errors
    /// If a code exceeds the maximum code of `format.depth`; clamping would
    /// store a different level without notice.
    pub fn from_image(image: &Image, format: Format) -> Result<Self, CodeRangeError> {
        check_codes(image, format.depth)?;
        let mut clipping = Clipping::default();
        let mut samples = Vec::with_capacity(image.pixels().len() * 3);
        for &Rgb { r, g, b } in image.pixels() {
            let mut clips = [Clip::None; 3];
            for (code, clip) in [r, g, b].into_iter().zip(&mut clips) {
                let (v, c) = png_sample(code, format);
                samples.push(v);
                *clip = c;
            }
            clipping.below_black += usize::from(clips.contains(&Clip::Below));
            clipping.above_peak += usize::from(clips.contains(&Clip::Above));
        }
        Ok(Self {
            width: image.width(),
            height: image.height(),
            format,
            samples,
            clipping,
        })
    }

    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    #[must_use]
    pub fn format(&self) -> Format {
        self.format
    }

    /// Row-major, interleaved R, G, B.
    #[must_use]
    pub fn samples(&self) -> &[u16] {
        &self.samples
    }

    #[must_use]
    pub fn clipping(&self) -> Clipping {
        self.clipping
    }

    /// `MaxCLL` and `MaxFALL` of the stored samples: $V / 65535$ is the PQ
    /// signal after clipping and rounding, not the input code.
    ///
    /// # Errors
    /// `Unsupported` for HLG: absolute light levels need a display and
    /// viewing model.
    pub fn light_level(&self) -> Result<LightLevel, OutputError> {
        if !self.format.encoding.is_pq() {
            return Err(OutputError::Unsupported {
                feature: "HDR10 metadata automatic light level".into(),
                reason: "HLG output has no absolute light level; enter explicit values".into(),
            });
        }
        let max = f64::from(u16::MAX);
        Ok(metadata::light_level(
            self.samples
                .as_chunks::<3>()
                .0
                .iter()
                .map(|px| px.map(|v| f64::from(v) / max)),
        ))
    }

    /// Resolves `request` for these samples. Explicit light levels bypass
    /// the calculation for every encoding.
    ///
    /// # Errors
    /// See [`Self::light_level`]; only for automatic light levels.
    pub fn resolve(&self, request: &Hdr10Request) -> Result<Hdr10Metadata, OutputError> {
        let level = match request.light_level {
            ContentLightLevel::FromFrame => Some(self.light_level()?),
            ContentLightLevel::Explicit { .. } => None,
        };
        // `resolve` calls the closure only for `FromFrame`, where `level`
        // is set.
        Ok(request.resolve(|| {
            level.unwrap_or(LightLevel {
                max_cll: 0.0,
                max_fall: 0.0,
            })
        }))
    }

    /// 16-bit RGB PNG with only the cICP colour tag: no sRGB, gAMA, cHRM
    /// or iCCP chunk can compete with it. With `metadata`, it also writes
    /// `mDCV` and `cLLI`; without, it writes neither.
    ///
    /// # Errors
    /// An empty image, a dimension above `u32`, or an encoder failure.
    pub fn encode(&self, metadata: Option<&Hdr10Metadata>) -> Result<EncodedFrame, EncodeError> {
        let (Ok(w), Ok(h)) = (u32::try_from(self.width), u32::try_from(self.height)) else {
            return Err(EncodeError(format!(
                "image size {}×{} exceeds the PNG limit",
                self.width, self.height
            )));
        };
        let data: Vec<u8> = self.samples.iter().flat_map(|v| v.to_be_bytes()).collect();
        let mut bytes = Vec::new();
        let mut encoder = png::Encoder::new(&mut bytes, w, h);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Sixteen);
        let mut writer = encoder.write_header()?;
        // cICP must precede PLTE and IDAT.
        // `png` 0.18.1 decodes cICP, but its encoder does not write it.
        writer.write_chunk(chunk::cICP, &cicp(self.format.encoding))?;
        // mDCV and cLLI must also precede PLTE and IDAT, and the encoder
        // does not write them either.
        if let Some(md) = metadata {
            writer.write_chunk(chunk::mDCV, &mdcv(md))?;
            writer.write_chunk(chunk::cLLI, &clli(md))?;
        }
        writer.write_image_data(&data)?;
        writer.finish()?;
        Ok(EncodedFrame {
            bytes,
            size: (self.width, self.height),
            clipping: self.clipping,
            metadata: metadata.copied(),
        })
    }
}

/// An encoded PNG, the clipping of its samples and the metadata in its
/// chunks.
#[derive(Debug, Clone, PartialEq)]
pub struct EncodedFrame {
    pub bytes: Vec<u8>,
    pub size: (usize, usize),
    pub clipping: Clipping,
    pub metadata: Option<Hdr10Metadata>,
}

/// Converts one frame, resolves `metadata` from the stored samples and
/// encodes it.
///
/// # Errors
/// `Unsupported` from [`PngSamples::resolve`]; `Platform` for the errors of
/// [`PngSamples::from_image`] and [`PngSamples::encode`].
pub fn encode_frame(
    image: &Image,
    format: Format,
    metadata: Option<&Hdr10Request>,
) -> Result<EncodedFrame, OutputError> {
    let samples = PngSamples::from_image(image, format).map_err(EncodeError::from)?;
    let resolved = metadata.map(|r| samples.resolve(r)).transpose()?;
    Ok(samples.encode(resolved.as_ref())?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(pub String);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PNG encoding failed: {}", self.0)
    }
}

impl std::error::Error for EncodeError {}

impl From<CodeRangeError> for EncodeError {
    fn from(e: CodeRangeError) -> Self {
        Self(e.to_string())
    }
}

impl From<EncodeError> for OutputError {
    fn from(e: EncodeError) -> Self {
        Self::Platform(e.to_string())
    }
}

impl From<png::EncodingError> for EncodeError {
    fn from(e: png::EncodingError) -> Self {
        Self(e.to_string())
    }
}

#[cfg(test)]
#[path = "encode_tests.rs"]
mod tests;
