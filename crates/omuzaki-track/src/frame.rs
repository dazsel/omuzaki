//! RGB frames and the rotated-ROI warp that turns a region of a frame into a model input tensor.

use crate::error::{FaceTrackError, Result};
use crate::roi::RotatedRect;

/// A packed RGB8 frame (stride >= 3 * width), borrowed from whoever captured it.
#[derive(Copy, Clone, Debug)]
pub struct RgbFrame<'a> {
    /// Pixel bytes: `height` rows of `stride` bytes, the first `3 * width` of each being R, G, B per pixel.
    pub data: &'a [u8],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes from the start of one row to the start of the next.
    pub stride: usize,
}

impl RgbFrame<'_> {
    /// Checks that `data` holds `height` rows of at least `3 * width` bytes at `stride` spacing.
    ///
    /// # Errors
    /// [`FaceTrackError::BadFrame`] for a zero side, a stride under `3 * width` or too few bytes.
    pub fn validate(&self) -> Result<()> {
        let bad = || FaceTrackError::BadFrame { width: self.width, height: self.height };
        let row = usize::try_from(self.width).ok().and_then(|w| w.checked_mul(3)).ok_or_else(bad)?;
        let rows = usize::try_from(self.height).map_err(|_| bad())?;
        if row == 0 || rows == 0 || self.stride < row {
            return Err(bad());
        }
        // The last row needs only its own pixels, not a full stride of padding.
        let needed = (rows - 1).checked_mul(self.stride).and_then(|n| n.checked_add(row)).ok_or_else(bad)?;
        if self.data.len() < needed {
            return Err(bad());
        }
        Ok(())
    }

    /// The width as a `usize`; only meaningful after [`validate`](Self::validate).
    fn w(&self) -> usize {
        self.width as usize
    }

    /// The height as a `usize`; only meaningful after [`validate`](Self::validate).
    fn h(&self) -> usize {
        self.height as usize
    }

    /// The pixel at integer coordinates. Outside the frame it is black ([`Border::Zero`]) or the nearest edge pixel
    /// ([`Border::Replicate`]).
    fn pixel(&self, x: i64, y: i64, border: Border) -> [u8; 3] {
        let (x, y) = match border {
            Border::Zero => match (usize::try_from(x), usize::try_from(y)) {
                (Ok(x), Ok(y)) if x < self.w() && y < self.h() => (x, y),
                _ => return [0; 3],
            },
            Border::Replicate => {
                (usize::try_from(x).unwrap_or(0).min(self.w() - 1), usize::try_from(y).unwrap_or(0).min(self.h() - 1))
            }
        };
        self.texel(x, y)
    }

    /// The pixel at in-range coordinates.
    fn texel(&self, x: usize, y: usize) -> [u8; 3] {
        let i = y * self.stride + x * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    /// Bilinear sample at fractional coordinates, reproducing OpenCV's `warpPerspective(INTER_LINEAR)` as MediaPipe
    /// calls it: pixel centres at integers, the position snapped to 1/32 pixel, and the result rounded to a whole
    /// 8-bit level. Matching that rounding matters: it is worth about 0.25 grey levels of noise per pixel, enough to
    /// move landmarks by a tenth of a pixel.
    ///
    /// The blend is done in integers: with weights in 1/32 steps every intermediate of the float version is exactly
    /// representable, so the integer sum and one rounding shift give the identical level, and a sample whose four
    /// neighbours are all inside the frame (nearly all of them) skips the border logic.
    // Coordinates are far inside i64, and the weights sum to 1024, so the shifted blend is a level in 0..=255.
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]
    fn sample(&self, x: f32, y: f32, border: Border) -> [u8; 3] {
        let (qx, qy) = ((x * SUBPIXEL_STEPS).round() as i64, (y * SUBPIXEL_STEPS).round() as i64);
        let (xi, yi) = (qx >> SUBPIXEL_BITS, qy >> SUBPIXEL_BITS);
        let (fx, fy) = ((qx & SUBPIXEL_MASK) as i32, (qy & SUBPIXEL_MASK) as i32);
        let inside = match (usize::try_from(xi), usize::try_from(yi)) {
            (Ok(xu), Ok(yu)) if xu + 1 < self.w() && yu + 1 < self.h() => Some((xu, yu)),
            _ => None,
        };
        let (p00, p10, p01, p11) = match inside {
            Some((xu, yu)) => {
                (self.texel(xu, yu), self.texel(xu + 1, yu), self.texel(xu, yu + 1), self.texel(xu + 1, yu + 1))
            }
            None => (
                self.pixel(xi, yi, border),
                self.pixel(xi + 1, yi, border),
                self.pixel(xi, yi + 1, border),
                self.pixel(xi + 1, yi + 1, border),
            ),
        };
        let steps = SUBPIXEL_STEPS_INT;
        let weights = [(steps - fx) * (steps - fy), fx * (steps - fy), (steps - fx) * fy, fx * fy];
        let mut out = [0u8; 3];
        for c in 0..3 {
            let sum = i32::from(p00[c]) * weights[0]
                + i32::from(p10[c]) * weights[1]
                + i32::from(p01[c]) * weights[2]
                + i32::from(p11[c]) * weights[3];
            // Round half up: every term is non-negative, so this is the float version's round-half-away-from-zero.
            out[c] = ((sum + ROUND_HALF) >> (2 * SUBPIXEL_BITS)) as u8;
        }
        out
    }
}

