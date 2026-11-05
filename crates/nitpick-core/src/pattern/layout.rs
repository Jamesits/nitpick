//! Scaling of reference layouts to the output resolution.

/// Scales a boundary position from a reference size to an actual size,
/// rounding half up.
///
/// Patterns scale boundaries, not widths: neighbouring regions then always
/// tile the output without gaps, and an exact multiple of the reference size
/// gives exact multiples of the reference positions.
#[must_use]
pub fn scale(position: usize, size: usize, reference: usize) -> usize {
    (position * size + reference / 2) / reference
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
