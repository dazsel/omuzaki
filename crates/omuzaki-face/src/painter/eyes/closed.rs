//! Closed eyes (`closedEye` of `face.js`, lines 243-256): a happy `^` arc for `arc >= 0.5` (a smile, a wink) and the
//! relaxed lash curve with three small lashes otherwise (a plain blink, or an eye nearly shut). Nothing else of the eye
//! is drawn for a closed one: the lid line, crease, lash polygon and tick are for open eyes.
//!
//! The two styles switch at `arc` 0.5, as the prototype's do; the face never invents an arc (the grammar stage does).

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{FaceLayerId, Paint};

use super::aperture::{Eye, Lid};
use super::{Side, fixed_colour};
use crate::painter::PaintCtx;

/// Points on the curve (`i = 0..=30`), `u` from [`FROM`] over [`SPAN`] (to 1.15).
const POINTS: usize = 31;
const FROM: f32 = -0.95;
const SPAN: f32 = 2.1;
/// The line's width, 4.8 mm.
const WIDTH: f32 = 0.0048;
/// The happy arc: `-0.05 + 0.40 (1 - u^2)`.
const ARC_BASE: f32 = -0.05;
const ARC_HEIGHT: f32 = 0.40;
/// The relaxed curve: `-0.06 - 0.16 (1 - u^2) + 0.10 u`, a gentle downward bow rising toward the outer corner.
const RELAXED_BASE: f32 = -0.06;
const RELAXED_BOW: f32 = 0.16;
const RELAXED_SLOPE: f32 = 0.10;
/// The relaxed curve's three small lashes: where they stand along it (eye units), how far each reaches outward and down
/// (metres), and how wide they are (1.8 mm).
const LASHES: [f32; 3] = [0.6, 0.85, 1.05];
const LASH_OUT: f32 = 0.003;
const LASH_DOWN: f32 = 0.005;
const LASH_WIDTH: f32 = 0.0018;

/// Paints one eye if it is shut, as the happy arc or the relaxed curve.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let height: fn(f32) -> f32 = match eye.lid() {
        Lid::Open => return,
        Lid::Arc => |u| ARC_BASE + ARC_HEIGHT * (1.0 - u * u),
        Lid::Relaxed => |u| RELAXED_BASE - RELAXED_BOW * (1.0 - u * u) + RELAXED_SLOPE * u,
    };
    let colour = fixed_colour(&ctx.profile.eyes.lash.colour);
    let mut points = [Vec2::ZERO; POINTS];
    for (i, p) in points.iter_mut().enumerate() {
        // `i` is at most 30, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let u = FROM + SPAN * i as f32 / (POINTS - 1) as f32;
        *p = eye.point(u, height(u));
    }
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&points).finish();
    list.stroke(path, Paint::Solid(colour), WIDTH);
    if eye.lid() == Lid::Relaxed {
        for u in LASHES {
            let from = eye.point(u, height(u));
            let to = from + Vec2::new(eye.outward() * LASH_OUT, -LASH_DOWN);
            let lash = list.path().move_to(from).line_to(to).finish();
            list.stroke(lash, Paint::Solid(colour), LASH_WIDTH);
        }
    }
}
