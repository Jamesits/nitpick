//! Pattern catalogue and the reference value table.

use std::fmt::{self, Write as _};
use std::str::FromStr;

use crate::driver::{Conversion, OutputLimits};
use crate::image::Image;
use crate::output::scrgb_is_exact;
use crate::pattern::bt2111::{self, SizeClass};
use crate::pattern::grayscale;
use crate::pattern::model::PatternModel;
use crate::signal::{Encoding, Format, Rgb, ycbcr_narrow};

/// Smallest supported output. Below it, label text and narrow regions collapse.
pub const MIN_SIZE: (usize, usize) = (640, 360);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    UnsupportedEncoding {
        pattern: PatternKind,
        encoding: Encoding,
    },
    TooSmall {
        width: usize,
        height: usize,
    },
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedEncoding { pattern, encoding } => {
                write!(f, "{} does not support {encoding}", pattern.label())
            }
            Self::TooSmall { width, height } => write!(
                f,
                "output {width}x{height} is smaller than {}x{}",
                MIN_SIZE.0, MIN_SIZE.1
            ),
        }
    }
}

impl std::error::Error for PatternError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatternKind {
    /// ITU-R BT.2111-3 HDR colour bars.
    ColourBars,
    /// PQ luminance steps, near-black steps and a ramp.
    Grayscale,
}

impl PatternKind {
    pub const ALL: [Self; 2] = [Self::ColourBars, Self::Grayscale];

    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::ColourBars => "bt2111",
            Self::Grayscale => "pq-grayscale",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ColourBars => "BT.2111-3 HDR colour bars",
            Self::Grayscale => "PQ grayscale / brightness",
        }
    }

    #[must_use]
    pub fn description(self) -> &'static str {
        match self {
            Self::ColourBars => {
                "ITU-R BT.2111-3 colour bars: 100% and 75%/58% BT.2020 bars, BT.709 bars, \
                 stair, ramp and PLUGE. For vectorscope and waveform checks."
            }
            Self::Grayscale => {
                "Absolute-luminance PQ patches (0 to 10000 cd/m2), near-black code steps and \
                 a full code ramp. Every patch is neutral: the vectorscope shows one dot."
            }
        }
    }

    #[must_use]
    pub fn encodings(self) -> &'static [Encoding] {
        match self {
            Self::ColourBars => &Encoding::ALL,
            // Absolute luminance has no meaning in HLG without a display OOTF.
            Self::Grayscale => &[Encoding::PqNarrow, Encoding::PqFull],
        }
    }

    fn check(self, encoding: Encoding) -> Result<(), PatternError> {
        if self.encodings().contains(&encoding) {
            Ok(())
        } else {
            Err(PatternError::UnsupportedEncoding {
                pattern: self,
                encoding,
            })
        }
    }

    /// # Errors
    /// If the encoding is not supported or the size is below [`MIN_SIZE`].
    pub fn render(
        self,
        format: Format,
        width: usize,
        height: usize,
    ) -> Result<Image, PatternError> {
        let model = self.model(format, (width, height))?;
        if width < MIN_SIZE.0 || height < MIN_SIZE.1 {
            return Err(PatternError::TooSmall { width, height });
        }
        Ok(model.render(width, height))
    }

    /// The model of the pattern for an output of `size` pixels. The model is
    /// resolution independent, but BT.2111 defines a different 12-bit ramp
    /// for each of its picture sizes (see [`SizeClass::for_output`]).
    ///
    /// # Errors
    /// If the encoding is not supported.
    pub fn model(self, format: Format, size: (usize, usize)) -> Result<PatternModel, PatternError> {
        self.check(format.encoding)?;
        Ok(match self {
            Self::ColourBars => bt2111::model(format, SizeClass::for_output(size.0, size.1)),
            Self::Grayscale => grayscale::model(format),
        })
    }

    /// Text table of every flat level, grouped by section, with its R'G'B'
    /// codes and the Y'Cb'Cr' codes a vectorscope reads, for an output of
    /// `size` pixels. `limits` gives the output path, its warnings and the
    /// per-level marks of its conversion (see [`Conversion`]).
    ///
    /// # Errors
    /// If the encoding is not supported.
    pub fn reference_table(
        self,
        format: Format,
        size: (usize, usize),
        limits: &OutputLimits,
    ) -> Result<String, PatternError> {
        let model = self.model(format, size)?;
        let depth = format.depth;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{} - {}, {depth} R'G'B', {}x{}",
            self.label(),
            format.encoding,
            size.0,
            size.1
        );
        let _ = writeln!(
            s,
            "Y'Cb'Cr': BT.2020 non-constant luminance, {depth} narrow range{}.",
            if format.is_narrow() {
                ""
            } else {
                ", as a GPU outputs it in YCbCr mode"
            }
        );
        let _ = match &limits.path {
            Some(path) => writeln!(s, "Output: {}.", path.description),
            None => writeln!(
                s,
                "Output: unavailable. No claim that these codes reach the display."
            ),
        };
        let conversion = limits.path.as_ref().map(|p| p.conversion);
        for warning in &limits.warnings {
            let _ = writeln!(s, "Warning: {warning}");
        }
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "{:<22} {:>5} {:>5} {:>5}   {:>5} {:>5} {:>5}",
            "patch", "R'", "G'", "B'", "Y'", "Cb", "Cr"
        );
        let mut marked = false;
        let mut section = "";
        for level in model.levels() {
            if level.section != section {
                section = level.section;
                let _ = writeln!(
                    s,
                    "
[{section}]"
                );
            }
            let (rgb, c) = (level.rgb, ycbcr_narrow(level.rgb, format));
            let mark = conversion.and_then(|k| level_mark(k, rgb, format));
            marked |= mark.is_some();
            let _ = writeln!(
                s,
                "{:<22} {:>5} {:>5} {:>5}   {:>5} {:>5} {:>5}{}",
                level.name,
                rgb.r,
                rgb.g,
                rgb.b,
                c.y,
                c.cb,
                c.cr,
                mark.map(|m| format!(" {m}")).unwrap_or_default()
            );
        }
        if let (true, Some(k)) = (marked, conversion) {
            let _ = writeln!(
                s,
                "
{}",
                mark_note(k)
            );
        }
        Ok(s)
    }
}

/// Mark of a level that the conversion cannot carry exactly.
fn level_mark(conversion: Conversion, rgb: Rgb, format: Format) -> Option<&'static str> {
    match conversion {
        Conversion::Direct => None,
        Conversion::ScRgb80 => (!scrgb_is_exact(rgb, format.depth)).then_some("*"),
        Conversion::Png16 => {
            let nominal = [rgb.r, rgb.g, rgb.b]
                .into_iter()
                .all(|v| format.is_nominal(v));
            (!nominal).then_some("clipped")
        }
    }
}

fn mark_note(conversion: Conversion) -> &'static str {
    match conversion {
        Conversion::Direct => "",
        Conversion::ScRgb80 => {
            "* The FP16 scRGB swap chain cannot carry these codes exactly: a component              near black next to bright components moves by one or more codes."
        }
        Conversion::Png16 => {
            "clipped: a component is below 0 % or above 100 % signal. The full-range PNG              stores it at the nominal limit, so the level does not test that excursion."
        }
    }
}

impl FromStr for PatternKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.id() == s)
            .ok_or_else(|| format!("unknown pattern `{s}`"))
    }
}

#[cfg(test)]
#[path = "catalogue_tests.rs"]
mod tests;
