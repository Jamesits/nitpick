//! Pattern catalogue and the reference value table.

use std::fmt::{self, Write as _};
use std::str::FromStr;

use crate::image::Image;
use crate::pattern::model::PatternModel;
use crate::pattern::{bt2111, grayscale};
use crate::signal::{Encoding, ycbcr_narrow};

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
        encoding: Encoding,
        width: usize,
        height: usize,
    ) -> Result<Image, PatternError> {
        let model = self.model(encoding)?;
        if width < MIN_SIZE.0 || height < MIN_SIZE.1 {
            return Err(PatternError::TooSmall { width, height });
        }
        Ok(model.render(width, height))
    }

    /// The resolution-independent model of the pattern.
    ///
    /// # Errors
    /// If the encoding is not supported.
    pub fn model(self, encoding: Encoding) -> Result<PatternModel, PatternError> {
        self.check(encoding)?;
        Ok(match self {
            Self::ColourBars => bt2111::model(encoding),
            Self::Grayscale => grayscale::model(encoding),
        })
    }

    /// Text table of every flat level, grouped by section, with its R'G'B'
    /// codes and the Y'Cb'Cr' codes a vectorscope reads.
    ///
    /// # Errors
    /// If the encoding is not supported.
    pub fn reference_table(self, encoding: Encoding) -> Result<String, PatternError> {
        let model = self.model(encoding)?;
        let mut s = String::new();
        let _ = writeln!(s, "{} - {encoding}, 10-bit R'G'B'", self.label());
        let _ = writeln!(
            s,
            "Y'Cb'Cr': BT.2020 non-constant luminance, 10-bit narrow range{}.",
            if encoding.is_narrow() {
                ""
            } else {
                ", as a GPU outputs it in YCbCr mode"
            }
        );
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "{:<22} {:>5} {:>5} {:>5}   {:>5} {:>5} {:>5}",
            "patch", "R'", "G'", "B'", "Y'", "Cb", "Cr"
        );
        let mut section = "";
        for level in model.levels() {
            if level.section != section {
                section = level.section;
                let _ = writeln!(s, "\n[{section}]");
            }
            let (rgb, c) = (level.rgb, ycbcr_narrow(level.rgb, encoding));
            let _ = writeln!(
                s,
                "{:<22} {:>5} {:>5} {:>5}   {:>5} {:>5} {:>5}",
                level.name, rgb.r, rgb.g, rgb.b, c.y, c.cb, c.cr
            );
        }
        Ok(s)
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
