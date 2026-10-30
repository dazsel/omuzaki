//! The pixels the parity measures work on: an image as CIELAB planes plus its alpha, single-channel planes, a Gaussian
//! blur, bilinear sampling and the similarity warp that puts one image into another's pixel grid.

use crate::colour::lab;
use crate::image::Rgba8;

/// One channel of `w x h` values, row by row from the top.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    /// The width in pixels.
    pub w: usize,
    /// The height in pixels.
    pub h: usize,
    /// `w * h` values.
    pub v: Vec<f32>,
}

impl Plane {
    /// A plane filled with `fill`.
    #[must_use]
    pub fn new(w: usize, h: usize, fill: f32) -> Self {
        Self { w, h, v: vec![fill; w * h] }
    }

    /// The value at `(x, y)`, clamped to the edge.
    #[must_use]
    pub fn at(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.w as isize - 1).unsigned_abs();
        let y = y.clamp(0, self.h as isize - 1).unsigned_abs();
        self.v[y * self.w + x]
    }

    /// The bilinear sample at pixel-centre coordinates `(x, y)` (pixel `i` covers `i..i+1`, its centre is `i + 0.5`),
    /// clamped to the edge.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // floor of a finite coordinate inside the image's range
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let (fx, fy) = (x - 0.5, y - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let top = self.at(x0, y0) * (1.0 - tx) + self.at(x0 + 1, y0) * tx;
        let bottom = self.at(x0, y0 + 1) * (1.0 - tx) + self.at(x0 + 1, y0 + 1) * tx;
        top * (1.0 - ty) + bottom * ty
    }

    /// A separable Gaussian blur with standard deviation `sigma` pixels (edges clamped).
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small radii
    pub fn blur(&self, sigma: f32) -> Self {
        if sigma <= 0.0 {
            return self.clone();
        }
        let radius = (sigma * 3.0).ceil() as isize;
        let mut kernel: Vec<f32> =
            (-radius..=radius).map(|i| (-((i * i) as f32) / (2.0 * sigma * sigma)).exp()).collect();
        let sum: f32 = kernel.iter().sum();
        for k in &mut kernel {
            *k /= sum;
        }
        let mut tmp = Self::new(self.w, self.h, 0.0);
        for y in 0..self.h {
            for x in 0..self.w {
                let mut acc = 0.0;
                for (k, wgt) in kernel.iter().enumerate() {
                    acc += wgt * self.at(x as isize + k as isize - radius, y as isize);
                }
                tmp.v[y * self.w + x] = acc;
            }
        }
        let mut out = Self::new(self.w, self.h, 0.0);
        for y in 0..self.h {
            for x in 0..self.w {
                let mut acc = 0.0;
                for (k, wgt) in kernel.iter().enumerate() {
                    acc += wgt * tmp.at(x as isize, y as isize + k as isize - radius);
                }
                out.v[y * self.w + x] = acc;
            }
        }
        out
    }

    /// The plane shrunk by a whole `factor`, each output pixel the mean of its `factor x factor` block (a partial
    /// block at the right or bottom edge is dropped).
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // small block sizes
    pub fn shrink(&self, factor: usize) -> Self {
        let factor = factor.max(1);
        let (w, h) = ((self.w / factor).max(1), (self.h / factor).max(1));
        let mut out = Self::new(w, h, 0.0);
        let n = (factor * factor) as f32;
        for y in 0..h {
            for x in 0..w {
                let mut acc = 0.0;
                for dy in 0..factor {
                    for dx in 0..factor {
                        acc += self.at((x * factor + dx) as isize, (y * factor + dy) as isize);
                    }
                }
                out.v[y * w + x] = acc / n;
            }
        }
        out
    }

    /// Element-wise `f(a, b)` of two planes of one size.
    #[must_use]
    pub fn zip(&self, other: &Self, f: impl Fn(f32, f32) -> f32) -> Self {
        Self { w: self.w, h: self.h, v: self.v.iter().zip(&other.v).map(|(a, b)| f(*a, *b)).collect() }
    }

    /// Element-wise `f(a)`.
    #[must_use]
    pub fn map(&self, f: impl Fn(f32) -> f32) -> Self {
        Self { w: self.w, h: self.h, v: self.v.iter().map(|a| f(*a)).collect() }
    }
}

