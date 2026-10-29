//! The pixel rectangle a draw command can touch, for the partial CPU repaint (#2443): conservative (it may hold pixels
//! the command leaves alone, never the reverse), cheap, and the same for the repaint's damage and the rasteriser's
//! skipping.

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cmd, DrawList, FaceLayerSpec, Join, PathEl, RasterRect};

/// Pixels added round a command's rectangle for anti-aliasing and rounding.
const AA_PAD: f32 = 2.0;

/// The canvas's miter limit: a miter reaches at most this many half-widths from its corner.
const MITER_LIMIT: f32 = 10.0;

/// Plane (metres, y up) to the raster's pixels (y down), as the CPU rasteriser maps them.
pub(super) fn to_pixels(spec: &FaceLayerSpec, (w, h): (u32, u32)) -> Affine2 {
    let rect = spec.rect;
    #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^24
    let (sx, sy) = (w as f32 / rect.width(), h as f32 / rect.height());
    Affine2::from_cols(Vec2::new(sx, 0.0), Vec2::new(0.0, -sy), Vec2::new(-rect.x0 * sx, rect.y1 * sy))
}

/// The rectangle a fill, stroke or clip draws (or limits drawing to) inside a `size` raster under `to_px` (plane to
/// pixels, transforms included); `None` for the other commands, or if it misses the raster.
pub(super) fn command_extent(cmd: &Cmd, list: &DrawList, to_px: Affine2, size: (u32, u32)) -> Option<RasterRect> {
    match *cmd {
        Cmd::Fill { path, .. } | Cmd::PushClip(path) => extent(list.elements(path), to_px, 0.0, size),
        Cmd::Stroke { path, width, join, .. } => {
            let scale = to_px.matrix2.x_axis.length().max(to_px.matrix2.y_axis.length());
            let corner = match join {
                Join::Miter => miter_reach(list.elements(path)),
                Join::Round => 1.0,
            };
            extent(list.elements(path), to_px, corner * 0.5 * width.abs() * scale, size)
        }
        Cmd::PopClip | Cmd::PushTransform(_) | Cmd::PopTransform => None,
    }
}

/// How many half-widths a mitred stroke of `elements` reaches past its points: the longest miter among its corners
/// (`1 / cos(turn / 2)`), or 1 where every miter over the limit is bevelled. Most of a face's strokes are smooth curves
/// whose corners barely turn, so this is far smaller than the limit (#2443).
fn miter_reach(elements: &[PathEl]) -> f32 {
    let mut reach = 1.0_f32;
    let mut corner = |into: Vec2, out: Vec2| {
        let (a, b) = (into.normalize_or_zero(), out.normalize_or_zero());
        if a == Vec2::ZERO || b == Vec2::ZERO {
            return;
        }
        // The miter's length over the half-width is 1 / cos(turn / 2), and cos(turn / 2)^2 = (1 + cos(turn)) / 2.
        let half_cos = f32::midpoint(1.0, a.dot(b)).max(0.0).sqrt();
        let ratio = if half_cos > 0.0 { half_cos.recip() } else { f32::INFINITY };
        if ratio <= MITER_LIMIT {
            reach = reach.max(ratio);
        }
    };
    // The direction a sub-path leaves its start in, the point it is at, and the direction it arrived in.
    let (mut first, mut start, mut at, mut last): (Option<Vec2>, Vec2, Vec2, Option<Vec2>) =
        (None, Vec2::ZERO, Vec2::ZERO, None);
    for el in elements {
        let (out, end, into) = match *el {
            PathEl::Move(p) => {
                (first, start, at, last) = (None, p, p, None);
                continue;
            }
            PathEl::Line(p) => (p - at, p, p - at),
            PathEl::Quad(c, p) => (c - at, p, p - c),
            PathEl::Cubic(c1, c2, p) => (c1 - at, p, p - c2),
            PathEl::Close => {
                // The closing line, then the corner back into the first segment.
                let close = start - at;
                if close != Vec2::ZERO {
                    if let Some(l) = last {
                        corner(l, close);
                    }
                    if let Some(f) = first {
                        corner(close, f);
                    }
                } else if let (Some(l), Some(f)) = (last, first) {
                    corner(l, f);
                }
                (first, at, last) = (None, start, None);
                continue;
            }
        };
        if let Some(l) = last {
            corner(l, out);
        }
        first.get_or_insert(out);
        (at, last) = (end, Some(into));
    }
    if reach.is_finite() { reach } else { MITER_LIMIT }
}

/// The pixel rectangle inside a `size` raster that a path's points (control points included, which bound the curves)
/// cover under `to_px`, grown by `reach` pixels and the anti-aliasing pad; `None` if that misses the raster. A point that
/// is not finite makes it the whole raster: the rasteriser rejects such a list anyway.
fn extent(elements: &[PathEl], to_px: Affine2, reach: f32, (w, h): (u32, u32)) -> Option<RasterRect> {
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    let mut add = |p: Vec2| {
        let q = to_px.transform_point2(p);
        lo = lo.min(q);
        hi = hi.max(q);
    };
    for el in elements {
        match *el {
            PathEl::Move(p) | PathEl::Line(p) => add(p),
            PathEl::Quad(c, p) => {
                add(c);
                add(p);
            }
            PathEl::Cubic(c1, c2, p) => {
                add(c1);
                add(c2);
                add(p);
            }
            PathEl::Close => {}
        }
    }
    if lo.x > hi.x || lo.y > hi.y {
        return None;
    }
    #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^24
    let (wf, hf) = (w as f32, h as f32);
    if !(lo.is_finite() && hi.is_finite()) {
        return Some(RasterRect::full(w, h));
    }
    let pad = reach + AA_PAD;
    let (x0, y0) = ((lo.x - pad).floor().clamp(0.0, wf), (lo.y - pad).floor().clamp(0.0, hf));
    let (x1, y1) = ((hi.x + pad).ceil().clamp(0.0, wf), (hi.y + pad).ceil().clamp(0.0, hf));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped into the raster first
    let rect = RasterRect { x: x0 as u32, y: y0 as u32, width: (x1 - x0) as u32, height: (y1 - y0) as u32 };
    (!rect.is_empty()).then_some(rect)
}
