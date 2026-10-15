//! The lash line with the cat-eye flick, the small lower lash and the inner tick (the `eye()` body of `face.js`, lines
//! 215-232), drawn after the crease, for an open eye only.
//!
//! - **Lash line**: a filled polygon along the upper lid from just inside the inner corner past the outer corner into the
//!   flick. Its thickness grows smoothly from the inner corner to the middle (`0.0036 + 0.0040 ss01((u + 0.9) / 1.6)` at
//!   the prototype's size) and 78 % of it grows upward from the lid's line, 22 % downward. Past the outer corner the
//!   line lifts and tapers, and one extra point is the sharp tip of the flick.
//! - **Lower lash**: a short separate curve under the outer corner, as strong as [`Lash::lower`].
//! - **Inner tick**: a short warm line at the inner corner, when [`Lash::inner_tick`] is on.
//!
//! # From the JS constants to the profile's [`Lash`]
//!
//! | Profile | Replaces (JS) |
//! |---|---|
//! | `thickness` | the thick end of the thickness profile: `0.0076` (`0.0036 + 0.0040`); the inner end keeps its `0.0036 / 0.0076` share of it |
//! | `flick` | the prototype's flick is `flick = 1`: it runs `0.22` eye units past the corner, lifts by `0.09` and ends in a tip `0.30` out and `0.16` up; a smaller flick scales all of that down to nothing at 0 |
//! | `lower` | the lower lash's opacity (the prototype always draws it, so `1`) |
//! | `inner_tick` | whether the tick is drawn (the prototype always draws it) |
//! | `colour` | `#3a2420` |
//!
//! [`Lash`]: omuzaki_face_api::Lash
//! [`Lash::lower`]: omuzaki_face_api::Lash::lower
//! [`Lash::inner_tick`]: omuzaki_face_api::Lash::inner_tick

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Colour, ColourExt, FaceLayerId, Paint, narrow};

use super::aperture::{Eye, smooth01};
use super::{FALLBACK_COLOUR, Side};
use crate::painter::PaintCtx;
use crate::painter::fixed_colour;

/// Segments along the lash line; the polygon has `N + 1` points on each edge.
const SEGMENTS: usize = 44;
/// The line starts this far before the inner corner (`u = -1.02`) and, at a full flick, ends this far past the outer
/// corner (`u = 1.22`).
const START: f32 = -1.02;
const FLICK_REACH: f32 = 0.22;
/// Past the outer corner the line lifts by this much per eye unit of reach ...
const FLICK_LIFT: f32 = 0.09 / 0.22;
/// ... and its thickness falls to `1 - (u - 1) / FLICK_TAPER` of itself.
const FLICK_TAPER: f32 = 0.24;
/// The tip of a full flick: this far past the outer corner and this far above the lid's height there.
const TIP_OUT: f32 = 0.30;
const TIP_UP: f32 = 0.16;
/// The thickness profile at the prototype's size, in metres: the inner end, the extra at the thick end, and where the
/// smoothstep rises (`(u + 0.9) / 1.6`). The profile's `thickness` is the sum, the thick end.
const THICK_INNER: f32 = 0.0036;
const THICK_EXTRA: f32 = 0.0040;
const THICK_FROM: f32 = 0.9;
const THICK_SPAN: f32 = 1.6;
/// The share of the thickness above the line's centre (the rest is below).
const ABOVE: f32 = 0.78;
const BELOW: f32 = 0.22;
/// The lower lash: from `(0.98, top - 0.10)` to `(1.22, top - 0.24)` (eye units), bowed 2 mm, 2 mm wide.
const LOWER_FROM: Vec2 = Vec2::new(0.98, -0.10);
const LOWER_TO: Vec2 = Vec2::new(1.22, -0.24);
const LOWER_BOW: f32 = 0.002;
const LOWER_WIDTH: f32 = 0.0020;
/// The inner tick: `rgba(120,60,60,0.7)`, 1.3 mm wide, reaching 4 mm toward the face centre and 3 mm down.
const TICK_COLOUR: Colour = Colour::rgba(120, 60, 60, 179);
const TICK_WIDTH: f32 = 0.0013;
const TICK_REACH: Vec2 = Vec2::new(0.004, 0.003);
/// A flick below this draws no flick.
const FLICK_MIN: f32 = 0.005;

