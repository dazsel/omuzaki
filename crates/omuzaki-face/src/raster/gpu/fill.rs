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