impl RgbFrame<'_> {
    /// [`sample`](Self::sample) for a position whose four neighbours are all inside the frame and whose snapped
    /// coordinates `qx`, `qy` are non-negative: the same arithmetic without the border logic.
    // The weights sum to 1024, so the shifted blend is a level in 0..=255; the caller checked the neighbours.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    #[inline]
    fn sample_inside(&self, qx: i32, qy: i32) -> [u8; 3] {
        let (xu, yu) = ((qx >> SUBPIXEL_BITS) as usize, (qy >> SUBPIXEL_BITS) as usize);
        let (fx, fy) = (qx & SUBPIXEL_MASK as i32, qy & SUBPIXEL_MASK as i32);
        let steps = SUBPIXEL_STEPS_INT;
        let weights = [(steps - fx) * (steps - fy), fx * (steps - fy), (steps - fx) * fy, fx * fy];
        let top = &self.data[yu * self.stride + xu * 3..][..6];
        let bottom = &self.data[(yu + 1) * self.stride + xu * 3..][..6];
        let mut out = [0u8; 3];
        for c in 0..3 {
            let sum = i32::from(top[c]) * weights[0]
                + i32::from(top[c + 3]) * weights[1]
                + i32::from(bottom[c]) * weights[2]
                + i32::from(bottom[c + 3]) * weights[3];
            out[c] = ((sum + ROUND_HALF) >> (2 * SUBPIXEL_BITS)) as u8;
        }
        out
    }

    /// The snapped (1/32 pixel) integer position of a sample point, as [`sample`](Self::sample) computes it.
    #[allow(clippy::cast_possible_truncation)] // coordinates are far inside i64
    fn snap(x: f32, y: f32) -> (i64, i64) {
        ((x * SUBPIXEL_STEPS).round() as i64, (y * SUBPIXEL_STEPS).round() as i64)
    }

    /// Whether every sample of a row between the snapped positions `a` and `b` has all four neighbours inside the
    /// frame. A row's positions are monotone in x (float multiply and add keep the order), so its two ends bound
    /// the rest.
    fn row_is_inside(&self, a: (i64, i64), b: (i64, i64)) -> bool {
        let inside = |(qx, qy): (i64, i64)| {
            let (xi, yi) = (qx >> SUBPIXEL_BITS, qy >> SUBPIXEL_BITS);
            xi >= 0 && yi >= 0 && xi + 1 < i64::from(self.width) && yi + 1 < i64::from(self.height)
        };
        inside(a) && inside(b)
    }
}

/// Round half away from zero for a value known to be above -0.5 and below 2^31, which is [`f32::round`] without
/// the library call: the fraction of an `f32` below 2^23 is exact.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // the caller bounds the value
#[inline]
fn round_small(v: f32) -> i32 {
    let whole = v as i32;
    whole + i32::from(v - whole as f32 >= 0.5)
}

