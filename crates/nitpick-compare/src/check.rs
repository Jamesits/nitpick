//! Check of one patch against a fixed expectation: per-component bounds
//! from quantization, plus the perceptual target.
//!
//! An [`Expectation`] is read-only after construction. A check reads it and
//! returns a new report; no result changes a bound.

use crate::colour::Space;
use crate::itp::Itp;
use crate::stats::ErrorStats;
use crate::{Invalid, finite};

/// Proposed largest `ΔE_ITP` of a patch. This is a quality target
/// for qualification, not a tolerance derived from quantization.
pub const PERCEPTUAL_TARGET: f64 = 1.0;

/// The expected value of a patch and its per-component bound.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Expectation {
    space: Space,
    value: [f64; 3],
    bound: [f64; 3],
}

impl Expectation {
    /// `bound` is the largest accepted absolute difference per component,
    /// in the units of `space`.
    ///
    /// # Errors
    /// A non-finite value, or a negative or non-finite bound.
    pub fn new(space: Space, value: [f64; 3], bound: [f64; 3]) -> Result<Self, Invalid> {
        finite(value)?;
        if let Some(&b) = bound.iter().find(|b| !(b.is_finite() && **b >= 0.0)) {
            return Err(Invalid::Bound(b));
        }
        Ok(Self {
            space,
            value,
            bound,
        })
    }

    #[must_use]
    pub fn space(&self) -> Space {
        self.space
    }

    #[must_use]
    pub fn value(&self) -> [f64; 3] {
        self.value
    }

    #[must_use]
    pub fn bound(&self) -> [f64; 3] {
        self.bound
    }

    /// `ΔE_ITP` that quantization alone can give: the largest value
    /// at the 8 corners of the bound box. For a small box the metric is
    /// close to a norm of the difference, whose maximum on a box is at a
    /// corner; this is not a proof for a large box.
    ///
    /// # Errors
    /// The expected value or a corner is outside the metric domain.
    pub fn quantization_delta_e_itp(&self) -> Result<f64, Invalid> {
        let centre = Itp::new(self.space.to_nits_2020(self.value)?)?;
        let mut max = 0.0f64;
        for corner in 0..8 {
            let v: [f64; 3] = std::array::from_fn(|c| {
                let sign = if corner >> c & 1 == 0 { -1.0 } else { 1.0 };
                self.value[c] + sign * self.bound[c]
            });
            max = max.max(centre.delta_e(Itp::new(self.space.to_nits_2020(v)?)?));
        }
        Ok(max)
    }
}

/// Why a patch failed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Failure {
    /// A sample has a non-finite component.
    NonFinite { sample: usize },
    /// The largest component difference is above its bound. `error` is
    /// signed: measured minus expected.
    Component {
        channel: usize,
        error: f64,
        bound: f64,
    },
    /// The largest `ΔE_ITP` is above `limit`.
    Perceptual { delta_e_itp: f64, limit: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Pass,
    Fail(Vec<Failure>),
    Unavailable(String),
}

/// The result of [`check_patch`].
#[derive(Debug, Clone, PartialEq)]
pub struct PatchReport {
    pub verdict: Verdict,
    /// The expectation of the check, unchanged.
    pub expectation: Expectation,
    /// Per component: the signed difference of largest magnitude. `None`
    /// without usable samples.
    pub worst: Option<[f64; 3]>,
    /// Per component: statistics of the absolute differences.
    pub component: Option<[ErrorStats; 3]>,
    /// Statistics of the per-sample `ΔE_ITP`. An error if a sample
    /// is outside the metric domain; then only the component check applies.
    pub delta_e_itp: Option<Result<ErrorStats, Invalid>>,
    /// See [`Expectation::quantization_delta_e_itp`]. An error: only the
    /// component check applies.
    pub quantization_delta_e_itp: Result<f64, Invalid>,
    /// `Some` if quantization alone exceeds [`PERCEPTUAL_TARGET`]: the
    /// perceptual check uses this value instead, and a pass is not an
    /// ordinary colour pass. It does not change the component check.
    pub precision_limit: Option<f64>,
}

/// Checks the samples of one patch, in the space of `e`.
///
/// The patch passes if every component difference is within its bound and
/// the largest `ΔE_ITP` is within the perceptual limit. The
/// perceptual check applies only where the metric is defined for every
/// sample and for the bound box; the report gives the reason otherwise.
/// Without samples the result is unavailable.
///
/// The perceptual limit is not below the `ΔE_ITP` of the bound
/// box, so with bounds from quantization it adds a failure only for a
/// sample outside the box.
#[must_use]
pub fn check_patch(e: &Expectation, samples: &[[f64; 3]]) -> PatchReport {
    let quantization = e.quantization_delta_e_itp();
    let precision_limit = quantization.ok().filter(|&q| q > PERCEPTUAL_TARGET);
    let mut report = PatchReport {
        verdict: Verdict::Unavailable("the patch has no samples".into()),
        expectation: *e,
        worst: None,
        component: None,
        delta_e_itp: None,
        quantization_delta_e_itp: quantization,
        precision_limit,
    };
    if samples.is_empty() {
        return report;
    }
    if let Some(sample) = samples.iter().position(|s| finite(*s).is_err()) {
        report.verdict = Verdict::Fail(vec![Failure::NonFinite { sample }]);
        return report;
    }

    let mut failures = Vec::new();
    let mut worst = [0.0f64; 3];
    let mut abs: [Vec<f64>; 3] = Default::default();
    for s in samples {
        for c in 0..3 {
            let d = s[c] - e.value[c];
            if d.abs() > worst[c].abs() {
                worst[c] = d;
            }
            abs[c].push(d.abs());
        }
    }
    for (channel, (&error, &bound)) in worst.iter().zip(&e.bound).enumerate() {
        if error.abs() > bound {
            failures.push(Failure::Component {
                channel,
                error,
                bound,
            });
        }
    }
    report.worst = Some(worst);
    // `None` only if a difference of two finite values overflows.
    report.component = match abs.map(|v| ErrorStats::new(v).ok().flatten()) {
        [Some(r), Some(g), Some(b)] => Some([r, g, b]),
        _ => None,
    };

    let delta_e = perceptual(e, samples);
    if let (Ok(stats), Ok(q)) = (&delta_e, quantization) {
        let limit = q.max(PERCEPTUAL_TARGET);
        if stats.max > limit {
            failures.push(Failure::Perceptual {
                delta_e_itp: stats.max,
                limit,
            });
        }
    }
    report.delta_e_itp = Some(delta_e);
    report.verdict = if failures.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Fail(failures)
    };
    report
}

/// `ΔE_ITP` statistics of non-empty, finite samples.
fn perceptual(e: &Expectation, samples: &[[f64; 3]]) -> Result<ErrorStats, Invalid> {
    let want = Itp::new(e.space.to_nits_2020(e.value)?)?;
    let mut out = Vec::with_capacity(samples.len());
    for s in samples {
        out.push(want.delta_e(Itp::new(e.space.to_nits_2020(*s)?)?));
    }
    match ErrorStats::new(out) {
        Ok(Some(stats)) => Ok(stats),
        // `samples` is not empty, so only a non-finite distance is left.
        _ => Err(Invalid::NonFinite(f64::INFINITY)),
    }
}

#[cfg(test)]
#[path = "check_tests.rs"]
mod tests;
