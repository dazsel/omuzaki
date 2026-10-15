//! The crease above the lid (the `eye()` body of `face.js`, lines 211-214): a fine curve that follows the animated upper
//! lid, lifted by [`Crease::lift`] eye units, so it moves with openness and tilt. Open eyes only.
//!
//! [`Crease::lift`]: omuzaki_face_api::Crease::lift

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{ColourExt, FaceLayerId, Paint, narrow};

use super::aperture::Eye;
use super::{FALLBACK_COLOUR, Side};
use crate::painter::PaintCtx;
use crate::painter::fixed_colour;

/// Points on the curve (`i = 0..=20`), from `u` [`FROM`] over [`SPAN`] (to 1.0).
const POINTS: usize = 21;
const FROM: f32 = -0.35;
const SPAN: f32 = 1.35;
/// The curve bows by this much of an eye unit, most over the middle (`0.04 (1 - u^2)`).
const BOW: f32 = 0.04;
/// The line's width, 1.1 mm.
const WIDTH: f32 = 0.0011;

/// Paints the crease of one open eye.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let style = &ctx.profile.eyes.crease;
    let strength = narrow(style.strength);
    if !eye.is_open() || strength <= 0.0 {
        return;
    }
    let raise = narrow(style.lift);
    let mut points = [Vec2::ZERO; POINTS];
    for (i, p) in points.iter_mut().enumerate() {
        // `i` is at most 20, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let u = FROM + SPAN * i as f32 / (POINTS - 1) as f32;
        *p = eye.point(u, eye.top(u.min(1.0)) + raise + BOW * (1.0 - u * u));
    }
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&points).finish();
    list.stroke(path, Paint::Solid(fixed_colour(&style.colour, FALLBACK_COLOUR).faded(strength)), WIDTH);
}
