//! The sheet's pixels: an RGBA8 canvas in **premultiplied sRGB**, and the few drawing primitives the layout needs.
//!
//! Compositing is source-over on sRGB-encoded bytes with alpha already multiplied in (the convention of spec BR-01 and
//! of `omuzaki_render_api::AlphaEncoding::Premultiplied`), in integer maths with one rounding rule, so the same sheet is
//! the same bytes on every machine. Edges of rounded shapes are anti-aliased from a signed distance, which needs only
//! `sqrt` (correctly rounded by IEEE 754), never a transcendental function.

use image::RgbaImage;
use omuzaki_core::colour::Colour;

use super::geometry::Rect;

/// `a * b / 255`, rounded to nearest, for two byte values.
pub(super) fn mul255(a: u32, b: u32) -> u32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

/// A colour's bytes with alpha multiplied in.
pub(super) fn premultiply(c: Colour) -> [u8; 4] {
    let a = u32::from(c.a);
    [mul255(u32::from(c.r), a) as u8, mul255(u32::from(c.g), a) as u8, mul255(u32::from(c.b), a) as u8, c.a]
}

/// A width x height RGBA8 canvas holding premultiplied sRGB.
#[derive(Clone, Debug)]
pub(super) struct Canvas {
    width: i32,
    height: i32,
    px: Vec<u8>,
}

impl Canvas {
    /// A canvas filled with `background`.
    pub(super) fn new(width: u32, height: u32, background: Colour) -> Self {
        let fill = premultiply(background);
        let px = fill.iter().copied().cycle().take(width as usize * height as usize * 4).collect();
        Self { width: width as i32, height: height as i32, px }
    }

