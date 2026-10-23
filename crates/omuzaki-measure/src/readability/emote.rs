//! The face reads at emote size (spec §32): a set of expressions seen at the sizes a viewer meets them, a Twitch emote's
//! 112, 56 and 28 px, over a stream's dark ground.
//!
//! Each expression image is laid over the ground (its alpha kept), scaled with a triangle filter (close to what a chat
//! client's scaler does) so its longer side is the size, and centred on a square of that size. Two expressions read the
//! same at a size when their **similarity** is above a threshold. Similarity is the structural similarity (SSIM, Wang et
//! al. 2004: 8 x 8 windows, a stride of 4, the paper's constants), the lowest over the red, green and blue channels,
//! taken the way the face's own range test takes it (`omuzaki-face`, FE-04), so the two agree on a painted face:
//! - with the eyes' and the mouth's boxes given, the lowest of the eyes' SSIM, the mouth's, and the whole picture's
//!   over its least similar 3% of windows;
//! - without them, the whole picture's over its least similar 3% of windows, so a change that is small next to the
//!   frame (a mouth on a full-body render) is not averaged away.
//!
//! With the boxes and the skin colour given, each size also gets the eyes' and the mouth's **contrast**: the skin's
//! lightness `L*` less the darkest pixel's in the box, on the first expression (the resting face). A line that blurs
//! into the skin at 28 px loses its contrast first.

use image::{Rgba, RgbaImage, imageops};

use crate::colour::lab;
use crate::image::Rgba8;

/// The emote sizes (Twitch: 112, 56 and 28 px squares).
pub const SIZES: [u32; 3] = [112, 56, 28];

/// Twitch's dark theme behind chat, `#18181b`: where an emote is seen most.
pub const STREAM_DARK: [u8; 3] = [0x18, 0x18, 0x1b];

/// The similarity above which two expressions read the same: the face range test's threshold (FE-04), measured on the
/// default face, where a pair that differs only by both outer brows raised a tenth scores above it at 56 px.
pub const SIMILAR: f64 = 0.94;

/// The least contrast, `ΔL*`, of the eyes and the mouth against the skin at every size: the value step a viewer still
/// tells apart at a glance.
pub const MIN_CONTRAST: f64 = 15.0;

/// The share of the least similar windows the whole picture is judged by.
const WINDOW_WORST: f64 = 0.03;

/// A box in an image's pixels: left, top, right and bottom edges, origin top-left.
pub type Region = [f64; 4];

/// One expression at one size.
#[derive(Clone, Debug)]
pub struct Seen {
    /// The square picture.
    pub picture: RgbaImage,
    /// The eyes' box, cropped from it.
    pub eyes: Option<RgbaImage>,
    /// The mouth's box.
    pub mouth: Option<RgbaImage>,
}

/// `image` over `ground`, scaled so its longer side is `size`, on a `size` square; `eyes` and `mouth` are boxes in the
/// image's pixels, cropped at the new size.
#[must_use]
pub fn see(image: &Rgba8, size: u32, ground: [u8; 3], eyes: Option<Region>, mouth: Option<Region>) -> Seen {
    let size = size.max(1);
    let mut flat = RgbaImage::new(image.width.max(1), image.height.max(1));
    for (out, p) in flat.pixels_mut().zip(image.pixels.as_chunks::<4>().0) {
        let a = u32::from(p[3]);
        let mix = |c: u8, g: u8| {
            // A weighted mean of two bytes is a byte.
            #[allow(clippy::cast_possible_truncation)]
            let v = ((u32::from(c) * a + u32::from(g) * (255 - a) + 127) / 255) as u8;
            v
        };
        *out = Rgba([mix(p[0], ground[0]), mix(p[1], ground[1]), mix(p[2], ground[2]), 255]);
    }
    let longer = f64::from(image.width.max(image.height).max(1));
    let scale = f64::from(size) / longer;
    // Rounded sides of at most `size`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let fitted = |v: u32| ((f64::from(v) * scale).round() as u32).clamp(1, size);
    let (w, h) = (fitted(image.width.max(1)), fitted(image.height.max(1)));
    let scaled = imageops::resize(&flat, w, h, imageops::FilterType::Triangle);
    let mut picture = RgbaImage::from_pixel(size, size, Rgba([ground[0], ground[1], ground[2], 255]));
    let (left, top) = ((size - w) / 2, (size - h) / 2);
    imageops::replace(&mut picture, &scaled, i64::from(left), i64::from(top));
    let crop = |r: Region| {
        let to = |v: f64, offset: u32| (v * scale + f64::from(offset)).clamp(0.0, f64::from(size));
        // Inside the square after the clamp.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (x0, y0) = (to(r[0], left).floor() as u32, to(r[1], top).floor() as u32);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (x1, y1) = (to(r[2], left).ceil() as u32, to(r[3], top).ceil() as u32);
        let (x0, y0) = (x0.min(size - 1), y0.min(size - 1));
        imageops::crop_imm(&picture, x0, y0, x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1)).to_image()
    };
    Seen { eyes: eyes.map(crop), mouth: mouth.map(crop), picture }
}