/// An image as the parity measures see it: CIELAB (D65) planes of the colour composited over a backdrop, the alpha
/// (0 to 1), and the composited sRGB bytes the overlays draw with.
#[derive(Clone, Debug, PartialEq)]
pub struct LabImage {
    /// The width in pixels.
    pub w: usize,
    /// The height in pixels.
    pub h: usize,
    /// L* (0 to 100).
    pub l: Plane,
    /// a*.
    pub a: Plane,
    /// b*.
    pub b: Plane,
    /// The coverage, 0 to 1 (1 everywhere for an opaque image).
    pub alpha: Plane,
    /// The composited sRGB, `w * h` triples.
    pub rgb: Vec<[u8; 3]>,
    /// Whether any pixel is less than opaque: the silhouette then comes from the alpha, not from a backdrop key.
    pub has_alpha: bool,
}

impl LabImage {
    /// Converts straight sRGB RGBA8, compositing each pixel over `backdrop` (sRGB, blended in sRGB bytes the way a
    /// video encoder sees a transparent render over a solid backdrop).
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a blend of bytes stays a byte
    pub fn from_rgba(image: &Rgba8, backdrop: [u8; 3]) -> Self {
        let (w, h) = (image.width as usize, image.height as usize);
        let mut l = Plane::new(w, h, 0.0);
        let mut a = Plane::new(w, h, 0.0);
        let mut b = Plane::new(w, h, 0.0);
        let mut alpha = Plane::new(w, h, 1.0);
        let mut rgb = Vec::with_capacity(w * h);
        let mut has_alpha = false;
        // Most renders hold few distinct colours; caching the conversion keeps a 1080p frame quick.
        let mut cache: std::collections::HashMap<[u8; 3], [f32; 3]> = std::collections::HashMap::new();
        for (i, px) in image.pixels.as_chunks::<4>().0.iter().enumerate() {
            let t = f32::from(px[3]) / 255.0;
            has_alpha |= px[3] < 255;
            let mix = |c: u8, k: u8| (f32::from(c) * t + f32::from(k) * (1.0 - t)).round().clamp(0.0, 255.0) as u8;
            let c = [mix(px[0], backdrop[0]), mix(px[1], backdrop[1]), mix(px[2], backdrop[2])];
            let v = *cache.entry(c).or_insert_with(|| lab(c).map(|x| x as f32));
            l.v[i] = v[0];
            a.v[i] = v[1];
            b.v[i] = v[2];
            alpha.v[i] = t;
            rgb.push(c);
        }
        Self { w, h, l, a, b, alpha, rgb, has_alpha }
    }

    /// The CIELAB colour at pixel index `i`.
    #[must_use]
    pub fn lab_at(&self, i: usize) -> [f32; 3] {
        [self.l.v[i], self.a.v[i], self.b.v[i]]
    }

    /// The image put through `warp` into a `w x h` grid: each output pixel samples this image where `warp` maps it
    /// back to (bilinear). Pixels that fall outside this image are left transparent over `backdrop`.
    #[must_use]
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pixel grids
    pub fn warped(&self, warp: &Similarity, w: usize, h: usize, backdrop: [u8; 3]) -> Self {
        let inv = warp.inverse();
        let mut out = Self {
            w,
            h,
            l: Plane::new(w, h, 0.0),
            a: Plane::new(w, h, 0.0),
            b: Plane::new(w, h, 0.0),
            alpha: Plane::new(w, h, 0.0),
            rgb: vec![backdrop; w * h],
            has_alpha: true,
        };
        let r: Plane = Plane { w: self.w, h: self.h, v: self.rgb.iter().map(|c| f32::from(c[0])).collect() };
        let g: Plane = Plane { w: self.w, h: self.h, v: self.rgb.iter().map(|c| f32::from(c[1])).collect() };
        let bl: Plane = Plane { w: self.w, h: self.h, v: self.rgb.iter().map(|c| f32::from(c[2])).collect() };
        let back = lab(backdrop).map(|x| x as f32);
        for y in 0..h {
            for x in 0..w {
                let [sx, sy] = inv.apply([x as f64 + 0.5, y as f64 + 0.5]);
                let i = y * w + x;
                let inside = sx >= 0.0 && sy >= 0.0 && sx <= self.w as f64 && sy <= self.h as f64;
                if !inside {
                    out.l.v[i] = back[0];
                    out.a.v[i] = back[1];
                    out.b.v[i] = back[2];
                    continue;
                }
                let (fx, fy) = (sx as f32, sy as f32);
                out.l.v[i] = self.l.sample(fx, fy);
                out.a.v[i] = self.a.sample(fx, fy);
                out.b.v[i] = self.b.sample(fx, fy);
                out.alpha.v[i] = self.alpha.sample(fx, fy);
                out.rgb[i] =
                    [r.sample(fx, fy), g.sample(fx, fy), bl.sample(fx, fy)].map(|c| c.round().clamp(0.0, 255.0) as u8);
            }
        }
        out
    }
}