    /// Composites `src` (premultiplied) at `(x, y)`, scaled by `coverage` (0..=255), over what is there.
    /// Pixels outside the canvas are ignored.
    pub(super) fn blend(&mut self, x: i32, y: i32, src: [u8; 4], coverage: u32) {
        if coverage == 0 || x < 0 || y < 0 || x >= self.width || y >= self.height {
            return;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let s = src.map(|c| if coverage == 255 { u32::from(c) } else { mul255(u32::from(c), coverage) });
        let inverse = 255 - s[3];
        for (d, s) in self.px[i..i + 4].iter_mut().zip(s) {
            *d = (s + mul255(u32::from(*d), inverse)).min(255) as u8;
        }
    }

    /// Fills `rect` with `colour`.
    pub(super) fn fill_rect(&mut self, rect: Rect, colour: Colour) {
        let src = premultiply(colour);
        for y in rect.y.max(0)..rect.bottom().min(self.height) {
            for x in rect.x.max(0)..rect.right().min(self.width) {
                self.blend(x, y, src, 255);
            }
        }
    }

    /// Fills `rect` with rounded corners of `radius` pixels.
    pub(super) fn fill_round_rect(&mut self, rect: Rect, radius: f32, colour: Colour) {
        self.round_rect(rect, radius, colour, |d| 0.5 - d);
    }

    /// Outlines `rect` with rounded corners, `width` pixels thick, drawn inside the rectangle's edge.
    pub(super) fn stroke_round_rect(&mut self, rect: Rect, radius: f32, width: f32, colour: Colour) {
        self.round_rect(rect, radius, colour, |d| 0.5 - (d + width / 2.0).abs() + width / 2.0);
    }

    /// Composites a `w x h` premultiplied RGBA8 picture with its top-left corner at `(x, y)`.
    pub(super) fn draw_premultiplied(&mut self, x: i32, y: i32, w: u32, data: &[u8]) {
        for (row, line) in data.chunks_exact(w as usize * 4).enumerate() {
            for (col, &p) in line.as_chunks::<4>().0.iter().enumerate() {
                self.blend(x + col as i32, y + row as i32, p, 255);
            }
        }
    }

    /// The finished picture as straight-alpha RGBA, the form PNG files and `image` use. An opaque sheet (any theme)
    /// is unchanged by the conversion.
    pub(super) fn into_rgba(self) -> RgbaImage {
        let mut px = self.px;
        for p in px.as_chunks_mut::<4>().0 {
            let a = u32::from(p[3]);
            if a != 255 && a != 0 {
                for c in &mut p[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        let mut image = RgbaImage::new(self.width as u32, self.height as u32);
        image.copy_from_slice(&px); // the same length by construction
        image
    }

    /// Rasterises a rounded rectangle; `coverage_of(signed_distance)` maps the distance from the outline (negative
    /// inside) to a 0..1 coverage, so fill and stroke share the distance field.
    fn round_rect(&mut self, rect: Rect, radius: f32, colour: Colour, coverage_of: impl Fn(f32) -> f32) {
        let src = premultiply(colour);
        let (hx, hy) = (rect.w as f32 / 2.0, rect.h as f32 / 2.0);
        let r = radius.clamp(0.0, hx.min(hy));
        let (cx, cy) = (rect.x as f32 + hx, rect.y as f32 + hy);
        for y in rect.y.max(0)..rect.bottom().min(self.height) {
            for x in rect.x.max(0)..rect.right().min(self.width) {
                let qx = (x as f32 + 0.5 - cx).abs() - (hx - r);
                let qy = (y as f32 + 0.5 - cy).abs() - (hy - r);
                let outside = (qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0)).sqrt();
                let d = outside + qx.max(qy).min(0.0) - r;
                let coverage = (coverage_of(d).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
                self.blend(x, y, src, coverage);
            }
        }
    }
}

/// Resamples `src` to `w x h` and returns it **premultiplied**.
///
/// The picture is premultiplied first and filtered in that form, so a soft edge against transparency never grows a
/// dark or light fringe. The filter is Mitchell-Netravali with its support widened by the shrink factor (an anti-aliasing
/// filter when shrinking, plain bicubic when growing), applied as two separable passes; weights use only `+ - * /`,
/// so the output is bit-identical everywhere.
pub(super) fn resample_premultiplied(src: &RgbaImage, premultiplied: bool, w: u32, h: u32) -> Vec<u8> {
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let (dw, dh) = (w as usize, h as usize);
    let channels = |p: &[u8]| -> [f32; 4] {
        let a = f32::from(p[3]);
        if premultiplied {
            [f32::from(p[0]), f32::from(p[1]), f32::from(p[2]), a]
        } else {
            [f32::from(p[0]) * a / 255.0, f32::from(p[1]) * a / 255.0, f32::from(p[2]) * a / 255.0, a]
        }
    };
    let raw = src.as_raw();

    // Horizontal pass: sw x sh -> dw x sh.
    let hw = axis_weights(sw, dw);
    let mut mid = vec![0.0_f32; dw * sh * 4];
    for y in 0..sh {
        for (x, (first, weights)) in hw.iter().enumerate() {
            let mut acc = [0.0_f32; 4];
            for (k, wt) in weights.iter().enumerate() {
                let i = (y * sw + first + k) * 4;
                let p = channels(&raw[i..i + 4]);
                for c in 0..4 {
                    acc[c] += p[c] * wt;
                }
            }
            mid[(y * dw + x) * 4..][..4].copy_from_slice(&acc);
        }
    }

    // Vertical pass: dw x sh -> dw x dh, clamped to a valid premultiplied pixel.
    let vw = axis_weights(sh, dh);
    let mut out = vec![0_u8; dw * dh * 4];
    for (y, (first, weights)) in vw.iter().enumerate() {
        for x in 0..dw {
            let mut acc = [0.0_f32; 4];
            for (k, wt) in weights.iter().enumerate() {
                let p = &mid[((first + k) * dw + x) * 4..][..4];
                for c in 0..4 {
                    acc[c] += p[c] * wt;
                }
            }
            let a = (acc[3] + 0.5).clamp(0.0, 255.0);
            for c in 0..3 {
                out[(y * dw + x) * 4 + c] = (acc[c] + 0.5).clamp(0.0, a) as u8;
            }
            out[(y * dw + x) * 4 + 3] = a as u8;
        }
    }
    out
}

/// For each of `dst` output samples along an axis of `src` input samples: the first input index and the normalised
/// filter weights from there.
fn axis_weights(src: usize, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let scale = src as f32 / dst as f32;
    let widen = scale.max(1.0);
    (0..dst)
        .map(|i| {
            let centre = (i as f32 + 0.5) * scale;
            let first = ((centre - 2.0 * widen).floor().max(0.0)) as usize;
            let last = (((centre + 2.0 * widen).ceil()) as usize).min(src);
            let mut weights: Vec<f32> = (first..last).map(|s| mitchell((s as f32 + 0.5 - centre) / widen)).collect();
            let sum: f32 = weights.iter().sum();
            if sum.abs() > 1e-6 {
                for w in &mut weights {
                    *w /= sum;
                }
            } else {
                // Only reachable for a degenerate range: fall back to the nearest sample.
                let nearest = (centre.floor() as usize).clamp(first, last.saturating_sub(1).max(first));
                weights = (first..last).map(|s| if s == nearest { 1.0 } else { 0.0 }).collect();
            }
            (first, weights)
        })
        .collect()
}

/// The Mitchell-Netravali cubic (B = C = 1/3): smooth, support 2, with so little negative lobe that a hard edge in the
/// art is not given a light or dark halo (Catmull-Rom is sharper but rings visibly on flat-shaded characters).
fn mitchell(x: f32) -> f32 {
    let x = x.abs();
    if x < 1.0 {
        ((7.0 * x - 12.0) * x * x + 16.0 / 3.0) / 6.0
    } else if x < 2.0 {
        (((-7.0 / 3.0 * x + 12.0) * x - 20.0) * x + 32.0 / 3.0) / 6.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn mul255_is_exact_at_the_ends_and_rounds_to_nearest() {
        assert_eq!((mul255(0, 200), mul255(255, 200), mul255(255, 255)), (0, 200, 255));
        assert_eq!(mul255(128, 128), 64);
    }

    #[test]
    fn source_over_in_premultiplied_form() {
        let mut c = Canvas::new(2, 1, Colour::WHITE);
        c.blend(0, 0, premultiply(Colour::rgba(0, 0, 0, 128)), 255);
        // Half-transparent black over white: 255 * (1 - 128/255) = 127.
        assert_eq!(&c.px[..4], &[127, 127, 127, 255]);
        assert_eq!(&c.px[4..], &[255, 255, 255, 255], "the neighbour is untouched");
    }

    #[test]
    fn rounded_rect_is_antialiased_at_the_corner_and_solid_inside() {
        let mut c = Canvas::new(40, 40, Colour::WHITE);
        c.fill_round_rect(Rect::new(4, 4, 32, 32), 10.0, Colour::BLACK);
        assert_eq!(&c.px[(20 * 40 + 20) * 4..][..4], &[0, 0, 0, 255]);
        assert_eq!(&c.px[(4 * 40 + 4) * 4..][..4], &[255, 255, 255, 255], "the sharp corner is cut away");
        let edge = c.px[(6 * 40 + 7) * 4]; // on the arc of the rounded corner: a partial-coverage grey
        assert!(edge > 0 && edge < 255, "{edge}");
    }

    #[test]
    fn stroke_leaves_the_inside_alone() {
        let mut c = Canvas::new(40, 40, Colour::WHITE);
        c.stroke_round_rect(Rect::new(4, 4, 32, 32), 6.0, 2.0, Colour::BLACK);
        assert_eq!(c.px[(20 * 40 + 20) * 4], 255, "centre untouched");
        assert_eq!(c.px[(20 * 40 + 4) * 4], 0, "left edge inked");
        assert_eq!(c.px[(20 * 40 + 6) * 4], 255, "two pixels in is paper again");
    }

    #[test]
    fn resampling_keeps_flat_colour_and_does_not_fringe_transparent_edges() {
        let mut img = RgbaImage::from_pixel(64, 64, Rgba([0, 0, 0, 0]));
        for y in 16..48 {
            for x in 16..48 {
                img.put_pixel(x, y, Rgba([200, 40, 40, 255]));
            }
        }
        let out = resample_premultiplied(&img, false, 20, 20);
        for p in out.as_chunks::<4>().0 {
            assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3], "premultiplied invariant: {p:?}");
            if p[3] == 255 {
                assert!((i32::from(p[0]) - 200).abs() <= 3, "inside the shape stays flat: {p:?}");
            }
            // (Below alpha 64 the byte quantisation alone moves the straight colour by more than the tolerance.)
            if (64..255).contains(&p[3]) {
                // The colour under a soft edge is the shape's own colour scaled by alpha: no dark halo.
                let straight = f32::from(p[0]) * 255.0 / f32::from(p[3]);
                assert!((straight - 200.0).abs() < 12.0, "{p:?}");
            }
        }
    }
}
