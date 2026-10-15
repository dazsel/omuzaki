//! Paths for the GPU encoder: face-plane elements as a kurbo path, and the pixel bounds of what was encoded.

use omuzaki_face_api::PathEl;
use vello::kurbo::{Affine, BezPath, Point, Rect};

use super::gradient::point;

/// Refills `out` with `els`, keeping its capacity.
pub(super) fn fill_bez(els: &[PathEl], out: &mut BezPath) {
    out.truncate(0);
    for el in els {
        match *el {
            PathEl::Move(a) => out.move_to(point(a)),
            PathEl::Line(a) => out.line_to(point(a)),
            PathEl::Quad(a, b) => out.quad_to(point(a), point(b)),
            PathEl::Cubic(a, b, c) => out.curve_to(point(a), point(b), point(c)),
            PathEl::Close => out.close_path(),
        }
    }
}

/// Refills `out` with `els` already in pixel space under `to_px`, the y of every on-curve point snapped to the nearest
/// quarter pixel.
///
/// This is what the CPU raster, and Skia under it, does to a filled or clipping path before it computes coverage
/// (`raster::scan`). Vello's coverage is exact, so without the snap a horizontal edge 0.2 px into a row would be 51 where
/// the CPU raster, and Chrome, write 64 (#2002). Control points are left alone: they are not on the curve, and moving an
/// end point already bends the curve by the same fraction of a pixel Skia's own snapping does.
pub(super) fn fill_bez_snapped(els: &[PathEl], to_px: Affine, out: &mut BezPath) {
    let px = |p| to_px * point(p);
    let on_curve = |p| {
        let p: Point = px(p);
        Point::new(p.x, snap_quarter(p.y))
    };
    out.truncate(0);
    for el in els {
        match *el {
            PathEl::Move(a) => out.move_to(on_curve(a)),
            PathEl::Line(a) => out.line_to(on_curve(a)),
            PathEl::Quad(a, b) => out.quad_to(px(a), on_curve(b)),
            PathEl::Cubic(a, b, c) => out.curve_to(px(a), px(b), on_curve(c)),
            PathEl::Close => out.close_path(),
        }
    }
}

/// `y` rounded to the nearest quarter, halves up, as Skia's `SnapY` does for anti-aliased edges.
fn snap_quarter(y: f64) -> f64 {
    (y * 4.0 + 0.5).floor() / 4.0
}

/// The pixel rectangle everything encoded so far fits in.
///
/// It is the hull of the transformed control points, which contains the curve, so it may be slightly larger than the
/// drawn shape but never smaller: the damage rectangle built from it can only over-repaint.
#[derive(Default)]
pub(super) struct Bounds(Option<Rect>);

impl Bounds {
    /// Forgets everything.
    pub(super) fn clear(&mut self) {
        self.0 = None;
    }

    /// The bounds, or `None` if nothing was encoded.
    pub(super) fn get(&self) -> Option<Rect> {
        self.0
    }

    /// Adds `path` under `to_px`, grown by `pad` pixels on every side (half a stroke's reach).
    pub(super) fn add(&mut self, path: &BezPath, to_px: Affine, pad: f64) {
        let mut hull: Option<Rect> = None;
        let mut grow = |p: Point| {
            let p = to_px * p;
            let r = Rect::new(p.x - pad, p.y - pad, p.x + pad, p.y + pad);
            hull = Some(hull.map_or(r, |h| h.union(r)));
        };
        for el in path.elements() {
            match *el {
                vello::kurbo::PathEl::MoveTo(a) | vello::kurbo::PathEl::LineTo(a) => grow(a),
                vello::kurbo::PathEl::QuadTo(a, b) => {
                    grow(a);
                    grow(b);
                }
                vello::kurbo::PathEl::CurveTo(a, b, c) => {
                    grow(a);
                    grow(b);
                    grow(c);
                }
                vello::kurbo::PathEl::ClosePath => {}
            }
        }
        if let Some(h) = hull {
            self.0 = Some(self.0.map_or(h, |b| b.union(h)));
        }
    }
}
