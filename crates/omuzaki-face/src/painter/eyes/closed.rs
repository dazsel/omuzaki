//! Closed eyes (`closedEye` of `face.js`, lines 243-256): a happy `^` arc for `arc >= 0.5` (a smile, a wink) and the
//! relaxed lash curve with three small lashes otherwise (a plain blink, or an eye nearly shut). Nothing else of the eye
//! is drawn for a closed one: the lid line, crease, lash polygon and tick are for open eyes.
//!
//! The two styles switch at `arc` 0.5, as the prototype's do; the face never invents an arc (the grammar stage does).

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cap, Cmd, FaceLayerId, Join, Paint};

use super::aperture::{Eye, Lid};
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

/// Paints one eye if it is shut, as the happy arc or the relaxed curve.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let squeeze = ctx.params.effects.squeeze;
    if !eye.shows_closed(squeeze) {
        return;
    }
    let arc = eye.arc_like();
    // The arc grows from the lowered lid line: over the open eye's upper lid until the eye is the `^` itself.
    let growth = if eye.lid() == Lid::Open { eye.arc_growth() } else { 1.0 };
    let shape: fn(f32) -> f32 = if arc {
        |u| ARC_BASE + ARC_HEIGHT * (1.0 - u * u)
    } else {
        |u| RELAXED_BASE - RELAXED_BOW * (1.0 - u * u) + RELAXED_SLOPE * u
    };
    // An eye the squeeze has just shut starts from the lens the open eye shrank to and drifts to the curve's own place
    // as the squeeze goes on, so the geometry is continuous where the open pieces give way. A blink or an arc has no
    // such open eye to follow (`drift` is 1).
    let drift = if arc { 1.0 } else { eye.handover_drift(squeeze) };
    let height = |u: f32| {
        if arc {
            growth * shape(u) + (1.0 - growth) * eye.top(u)
        } else {
            drift * shape(u) + (1.0 - drift) * eye.midline(u)
        }
    };
    // A squeeze turns the eye into the chevron of `effects`: the closed curve and its lashes fade out as the chevron
    // fades in, and take over from the open eye at full strength where it shuts, so there is no jump at any squeeze.
    let opacity = eye.closed_strength(squeeze);
    if opacity <= 0.0 {
        return;
    }
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
        opacity,
    });
    if !arc {
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
                opacity,
            });
        }
    }
}
