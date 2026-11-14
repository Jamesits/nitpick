//! Standard colour tags of an RGB display profile.

use crate::native::{Curve, Profile};
use crate::{Defect, ProfileError, Tag, Unsupported};

/// Colour tags of an RGB display profile, as stored. No tag is adapted or
/// normalized.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayTags {
    /// `wtpt`.
    pub media_white: [f64; 3],
    /// `chad`, rows. `None` if the profile has no `chad` tag.
    pub chad: Option<[[f64; 3]; 3]>,
    /// `rXYZ`, `gXYZ` and `bXYZ`: one XYZ triple per primary.
    pub colorants: [[f64; 3]; 3],
    /// `rTRC`, `gTRC` and `bTRC`.
    pub trc: [ParametricCurve; 3],
    /// Y of `lumi` in cd/m². `None` if the profile has no `lumi` tag.
    pub lumi: Option<f64>,
}

/// An ICC `parametricCurveType` curve.
///
/// Little CMS reads a `curv` tag with zero or one entry as a gamma curve;
/// this adapter then reports function 0, the same as for `para`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParametricCurve {
    /// ICC function number, 0 to 4.
    pub function: u16,
    /// The parameters of that function, in ICC order.
    pub params: Vec<f64>,
}

/// Parameter count of each ICC function (ICC.1:2010 10.18).
const PARAM_COUNT: [usize; 5] = [1, 3, 4, 5, 7];

fn optional<T>(
    p: &Profile,
    tag: Tag,
    read: impl FnOnce(&Profile) -> Option<T>,
) -> Result<Option<T>, ProfileError> {
    if !p.has_tag(tag) {
        return Ok(None);
    }
    read(p)
        .map(Some)
        .ok_or(ProfileError::MalformedTag(tag, Defect::Unreadable))
}

fn required<T>(
    p: &Profile,
    tag: Tag,
    read: impl FnOnce(&Profile) -> Option<T>,
) -> Result<T, ProfileError> {
    optional(p, tag, read)?.ok_or(ProfileError::MissingTag(tag))
}

fn curve(p: &Profile, tag: Tag) -> Result<ParametricCurve, ProfileError> {
    let unsupported = ProfileError::Unsupported(tag, Unsupported::NonParametricCurve);
    match required(p, tag, |p| p.curve(tag))? {
        Curve::Parametric { kind, params } => {
            let function = u16::try_from(kind - 1).map_err(|_| unsupported)?;
            let n = *PARAM_COUNT.get(usize::from(function)).ok_or(unsupported)?;
            Ok(ParametricCurve {
                function,
                params: params[..n].to_vec(),
            })
        }
        Curve::Other => Err(unsupported),
    }
}

pub(crate) fn read(p: &Profile) -> Result<DisplayTags, ProfileError> {
    let xyz = |tag| required(p, tag, |p| p.xyz(tag));
    Ok(DisplayTags {
        media_white: xyz(Tag::MediaWhite)?,
        chad: optional(p, Tag::ChromaticAdaptation, Profile::chad)?,
        colorants: [
            xyz(Tag::RedColorant)?,
            xyz(Tag::GreenColorant)?,
            xyz(Tag::BlueColorant)?,
        ],
        trc: [
            curve(p, Tag::RedTrc)?,
            curve(p, Tag::GreenTrc)?,
            curve(p, Tag::BlueTrc)?,
        ],
        lumi: optional(p, Tag::Luminance, |p| p.xyz(Tag::Luminance))?.map(|v| v[1]),
    })
}
