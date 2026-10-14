//! RGB frames and the rotated-ROI warp that turns a region of a frame into a model input tensor.

use std::path::Path;

use crate::error::{Result, TrackError};
use crate::roi::RotatedRect;

/// An 8-bit RGB image, row-major, as a webcam frame would arrive after decoding.
#[derive(Debug, Clone)]
pub struct Frame {
    width: usize,
    height: usize,
    rgb: Vec<u8>,
}

impl Frame {
    /// Wraps a tightly packed RGB buffer.
    ///
    /// # Errors
    /// Returns [`TrackError::Input`] when the buffer length is not `width * height * 3` or a side is zero.
    pub fn new(width: usize, height: usize, rgb: Vec<u8>) -> Result<Self> {
        if width == 0 || height == 0 || rgb.len() != width * height * 3 {
            return Err(TrackError::Input(format!(
                "{width}x{height} RGB frame needs {} bytes, got {}",
                width * height * 3,
                rgb.len()
            )));
        }
        Ok(Self { width, height, rgb })
    }

    /// Decodes a PNG file.
    ///
    /// # Errors
    /// Returns [`TrackError::Image`] when the file cannot be read or decoded.
    pub fn from_png(path: &Path) -> Result<Self> {
        let img =
            image::open(path).map_err(|source| TrackError::Image { path: path.to_path_buf(), source })?.into_rgb8();
        let (w, h) = img.dimensions();
        Self::new(w as usize, h as usize, img.into_raw())
    }

    /// Frame width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Frame height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The pixel at integer coordinates as floats in 0..=255. Outside the frame it is black ([`Border::Zero`]) or the
    /// nearest edge pixel ([`Border::Replicate`]).
    fn pixel(&self, x: i64, y: i64, border: Border) -> [f32; 3] {
        let (x, y) = match border {
            Border::Zero => match (usize::try_from(x), usize::try_from(y)) {
                (Ok(x), Ok(y)) if x < self.width && y < self.height => (x, y),
                _ => return [0.0; 3],
            },
            Border::Replicate => (
                usize::try_from(x).unwrap_or(0).min(self.width - 1),
                usize::try_from(y).unwrap_or(0).min(self.height - 1),
            ),
        };
        let i = (y * self.width + x) * 3;
        [f32::from(self.rgb[i]), f32::from(self.rgb[i + 1]), f32::from(self.rgb[i + 2])]
    }

    /// Bilinear sample at fractional coordinates, reproducing OpenCV's `warpPerspective(INTER_LINEAR)` as MediaPipe
    /// calls it: pixel centres at integers, the position snapped to 1/32 pixel, and the result rounded to a whole
    /// 8-bit level. Matching that rounding matters: it is worth about 0.25 grey levels of noise per pixel, enough to
    /// move landmarks by a tenth of a pixel.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // coordinates are far inside i64
    fn sample(&self, x: f32, y: f32, border: Border) -> [f32; 3] {
        let (qx, qy) = ((x * SUBPIXEL_STEPS).round() as i64, (y * SUBPIXEL_STEPS).round() as i64);
        let (xi, yi) = (qx >> SUBPIXEL_BITS, qy >> SUBPIXEL_BITS);
        let (fx, fy) = ((qx & (SUBPIXEL_MASK)) as f32 / SUBPIXEL_STEPS, (qy & SUBPIXEL_MASK) as f32 / SUBPIXEL_STEPS);
        let (p00, p10, p01, p11) = (
            self.pixel(xi, yi, border),
            self.pixel(xi + 1, yi, border),
            self.pixel(xi, yi + 1, border),
            self.pixel(xi + 1, yi + 1, border),
        );
        let mut out = [0.0; 3];
        for c in 0..3 {
            let top = p00[c] + (p10[c] - p00[c]) * fx;
            let bottom = p01[c] + (p11[c] - p01[c]) * fx;
            out[c] = (top + (bottom - top) * fy).round();
        }
        out
    }
}

