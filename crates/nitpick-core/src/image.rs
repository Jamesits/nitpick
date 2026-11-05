//! Image buffer of R'G'B' code values.

use crate::signal::Rgb;

/// Row-major image of R'G'B' code values. [`crate::output`] converts it to
/// swap chain pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    width: usize,
    height: usize,
    pixels: Vec<Rgb>,
}

impl Image {
    #[must_use]
    pub fn new(width: usize, height: usize, fill: Rgb) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill; width * height],
        }
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.width
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.height
    }

    #[must_use]
    pub fn pixels(&self) -> &[Rgb] {
        &self.pixels
    }

    /// # Panics
    /// If the coordinate is outside the image.
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> Rgb {
        assert!(x < self.width && y < self.height);
        self.pixels[y * self.width + x]
    }

    /// Sets one pixel. Coordinates outside the image are ignored.
    pub fn set(&mut self, x: usize, y: usize, colour: Rgb) {
        if x < self.width && y < self.height {
            self.pixels[y * self.width + x] = colour;
        }
    }

    /// Fills the half-open rectangle `[x0, x1) × [y0, y1)`, clipped to the image.
    pub fn fill_rect(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, colour: Rgb) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        for row in self.pixels.chunks_exact_mut(self.width).take(y1).skip(y0) {
            row[x0..x1].fill(colour);
        }
    }

    /// Fills columns `[x0, x1)` of rows `[y0, y1)` with a value per column.
    pub fn fill_columns(
        &mut self,
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        mut colour: impl FnMut(usize) -> Rgb,
    ) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let line: Vec<Rgb> = (x0..x1).map(&mut colour).collect();
        for row in self.pixels.chunks_exact_mut(self.width).take(y1).skip(y0) {
            row[x0..x1].copy_from_slice(&line);
        }
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