/// Paints the lash line, the lower lash and the inner tick of one open eye.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    if !eye.is_open() {
        return;
    }
    let style = &ctx.profile.eyes.lash;
    let colour = fixed_colour(&style.colour, FALLBACK_COLOUR);
    let (thickness, flick) = (narrow(style.thickness), narrow(style.flick).clamp(0.0, 1.0));
    lash_line(ctx, &eye, colour, thickness, flick);
    let lower = narrow(style.lower).clamp(0.0, 1.0);
    if lower > 0.0 {
        lower_lash(ctx, &eye, colour.faded(lower));
    }
    if style.inner_tick {
        inner_tick(ctx, &eye);
    }
}

/// The lash polygon: the top edge in order, the flick's tip, then the bottom edge back, filled.
fn lash_line(ctx: &mut PaintCtx<'_>, eye: &Eye, colour: Colour, thickness: f32, flick: f32) {
    let scale = thickness / (THICK_INNER + THICK_EXTRA);
    let end = 1.0 + FLICK_REACH * flick;
    // Each sample's centre-line point and its thickness in metres.
    let sample = |i: usize| {
        // `i` is at most 44, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let u = START + (end - START) * i as f32 / SEGMENTS as f32;
        let uu = u.clamp(-1.0, 1.0);
        let lift = if u > 1.0 { FLICK_LIFT * (u - 1.0) } else { 0.0 };
        let taper = if u > 1.0 { 1.0 - (u - 1.0) / FLICK_TAPER } else { 1.0 };
        let th = (THICK_INNER + THICK_EXTRA * smooth01((uu + THICK_FROM) / THICK_SPAN)) * scale * taper;
        (eye.point(u, eye.top(uu) + lift), th)
    };
    let list = ctx.list(FaceLayerId::Base);
    let mut outline = list.path();
    for i in 0..=SEGMENTS {
        let (p, th) = sample(i);
        let top = p + Vec2::new(0.0, ABOVE * th);
        outline = if i == 0 { outline.move_to(top) } else { outline.line_to(top) };
    }
    if flick > FLICK_MIN {
        outline = outline.line_to(eye.point(1.0 + TIP_OUT * flick, eye.top(1.0) + TIP_UP * flick));
    }
    for i in (0..=SEGMENTS).rev() {
        let (p, th) = sample(i);
        outline = outline.line_to(p - Vec2::new(0.0, BELOW * th));
    }
    let path = outline.close().finish();
    list.fill(path, Paint::Solid(colour));
}

/// The small separate lash under the outer corner: a quadratic bowed a little downward.
fn lower_lash(ctx: &mut PaintCtx<'_>, eye: &Eye, colour: Colour) {
    let top = eye.top(1.0);
    let (from, to) = (eye.point(LOWER_FROM.x, top + LOWER_FROM.y), eye.point(LOWER_TO.x, top + LOWER_TO.y));
    let control = from.midpoint(to) - Vec2::new(0.0, LOWER_BOW);
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().move_to(from).quad_to(control, to).finish();
    list.stroke(path, Paint::Solid(colour), LOWER_WIDTH);
}

/// The inner-corner tick: from the inner corner of the lid toward the face centre and down.
fn inner_tick(ctx: &mut PaintCtx<'_>, eye: &Eye) {
    let from = eye.point(-1.0, eye.top(-1.0));
    let to = from + Vec2::new(-eye.outward() * TICK_REACH.x, -TICK_REACH.y);
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().move_to(from).line_to(to).finish();
    list.stroke(path, Paint::Solid(TICK_COLOUR), TICK_WIDTH);
}
