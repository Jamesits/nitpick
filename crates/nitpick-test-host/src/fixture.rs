//! Frames of the output fixtures for `show`. Both hosts read the same
//! fixture IDs from `tests/fixtures/output`.

use std::collections::HashMap;
use std::io;

use nitpick_core::driver::{ContentLightLevel, Hdr10Request};
use nitpick_core::image::Image;
use nitpick_core::signal::{BitDepth, Encoding, Format, Rgb};
use nitpick_fixtures::{CASE_GROUPS, CaseInput, LightLevelRequest, MetadataRequest};

/// A fixture case as a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FixtureFrame {
    pub format: Format,
    pub size: (usize, usize),
    pub palette: Vec<Rgb>,
    /// Palette index of each pixel, row-major.
    raster: Vec<usize>,
    pub metadata: Option<MetadataRequest>,
}

impl FixtureFrame {
    /// # Errors
    /// The case is invalid under `nitpick_fixtures::validate`.
    pub fn from_case(input: &CaseInput) -> Result<Self, String> {
        nitpick_fixtures::validate(input).map_err(|e| format!("{}: {e}", input.id))?;
        let encoding: Encoding = input.format.encoding.parse()?;
        let depth: BitDepth = input.format.depth.to_string().parse()?;
        let palette = input
            .palette
            .iter()
            .map(|e| {
                let [r, g, b] = e.codes.map(u16::try_from);
                Ok(Rgb::new(r?, g?, b?))
            })
            .collect::<Result<Vec<_>, std::num::TryFromIntError>>()
            .map_err(|e| format!("{}: {e}", input.id))?;
        Ok(Self {
            format: Format::new(encoding, depth),
            size: (input.size[0], input.size[1]),
            palette,
            raster: nitpick_fixtures::raster(input),
            metadata: input.metadata.clone(),
        })
    }

    /// The palette index at `(x, y)` of the fixture.
    ///
    /// # Panics
    /// The position is outside the fixture.
    #[must_use]
    pub fn index(&self, x: usize, y: usize) -> usize {
        assert!(x < self.size.0 && y < self.size.1, "inside the fixture");
        self.raster[y * self.size.0 + x]
    }

    /// The frame at the surface size: the fixture at the top-left corner,
    /// nominal black of its format elsewhere.
    ///
    /// **Deviation from the fixture:** automatic light levels of the
    /// result include the black area, so they differ from the expected
    /// values of the fixture unless the sizes are equal.
    ///
    /// # Errors
    /// The fixture is larger than `surface`.
    pub fn render(&self, surface: (usize, usize)) -> Result<Image, String> {
        let (w, h) = self.size;
        if w > surface.0 || h > surface.1 {
            return Err(format!(
                "the fixture is {w}×{h} px; the surface is {}×{} px",
                surface.0, surface.1
            ));
        }
        let mut image = Image::new(surface.0, surface.1, Rgb::grey(self.format.black()));
        for (i, &p) in self.raster.iter().enumerate() {
            image.set(i % w, i / w, self.palette[p]);
        }
        Ok(image)
    }
}

/// The request of the fixture or of a `show` as a driver request.
#[must_use]
pub fn hdr10_request(md: &MetadataRequest) -> Hdr10Request {
    Hdr10Request {
        primaries: md.primaries,
        white: md.white,
        max_mastering_nits: md.max_mastering_nits,
        min_mastering_nits: md.min_mastering_nits,
        light_level: match md.light_level {
            LightLevelRequest::FromFrame => ContentLightLevel::FromFrame,
            LightLevelRequest::Explicit { max_cll, max_fall } => {
                ContentLightLevel::Explicit { max_cll, max_fall }
            }
        },
    }
}

/// The valid cases of all fixture groups, by ID.
#[derive(Debug, Default)]
pub struct Fixtures {
    frames: HashMap<String, FixtureFrame>,
}

impl Fixtures {
    /// Reads every case group.
    ///
    /// # Errors
    /// A file is missing or invalid, or a case ID repeats.
    pub fn load() -> io::Result<Self> {
        let mut fixtures = Self::default();
        for group in CASE_GROUPS {
            for case in nitpick_fixtures::group(group)?.cases {
                fixtures
                    .insert(&case.input)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            }
        }
        Ok(fixtures)
    }

    /// # Errors
    /// The case is invalid, or its ID exists.
    pub fn insert(&mut self, input: &CaseInput) -> Result<(), String> {
        let frame = FixtureFrame::from_case(input)?;
        if self.frames.insert(input.id.clone(), frame).is_some() {
            return Err(format!("case {} repeats", input.id));
        }
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&FixtureFrame> {
        self.frames.get(id)
    }
}

#[cfg(test)]
#[path = "fixture_tests.rs"]
mod tests;