/// A similarity transform of the image plane, `p' = scale * R(angle) * p + shift`, in pixels (y down, so a positive
/// angle turns clockwise on screen).
#[derive(Copy, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Similarity {
    /// The uniform scale.
    pub scale: f64,
    /// The rotation, in degrees.
    pub angle_deg: f64,
    /// The shift, in pixels.
    pub shift: [f64; 2],
}

impl Default for Similarity {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Similarity {
    /// No change.
    pub const IDENTITY: Self = Self { scale: 1.0, angle_deg: 0.0, shift: [0.0, 0.0] };

    /// Where `p` goes.
    #[must_use]
    pub fn apply(&self, p: [f64; 2]) -> [f64; 2] {
        let (s, c) = self.angle_deg.to_radians().sin_cos();
        [self.scale * (c * p[0] - s * p[1]) + self.shift[0], self.scale * (s * p[0] + c * p[1]) + self.shift[1]]
    }

    /// The transform that undoes this one.
    #[must_use]
    pub fn inverse(&self) -> Self {
        let scale = if self.scale.abs() < 1e-12 { 1.0 } else { 1.0 / self.scale };
        let back = Self { scale, angle_deg: -self.angle_deg, shift: [0.0, 0.0] };
        let moved = back.apply(self.shift);
        Self { shift: [-moved[0], -moved[1]], ..back }
    }

    /// Whether this is the identity within a hundredth of a pixel and degree.
    #[must_use]
    pub fn is_identity(&self) -> bool {
        (self.scale - 1.0).abs() < 1e-4
            && self.angle_deg.abs() < 1e-2
            && self.shift[0].abs() < 1e-2
            && self.shift[1].abs() < 1e-2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_similarity_and_its_inverse_cancel() {
        let t = Similarity { scale: 1.3, angle_deg: 12.0, shift: [5.0, -7.0] };
        let p = [31.0, 17.5];
        let back = t.inverse().apply(t.apply(p));
        assert!((back[0] - p[0]).abs() < 1e-9 && (back[1] - p[1]).abs() < 1e-9, "{back:?}");
    }

    #[test]
    fn a_blur_keeps_a_flat_plane_flat_and_spreads_a_dot() {
        let flat = Plane::new(9, 9, 4.0).blur(1.5);
        assert!(flat.v.iter().all(|v| (v - 4.0).abs() < 1e-4));
        let mut dot = Plane::new(9, 9, 0.0);
        dot.v[40] = 1.0;
        let spread = dot.blur(1.0);
        assert!(spread.v[40] < 0.5 && spread.v[41] > 0.0);
        assert!((spread.v.iter().sum::<f32>() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn bilinear_sampling_hits_pixel_centres_exactly() {
        let p = Plane { w: 2, h: 1, v: vec![0.0, 10.0] };
        assert!((p.sample(0.5, 0.5) - 0.0).abs() < 1e-6);
        assert!((p.sample(1.5, 0.5) - 10.0).abs() < 1e-6);
        assert!((p.sample(1.0, 0.5) - 5.0).abs() < 1e-6);
    }
}
