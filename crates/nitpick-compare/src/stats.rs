//! Statistics of per-sample errors of one patch.

/// The maximum and median of per-sample errors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErrorStats {
    pub count: usize,
    pub max: f64,
    /// The mean of the two middle values for an even count.
    pub median: f64,
}

impl ErrorStats {
    /// `Ok(None)`: no samples.
    ///
    /// # Errors
    /// An error value is not finite.
    pub fn new(mut errors: Vec<f64>) -> Result<Option<Self>, String> {
        if let Some(e) = errors.iter().find(|e| !e.is_finite()) {
            return Err(format!("a sample error is {e}"));
        }
        if errors.is_empty() {
            return Ok(None);
        }
        errors.sort_by(f64::total_cmp);
        let n = errors.len();
        let median = if n % 2 == 1 {
            errors[n / 2]
        } else {
            f64::midpoint(errors[n / 2 - 1], errors[n / 2])
        };
        Ok(Some(Self {
            count: n,
            max: errors[n - 1],
            median,
        }))
    }
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;
