//! The pixels: a [`Canvas`] of premultiplied pixels, and a [`Mask`] of coverage over a rectangle of it.

use crate::blend::{BlendMode, Keep, composite, erase};
use crate::colour::{Colour, Px};
use crate::error::PaintError;

/// A rectangle of whole pixels, `x0..x1` by `y0..y1`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge (inclusive).
    pub x0: i32,
    /// Top edge (inclusive).
    pub y0: i32,
    /// Right edge (exclusive).
    pub x1: i32,
    /// Bottom edge (exclusive).
    pub y1: i32,
}

impl Rect {
    /// The rectangle that holds `x0..x1` by `y0..y1` (in pixels, any order), grown out to whole pixels.
    #[allow(clippy::cast_possible_truncation)] // clamped to the i32 range first
    pub fn covering(x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        let c = |v: f64| v.clamp(f64::from(i32::MIN / 2), f64::from(i32::MAX / 2));
        Self {
            x0: c(x0.min(x1)).floor() as i32,
            y0: c(y0.min(y1)).floor() as i32,
            x1: c(x0.max(x1)).ceil() as i32,
            y1: c(y0.max(y1)).ceil() as i32,
        }
    }

    /// This rectangle grown by `n` pixels on each side.
    #[must_use]
    pub fn grow(self, n: i32) -> Self {
        Self { x0: self.x0 - n, y0: self.y0 - n, x1: self.x1 + n, y1: self.y1 + n }
    }

    /// The part of this rectangle inside `other` (empty when they don't meet).
    #[must_use]
    pub fn clip(self, other: Self) -> Self {
        let r = Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        if r.is_empty() { Self::default() } else { r }
    }

    /// The smallest rectangle holding both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// No pixels.
    pub fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    /// Width in pixels (0 when empty).
    #[allow(clippy::cast_sign_loss)] // non-negative by the max
    pub fn width(self) -> usize {
        (self.x1 - self.x0).max(0) as usize
    }

    /// Height in pixels (0 when empty).
    #[allow(clippy::cast_sign_loss)] // non-negative by the max
    pub fn height(self) -> usize {
        (self.y1 - self.y0).max(0) as usize
    }
}

/// Coverage, 0 to 1, over a rectangle: what one mark touches, before its colour is laid down.
#[derive(Clone, Debug, PartialEq)]
pub struct Mask {
    /// Where the values are, in canvas pixels.
    pub rect: Rect,
    /// Row by row, `rect.width()` values per row.
    pub values: Vec<f32>,
}

impl Mask {
    /// An empty mask over `rect`.
    pub fn new(rect: Rect) -> Self {
        Self { rect, values: vec![0.0; rect.width() * rect.height()] }
    }

    /// The value at canvas pixel `(x, y)`; 0 outside.
    #[allow(clippy::cast_sign_loss)] // inside the rect, so non-negative
    pub fn get(&self, x: i32, y: i32) -> f32 {
        let r = self.rect;
        if x < r.x0 || y < r.y0 || x >= r.x1 || y >= r.y1 {
            return 0.0;
        }
        self.values[(y - r.y0) as usize * r.width() + (x - r.x0) as usize]
    }

    /// The value at canvas pixel `(x, y)` for writing, if it is inside.
    #[allow(clippy::cast_sign_loss)] // inside the rect, so non-negative
    pub fn at_mut(&mut self, x: i32, y: i32) -> Option<&mut f32> {
        let r = self.rect;
        if x < r.x0 || y < r.y0 || x >= r.x1 || y >= r.y1 {
            return None;
        }
        self.values.get_mut((y - r.y0) as usize * r.width() + (x - r.x0) as usize)
    }

    /// Multiplies each value by `other`'s at the same pixel (an intersection).
    pub fn intersect(&mut self, other: &Self) {
        let r = self.rect;
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let o = other.get(x, y);
                if let Some(v) = self.at_mut(x, y) {
                    *v *= o;
                }
            }
        }
    }

    /// The sum of the values: the painted area in pixels.
    pub fn area(&self) -> f64 {
        self.values.iter().map(|&v| f64::from(v)).sum()
    }
}

