//! A tiny RGBA plotter for the evidence image (`measure.face_overlay`): dots, lines, crosses, squares and rings drawn
//! onto a copy of the measured image. Whole pixels and straight alpha blending only, so the same input gives the same
//! bytes on every platform.

use crate::image::Rgba8;

/// Green: an `ok` sample.
pub const OK: [u8; 3] = [0x16, 0xa3, 0x4a];
/// Red: an `occluded` sample's ray.
pub const OCCLUDED: [u8; 3] = [0xdc, 0x26, 0x26];
/// Grey: an `excluded` sample's ray.
pub const EXCLUDED: [u8; 3] = [0x9c, 0xa3, 0xaf];
/// Magenta: the snapped landmarks.
pub const LANDMARK: [u8; 3] = [0xd9, 0x46, 0xef];
/// Blue: the reference's outline.
pub const REFERENCE: [u8; 3] = [0x25, 0x63, 0xeb];
/// Amber: the worst sector's shading.
pub const WORST: [u8; 3] = [0xf5, 0x9e, 0x0b];

/// A canvas over a copy of an image.
pub struct Canvas {
    image: Rgba8,
}

impl Canvas {
    /// A canvas on a copy of `image`.
    #[must_use]
    pub fn new(image: &Rgba8) -> Self {
        Self { image: image.clone() }
    }

    /// The drawn image.
    #[must_use]
    pub fn finish(self) -> Rgba8 {
        self.image
    }

    /// Blends `colour` at `alpha` (0 to 1) into the pixel at integer `(x, y)`; off the image does nothing.
    fn blend(&mut self, x: i64, y: i64, colour: [u8; 3], alpha: f64) {
        let (width, height) = (i64::from(self.image.width), i64::from(self.image.height));
        if x < 0 || y < 0 || x >= width || y >= height {
            return;
        }
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)] // checked in range above
        let i = ((y * width + x) * 4) as usize;
        for (c, &v) in colour.iter().enumerate() {
            let old = f64::from(self.image.pixels[i + c]);
            #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)] // a blend of bytes
            let new = (old + (f64::from(v) - old) * alpha).round() as u8;
            self.image.pixels[i + c] = new;
        }
        self.image.pixels[i + 3] = 255;
    }

    /// A filled disc of `radius` pixels centred on `p`.
    pub fn dot(&mut self, p: [f64; 2], radius: f64, colour: [u8; 3]) {
        let r2 = radius * radius;
        #[allow(clippy::cast_possible_truncation)] // image coordinates
        let (x0, x1, y0, y1) = (
            (p[0] - radius).floor() as i64,
            (p[0] + radius).ceil() as i64,
            (p[1] - radius).floor() as i64,
            (p[1] + radius).ceil() as i64,
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                #[allow(clippy::cast_precision_loss)] // image coordinates
                let (cx, cy) = (x as f64 + 0.5 - p[0], y as f64 + 0.5 - p[1]);
                if cx * cx + cy * cy <= r2 {
                    self.blend(x, y, colour, 1.0);
                }
            }
        }
    }

    /// A ring (hollow circle) of `radius` pixels centred on `p`, one pixel thick.
    pub fn ring(&mut self, p: [f64; 2], radius: f64, colour: [u8; 3]) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small positive count
        let steps = (radius * 8.0).ceil().max(8.0) as u32;
        for i in 0..steps {
            let a = std::f64::consts::TAU * f64::from(i) / f64::from(steps);
            let (s, c) = a.sin_cos();
            self.pixel([p[0] + radius * c, p[1] + radius * s], colour, 1.0);
        }
    }

    /// The pixel under the point `p`.
    fn pixel(&mut self, p: [f64; 2], colour: [u8; 3], alpha: f64) {
        #[allow(clippy::cast_possible_truncation)] // image coordinates
        self.blend(p[0].floor() as i64, p[1].floor() as i64, colour, alpha);
    }

    /// A line from `a` to `b`, one pixel thick, sampled every half pixel.
    pub fn line(&mut self, a: [f64; 2], b: [f64; 2], colour: [u8; 3], alpha: f64) {
        let length = (b[0] - a[0]).hypot(b[1] - a[1]);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a positive pixel count
        let steps = (length * 2.0).ceil().max(1.0) as u32;
        let mut last = None;
        for i in 0..=steps {
            let t = f64::from(i) / f64::from(steps);
            let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            #[allow(clippy::cast_possible_truncation)] // image coordinates
            let px = (p[0].floor() as i64, p[1].floor() as i64);
            if last != Some(px) {
                self.blend(px.0, px.1, colour, alpha);
                last = Some(px);
            }
        }
    }

    /// A cross of half-size `size` pixels centred on `p`.
    pub fn cross(&mut self, p: [f64; 2], size: f64, colour: [u8; 3]) {
        self.line([p[0] - size, p[1] - size], [p[0] + size, p[1] + size], colour, 1.0);
        self.line([p[0] - size, p[1] + size], [p[0] + size, p[1] - size], colour, 1.0);
    }

    /// An outlined square of half-side `size` pixels centred on `p`.
    pub fn square(&mut self, p: [f64; 2], size: f64, colour: [u8; 3]) {
        let corners = [
            [p[0] - size, p[1] - size],
            [p[0] + size, p[1] - size],
            [p[0] + size, p[1] + size],
            [p[0] - size, p[1] + size],
        ];
        for i in 0..4 {
            self.line(corners[i], corners[(i + 1) % 4], colour, 1.0);
        }
    }

    /// A filled wedge from `centre` between the two outline points `a` and `b`, at `alpha`, for the worst sector.
    pub fn wedge(&mut self, centre: [f64; 2], a: [f64; 2], b: [f64; 2], colour: [u8; 3], alpha: f64) {
        let (min_x, max_x) = (centre[0].min(a[0]).min(b[0]), centre[0].max(a[0]).max(b[0]));
        let (min_y, max_y) = (centre[1].min(a[1]).min(b[1]), centre[1].max(a[1]).max(b[1]));
        let cross =
            |o: [f64; 2], p: [f64; 2], q: [f64; 2]| (p[0] - o[0]) * (q[1] - o[1]) - (p[1] - o[1]) * (q[0] - o[0]);
        #[allow(clippy::cast_possible_truncation)] // image coordinates
        for y in min_y.floor() as i64..=max_y.ceil() as i64 {
            for x in min_x.floor() as i64..=max_x.ceil() as i64 {
                #[allow(clippy::cast_precision_loss)] // image coordinates
                let p = [x as f64 + 0.5, y as f64 + 0.5];
                let (d1, d2, d3) = (cross(centre, a, p), cross(a, b, p), cross(b, centre, p));
                let inside = (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0);
                if inside {
                    self.blend(x, y, colour, alpha);
                }
            }
        }
    }
}