/// OpenCV snaps sample positions to 1/32 pixel (`INTER_BITS = 5`).
const SUBPIXEL_BITS: u32 = 5;
const SUBPIXEL_STEPS: f32 = 32.0; // 1 << SUBPIXEL_BITS
const SUBPIXEL_STEPS_INT: i32 = 1 << SUBPIXEL_BITS;
const SUBPIXEL_MASK: i64 = (1 << SUBPIXEL_BITS) - 1;
/// Half of the 1024 total weight, added before the shift so it rounds to nearest.
const ROUND_HALF: i32 = 1 << (2 * SUBPIXEL_BITS - 1);

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
pub struct ImageSpec {
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
/// outside the frame. Writing into a caller-owned buffer keeps the hot path allocation-free. The frame must have
/// passed [`RgbFrame::validate`].
///
/// # Errors
/// [`FaceTrackError::Shape`] (graph `"input"`) when `out` has the wrong length.
#[allow(clippy::cast_precision_loss)] // pixel counts are tiny next to f32's 24 bits
pub fn warp_to_tensor(frame: &RgbFrame<'_>, roi: &RotatedRect, spec: ImageSpec, out: &mut [f32]) -> Result<()> {
    if out.len() != spec.width * spec.height * 3 {
        return Err(FaceTrackError::Shape { graph: "input" });
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
        let at = |xd: usize| (row_start[0] + xd as f32 * per_column[0], row_start[1] + xd as f32 * per_column[1]);
        let last = at(spec.width - 1);
        let row = &mut out[o..o + spec.width * 3];
        // A face ROI usually lies inside the frame: those rows skip the border logic, with identical results.
        if frame.row_is_inside(RgbFrame::snap(at(0).0, at(0).1), RgbFrame::snap(last.0, last.1)) {
            for (xd, pixel) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let (x, y) = at(xd);
                let px = frame.sample_inside(round_small(x * SUBPIXEL_STEPS), round_small(y * SUBPIXEL_STEPS));
                *pixel = px.map(|v| f32::from(v) * scale + spec.range.0);
            }
        } else {
            for (xd, pixel) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let (x, y) = at(xd);
                *pixel = frame.sample(x, y, spec.border).map(|v| f32::from(v) * scale + spec.range.0);
            }
        }
        o += spec.width * 3;
    }
    Ok(())
}

/// Owned test images, so the tests of every module can build frames without files.
#[cfg(test)]
pub(crate) mod testing {
    use super::RgbFrame;

    /// An owned packed RGB image.
    pub struct Image {
        pub width: u32,
        pub height: u32,
        pub data: Vec<u8>,
    }

    impl Image {
        /// Every pixel the same colour.
        pub fn flat(width: u32, height: u32, rgb: [u8; 3]) -> Self {
            Self { width, height, data: rgb.repeat((width * height) as usize) }
        }

