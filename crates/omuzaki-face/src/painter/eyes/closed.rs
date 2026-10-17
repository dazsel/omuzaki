//! Closed eyes (`closedEye` of `face.js`, lines 243-256): the relaxed lash curve with three small lashes (a plain
//! blink, or an eye nearly shut) and the happy `^` arc (a smile, a wink). Nothing else of the eye is drawn for a closed
//! one: the lid line, crease, lash polygon and tick are for open eyes.
//!
//! There is no switch between open and closed, nor between the two curves. [`Eye::shut`] says how shut the eye is,
//! whatever shuts it (a blink, an arc, a squeeze); the open pieces fade out by [`Eye::open_fade`] as this curve comes in
//! at [`Eye::closed_weight`], rising from the open eye's upper lid. The curve itself runs from the relaxed one to the
//! arc as [`Eye::arc_growth`] goes from 0 to 1 (arc 0.2 to 0.5), and the three small lashes fade out with it. The face
//! never invents an arc (the grammar stage does).

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cap, Cmd, FaceLayerId, Join, Paint};

use super::aperture::Eye;
use super::{FALLBACK_COLOUR, Side};
use crate::painter::PaintCtx;
use crate::painter::fixed_colour;

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

/// Paints one eye's closed curve, as much of it as the eye is shut (see [`Eye::shut`]): the relaxed lash curve, or the
/// happy arc as `arc` grows, rising from the open eye's upper lid as the eye shuts, so the curve is at the lid where the
/// open pieces still show and in its own place once they are gone. One formula for every combination of blink, arc and
/// squeeze: its weight, shape and position are all continuous in each of them.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let weight = eye.closed_weight();
    if weight <= 0.0 {
        return;
    }
    let (shut, grown) = (eye.shut(), eye.arc_growth());
    let height = |u: f32| {
        let relaxed = RELAXED_BASE - RELAXED_BOW * (1.0 - u * u) + RELAXED_SLOPE * u;
        let arc = ARC_BASE + ARC_HEIGHT * (1.0 - u * u);
        let shape = (1.0 - grown) * relaxed + grown * arc;
        (1.0 - shut) * eye.top(u.clamp(-1.0, 1.0)) + shut * shape
    };
    let colour = fixed_colour(&ctx.profile.eyes.lash.colour, FALLBACK_COLOUR);
    let mut points = [Vec2::ZERO; POINTS];
    for (i, p) in points.iter_mut().enumerate() {
        // `i` is at most 30, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let u = FROM + SPAN * i as f32 / (POINTS - 1) as f32;
        *p = eye.point(u, height(u));
    }
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&points).finish();
    list.push(Cmd::Stroke {
        path,
        paint: Paint::Solid(colour),
        width: WIDTH,
        cap: Cap::Round,
        join: Join::Round,
        opacity: weight,
    });
    // The three small lashes belong to the relaxed curve: they give way as the arc grows.
    let lashes = weight * (1.0 - grown);
    if lashes > 0.0 {
        for u in LASHES {
            let from = eye.point(u, height(u));
            let to = from + Vec2::new(eye.outward() * LASH_OUT, -LASH_DOWN);
            let lash = list.path().move_to(from).line_to(to).finish();
            list.push(Cmd::Stroke {
                path: lash,
                paint: Paint::Solid(colour),
                width: LASH_WIDTH,
                cap: Cap::Round,
                join: Join::Round,
                opacity: lashes,
            });
        }
    }
}
