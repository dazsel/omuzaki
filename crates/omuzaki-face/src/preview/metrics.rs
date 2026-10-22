//! How different two face pictures are: pixel counts for the channel sweep, and the structural similarity (SSIM) and a
//! luminance distance for the expression range test.
//!
//! SSIM is Wang et al. 2004 over 8 x 8 windows with a stride of 4 and the paper's constants, which is the usual form for
//! small images and needs no dependency. It is taken on each of the sRGB channels and the lowest wins: a face change
//! can be colour alone (a light blue sweat drop or a blush on pale skin has nearly the skin's luma), and a luma-only
//! SSIM would call it unchanged. The luma distance is Rec. 709 on the sRGB bytes: the face is judged by how it reads,
//! not by its light.

use image::RgbaImage;
use omuzaki_face_api::Raster;

/// Pixels whose largest channel difference is at most this are counted as unchanged: it absorbs the last bit of
/// anti-aliasing rounding, so only a visible change counts.
pub const PIXEL_TOLERANCE: u8 = 8;

/// How one layer changed between two rasters of the same layer.
#[derive(Copy, Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct LayerChange {
    /// Pixels whose largest channel difference is more than [`PIXEL_TOLERANCE`].
    pub changed_px: u32,
    /// The mean absolute difference over every byte, `0..=1`.
    pub distance: f64,
}

/// How `b` differs from `a`, two premultiplied rasters of one layer. Rasters of different sizes (which one layer never
/// has) count every pixel of the larger as changed.
pub fn layer_change(a: &Raster, b: &Raster) -> LayerChange {
    if (a.width, a.height) != (b.width, b.height) {
        let px = a.width.max(b.width).saturating_mul(a.height.max(b.height));
        return LayerChange { changed_px: px, distance: 1.0 };
    }
    let mut changed = 0u32;
    let mut sum = 0u64;
    for (pa, pb) in a.pixels.as_chunks::<4>().0.iter().zip(b.pixels.as_chunks::<4>().0) {
        let mut most = 0u8;
        for (x, y) in pa.iter().zip(pb) {
            let d = x.abs_diff(*y);
            most = most.max(d);
            sum += u64::from(d);
        }
        if most > PIXEL_TOLERANCE {
            changed += 1;
        }
    }
    let bytes = a.pixels.len().max(1);
    #[allow(clippy::cast_precision_loss)] // a ratio of counts
    let distance = sum as f64 / (bytes as f64 * 255.0);
    LayerChange { changed_px: changed, distance }
}

/// Where the visible change between two rasters of one layer falls: pixels changed (as [`layer_change`] counts them) on
/// the character's left half of the face (`+x`, the right half of the raster) and on its right half. The centre column
/// of an odd width counts on neither side. Rasters of different sizes count nothing.
pub fn side_change(a: &Raster, b: &Raster) -> super::direction::SideChange {
    let mut sides = super::direction::SideChange::default();
    if (a.width, a.height) != (b.width, b.height) || a.width == 0 {
        return sides;
    }
    let width = a.width as usize;
    let pixels = a.pixels.as_chunks::<4>().0.iter().zip(b.pixels.as_chunks::<4>().0);
    for (i, (pa, pb)) in pixels.enumerate() {
        if pa.iter().zip(pb).all(|(x, y)| x.abs_diff(*y) <= PIXEL_TOLERANCE) {
            continue;
        }
        // Twice the column against the width, so the centre of an odd width lands on neither half.
        let column = 2 * (i % width) + 1;
        if column > width {
            sides.left += 1;
        } else if column < width {
            sides.right += 1;
        }
    }
    sides
}

/// Luma of every pixel, `0..=1`, row by row.
fn luma(image: &RgbaImage) -> Vec<f64> {
    image
        .pixels()
        .map(|p| (0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])) / 255.0)
        .collect()
}

/// The SSIM window side and stride, in pixels.
const WINDOW: u32 = 8;
const STRIDE: u32 = 4;
/// The paper's stabilising constants for a dynamic range of 1: `(0.01 L)^2` and `(0.03 L)^2`.
const C1: f64 = 0.0001;
const C2: f64 = 0.0009;

/// The mean SSIM of two pictures of one size, `-1..=1` (1 = the same): the lowest over the red, green and blue channels.
/// Pictures smaller than a window are compared as one window. Pictures of different sizes score 0.
pub fn ssim(first: &RgbaImage, second: &RgbaImage) -> f64 {
    if first.dimensions() != second.dimensions() {
        return 0.0;
    }
    (0..3).map(|c| channel_ssim(first, second, c, 1.0)).fold(1.0, f64::min)
}

/// [`ssim`] over only the least similar `share` of the windows (`0 < share <= 1`): a small local change, a sweat drop
/// on a whole face, is not averaged away by the windows that did not change.
pub fn ssim_worst(first: &RgbaImage, second: &RgbaImage, share: f64) -> f64 {
    if first.dimensions() != second.dimensions() {
        return 0.0;
    }
    (0..3).map(|c| channel_ssim(first, second, c, share)).fold(1.0, f64::min)
}

/// One channel of an sRGB picture, `0..=1`, row by row.
fn plane(image: &RgbaImage, channel: usize) -> Vec<f64> {
    image.pixels().map(|p| f64::from(p[channel]) / 255.0).collect()
}