/// How alike two expressions read at one size, `-1..=1` (1 the same): see the module docs.
#[must_use]
pub fn similarity(a: &Seen, b: &Seen) -> f64 {
    let mut score = ssim(&a.picture, &b.picture, WINDOW_WORST);
    for (p, q) in [(&a.eyes, &b.eyes), (&a.mouth, &b.mouth)] {
        if let (Some(p), Some(q)) = (p, q) {
            score = score.min(ssim(p, q, 1.0));
        }
    }
    score
}

/// The skin's lightness less the darkest pixel's in `region`; 0 for an empty region.
#[must_use]
pub fn contrast(region: &RgbaImage, skin: [u8; 3]) -> f64 {
    let darkest = region.pixels().map(|p| lab([p[0], p[1], p[2]])[0]).fold(f64::INFINITY, f64::min);
    if darkest.is_finite() { lab(skin)[0] - darkest } else { 0.0 }
}

/// The SSIM window side and stride, pixels, and the paper's constants for a range of 1.
const WINDOW: u32 = 8;
const STRIDE: u32 = 4;
const C1: f64 = 0.0001;
const C2: f64 = 0.0009;

/// The mean SSIM over the least similar `share` of windows, the lowest over the three channels; 0 for pictures of
/// different sizes.
#[must_use]
pub fn ssim(first: &RgbaImage, second: &RgbaImage, share: f64) -> f64 {
    if first.dimensions() != second.dimensions() {
        return 0.0;
    }
    (0..3).map(|c| channel_ssim(first, second, c, share)).fold(1.0, f64::min)
}

fn channel_ssim(first: &RgbaImage, second: &RgbaImage, channel: usize, share: f64) -> f64 {
    let (width, height) = first.dimensions();
    let plane = |image: &RgbaImage| image.pixels().map(|p| f64::from(p[channel]) / 255.0).collect::<Vec<_>>();
    let (p, q) = (plane(first), plane(second));
    let (side_x, side_y) = (WINDOW.min(width), WINDOW.min(height));
    let mut scores = Vec::new();
    for top in (0..=height - side_y).step_by(STRIDE as usize) {
        for left in (0..=width - side_x).step_by(STRIDE as usize) {
            let count = f64::from(side_x * side_y);
            let (mut sp, mut sq, mut pp, mut qq, mut pq) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for row in top..top + side_y {
                for column in left..left + side_x {
                    let at = (row * width + column) as usize;
                    let (a, b) = (p.get(at).copied().unwrap_or(0.0), q.get(at).copied().unwrap_or(0.0));
                    (sp, sq, pp, qq, pq) = (sp + a, sq + b, pp + a * a, qq + b * b, pq + a * b);
                }
            }
            let (mp, mq) = (sp / count, sq / count);
            let (vp, vq, cov) = (pp / count - mp * mp, qq / count - mq * mq, pq / count - mp * mq);
            scores.push(((2.0 * mp * mq + C1) * (2.0 * cov + C2)) / ((mp * mp + mq * mq + C1) * (vp + vq + C2)));
        }
    }
    if scores.is_empty() {
        return 1.0;
    }
    scores.sort_by(f64::total_cmp);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a window count
    let keep = ((scores.len() as f64 * share.clamp(0.0, 1.0)).ceil() as usize).clamp(1, scores.len());
    #[allow(clippy::cast_precision_loss)] // a window count
    let mean = scores[..keep].iter().sum::<f64>() / keep as f64;
    mean
}