/// OpenCV snaps sample positions to 1/32 pixel (`INTER_BITS = 5`).
const SUBPIXEL_BITS: u32 = 5;
const SUBPIXEL_STEPS: f32 = 32.0; // 1 << SUBPIXEL_BITS
const SUBPIXEL_MASK: i64 = (1 << SUBPIXEL_BITS) - 1;

/// What a warp reads where the ROI reaches beyond the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Border {
    /// Black. The detector uses this: the padding around a letterboxed frame must not look like image content.
    Zero,
    /// The nearest edge pixel. MediaPipe's default for `ImageToTensor`, so the landmark model sees this for a face
    /// ROI that extends past the frame edge.
    Replicate,
}

/// The size and value range of a model's image input (NHWC, one image).
#[derive(Debug, Clone, Copy)]
pub struct TensorSpec {
    /// Input width in pixels.
    pub width: usize,
    /// Input height in pixels.
    pub height: usize,
    /// Values the 0..=255 pixel range is mapped to: `(0.0, 1.0)` or `(-1.0, 1.0)`.
    pub range: (f32, f32),
    /// What to read outside the frame.
    pub border: Border,
}

/// Resamples the (possibly rotated) `roi` of `frame` into `out`, an NHWC `f32` buffer of `spec.width * spec.height * 3`.
///
/// This is MediaPipe's `ImageToTensor`: output pixel `(x, y)` samples the point `(x / width, y / height)` of the ROI,
/// bilinearly and without pre-filtering (so downscaling aliases exactly as it does in MediaPipe), and `spec.border`
/// outside the frame. Writing into a caller-owned buffer keeps the hot path allocation-free.
///
/// # Errors
/// Returns [`TrackError::Input`] when `out` has the wrong length.
#[allow(clippy::cast_precision_loss)] // pixel counts are tiny next to f32's 24 bits
pub fn warp_to_tensor(frame: &Frame, roi: &RotatedRect, spec: TensorSpec, out: &mut [f32]) -> Result<()> {
    if out.len() != spec.width * spec.height * 3 {
        return Err(TrackError::Input(format!(
            "tensor buffer holds {} values, need {}",
            out.len(),
            spec.width * spec.height * 3
        )));
    }
    let (cos, sin) = (roi.rotation.cos(), roi.rotation.sin());
    // Source position of tensor pixel (xd, yd): centre + R * ((u - 0.5) * w, (v - 0.5) * h), with u = xd / W, v = yd / H.
    let per_column = [cos * roi.width / spec.width as f32, sin * roi.width / spec.width as f32];
    let per_row = [-sin * roi.height / spec.height as f32, cos * roi.height / spec.height as f32];
    let (half_w, half_h) = (roi.width / 2.0, roi.height / 2.0);
    let start = [roi.cx - cos * half_w + sin * half_h, roi.cy - sin * half_w - cos * half_h];
    let scale = (spec.range.1 - spec.range.0) / 255.0;
    let mut o = 0;
    for yd in 0..spec.height {
        let row_start = [start[0] + yd as f32 * per_row[0], start[1] + yd as f32 * per_row[1]];
        for xd in 0..spec.width {
            let px = frame.sample(
                row_start[0] + xd as f32 * per_column[0],
                row_start[1] + xd as f32 * per_column[1],
                spec.border,
            );
            for c in 0..3 {
                out[o + c] = px[c] * scale + spec.range.0;
            }
            o += 3;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test fixtures are tiny
    use super::*;

    /// A frame whose red channel is the x coordinate and green the y coordinate, so a warp is easy to check by value.
    fn gradient(w: usize, h: usize) -> Frame {
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                rgb.extend_from_slice(&[x as u8, y as u8, 0]);
            }
        }
        Frame::new(w, h, rgb).unwrap()
    }

    fn roi(cx: f32, cy: f32, w: f32, h: f32, rotation: f32) -> RotatedRect {
        RotatedRect { cx, cy, width: w, height: h, rotation }
    }

    #[test]
    fn identity_warp_copies_pixels() {
        let frame = gradient(8, 8);
        let spec = TensorSpec { width: 8, height: 8, range: (0.0, 255.0), border: Border::Zero };
        let mut out = vec![0.0; 8 * 8 * 3];
        // Whole-frame ROI centred at the frame centre maps tensor pixel (x, y) to source (x, y).
        warp_to_tensor(&frame, &roi(4.0, 4.0, 8.0, 8.0, 0.0), spec, &mut out).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                let i = (y * 8 + x) * 3;
                assert!((out[i] - x as f32).abs() < 1e-4 && (out[i + 1] - y as f32).abs() < 1e-4, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn quarter_turn_swaps_axes() {
        let frame = gradient(9, 9);
        let spec = TensorSpec { width: 5, height: 5, range: (0.0, 255.0), border: Border::Zero };
        let mut out = vec![0.0; 5 * 5 * 3];
        // A 5x5 ROI centred on (4, 4), rotated a quarter turn clockwise on screen: tensor +x runs down the frame.
        warp_to_tensor(&frame, &roi(4.0, 4.0, 5.0, 5.0, std::f32::consts::FRAC_PI_2), spec, &mut out).unwrap();
        let first_row_start = &out[0..3];
        let first_row_next = &out[3..6];
        assert!((first_row_next[1] - first_row_start[1] - 1.0).abs() < 1e-3, "green (y) grows along tensor x");
        assert!((first_row_next[0] - first_row_start[0]).abs() < 1e-3, "red (x) is constant along tensor x");
    }

    #[test]
    fn outside_the_frame_is_black_and_range_is_applied() {
        let frame = Frame::new(2, 2, vec![255; 12]).unwrap();
        let spec = TensorSpec { width: 2, height: 2, range: (-1.0, 1.0), border: Border::Zero };
        let mut out = vec![0.0; 12];
        warp_to_tensor(&frame, &roi(100.0, 100.0, 2.0, 2.0, 0.0), spec, &mut out).unwrap();
        assert!(out.iter().all(|v| (*v + 1.0).abs() < 1e-6), "far outside the frame reads as black = range minimum");
        warp_to_tensor(&frame, &roi(1.0, 1.0, 2.0, 2.0, 0.0), spec, &mut out).unwrap();
        assert!((out[0] - 1.0).abs() < 1e-6, "white inside the frame reads as the range maximum");
    }

    #[test]
    fn samples_snap_to_32nds_of_a_pixel_and_round_to_whole_levels() {
        // Two pixels, 0 and 255 side by side: the sample at x = 0.5 is exactly halfway (127.5, which rounds to 128).
        let frame = Frame::new(2, 1, vec![0, 0, 0, 255, 255, 255]).unwrap();
        assert_eq!(frame.sample(0.5, 0.0, Border::Zero), [128.0; 3]);
        // 1/64 of a pixel is below half a 1/32 step: it snaps back to the pixel centre.
        assert_eq!(frame.sample(0.015, 0.0, Border::Zero), [0.0; 3]);
        // 3/64 rounds up to 2/32 (= 16 levels after rounding 15.94).
        assert_eq!(frame.sample(3.0 / 64.0 + 0.0001, 0.0, Border::Zero)[0], 16.0);
        // Every output is a whole level.
        assert!((0..40).all(|i| frame.sample(i as f32 * 0.0271, 0.0, Border::Zero)[0].fract() == 0.0));
    }

    #[test]
    fn replicate_border_extends_the_edge_pixels() {
        let frame = Frame::new(2, 1, vec![10, 20, 30, 200, 210, 220]).unwrap();
        assert_eq!(frame.sample(-5.0, 0.0, Border::Replicate), [10.0, 20.0, 30.0]);
        assert_eq!(frame.sample(9.0, 3.0, Border::Replicate), [200.0, 210.0, 220.0]);
        assert_eq!(frame.sample(-5.0, 0.0, Border::Zero), [0.0; 3]);
    }

    #[test]
    fn rejects_bad_buffers() {
        assert!(Frame::new(2, 2, vec![0; 5]).is_err());
        let frame = gradient(4, 4);
        let spec = TensorSpec { width: 4, height: 4, range: (0.0, 1.0), border: Border::Zero };
        assert!(warp_to_tensor(&frame, &roi(2.0, 2.0, 4.0, 4.0, 0.0), spec, &mut [0.0; 3]).is_err());
    }
}
