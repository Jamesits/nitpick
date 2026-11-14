//! Test-support profile adapter over Little CMS.
//!
//! Reads ICC display tags and the Microsoft `MHC2` calibration tag, and
//! returns owned values. Library handles stay inside the adapter. It does
//! not install, assign or apply a profile.
//!
//! Each call opens its own profile handle, so a raw tag read always comes
//! before the cooked read of the same tag.

use std::fmt;

mod display;
mod mhc2;
mod native;

pub use display::{DisplayTags, ParametricCurve};
pub use mhc2::{Channel, Element, MAX_LUT_LEN, Mhc2};

/// Oldest accepted engine. Little CMS added MHC2 support in 2.16 and fixed
/// its MHC2 handling in 2.18.
pub const MIN_ENGINE_VERSION: u32 = 2180;

/// Encoded version of the linked Little CMS engine, for example 2190 for
/// 2.19. Record it with each qualification result.
#[must_use]
pub fn engine_version() -> u32 {
    native::engine_version()
}

/// Tags that this adapter reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Mhc2,
    MediaWhite,
    ChromaticAdaptation,
    RedColorant,
    GreenColorant,
    BlueColorant,
    RedTrc,
    GreenTrc,
    BlueTrc,
    Luminance,
}

impl Tag {
    #[must_use]
    pub fn signature(self) -> [u8; 4] {
        *match self {
            Self::Mhc2 => b"MHC2",
            Self::MediaWhite => b"wtpt",
            Self::ChromaticAdaptation => b"chad",
            Self::RedColorant => b"rXYZ",
            Self::GreenColorant => b"gXYZ",
            Self::BlueColorant => b"bXYZ",
            Self::RedTrc => b"rTRC",
            Self::GreenTrc => b"gTRC",
            Self::BlueTrc => b"bTRC",
            Self::Luminance => b"lumi",
        }
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.signature()))
    }
}

/// Why a tag is malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Defect {
    /// Shorter than its fixed header.
    Truncated,
    /// Wrong tag type signature.
    TypeSignature,
    /// More than [`MAX_LUT_LEN`] LUT entries.
    LutCount,
    /// The element starts inside the tag header or ends after the tag.
    OutOfBounds(Element),
    /// The LUT does not start with `sf32`.
    LutSignature(Channel),
    /// Little CMS cannot read a tag that the adapter does not check itself.
    Unreadable,
}

/// Valid data outside the model that this adapter supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// One LUT entry; its interpolation is not defined.
    SingleEntryLut,
    /// LUT offset 0 with a nonzero LUT count.
    NullLutOffset(Channel),
    /// A tone curve that is not one parametric segment.
    NonParametricCurve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileError {
    /// The linked engine is older than [`MIN_ENGINE_VERSION`].
    Engine(u32),
    /// Little CMS cannot open the profile.
    InvalidProfile,
    MissingTag(Tag),
    MalformedTag(Tag, Defect),
    Unsupported(Tag, Unsupported),
    /// Little CMS cannot read a tag that passed the adapter checks, or reads
    /// other values. The linked build fails qualification for this case.
    Library(Tag),
}

impl ProfileError {
    /// Error kind as in the profile fixture manifest.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Engine(_) => "engine",
            Self::InvalidProfile => "invalid_profile",
            Self::MissingTag(_) => "missing_tag",
            Self::MalformedTag(..) => "malformed_tag",
            Self::Unsupported(..) => "unsupported",
            Self::Library(_) => "library",
        }
    }
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Engine(v) => write!(f, "Little CMS {v} is older than {MIN_ENGINE_VERSION}"),
            Self::InvalidProfile => f.write_str("invalid ICC profile"),
            Self::MissingTag(t) => write!(f, "missing tag {t}"),
            Self::MalformedTag(t, d) => write!(f, "malformed tag {t}: {d:?}"),
            Self::Unsupported(t, u) => write!(f, "unsupported tag {t}: {u:?}"),
            Self::Library(t) => write!(f, "Little CMS read of tag {t} disagrees with its bytes"),
        }
    }
}

impl std::error::Error for ProfileError {}

fn open(icc: &[u8]) -> Result<native::Profile, ProfileError> {
    let v = engine_version();
    if v < MIN_ENGINE_VERSION {
        return Err(ProfileError::Engine(v));
    }
    native::Profile::open(icc).ok_or(ProfileError::InvalidProfile)
}

/// Reads the colour tags of an RGB display profile.
///
/// # Errors
/// [`ProfileError::InvalidProfile`] if Little CMS rejects the profile; a
/// missing required tag; a present tag that Little CMS cannot read; a tone
/// curve that is not parametric.
pub fn read_display(icc: &[u8]) -> Result<DisplayTags, ProfileError> {
    display::read(&open(icc)?)
}

/// Reads the `MHC2` tag. Values are exact: each is a stored signed 15.16
/// number divided by 65536.
///
/// # Errors
/// [`ProfileError::InvalidProfile`] if Little CMS rejects the profile;
/// [`ProfileError::MissingTag`] without an `MHC2` tag, never identity;
/// [`ProfileError::MalformedTag`] or [`ProfileError::Unsupported`] from the
/// raw tag checks; [`ProfileError::Library`] if Little CMS then fails or
/// disagrees.
pub fn read_mhc2(icc: &[u8]) -> Result<Mhc2, ProfileError> {
    let p = open(icc)?;
    if !p.has_tag(Tag::Mhc2) {
        return Err(ProfileError::MissingTag(Tag::Mhc2));
    }
    let raw = p
        .raw_tag(Tag::Mhc2)
        .ok_or(ProfileError::MalformedTag(Tag::Mhc2, Defect::Unreadable))?;
    let values = mhc2::parse(&raw)?;
    match p.mhc2() {
        Some(lib) if mhc2::agrees(&values, &lib) => Ok(values),
        // Little CMS 2.19 fails for count 0 because its zero-size
        // allocation returns null; Microsoft defines count 0 as identity.
        None if values.lut.is_none() => Ok(values),
        _ => Err(ProfileError::Library(Tag::Mhc2)),
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
