//! How different two face pictures are: pixel counts for the channel sweep, and the structural similarity (SSIM) and a
//! luminance distance for the expression range test.
//!
//! SSIM is Wang et al. 2004 on luma, over 8 x 8 windows with a stride of 4 and the paper's constants, which is the usual
//! form for small images and needs no dependency. Luma is Rec. 709 on the sRGB bytes: the face is judged by how it
//! reads, not by its light.

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

/// The mean SSIM of two pictures of one size, `-1..=1` (1 = the same). Pictures smaller than a window are compared as one
/// window. Pictures of different sizes score 0.
pub fn ssim(a: &RgbaImage, b: &RgbaImage) -> f64 {
    if a.dimensions() != b.dimensions() {
        return 0.0;
    }
    let (w, h) = a.dimensions();
    let (la, lb) = (luma(a), luma(b));
    let (win_w, win_h) = (WINDOW.min(w), WINDOW.min(h));
    let mut total = 0.0;
    let mut windows = 0u32;
    let mut y = 0;
    while y + win_h <= h {
        let mut x = 0;
        while x + win_w <= w {
            total += window_ssim(&la, &lb, w, x, y, win_w, win_h);
            windows += 1;
            x += STRIDE;
        }
        y += STRIDE;
    }
    if windows == 0 { 1.0 } else { total / f64::from(windows) }
}

/// SSIM of one window.
fn window_ssim(a: &[f64], b: &[f64], stride: u32, x0: u32, y0: u32, w: u32, h: u32) -> f64 {
    let n = f64::from(w * h);
    let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let i = (y * stride + x) as usize;
            let (va, vb) = (a.get(i).copied().unwrap_or(0.0), b.get(i).copied().unwrap_or(0.0));
            sa += va;
            sb += vb;
            saa += va * va;
            sbb += vb * vb;
            sab += va * vb;
        }
    }
    let (ma, mb) = (sa / n, sb / n);
    let (va, vb, cov) = (saa / n - ma * ma, sbb / n - mb * mb, sab / n - ma * mb);
    ((2.0 * ma * mb + C1) * (2.0 * cov + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2))
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