/// A picture of premultiplied pixels (see [`Px`]), row by row from the top left.
#[derive(Clone, Debug, PartialEq)]
pub struct Canvas {
    width: u32,
    height: u32,
    px: Vec<Px>,
}

impl Canvas {
    /// The largest side, in pixels.
    pub const MAX_SIDE: u32 = 8192;

    /// A transparent canvas.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] when a side is 0 or above [`Self::MAX_SIDE`].
    pub fn new(width: u32, height: u32) -> Result<Self, PaintError> {
        for (field, v) in [("width", width), ("height", height)] {
            if v == 0 || v > Self::MAX_SIDE {
                return Err(PaintError::invalid(field, format!("{v} px is outside 1 to {}", Self::MAX_SIDE)));
            }
        }
        Ok(Self { width, height, px: vec![[0.0; 4]; width as usize * height as usize] })
    }

    /// A canvas filled with `colour`.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn filled(width: u32, height: u32, colour: Colour) -> Result<Self, PaintError> {
        let mut c = Self::new(width, height)?;
        c.px.fill(colour.premul());
        Ok(c)
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The whole canvas as a rectangle.
    #[allow(clippy::cast_possible_wrap)] // sides are at most MAX_SIDE
    pub fn bounds(&self) -> Rect {
        Rect { x0: 0, y0: 0, x1: self.width as i32, y1: self.height as i32 }
    }

    /// The pixels, row by row.
    pub fn pixels(&self) -> &[Px] {
        &self.px
    }

    /// The pixels for writing.
    pub fn pixels_mut(&mut self) -> &mut [Px] {
        &mut self.px
    }

