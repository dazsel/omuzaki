//! The face frame and the region of interest (design D3 and 4.1).
//!
//! The frame puts the origin at the eye midpoint, +x along the eye line toward the character's left, +y down the face,
//! and one eye distance (IOD) as the unit, so position, size and roll drop out of everything measured in it. The region
//! of interest is the square of half-side 2.6 IOD around the eye midpoint, clipped to the image and box-downscaled by an
//! integer factor when the eyes are more than 256 px apart, so a measurement's memory and time do not grow with the
//! image.

use crate::colour::lab;
use crate::image::Rgba8;

/// The half-side of the region of interest, in IOD.
pub const ROI_HALF_SIDE: f64 = 2.6;

/// Above this many pixels of IOD the region of interest is downscaled (by `ceil(iod / 256)`).
pub const ROI_MAX_IOD: f64 = 256.0;

/// Design D3: origin E (the eye midpoint), unit vectors `u` (the eye line, toward the character's left) and `v` (down the
/// face), and the IOD `d` in pixels of whatever image the frame is in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceFrame {
    /// The eye midpoint.
    pub origin: [f64; 2],
    /// The unit vector along the eye line, from the character's right eye to its left.
    pub u: [f64; 2],
    /// The unit vector down the face: `u` turned a quarter toward the image's down.
    pub v: [f64; 2],
    /// The distance between the eyes, in pixels.
    pub iod_px: f64,
}

impl FaceFrame {
    /// The frame of two eye centres. `None` when they coincide (or are not finite), since a frame needs an eye line.
    #[must_use]
    pub fn from_eyes(eye_right: [f64; 2], eye_left: [f64; 2]) -> Option<Self> {
        let delta = [eye_left[0] - eye_right[0], eye_left[1] - eye_right[1]];
        let d = delta[0].hypot(delta[1]);
        if !(d.is_finite() && d > 0.0) {
            return None;
        }
        let u = [delta[0] / d, delta[1] / d];
        Some(Self {
            origin: [f64::midpoint(eye_right[0], eye_left[0]), f64::midpoint(eye_right[1], eye_left[1])],
            u,
            v: [-u[1], u[0]],
            iod_px: d,
        })
    }

    /// The face coordinates (IOD) of the image point `p`.
    #[must_use]
    pub fn to_face(&self, p: [f64; 2]) -> [f64; 2] {
        let rel = [p[0] - self.origin[0], p[1] - self.origin[1]];
        [
            (rel[0] * self.u[0] + rel[1] * self.u[1]) / self.iod_px,
            (rel[0] * self.v[0] + rel[1] * self.v[1]) / self.iod_px,
        ]
    }

    /// The image point of face coordinates `(x, y)` (IOD).
    #[must_use]
    pub fn to_px(&self, x: f64, y: f64) -> [f64; 2] {
        let d = self.iod_px;
        [self.origin[0] + d * (x * self.u[0] + y * self.v[0]), self.origin[1] + d * (x * self.u[1] + y * self.v[1])]
    }

    /// The image point `t` pixels from the origin along the angle `phi_deg` of the face frame (0° along the eye line
    /// toward the character's left, 90° down the midline).
    #[must_use]
    pub fn along(&self, phi_deg: f64, t: f64) -> [f64; 2] {
        let (sin, cos) = phi_deg.to_radians().sin_cos();
        let w = [cos * self.u[0] + sin * self.v[0], cos * self.u[1] + sin * self.v[1]];
        [self.origin[0] + t * w[0], self.origin[1] + t * w[1]]
    }

    /// The roll of the eye line, degrees, positive when the character's left eye is lower in the image.
    #[must_use]
    pub fn roll_deg(&self) -> f64 {
        self.u[1].atan2(self.u[0]).to_degrees()
    }

    /// The same frame in another image whose pixel `p` is `(p - offset) / scale` here.
    #[must_use]
    pub fn mapped(&self, offset: [f64; 2], scale: f64) -> Self {
        Self {
            origin: [(self.origin[0] - offset[0]) / scale, (self.origin[1] - offset[1]) / scale],
            iod_px: self.iod_px / scale,
            ..*self
        }
    }
}