#[cfg(test)]
mod tests {
    use omuzaki_store_api::ContentHash;

    use super::*;

    const SKIN: [u8; 3] = [249, 211, 197];
    const INK: [u8; 3] = [40, 30, 40];

    /// A 400 px face: skin, two eyes and a mouth drawn by `eye` (half-height of the eyes) and `mouth` (its half-width
    /// and half-height), with a `line` px dark outline round the face disc.
    fn face(eye: f64, mouth: (f64, f64), line: f64) -> Rgba8 {
        let mut pixels = Vec::new();
        for y in 0..400 {
            for x in 0..400 {
                let (fx, fy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                let r = (fx - 200.0).hypot(fy - 200.0);
                let in_eye = [140.0, 260.0].iter().any(|cx| (fx - cx).abs() <= 22.0 && (fy - 170.0).abs() <= eye);
                let in_mouth = ((fx - 200.0) / mouth.0).powi(2) + ((fy - 280.0) / mouth.1).powi(2) <= 1.0;
                let rgb = if r > 190.0 {
                    None
                } else if r > 190.0 - line || in_eye || in_mouth {
                    Some(INK)
                } else {
                    Some(SKIN)
                };
                pixels.extend_from_slice(&rgb.map_or([0, 0, 0, 0], |[r, g, b]| [r, g, b, 255]));
            }
        }
        Rgba8 { width: 400, height: 400, pixels, hash: ContentHash::of(b"") }
    }

    const EYES: Region = [100.0, 120.0, 300.0, 220.0];
    const MOUTH: Region = [130.0, 240.0, 270.0, 320.0];

    fn seen(image: &Rgba8, size: u32) -> Seen {
        see(image, size, STREAM_DARK, Some(EYES), Some(MOUTH))
    }

    #[test]
    fn a_picture_is_fitted_and_centred_on_the_ground() {
        let wide = Rgba8 {
            width: 200,
            height: 100,
            pixels: [255, 255, 255, 255].repeat(200 * 100),
            hash: ContentHash::of(b""),
        };
        let s = see(&wide, 28, STREAM_DARK, None, None);
        assert_eq!(s.picture.dimensions(), (28, 28));
        assert_eq!(s.picture.get_pixel(14, 2).0[..3], STREAM_DARK);
        assert_eq!(s.picture.get_pixel(14, 14).0, [255, 255, 255, 255]);
    }

    #[test]
    fn the_same_face_reads_the_same_and_a_different_mouth_reads_apart() {
        let rest = face(18.0, (40.0, 4.0), 6.0);
        let open = face(18.0, (40.0, 30.0), 6.0);
        let closed_eyes = face(3.0, (40.0, 4.0), 6.0);
        for size in SIZES {
            let (r, o, c) = (seen(&rest, size), seen(&open, size), seen(&closed_eyes, size));
            assert!((similarity(&r, &seen(&rest, size)) - 1.0).abs() < 1e-9);
            assert!(similarity(&r, &o) < SIMILAR, "{size}: {}", similarity(&r, &o));
            assert!(similarity(&r, &c) < SIMILAR, "{size}: {}", similarity(&r, &c));
        }
    }

    #[test]
    fn a_hairline_change_reads_apart_large_and_blurs_together_at_28() {
        let a = face(18.0, (40.0, 4.0), 6.0);
        let b = face(18.0, (41.5, 4.0), 6.0);
        let (large, small) = (similarity(&seen(&a, 112), &seen(&b, 112)), similarity(&seen(&a, 28), &seen(&b, 28)));
        assert!(small > SIMILAR, "28 px: {small}");
        assert!(small >= large - 1e-9, "{large} at 112 against {small} at 28");
    }

    #[test]
    fn a_thin_mouth_loses_its_contrast_first() {
        let thin = face(18.0, (40.0, 1.5), 6.0);
        let bold = face(18.0, (40.0, 6.0), 6.0);
        let mouth = |image: &Rgba8, size| contrast(seen(image, size).mouth.as_ref().unwrap(), SKIN);
        assert!(mouth(&bold, 112) > 50.0, "{}", mouth(&bold, 112));
        assert!(mouth(&thin, 28) < mouth(&bold, 28), "{} {}", mouth(&thin, 28), mouth(&bold, 28));
        assert!(mouth(&thin, 28) < mouth(&thin, 112));
    }
}