/// The mean SSIM of one channel over the least similar `share` of its windows.
fn channel_ssim(first: &RgbaImage, second: &RgbaImage, channel: usize, share: f64) -> f64 {
    let (width, height) = first.dimensions();
    let planes = Planes { first: plane(first, channel), second: plane(second, channel), width };
    let (side_x, side_y) = (WINDOW.min(width), WINDOW.min(height));
    let mut scores = Vec::new();
    for top in (0..=height - side_y).step_by(STRIDE as usize) {
        for left in (0..=width - side_x).step_by(STRIDE as usize) {
            scores.push(planes.window_ssim(left..left + side_x, top..top + side_y));
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

/// The luma planes of two pictures of one width.
struct Planes {
    first: Vec<f64>,
    second: Vec<f64>,
    width: u32,
}

impl Planes {
    /// SSIM of the window over `columns` and `rows`.
    fn window_ssim(&self, columns: std::ops::Range<u32>, rows: std::ops::Range<u32>) -> f64 {
        let count = f64::from((columns.end - columns.start) * (rows.end - rows.start));
        let (mut sum_p, mut sum_q, mut squares_p, mut squares_q, mut products) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for row in rows {
            for column in columns.clone() {
                let at = (row * self.width + column) as usize;
                let (p, q) = (self.first.get(at).copied().unwrap_or(0.0), self.second.get(at).copied().unwrap_or(0.0));
                sum_p += p;
                sum_q += q;
                squares_p += p * p;
                squares_q += q * q;
                products += p * q;
            }
        }
        let (mean_p, mean_q) = (sum_p / count, sum_q / count);
        let var_p = squares_p / count - mean_p * mean_p;
        let var_q = squares_q / count - mean_q * mean_q;
        let cov = products / count - mean_p * mean_q;
        ((2.0 * mean_p * mean_q + C1) * (2.0 * cov + C2))
            / ((mean_p * mean_p + mean_q * mean_q + C1) * (var_p + var_q + C2))
    }
}

/// The root mean square luma difference of two pictures of one size, `0..=1`; pictures of different sizes score 1.
pub fn luma_l2(a: &RgbaImage, b: &RgbaImage) -> f64 {
    if a.dimensions() != b.dimensions() {
        return 1.0;
    }
    let (la, lb) = (luma(a), luma(b));
    let n = la.len().max(1);
    let sum: f64 = la.iter().zip(&lb).map(|(x, y)| (x - y) * (x - y)).sum();
    #[allow(clippy::cast_precision_loss)] // a pixel count
    let mean = sum / n as f64;
    mean.sqrt()
}

/// `image` scaled to `width` pixels wide (the height keeps the aspect), with a triangle filter: deterministic, and close
/// to what a stream's scaler does to a face.
pub fn downsample(image: &RgbaImage, width: u32) -> RgbaImage {
    let (w, h) = image.dimensions();
    if w == width || w == 0 {
        return image.clone();
    }
    let height = u32::try_from(u64::from(h) * u64::from(width) / u64::from(w)).unwrap_or(1).max(1);
    image::imageops::resize(image, width.max(1), height, image::imageops::FilterType::Triangle)
}

#[cfg(test)]
mod tests {
    use image::Rgba;

    use super::*;

    fn flat(w: u32, h: u32, v: u8) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([v, v, v, 255]))
    }

    #[test]
    fn identical_pictures_are_ssim_1_and_l2_0() {
        let mut a = flat(32, 24, 200);
        a.put_pixel(5, 5, Rgba([0, 0, 0, 255]));
        assert!((ssim(&a, &a) - 1.0).abs() < 1e-12);
        assert_eq!(luma_l2(&a, &a), 0.0);
    }

    #[test]
    fn a_mark_lowers_ssim_and_a_bigger_one_lowers_it_more() {
        let a = flat(32, 32, 200);
        let mut small = a.clone();
        let mut big = a.clone();
        for y in 10..12 {
            for x in 10..12 {
                small.put_pixel(x, y, Rgba([20, 20, 20, 255]));
            }
        }
        for y in 8..20 {
            for x in 8..20 {
                big.put_pixel(x, y, Rgba([20, 20, 20, 255]));
            }
        }
        let (s_small, s_big) = (ssim(&a, &small), ssim(&a, &big));
        assert!(s_small < 1.0 && s_big < s_small, "{s_small} {s_big}");
        assert!(luma_l2(&a, &big) > luma_l2(&a, &small));
    }

    #[test]
    fn a_change_of_colour_alone_lowers_ssim() {
        // Two colours of nearly one luma: a pale blue drop on pale skin.
        let skin = RgbaImage::from_pixel(32, 32, Rgba([249, 211, 197, 255]));
        let mut drop = skin.clone();
        for y in 8..20 {
            for x in 8..16 {
                drop.put_pixel(x, y, Rgba([190, 225, 255, 255]));
            }
        }
        assert!(luma_l2(&skin, &drop) < 0.02, "the luma barely moves");
        assert!(ssim(&skin, &drop) < 0.9, "{}", ssim(&skin, &drop));
    }

    #[test]
    fn layer_change_counts_only_visible_pixels() {
        let mut a = Raster::new(4, 4);
        let mut b = Raster::new(4, 4);
        b.pixels[0] = PIXEL_TOLERANCE; // within the tolerance
        b.pixels[4] = 200;
        let change = layer_change(&a, &b);
        assert_eq!(change.changed_px, 1);
        assert!(change.distance > 0.0);
        a.pixels.clone_from(&b.pixels);
        assert_eq!(layer_change(&a, &b), LayerChange::default());
    }
}