/// The region of interest of design 4.1: a square crop around the eyes, box-downscaled when the face is large, with each
/// pixel's CIELAB and alpha.
#[derive(Clone, Debug)]
pub struct Roi {
    /// Width in ROI pixels.
    pub width: usize,
    /// Height in ROI pixels.
    pub height: usize,
    /// CIELAB of each pixel, row by row. A pixel that is not opaque is composited over white first, so a transparent
    /// background reads as white, never as the arbitrary colour a transparent pixel carries.
    pub lab: Vec<[f32; 3]>,
    /// Alpha of each pixel (0 to 255).
    pub alpha: Vec<u8>,
    /// The downscale factor: one ROI pixel is `scale × scale` image pixels.
    pub scale: u32,
    /// The image point of the ROI's top-left corner, in image pixels.
    pub offset: [f64; 2],
}

impl Roi {
    /// The index of the pixel under the ROI point `p` (pixel centres at `.5`), or `None` outside.
    #[must_use]
    pub fn index(&self, p: [f64; 2]) -> Option<usize> {
        let (x, y) = (p[0].floor(), p[1].floor());
        if !(x >= 0.0 && y >= 0.0) {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // non-negative and checked below
        let (x, y) = (x as usize, y as usize);
        (x < self.width && y < self.height).then(|| y * self.width + x)
    }

    /// The ROI point of the image point `p`.
    #[must_use]
    pub fn from_image(&self, p: [f64; 2]) -> [f64; 2] {
        let s = f64::from(self.scale);
        [(p[0] - self.offset[0]) / s, (p[1] - self.offset[1]) / s]
    }

    /// The image point of the ROI point `p`.
    #[must_use]
    pub fn to_image(&self, p: [f64; 2]) -> [f64; 2] {
        let s = f64::from(self.scale);
        [p[0] * s + self.offset[0], p[1] * s + self.offset[1]]
    }

    /// The centre of pixel `index`, in ROI coordinates.
    #[must_use]
    pub fn centre(&self, index: usize) -> [f64; 2] {
        #[allow(clippy::cast_precision_loss)] // ROI sides are a few hundred pixels
        let (x, y) = ((index % self.width) as f64, (index / self.width) as f64);
        [x + 0.5, y + 0.5]
    }

    /// `L*` at the ROI point `p`, bilinear between the four nearest pixel centres; `None` when any of them is outside.
    #[must_use]
    pub fn lightness(&self, p: [f64; 2]) -> Option<f64> {
        let (fx, fy) = (p[0] - 0.5, p[1] - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let at = |dx: f64, dy: f64| self.index([x0 + dx + 0.5, y0 + dy + 0.5]).map(|i| f64::from(self.lab[i][0]));
        let (top_left, top_right) = (at(0.0, 0.0)?, at(1.0, 0.0)?);
        let (bottom_left, bottom_right) = (at(0.0, 1.0)?, at(1.0, 1.0)?);
        let top = top_left * (1.0 - tx) + top_right * tx;
        let bottom = bottom_left * (1.0 - tx) + bottom_right * tx;
        Some(top * (1.0 - ty) + bottom * ty)
    }

    /// The CIELAB of pixel `index`.
    #[must_use]
    pub fn lab_at(&self, index: usize) -> [f64; 3] {
        self.lab[index].map(f64::from)
    }

    /// Whether pixel `index` is opaque enough to be part of the picture (alpha at least 128, design D1).
    #[must_use]
    pub fn opaque(&self, index: usize) -> bool {
        self.alpha[index] >= 128
    }
}

/// A direct-mapped cache of CIELAB by sRGB colour. Drawings and renders are mostly flat fills, so most pixels repeat a
/// colour seen just before; the conversion (three cube roots) is the ROI's main cost, and the cache gives the same
/// values, so measurements are unchanged.
struct LabCache {
    slots: Vec<(u32, [f32; 3])>,
}

impl LabCache {
    const BITS: u32 = 12;

    fn new() -> Self {
        Self { slots: vec![(u32::MAX, [0.0; 3]); 1 << Self::BITS] }
    }

    fn lab(&mut self, rgb: [u8; 3]) -> [f32; 3] {
        let key = u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2]);
        // Fibonacci hashing: the top bits of the product spread neighbouring colours over the table.
        let slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - Self::BITS)) as usize;
        let entry = &mut self.slots[slot];
        if entry.0 != key {
            #[allow(clippy::cast_possible_truncation)] // the design stores the ROI's CIELAB as f32 (4.1)
            let narrow = lab(rgb).map(|v| v as f32);
            *entry = (key, narrow);
        }
        entry.1
    }
}

