//! Monitor selection by a point in desktop coordinates.

use std::fmt;

/// Coordinate system of the `--at` point and of the listed monitors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coordinates {
    /// Native pixels of the virtual screen (Windows).
    Physical,
    /// The toolkit's logical desktop units: points on macOS, GDK
    /// application pixels on Linux. The windowing library reports
    /// monitor geometry in these units multiplied by the scale of each
    /// monitor, so mixed-scale rectangles can overlap or leave gaps in
    /// its "physical" values.
    Logical,
}

impl Coordinates {
    /// The desktop coordinates of the build target.
    pub const NATIVE: Self = if cfg!(windows) {
        Self::Physical
    } else {
        Self::Logical
    };
}

/// A monitor rectangle; the left and top edges are inside, the right and
/// bottom edges are outside.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    fn contains(&self, (x, y): (i64, i64)) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }
}

impl fmt::Display for Rect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{},{} {}x{}", self.x, self.y, self.width, self.height)
    }
}

/// Monitor geometry as the windowing library reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub name: String,
    /// Position and size in the library's physical units.
    pub position: (i32, i32),
    pub size: (u32, u32),
    pub scale: f64,
}

impl Monitor {
    /// The rectangle in `coordinates`. Logical values are rounded to the
    /// nearest unit, because the library computes its physical values
    /// from integer logical ones.
    pub fn desktop_rect(&self, coordinates: Coordinates) -> Rect {
        let (x, y) = (i64::from(self.position.0), i64::from(self.position.1));
        let (w, h) = (i64::from(self.size.0), i64::from(self.size.1));
        match coordinates {
            Coordinates::Physical => Rect {
                x,
                y,
                width: w,
                height: h,
            },
            Coordinates::Logical => {
                #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
                let logical = |v: i64| (v as f64 / self.scale).round() as i64;
                Rect {
                    x: logical(x),
                    y: logical(y),
                    width: logical(w),
                    height: logical(h),
                }
            }
        }
    }

    /// The centre in the library's physical units. The library selects
    /// the full-screen monitor by a physical point; the centre is the
    /// point least likely to fall into an overlapping mixed-scale
    /// rectangle.
    pub fn physical_centre(&self) -> (f64, f64) {
        (
            f64::from(self.position.0) + f64::from(self.size.0) / 2.0,
            f64::from(self.position.1) + f64::from(self.size.1) / 2.0,
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SelectError {
    /// No monitor contains the point.
    None,
    /// More than one monitor contains the point; the indices.
    Ambiguous(Vec<usize>),
}

impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("no monitor contains the position"),
            Self::Ambiguous(indices) => {
                write!(
                    f,
                    "more than one monitor contains the position: {indices:?}"
                )
            }
        }
    }
}

/// The index of the one monitor that contains `point`.
///
/// # Errors
///
/// [`SelectError`] if no monitor or more than one monitor contains it.
pub fn select(
    monitors: &[Monitor],
    coordinates: Coordinates,
    point: (i64, i64),
) -> Result<usize, SelectError> {
    let hits: Vec<usize> = monitors
        .iter()
        .enumerate()
        .filter(|(_, m)| m.desktop_rect(coordinates).contains(point))
        .map(|(i, _)| i)
        .collect();
    match hits.as_slice() {
        [] => Err(SelectError::None),
        [one] => Ok(*one),
        _ => Err(SelectError::Ambiguous(hits)),
    }
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