        /// A tightly packed frame over this image.
        pub fn frame(&self) -> RgbFrame<'_> {
            RgbFrame { data: &self.data, width: self.width, height: self.height, stride: self.width as usize * 3 }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)] // test fixtures are tiny
    use super::testing::Image;
    use super::*;

    /// A frame whose red channel is the x coordinate and green the y coordinate, so a warp is easy to check by value.
    fn gradient(w: u32, h: u32) -> Image {
        let mut data = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[x as u8, y as u8, 0]);
            }
        }
        Image { width: w, height: h, data }
    }

    fn roi(cx: f32, cy: f32, w: f32, h: f32, rotation: f32) -> RotatedRect {
        RotatedRect { cx, cy, width: w, height: h, rotation }
    }

    /// Two pixels side by side as a frame.
    fn pair(left: [u8; 3], right: [u8; 3]) -> Image {
        Image { width: 2, height: 1, data: [left, right].concat() }
    }

    #[test]
    fn identity_warp_copies_pixels() {
        let image = gradient(8, 8);
        let spec = ImageSpec { width: 8, height: 8, range: (0.0, 255.0), border: Border::Zero };
        let mut out = vec![0.0; 8 * 8 * 3];
        // Whole-frame ROI centred at the frame centre maps tensor pixel (x, y) to source (x, y).
        warp_to_tensor(&image.frame(), &roi(4.0, 4.0, 8.0, 8.0, 0.0), spec, &mut out).unwrap();
        for y in 0..8 {
            for x in 0..8 {
                let i = (y * 8 + x) * 3;
                assert!((out[i] - x as f32).abs() < 1e-4 && (out[i + 1] - y as f32).abs() < 1e-4, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn quarter_turn_swaps_axes() {
        let image = gradient(9, 9);
        let spec = ImageSpec { width: 5, height: 5, range: (0.0, 255.0), border: Border::Zero };
        let mut out = vec![0.0; 5 * 5 * 3];
        // A 5x5 ROI centred on (4, 4), rotated a quarter turn clockwise on screen: tensor +x runs down the frame.
        warp_to_tensor(&image.frame(), &roi(4.0, 4.0, 5.0, 5.0, std::f32::consts::FRAC_PI_2), spec, &mut out).unwrap();
        let first_row_start = &out[0..3];
        let first_row_next = &out[3..6];
        assert!((first_row_next[1] - first_row_start[1] - 1.0).abs() < 1e-3, "green (y) grows along tensor x");
        assert!((first_row_next[0] - first_row_start[0]).abs() < 1e-3, "red (x) is constant along tensor x");
    }

    #[test]
    fn outside_the_frame_is_black_and_range_is_applied() {
        let image = Image::flat(2, 2, [255; 3]);
        let spec = ImageSpec { width: 2, height: 2, range: (-1.0, 1.0), border: Border::Zero };
        let mut out = vec![0.0; 12];
        warp_to_tensor(&image.frame(), &roi(100.0, 100.0, 2.0, 2.0, 0.0), spec, &mut out).unwrap();
        assert!(out.iter().all(|v| (*v + 1.0).abs() < 1e-6), "far outside the frame reads as black = range minimum");
        warp_to_tensor(&image.frame(), &roi(1.0, 1.0, 2.0, 2.0, 0.0), spec, &mut out).unwrap();
        assert!((out[0] - 1.0).abs() < 1e-6, "white inside the frame reads as the range maximum");
    }

    #[test]
    fn samples_snap_to_32nds_of_a_pixel_and_round_to_whole_levels() {
        // Two pixels, 0 and 255 side by side: the sample at x = 0.5 is exactly halfway (127.5, which rounds to 128).
        let image = pair([0; 3], [255; 3]);
        let frame = image.frame();
        assert_eq!(frame.sample(0.5, 0.0, Border::Zero), [128; 3]);
        // 1/64 of a pixel is below half a 1/32 step: it snaps back to the pixel centre.
        assert_eq!(frame.sample(0.015, 0.0, Border::Zero), [0; 3]);
        // 3/64 rounds up to 2/32 (= 16 levels after rounding 15.94).
        assert_eq!(frame.sample(3.0 / 64.0 + 0.0001, 0.0, Border::Zero)[0], 16);
    }

    #[test]
    fn replicate_border_extends_the_edge_pixels() {
        let image = pair([10, 20, 30], [200, 210, 220]);
        let frame = image.frame();
        assert_eq!(frame.sample(-5.0, 0.0, Border::Replicate), [10, 20, 30]);
        assert_eq!(frame.sample(9.0, 3.0, Border::Replicate), [200, 210, 220]);
        assert_eq!(frame.sample(-5.0, 0.0, Border::Zero), [0; 3]);
    }

    /// The integer blend must equal the float one it replaces (OpenCV's arithmetic as the spike wrote it) on every
    /// position, inside the frame and across its borders.
    #[test]
    fn integer_blend_matches_the_float_reference() {
        let image = gradient(7, 5);
        let frame = image.frame();
        let reference = |x: f32, y: f32, border: Border| -> [u8; 3] {
            let (qx, qy) = ((x * 32.0).round() as i64, (y * 32.0).round() as i64);
            let (xi, yi) = (qx >> 5, qy >> 5);
            let (fx, fy) = ((qx & 31) as f32 / 32.0, (qy & 31) as f32 / 32.0);
            let at = |x, y| frame.pixel(x, y, border).map(f32::from);
            let (p00, p10, p01, p11) = (at(xi, yi), at(xi + 1, yi), at(xi, yi + 1), at(xi + 1, yi + 1));
            let mut out = [0u8; 3];
            for c in 0..3 {
                let top = p00[c] + (p10[c] - p00[c]) * fx;
                let bottom = p01[c] + (p11[c] - p01[c]) * fx;
                out[c] = (top + (bottom - top) * fy).round() as u8;
            }
            out
        };
        for border in [Border::Zero, Border::Replicate] {
            for i in -40..300 {
                for j in -40..220 {
                    let (x, y) = (i as f32 * 0.0371, j as f32 * 0.0293);
                    assert_eq!(frame.sample(x, y, border), reference(x, y, border), "({x}, {y}) {border:?}");
                }
            }
        }
    }

    /// The row fast path must give the same tensor as sampling every pixel through the border logic, for ROIs that
    /// are inside, cross an edge, are rotated, shrink or enlarge.
    #[test]
    fn warp_matches_per_pixel_sampling() {
        let image = gradient(40, 30);
        let frame = image.frame();
        let rois = [
            roi(20.0, 15.0, 20.0, 20.0, 0.0),
            roi(20.0, 15.0, 31.7, 22.3, 0.4),
            roi(3.0, 4.0, 18.0, 18.0, -0.7),
            roi(38.0, 28.0, 25.0, 25.0, 1.9),
            roi(20.0, 15.0, 80.0, 60.0, 0.0),
            roi(20.0, 15.0, 6.5, 6.5, 2.5),
        ];
        for border in [Border::Zero, Border::Replicate] {
            let spec = ImageSpec { width: 16, height: 16, range: (-1.0, 1.0), border };
            for roi in rois {
                let mut out = vec![0.0; 16 * 16 * 3];
                warp_to_tensor(&frame, &roi, spec, &mut out).unwrap();
                let (cos, sin) = (roi.rotation.cos(), roi.rotation.sin());
                let scale = 2.0 / 255.0;
                for yd in 0..16 {
                    for xd in 0..16 {
                        let (u, v) = ((xd as f32 / 16.0 - 0.5) * roi.width, (yd as f32 / 16.0 - 0.5) * roi.height);
                        let expected = frame.sample(roi.cx + cos * u - sin * v, roi.cy + sin * u + cos * v, border);
                        for c in 0..3 {
                            let got = out[(yd * 16 + xd) * 3 + c];
                            let want = f32::from(expected[c]) * scale - 1.0;
                            assert!((got - want).abs() < 1e-5, "{roi:?} {border:?} ({xd}, {yd}) channel {c}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn rejects_bad_buffers_and_frames() {
        let image = gradient(4, 4);
        let spec = ImageSpec { width: 4, height: 4, range: (0.0, 1.0), border: Border::Zero };
        assert_eq!(
            warp_to_tensor(&image.frame(), &roi(2.0, 2.0, 4.0, 4.0, 0.0), spec, &mut [0.0; 3]),
            Err(FaceTrackError::Shape { graph: "input" })
        );
        let ok = RgbFrame { data: &image.data, width: 4, height: 4, stride: 12 };
        assert!(ok.validate().is_ok());
        for bad in [
            RgbFrame { width: 0, ..ok },
            RgbFrame { height: 0, ..ok },
            RgbFrame { stride: 11, ..ok },
            RgbFrame { data: &image.data[..47], ..ok },
            RgbFrame { stride: 13, ..ok },
        ] {
            assert!(matches!(bad.validate(), Err(FaceTrackError::BadFrame { .. })), "{bad:?}");
        }
        // A padded stride is fine; the last row needs no padding.
        let padded = vec![0u8; 3 * 14 + 12];
        assert!(RgbFrame { data: &padded, width: 4, height: 4, stride: 14 }.validate().is_ok());
    }
}
