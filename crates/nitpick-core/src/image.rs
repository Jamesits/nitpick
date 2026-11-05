//! Packed 10-bit image buffer.

use crate::signal::Rgb10;

/// Row-major image of packed `R10G10B10A2` pixels (see [`Rgb10::pack`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
}

impl Image {
    #[must_use]
    pub fn new(width: usize, height: usize, fill: Rgb10) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill.pack(); width * height],
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
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// # Panics
    /// If the coordinate is outside the image.
    #[must_use]
    pub fn get(&self, x: usize, y: usize) -> Rgb10 {
        assert!(x < self.width && y < self.height);
        Rgb10::unpack(self.pixels[y * self.width + x])
    }

    /// Sets one pixel. Coordinates outside the image are ignored.
    pub fn set(&mut self, x: usize, y: usize, colour: Rgb10) {
        if x < self.width && y < self.height {
            self.pixels[y * self.width + x] = colour.pack();
        }
    }

    /// Fills the half-open rectangle `[x0, x1) × [y0, y1)`, clipped to the image.
    pub fn fill_rect(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, colour: Rgb10) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let v = colour.pack();
        for row in self.pixels.chunks_exact_mut(self.width).take(y1).skip(y0) {
            row[x0..x1].fill(v);
        }
    }

    /// Fills columns `[x0, x1)` of rows `[y0, y1)` with a value per column.
    pub fn fill_columns(
        &mut self,
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        mut colour: impl FnMut(usize) -> Rgb10,
    ) {
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let line: Vec<u32> = (x0..x1).map(|x| colour(x).pack()).collect();
        for row in self.pixels.chunks_exact_mut(self.width).take(y1).skip(y0) {
            row[x0..x1].copy_from_slice(&line);
        }
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