    /// The pixel at `(x, y)`; clear outside the canvas.
    #[allow(clippy::cast_sign_loss)] // checked non-negative
    pub fn get(&self, x: i32, y: i32) -> Px {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return [0.0; 4];
        }
        self.px[y as usize * self.width as usize + x as usize]
    }

    #[allow(clippy::cast_sign_loss)] // checked non-negative
    fn index(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x as u32 >= self.width || y as u32 >= self.height {
            return None;
        }
        Some(y as usize * self.width as usize + x as usize)
    }

    /// The pixel at `(x, y)` for writing, if it is on the canvas.
    pub fn at_mut(&mut self, x: i32, y: i32) -> Option<&mut Px> {
        let i = self.index(x, y)?;
        self.px.get_mut(i)
    }

    /// Lays `colour` down through `mask`, scaled by `opacity`, with `mode` (or erases where `mode` is `None`).
    pub fn paint_mask(&mut self, mask: &Mask, colour: Px, opacity: f32, mode: Option<BlendMode>, keep: Keep) {
        let r = mask.rect.clip(self.bounds());
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let w = mask.get(x, y) * opacity;
                if w <= 0.0 {
                    continue;
                }
                if let Some(d) = self.at_mut(x, y) {
                    match mode {
                        Some(mode) => composite(d, colour, w, mode, keep),
                        None => erase(d, w),
                    }
                }
            }
        }
    }

    /// Lays a per-pixel `colour_at` down through `mask` (gradients), as [`Self::paint_mask`] does.
    pub fn paint_mask_with(
        &mut self,
        mask: &Mask,
        colour_at: &dyn Fn(f64, f64) -> Px,
        opacity: f32,
        mode: BlendMode,
        keep: Keep,
    ) {
        let r = mask.rect.clip(self.bounds());
        for y in r.y0..r.y1 {
            for x in r.x0..r.x1 {
                let w = mask.get(x, y) * opacity;
                if w <= 0.0 {
                    continue;
                }
                let c = colour_at(f64::from(x) + 0.5, f64::from(y) + 0.5);
                if let Some(d) = self.at_mut(x, y) {
                    composite(d, c, w, mode, keep);
                }
            }
        }
    }

    /// Composites all of `src` onto this canvas with `mode` and `opacity`.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] when the canvases differ in size.
    pub fn composite_from(&mut self, src: &Self, opacity: f32, mode: BlendMode, keep: Keep) -> Result<(), PaintError> {
        self.same_size(src)?;
        for (d, s) in self.px.iter_mut().zip(&src.px) {
            composite(d, *s, opacity, mode, keep);
        }
        Ok(())
    }

    /// Multiplies every pixel by `mask` (alpha and colour, so it stays premultiplied); outside the mask is cleared.
    pub fn apply_mask(&mut self, mask: &Mask) {
        let w = self.width as usize;
        for (i, p) in self.px.iter_mut().enumerate() {
            #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)] // sides are at most MAX_SIDE
            let k = mask.get((i % w) as i32, (i / w) as i32);
            for v in p.iter_mut() {
                *v *= k;
            }
        }
    }

    /// Blurs every channel by a Gaussian of `sigma` pixels (pixels past the edge count as clear).
    pub fn blur(&mut self, sigma: f64) {
        if sigma <= 0.0 {
            return;
        }
        let rect = self.bounds();
        for c in 0..4 {
            let mut m = Mask::new(rect);
            for (v, p) in m.values.iter_mut().zip(&self.px) {
                *v = p[c];
            }
            let b = crate::raster::feather(&m, sigma, rect);
            for (p, v) in self.px.iter_mut().zip(&b.values) {
                p[c] = *v;
            }
        }
    }

    /// Mixes towards `other` by `t` (0 keeps this, 1 is `other`), pixel by pixel.
    ///
    /// # Errors
    /// [`PaintError::Invalid`] when the canvases differ in size.
    pub fn lerp_to(&mut self, other: &Self, t: f32) -> Result<(), PaintError> {
        self.same_size(other)?;
        for (d, s) in self.px.iter_mut().zip(&other.px) {
            for i in 0..4 {
                d[i] += (s[i] - d[i]) * t;
            }
        }
        Ok(())
    }

    fn same_size(&self, other: &Self) -> Result<(), PaintError> {
        if (self.width, self.height) == (other.width, other.height) {
            Ok(())
        } else {
            Err(PaintError::invalid(
                "canvas",
                format!("{}x{} and {}x{} differ in size", self.width, self.height, other.width, other.height),
            ))
        }
    }

    /// The canvas as straight-alpha 8-bit RGBA, row by row.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len() * 4);
        for p in &self.px {
            let c = Colour::from_premul(*p);
            out.extend_from_slice(&[c.r, c.g, c.b, c.a]);
        }
        out
    }

    /// The canvas as a PNG file (8-bit RGBA).
    ///
    /// # Errors
    /// [`PaintError::Encode`] when the encoder fails.
    pub fn to_png(&self) -> Result<Vec<u8>, PaintError> {
        let mut bytes = Vec::new();
        image::write_buffer_with_format(
            &mut std::io::Cursor::new(&mut bytes),
            &self.to_rgba8(),
            self.width,
            self.height,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .map_err(|e| PaintError::Encode(e.to_string()))?;
        Ok(bytes)
    }

    /// A canvas from straight-alpha 8-bit RGBA, row by row (the painter's own pictures read back, for tests and the
    /// puppet's layered art).
    ///
    /// # Errors
    /// [`PaintError::Invalid`] when the bytes don't hold `width` by `height` pixels.
    pub fn from_rgba8(width: u32, height: u32, rgba: &[u8]) -> Result<Self, PaintError> {
        let mut c = Self::new(width, height)?;
        if rgba.len() != c.px.len() * 4 {
            return Err(PaintError::invalid("rgba", format!("{} bytes is not {width}x{height} pixels", rgba.len())));
        }
        for (p, b) in c.px.iter_mut().zip(rgba.as_chunks::<4>().0) {
            *p = Colour { r: b[0], g: b[1], b: b[2], a: b[3] }.premul();
        }
        Ok(c)
    }

    /// The bounding rectangle of the pixels that aren't clear (empty when all are).
    pub fn content_bounds(&self) -> Rect {
        let w = self.width as usize;
        let mut r = Rect::default();
        for (i, p) in self.px.iter().enumerate() {
            if p[3] > 0.0 {
                #[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)] // sides are at most MAX_SIDE
                let (x, y) = ((i % w) as i32, (i / w) as i32);
                r = r.union(Rect { x0: x, y0: y, x1: x + 1, y1: y + 1 });
            }
        }
        r
    }
}
