//! A linear-light RGB canvas the eye's textures are drawn on: per-pixel fills, soft discs and anti-aliased strokes,
//! then sRGB bytes for upload.

use omuzaki_core::colour::{srgb_encode, to_byte};
use omuzaki_core::glam::{Vec2, Vec3};

/// A square or rectangular canvas of linear RGB.
#[derive(Clone, Debug)]
pub struct Canvas {
    /// Pixels across.
    pub width: u32,
    /// Pixels down.
    pub height: u32,
    /// Linear RGB, row by row from the top.
    pub pixels: Vec<Vec3>,
}

impl Canvas {
    /// A canvas filled with `colour`.
    pub fn new(width: u32, height: u32, colour: Vec3) -> Self {
        Self { width, height, pixels: vec![colour; (width as usize) * (height as usize)] }
    }

    /// Sets every pixel to `f(x, y)`, with `x` and `y` the pixel's centre in `0..1` across and down.
    pub fn fill(&mut self, f: impl Fn(f32, f32) -> Vec3 + Sync) {
        let (w, h) = (self.width, self.height);
        for (i, px) in self.pixels.iter_mut().enumerate() {
            // Indices fit u32: the canvas is at most a few thousand pixels a side.
            #[allow(clippy::cast_possible_truncation)]
            let (x, y) = ((i as u32) % w, (i as u32) / w);
            #[allow(clippy::cast_precision_loss)]
            let (u, v) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32);
            *px = f(u, v);
        }
    }

    fn blend(&mut self, x: i64, y: i64, colour: Vec3, alpha: f32) {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) || alpha <= 0.0 {
            return;
        }
        // In range: checked above.
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let i = y as usize * self.width as usize + x as usize;
        let px = &mut self.pixels[i];
        *px = px.lerp(colour, alpha.min(1.0));
    }

    /// A soft disc at `centre` (pixels) of radius `radius`, full `alpha` inside `core` of the radius and fading to 0 at
    /// its edge.
    pub fn disc(&mut self, centre: Vec2, radius: Vec2, colour: Vec3, alpha: f32, core: f32) {
        let r = radius.max(Vec2::splat(0.5));
        // Pixel bounds of the disc: small numbers.
        #[allow(clippy::cast_possible_truncation)]
        let (x0, x1, y0, y1) = (
            (centre.x - r.x - 1.0).floor() as i64,
            (centre.x + r.x + 1.0).ceil() as i64,
            (centre.y - r.y - 1.0).floor() as i64,
            (centre.y + r.y + 1.0).ceil() as i64,
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                #[allow(clippy::cast_precision_loss)]
                let p = (Vec2::new(x as f32 + 0.5, y as f32 + 0.5) - centre) / r;
                let d = p.length();
                let a = 1.0 - super::noise::smoothstep(core, 1.0, d);
                self.blend(x, y, colour, alpha * a);
            }
        }
    }

    /// An anti-aliased stroke through `points` (pixels), its width running from `width.0` at the first point to
    /// `width.1` at the last, and its opacity from `alpha.0` to `alpha.1`.
    pub fn stroke(&mut self, points: &[Vec2], width: (f32, f32), colour: Vec3, alpha: (f32, f32)) {
        if points.len() < 2 {
            return;
        }
        let segments = points.len() - 1;
        for (k, pair) in points.windows(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            #[allow(clippy::cast_precision_loss)]
            let (t0, t1) = (k as f32 / segments as f32, (k + 1) as f32 / segments as f32);
            let half = 0.5 * (width.0 + (width.1 - width.0) * t0).max(width.0 + (width.1 - width.0) * t1);
            #[allow(clippy::cast_possible_truncation)]
            let (x0, x1, y0, y1) = (
                (a.x.min(b.x) - half - 1.5).floor() as i64,
                (a.x.max(b.x) + half + 1.5).ceil() as i64,
                (a.y.min(b.y) - half - 1.5).floor() as i64,
                (a.y.max(b.y) + half + 1.5).ceil() as i64,
            );
            let ab = b - a;
            let len2 = ab.length_squared().max(1e-9);
            for y in y0..=y1 {
                for x in x0..=x1 {
                    #[allow(clippy::cast_precision_loss)]
                    let p = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                    let s = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
                    let t = t0 + (t1 - t0) * s;
                    let full = width.0 + (width.1 - width.0) * t;
                    let w = 0.5 * full;
                    let d = (p - (a + ab * s)).length();
                    // Coverage of a pixel by a line `w` wide; thin lines fade instead of breaking up.
                    let cover = (w + 0.5 - d).clamp(0.0, 1.0) * (2.0 * w).min(1.0);
                    let op = alpha.0 + (alpha.1 - alpha.0) * t;
                    // Each segment blends where its own coverage beats what's drawn: joints don't double up much.
                    self.blend(x, y, colour, op * cover * if s > 0.0 && s < 1.0 { 1.0 } else { 0.55 });
                }
            }
        }
    }

    /// The canvas as sRGB bytes, RGBA with opaque alpha.
    pub fn to_srgb(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len() * 4);
        for p in &self.pixels {
            out.extend_from_slice(&[
                to_byte(srgb_encode(p.x.clamp(0.0, 1.0))),
                to_byte(srgb_encode(p.y.clamp(0.0, 1.0))),
                to_byte(srgb_encode(p.z.clamp(0.0, 1.0))),
                255,
            ]);
        }
        out
    }
}
