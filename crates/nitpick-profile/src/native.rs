//! Little CMS calls. Profile handles and tag storage owned by Little CMS stay
//! in this module; every read returns a copy.

use std::ffi::c_void;
use std::ptr::NonNull;

use lcms2_sys as ffi;

use crate::Tag;

/// Encoded version of the linked engine, for example 2190 for 2.19.
pub(crate) fn engine_version() -> u32 {
    // SAFETY: no arguments; reads a compile-time constant of the library.
    let v = unsafe { ffi::cmsGetEncodedCMMversion() };
    u32::try_from(v).unwrap_or(0)
}

fn signature(tag: Tag) -> ffi::TagSignature {
    use ffi::TagSignature as S;
    match tag {
        Tag::Mhc2 => S::MHC2Tag,
        Tag::MediaWhite => S::MediaWhitePointTag,
        Tag::ChromaticAdaptation => S::ChromaticAdaptationTag,
        Tag::RedColorant => S::RedColorantTag,
        Tag::GreenColorant => S::GreenColorantTag,
        Tag::BlueColorant => S::BlueColorantTag,
        Tag::RedTrc => S::RedTRCTag,
        Tag::GreenTrc => S::GreenTRCTag,
        Tag::BlueTrc => S::BlueTRCTag,
        Tag::Luminance => S::LuminanceTag,
    }
}

/// MHC2 values as Little CMS reads them.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mhc2Values {
    pub min_nits: f64,
    pub peak_nits: f64,
    /// Identity if the matrix offset is 0.
    pub matrix: [[f64; 4]; 3],
    /// Empty if the LUT count is 0.
    pub lut: [Vec<f64>; 3],
}

/// A tone curve as Little CMS holds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Curve {
    /// One parametric segment. `kind` is the Little CMS type: ICC function
    /// number plus 1, negative for an inverse. Unused parameters are zero.
    Parametric { kind: i32, params: [f64; 10] },
    /// A sampled table or several segments.
    Other,
}

/// An open profile. Not `Send`: Little CMS profile handles are not safe for
/// concurrent use.
pub(crate) struct Profile(NonNull<ffi::_HPROFILE>);

impl Profile {
    /// `None` if Little CMS rejects the profile.
    pub fn open(icc: &[u8]) -> Option<Self> {
        let len = u32::try_from(icc.len()).ok()?;
        // SAFETY: the pointer and length describe `icc`. Little CMS copies
        // the block in read mode, so the handle does not borrow `icc`.
        let h = unsafe { ffi::cmsOpenProfileFromMem(icc.as_ptr().cast::<c_void>(), len) };
        NonNull::new(h).map(Self)
    }

    fn handle(&self) -> ffi::HPROFILE {
        self.0.as_ptr()
    }

    pub fn has_tag(&self, tag: Tag) -> bool {
        // SAFETY: valid handle.
        unsafe { ffi::cmsIsTag(self.handle(), signature(tag)) != 0 }
    }

    /// The stored tag bytes, type signature included. `None` if the tag is
    /// missing or cannot be read from the profile.
    ///
    /// Call this before any cooked read of the same tag on this handle.
    /// After a cooked read, Little CMS serializes its parsed object again
    /// instead of returning the stored bytes.
    pub fn raw_tag(&self, tag: Tag) -> Option<Vec<u8>> {
        let sig = signature(tag);
        // SAFETY: a null buffer asks for the size only.
        let size = unsafe { ffi::cmsReadRawTag(self.handle(), sig, std::ptr::null_mut(), 0) };
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; usize::try_from(size).ok()?];
        // SAFETY: `buf` has `size` writable bytes.
        let read = unsafe {
            ffi::cmsReadRawTag(self.handle(), sig, buf.as_mut_ptr().cast::<c_void>(), size)
        };
        (read == size).then_some(buf)
    }

    fn cooked(&self, tag: Tag) -> Option<NonNull<c_void>> {
        // SAFETY: valid handle. The result is owned by the profile and lives
        // until it closes; callers copy from it within `&self`.
        NonNull::new(unsafe { ffi::cmsReadTag(self.handle(), signature(tag)) })
    }

    /// An `XYZType` tag.
    pub fn xyz(&self, tag: Tag) -> Option<[f64; 3]> {
        let p = self.cooked(tag)?.cast::<ffi::CIEXYZ>();
        // SAFETY: Little CMS reads this tag type into one `cmsCIEXYZ`.
        let v = unsafe { p.as_ref() };
        Some([v.X, v.Y, v.Z])
    }

    /// The `chad` tag: an `s15Fixed16ArrayType` of nine values in row order.
    pub fn chad(&self) -> Option<[[f64; 3]; 3]> {
        let p = self.cooked(Tag::ChromaticAdaptation)?.cast::<f64>();
        // SAFETY: Little CMS checks that this tag holds at least nine items
        // and returns them as a `cmsFloat64Number` array.
        let v = unsafe { std::slice::from_raw_parts(p.as_ptr(), 9) };
        Some([0, 1, 2].map(|r| [v[3 * r], v[3 * r + 1], v[3 * r + 2]]))
    }

    pub fn curve(&self, tag: Tag) -> Option<Curve> {
        let p = self.cooked(tag)?.cast::<ffi::ToneCurve>();
        // SAFETY: a TRC tag is read into a `cmsToneCurve`.
        let kind = unsafe { ffi::cmsGetToneCurveParametricType(p.as_ptr()) };
        if kind == 0 {
            return Some(Curve::Other);
        }
        // SAFETY: a nonzero parametric type implies exactly one segment, so
        // segment 0 exists. The binding declares a reference return although
        // the C function returns null for a missing segment.
        let seg = unsafe { ffi::cmsGetToneCurveSegment(0, p.as_ref()) };
        Some(Curve::Parametric {
            kind,
            params: seg.Params,
        })
    }

    /// The `MHC2` tag through the Little CMS type reader.
    pub fn mhc2(&self) -> Option<Mhc2Values> {
        let p = self.cooked(Tag::Mhc2)?.cast::<ffi::MHC2Type>();
        // SAFETY: Little CMS reads this tag type into one `cmsMHC2Type`.
        let m = unsafe { p.as_ref() };
        let n = usize::try_from(m.CurveEntries).ok()?;
        let copy = |c: *mut f64| {
            if n == 0 || c.is_null() {
                Vec::new()
            } else {
                // SAFETY: each non-null curve has `CurveEntries` values.
                unsafe { std::slice::from_raw_parts(c, n) }.to_vec()
            }
        };
        Some(Mhc2Values {
            min_nits: m.MinLuminance,
            peak_nits: m.PeakLuminance,
            matrix: m.XYZ2XYZmatrix,
            lut: [copy(m.RedCurve), copy(m.GreenCurve), copy(m.BlueCurve)],
        })
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        // SAFETY: the handle is open and not used after this.
        unsafe {
            ffi::cmsCloseProfile(self.handle());
        }
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
