//! Hairlines for the GPU encoder: strokes thinner than a pixel, drawn the way the CPU raster (and the canvas) draws them.
//!
//! Skia doesn't outline a stroke that is under a pixel across in both directions under the transform
//! (`SkDrawTreatAsHairline`): it draws the path as a one-pixel-wide anti-aliased line and scales the paint's 8-bit
//! alpha by the width, in whole 256ths. Vello outlines every stroke, and an outline that thin covers less and less of
//! each pixel: a 0.05 px line drew a quarter more ink than the canvas, a 0.3 px one 7 % more, and the thinnest fade
//! differently along a curve. Drawing a one-pixel line with Skia's alpha keeps the GPU within 4 % of the CPU raster's
//! coverage at every width, and a hairline never vanishes.
//!
//! Strokes a pixel or more across stay with Vello's stroker. Outlining them with the CPU raster's own stroker (Skia's
//! `SkStroke`) and filling the outline was tried for #318: it moved the stroke probes' bad pixels by a tenth of a
//! percent at most, because what separates the two rasters there is the CPU raster's quarter-row edge snapping, which
//! the live path leaves out on purpose so a thin line's ink doesn't swing with its sub-pixel position (#888, ruling
//! (a)). It also cost a CPU outline per stroke on every encode.

use omuzaki_core::glam::Affine2;
use omuzaki_face_api::{Paint, PathEl};
use vello::kurbo::{Affine, BezPath};

use super::super::hairline;
use super::fill::fill_bez;

/// Writes `els` into `out` in pixel space under `to_px` and returns the opacity factor to draw it with as a one-pixel
/// line, if a stroke `width` wide counts as a hairline there; a factor of 0 is a hairline too faint to draw at all.
/// `None`: not a hairline, and `out` is left as it was.
pub(super) fn hairline(
    els: &[PathEl],
    width: f32,
    to_px: Affine,
    paint: &Paint,
    opacity: f32,
    out: &mut BezPath,
) -> Option<f32> {
    let coverage = hairline::coverage(width, glam_affine(to_px))?;
    fill_bez(els, out);
    out.apply_affine(to_px);
    Some(factor(paint, opacity, coverage))
}

/// A kurbo affine as the CPU raster's glam one.
fn glam_affine(a: Affine) -> Affine2 {
    #[allow(clippy::cast_possible_truncation)] // the CPU raster works in f32; so does the plane
    Affine2::from_cols_array(&a.as_coeffs().map(|c| c as f32))
}

/// The opacity factor Skia gives a hairline of `coverage` (its width in pixels): the paint's 8-bit alpha times the
/// coverage in whole 256ths, as a fraction of that alpha; 0 when it rounds to nothing.
fn factor(paint: &Paint, opacity: f32, coverage: f32) -> f32 {
    let alpha = match paint {
        Paint::Solid(c) => f32::from(c.a) * opacity.clamp(0.0, 1.0),
        Paint::Linear { .. } | Paint::Radial { .. } => 255.0 * opacity.clamp(0.0, 1.0),
    };
    #[allow(clippy::cast_possible_truncation)] // 0..=255 and 0..=256
    let (a8, scale) = (alpha.round() as i32, (coverage * 256.0) as i32);
    let scaled = (a8 * scale) >> 8;
    #[allow(clippy::cast_precision_loss)] // 8-bit values
    if scaled > 0 { scaled as f32 / a8 as f32 } else { 0.0 }
}
