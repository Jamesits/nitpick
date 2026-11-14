//! Conventional ICC profile-to-profile transforms.
//!
//! This is ordinary ICC colour management only. It does not apply an `MHC2`
//! calibration or a `vcgt` table, and it is not a reference for expected
//! values: Little CMS stores values between pipeline stages as f32.

use crate::native::{self, Profile};
use crate::{MIN_ENGINE_VERSION, ProfileError, engine_version};

/// One end of a transform.
#[derive(Debug, Clone, Copy)]
pub enum Endpoint<'a> {
    /// ICC profile bytes.
    Profile(&'a [u8]),
    /// The Little CMS built-in XYZ identity profile: D50 PCS XYZ, Y = 1 for
    /// the PCS white. Its `wtpt` is the unrounded D50 illuminant, and its
    /// `chad` is identity.
    PcsXyz,
}

/// Pixel layout: three interleaved f64 channels, unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Device RGB, 1.0 for full scale.
    RgbF64,
    /// XYZ, 1.0 for Y of the PCS white.
    XyzF64,
}

/// ICC.1:2010 rendering intents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Perceptual,
    RelativeColorimetric,
    Saturation,
    AbsoluteColorimetric,
}

/// Every setting of a transform. Little CMS has no defaults here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformOptions {
    pub input: Format,
    pub output: Format,
    pub intent: Intent,
    /// Little CMS ignores this for [`Intent::AbsoluteColorimetric`] and
    /// forces it on for [`Intent::Perceptual`] and [`Intent::Saturation`]
    /// with ICC v4 profiles.
    pub black_point_compensation: bool,
    /// 0 to 1. Used only with [`Intent::AbsoluteColorimetric`]: 1 keeps the
    /// `chad` adaptation of the PCS, 0 undoes it.
    pub adaptation_state: f64,
}

/// Converts `pixels` from `source` to `target`.
///
/// # Errors
/// [`ProfileError::Engine`] for an old engine;
/// [`ProfileError::InvalidProfile`] if Little CMS rejects a profile;
/// [`ProfileError::InvalidOption`] for an adaptation state outside 0 to 1;
/// [`ProfileError::Transform`] if Little CMS cannot build the transform,
/// for example if a format does not match its profile.
pub fn transform(
    source: Endpoint<'_>,
    target: Endpoint<'_>,
    options: &TransformOptions,
    pixels: &[[f64; 3]],
) -> Result<Vec<[f64; 3]>, ProfileError> {
    let v = engine_version();
    if v < MIN_ENGINE_VERSION {
        return Err(ProfileError::Engine(v));
    }
    if !(0.0..=1.0).contains(&options.adaptation_state) {
        return Err(ProfileError::InvalidOption);
    }
    let open = |e: Endpoint<'_>| match e {
        Endpoint::Profile(icc) => Profile::open(icc).ok_or(ProfileError::InvalidProfile),
        Endpoint::PcsXyz => Profile::pcs_xyz().ok_or(ProfileError::Transform),
    };
    let (src, dst) = (open(source)?, open(target)?);
    let t = native::Transform::new(&src, &dst, options).ok_or(ProfileError::Transform)?;
    Ok(t.apply(pixels))
}

#[cfg(test)]
#[path = "transform_tests.rs"]
mod tests;