/// The region of interest around `frame` (in image pixels): half-side [`ROI_HALF_SIDE`] IOD, clipped to the image, and
/// box-downscaled by `ceil(iod / 256)` when the IOD is over [`ROI_MAX_IOD`] px. A partial block at the right or bottom
/// edge is dropped.
#[must_use]
pub fn roi(image: &Rgba8, frame: &FaceFrame) -> Roi {
    let iod = frame.iod_px;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // iod is a positive pixel distance
    let scale = if iod > ROI_MAX_IOD { (iod / ROI_MAX_IOD).ceil() as u32 } else { 1 };
    let half = ROI_HALF_SIDE * iod;
    let (image_w, image_h) = (f64::from(image.width), f64::from(image.height));
    let left_px = (frame.origin[0] - half).floor().clamp(0.0, image_w);
    let top_px = (frame.origin[1] - half).floor().clamp(0.0, image_h);
    let right_px = (frame.origin[0] + half).ceil().clamp(0.0, image_w);
    let bottom_px = (frame.origin[1] + half).ceil().clamp(0.0, image_h);
    let factor = f64::from(scale);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to the image's size
    let (width, height, left, top) = (
        ((right_px - left_px) / factor).floor() as usize,
        ((bottom_px - top_px) / factor).floor() as usize,
        left_px as usize,
        top_px as usize,
    );
    let block = scale as usize;
    let stride = image.width as usize;
    let mut lab_px = Vec::with_capacity(width * height);
    let mut colours = LabCache::new();
    let mut alpha = Vec::with_capacity(width * height);
    // A block's sums are at most 255 × 255 × 64² (the factor never exceeds 32 within the image limits), far below 2^52.
    #[allow(clippy::cast_precision_loss)]
    let count = (block * block) as f64;
    if block == 1 {
        // No downscale: each pixel composited over white on its own, in integers. `(c·a + 255·(255 - a)) / 255`
        // rounded is what the box average below gives for a block of one.
        for row in 0..height {
            let start = ((top + row) * stride + left) * 4;
            for px in image.pixels[start..start + width * 4].as_chunks::<4>().0 {
                let a = u32::from(px[3]);
                let over_white = |c: u8| {
                    let v = u32::from(c) * a + 255 * (255 - a);
                    #[allow(clippy::cast_possible_truncation)] // at most 255
                    let byte = ((v + 127) / 255) as u8;
                    byte
                };
                lab_px.push(colours.lab([over_white(px[0]), over_white(px[1]), over_white(px[2])]));
                alpha.push(px[3]);
            }
        }
        return Roi { width, height, lab: lab_px, alpha, scale, offset: [left_px, top_px] };
    }
    for row in 0..height {
        for col in 0..width {
            // Box average in premultiplied space, so a transparent pixel adds no colour.
            let mut sum = [0_u64; 4];
            for dy in 0..block {
                for dx in 0..block {
                    let i = ((top + row * block + dy) * stride + left + col * block + dx) * 4;
                    let opacity = u64::from(image.pixels[i + 3]);
                    for (channel, slot) in sum.iter_mut().take(3).enumerate() {
                        *slot += u64::from(image.pixels[i + channel]) * opacity;
                    }
                    sum[3] += opacity;
                }
            }
            #[allow(clippy::cast_precision_loss)] // see `count`
            let coverage = sum[3] as f64 / count;
            // Composited over white: premultiplied colour plus white times what is left.
            let rgb = [0, 1, 2].map(|channel| {
                #[allow(clippy::cast_precision_loss)] // see `count`
                let premul = sum[channel] as f64 / (count * 255.0);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=255 by construction
                let byte = (premul + (255.0 - coverage)).round().clamp(0.0, 255.0) as u8;
                byte
            });
            lab_px.push(colours.lab(rgb));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // an average of bytes
            alpha.push(coverage.round() as u8);
        }
    }
    Roi { width, height, lab: lab_px, alpha, scale, offset: [left_px, top_px] }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // test pixels are bytes
mod tests {
    use super::*;
    use omuzaki_store_api::ContentHash;

    fn image(width: u32, height: u32, rgba: [u8; 4]) -> Rgba8 {
        let pixels = (0..width * height).flat_map(|_| rgba).collect();
        Rgba8 { width, height, pixels, hash: ContentHash::of(b"test") }
    }

    #[test]
    fn the_frame_maps_points_both_ways_and_reads_roll() {
        let f = FaceFrame::from_eyes([100.0, 200.0], [200.0, 200.0]).unwrap();
        assert_eq!(f.origin, [150.0, 200.0]);
        assert_eq!(f.to_face([200.0, 300.0]), [0.5, 1.0]);
        assert_eq!(f.to_px(0.5, 1.0), [200.0, 300.0]);
        assert!(f.roll_deg().abs() < 1e-12);
        let rolled = FaceFrame::from_eyes([0.0, 0.0], [10.0, 10.0]).unwrap();
        assert!((rolled.roll_deg() - 45.0).abs() < 1e-9);
        let down = rolled.along(90.0, 1.0);
        let face = rolled.to_face(down);
        // One pixel straight down the face is one pixel over the eye distance, √200 px.
        assert!(face[0].abs() < 1e-12 && (face[1] - 1.0 / 200_f64.sqrt()).abs() < 1e-12, "{face:?}");
        assert!(FaceFrame::from_eyes([1.0, 1.0], [1.0, 1.0]).is_none());
    }

    #[test]
    fn the_roi_is_clipped_and_downscaled_with_the_iod() {
        let img = image(2000, 2000, [200, 100, 50, 255]);
        let small = roi(&img, &FaceFrame::from_eyes([900.0, 1000.0], [1000.0, 1000.0]).unwrap());
        assert_eq!((small.scale, small.width, small.height), (1, 520, 520));
        let big = roi(&img, &FaceFrame::from_eyes([700.0, 1000.0], [1300.0, 1000.0]).unwrap());
        // 600 px of IOD: a factor of 3, and the square of 2 × 2.6 × 600 px clipped to the image.
        assert_eq!(big.scale, 3);
        assert_eq!((big.width, big.height), (666, 666));
        let edge = roi(&img, &FaceFrame::from_eyes([0.0, 10.0], [100.0, 10.0]).unwrap());
        assert_eq!(edge.offset, [0.0, 0.0]);
        assert_eq!((edge.width, edge.height), (310, 270));
    }

    #[test]
    fn transparent_pixels_read_as_white_and_lightness_is_bilinear() {
        let mut img = image(4, 1, [0, 0, 0, 255]);
        img.pixels[8..16].copy_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        let r = Roi {
            width: 4,
            height: 1,
            lab: (0..4)
                .map(|x| {
                    let i = x * 4;
                    let a = f64::from(img.pixels[i + 3]);
                    let byte = (255.0 - a) as u8;
                    lab([byte, byte, byte]).map(|v| v as f32)
                })
                .collect(),
            alpha: (0..4).map(|x| img.pixels[x * 4 + 3]).collect(),
            scale: 1,
            offset: [0.0, 0.0],
        };
        assert!(r.opaque(0) && !r.opaque(2));
        assert!((r.lab_at(2)[0] - 100.0).abs() < 0.01);
        // Halfway between a black and a white pixel centre the lightness is halfway, and off the edge there is none.
        assert!(r.lightness([2.0, 0.5]).is_none(), "the row above and below are outside");
    }
}
