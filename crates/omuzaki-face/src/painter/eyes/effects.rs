//! Eye effects: star eyes, gloom, shock, spiral, squeeze and tears. The heart and star catchlights, the sparkle dot and
//! the shimmer are [`super::highlight`]'s; the sweat drop and the gloom hatch are [`crate::painter::overlay`]'s.
//!
//! The prototype has three effects (the heart catchlight, the sparkle and the tear drop); the other five are new designs
//! in its drawing language: flat colours, fine dark lines, one soft shade. Every effect scales smoothly from 0 to 1, by
//! opacity or by shape, never by a switch, and draws nothing at 0, so effects compose (a heart with tears and blush) and
//! nothing pops.
//!
//! | Effect | What it draws |
//! |---|---|
//! | `star` | a five-point star over the iris, lighter than it, with the iris ring's outline (and the star catchlight, in `highlight`) |
//! | `gloom` | the iris darkened to one flat shade; the highlights lose their light (`highlight`); the hatch on the Overlay layer |
//! | `shock` | the iris shrinks to a small disc with a white dot for a pupil, the eye white showing round it |
//! | `spiral` | a two-turn spiral over the iris, in the ring's dark |
//! | `squeeze` | the lid closes and the eye becomes a `>` or `<` pointing at the nose |
//! | `tears` | the prototype's tear drop under the eye (`tear`, `face.js` 257-263) |
//!
//! The iris effects ([`paint_iris`]) draw inside the aperture clip, so they are cut by the lids; the chevron and the tear
//! ([`paint`]) draw outside it, after the lash.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cap, Cmd, FaceLayerId, Join, Paint};

use super::aperture::Eye;
use super::iris::{Iris, lighten};
use super::{FALLBACK_COLOUR, Side};
use crate::painter::PaintCtx;
use crate::painter::fixed_colour;

/// Below this an effect counts as off.
const OFF: f32 = 0.01;

// ---- gloom

/// The flat shade of a gloomy iris: the iris colour darkened to this share, at this opacity.
const GLOOM_DARKEN: f32 = 0.35;
const GLOOM_OPACITY: f32 = 0.9;

// ---- shock

/// The shocked iris's radii as a share of the iris's, and the white dot's radius as a share of the iris half-width.
const SHOCK_IRIS: f32 = 0.45;
const SHOCK_DOT: f32 = 0.16;
/// The eye white's cover is this much bigger than the iris, so no edge of the iris shows round it.
const SHOCK_COVER: f32 = 1.08;
/// The white dot sits this far above the small iris's centre, in its own half-heights.
const SHOCK_DOT_RISE: f32 = 0.1;

// ---- spiral

/// Turns of the spiral, points along it, its width in metres, and its opacity at full strength.
const SPIRAL_TURNS: f32 = 2.0;
const SPIRAL_POINTS: usize = 49;
const SPIRAL_WIDTH: f32 = 0.0012;
const SPIRAL_OPACITY: f32 = 0.9;
/// The spiral's outermost radius as a share of the iris's.
const SPIRAL_REACH: f32 = 0.92;

// ---- star

/// The star's points, its outer and inner radii as shares of the iris half-width, how much lighter than the iris's low
/// colour it is, its opacity at full strength, and the outline's.
const STAR_POINTS: usize = 5;
const STAR_OUTER: f32 = 0.95;
const STAR_INNER: f32 = 0.42;
const STAR_LIGHTEN: f32 = 0.55;
const STAR_OPACITY: f32 = 0.92;
const STAR_OUTLINE_WIDTH: f32 = 0.0008;
const STAR_OUTLINE_OPACITY: f32 = 0.6;

// ---- squeeze

/// The chevron: its arms' ends, `(u, v)` in eye units, the vertex toward the nose, the width in metres.
const SQUEEZE_ARM: Vec2 = Vec2::new(0.85, 0.40);
const SQUEEZE_VERTEX: Vec2 = Vec2::new(-0.60, 0.0);
const SQUEEZE_WIDTH: f32 = 0.0048;

// ---- tears (`face.js` 257-263)

/// The tear's place: toward the nose by this many eye half-widths, below the eye's centre by this many (below the lowest
/// point of the lower lid, which is at 0.56).
const TEAR_IN: f32 = 0.2;
const TEAR_DOWN: f32 = 0.62;
/// The drop: from its top, the control points' offsets, the bottom, in metres (about 4 mm wide, 13 mm tall).
const TEAR_CONTROL: Vec2 = Vec2::new(0.004, 0.010);
const TEAR_HEIGHT: f32 = 0.013;

/// The iris effects of one open eye, inside its aperture clip: gloom, shock, spiral and star eyes, in that order.
pub(super) fn paint_iris(ctx: &mut PaintCtx<'_>, side: Side) {
    let effects = ctx.params.effects;
    let wanted = [effects.gloom, effects.shock, effects.spiral, effects.star].iter().any(|e| *e > OFF);
    if !wanted {
        return;
    }
    let eye = Eye::new(ctx.profile, ctx.params, side);
    if !eye.is_open() {
        return;
    }
    let iris = Iris::new(ctx.profile, ctx.params, &eye);
    if effects.gloom > OFF {
        gloom(ctx, &iris, effects.gloom.min(1.0));
    }
    if effects.shock > OFF {
        shock(ctx, &iris, effects.shock.min(1.0));
    }
    if effects.spiral > OFF {
        spiral(ctx, &iris, effects.spiral.min(1.0));
    }
    if effects.star > OFF {
        star_iris(ctx, &iris, effects.star.min(1.0));
    }
}

/// The effects outside the clip, after the lash and the tick: the squeeze chevron, then the tear.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let effects = ctx.params.effects;
    if effects.squeeze > OFF {
        squeeze(ctx, side, effects.squeeze.min(1.0));
    }
    if effects.tears > OFF {
        tear(ctx, side, effects.tears.min(1.0));
    }
}

/// `colour` with each channel scaled by `k`.
fn darken(colour: Colour, k: f32) -> Colour {
    let down = |c: u8| {
        // In 0..=255 by construction.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let out = (f32::from(c) * k).round() as u8;
        out
    };
    Colour::rgba(down(colour.r), down(colour.g), down(colour.b), colour.a)
}

/// The iris darkened to one flat shade.
fn gloom(ctx: &mut PaintCtx<'_>, iris: &Iris, strength: f32) {
    let shade = darken(fixed_colour(&ctx.profile.eyes.iris.mid, FALLBACK_COLOUR), GLOOM_DARKEN);
    let list = ctx.list(FaceLayerId::Base);
    let path = iris.outline(list);
    list.push(Cmd::Fill { path, paint: Paint::Solid(shade), opacity: GLOOM_OPACITY * strength });
}

/// The iris covered by the eye white and replaced by a small disc with a white dot for a pupil.
fn shock(ctx: &mut PaintCtx<'_>, iris: &Iris, strength: f32) {
    let eyes = &ctx.profile.eyes;
    let (white, disc) =
        (fixed_colour(&eyes.sclera.mid, FALLBACK_COLOUR), fixed_colour(&eyes.iris.mid, FALLBACK_COLOUR));
    let list = ctx.list(FaceLayerId::Base);
    // A little bigger than the iris, so no edge of it shows round the cover.
    let cover = list.path().ellipse(iris.centre, iris.rx * SHOCK_COVER, iris.ry * SHOCK_COVER).finish();
    list.push(Cmd::Fill { path: cover, paint: Paint::Solid(white), opacity: strength });
    let small = list.path().ellipse(iris.centre, iris.rx * SHOCK_IRIS, iris.ry * SHOCK_IRIS).finish();
    list.push(Cmd::Fill { path: small, paint: Paint::Solid(disc), opacity: strength });
    let dot_at = iris.centre + Vec2::new(0.0, iris.ry * SHOCK_IRIS * SHOCK_DOT_RISE);
    let r = iris.rx * SHOCK_DOT;
    let dot = list.path().ellipse(dot_at, r, r).finish();
    list.push(Cmd::Fill { path: dot, paint: Paint::Solid(Colour::rgb(255, 255, 255)), opacity: strength });
}

/// A two-turn spiral from the iris's centre outward.
fn spiral(ctx: &mut PaintCtx<'_>, iris: &Iris, strength: f32) {
    let colour = fixed_colour(&ctx.profile.eyes.iris.ring, FALLBACK_COLOUR);
    let mut points = [Vec2::ZERO; SPIRAL_POINTS];
    for (i, p) in points.iter_mut().enumerate() {
        // `i` is at most 48, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let t = i as f32 / (SPIRAL_POINTS - 1) as f32;
        let angle = std::f32::consts::TAU * SPIRAL_TURNS * t;
        *p = iris.at(t * SPIRAL_REACH * angle.cos(), t * SPIRAL_REACH * angle.sin());
    }
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&points).finish();
    list.push(Cmd::Stroke {
        path,
        paint: Paint::Solid(Colour::rgb(colour.r, colour.g, colour.b)),
        width: SPIRAL_WIDTH,
        cap: Cap::Round,
        join: Join::Round,
        opacity: SPIRAL_OPACITY * strength,
    });
}

/// A five-point star over the iris, lighter than it, outlined like the iris.
fn star_iris(ctx: &mut PaintCtx<'_>, iris: &Iris, strength: f32) {
    let eyes = &ctx.profile.eyes.iris;
    let fill = lighten(fixed_colour(&eyes.low, FALLBACK_COLOUR), STAR_LIGHTEN);
    let ring = fixed_colour(&eyes.ring, FALLBACK_COLOUR);
    let mut points = [Vec2::ZERO; 2 * STAR_POINTS];
    for (i, p) in points.iter_mut().enumerate() {
        // `i` is at most 9, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let angle = std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / STAR_POINTS as f32;
        let reach = if i % 2 == 0 { STAR_OUTER } else { STAR_INNER };
        // `at` scales x by `rx` and y by `ry`; the star's radii are measured from the iris half-width `rx`, so y is
        // corrected by `rx / ry`.
        *p = iris.at(reach * angle.cos(), reach * angle.sin() * iris.rx / iris.ry);
    }
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&points).close().finish();
    list.push(Cmd::Fill { path, paint: Paint::Solid(fill), opacity: STAR_OPACITY * strength });
    list.push(Cmd::Stroke {
        path,
        paint: Paint::Solid(ring),
        width: STAR_OUTLINE_WIDTH,
        cap: Cap::Round,
        join: Join::Round,
        opacity: STAR_OUTLINE_OPACITY * strength,
    });
}

/// The `>` or `<` of a squeezed eye: two arms from the outer corner region to a vertex toward the nose, in the lash
/// colour. The lid closes with the effect (see [`Eye::new`]), so the chevron fades in over a closing eye.
fn squeeze(ctx: &mut PaintCtx<'_>, side: Side, strength: f32) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let colour = fixed_colour(&ctx.profile.eyes.lash.colour, FALLBACK_COLOUR);
    let (arm, vertex) = (SQUEEZE_ARM, SQUEEZE_VERTEX);
    let points = [eye.point(arm.x, arm.y), eye.point(vertex.x, vertex.y), eye.point(arm.x, -arm.y)];
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&points).finish();
    list.push(Cmd::Stroke {
        path,
        paint: Paint::Solid(colour),
        width: SQUEEZE_WIDTH,
        cap: Cap::Round,
        join: Join::Round,
        opacity: strength,
    });
}

/// The tear drop under an open eye: a pointed drop toward the nose, fading in with the effect (its size is fixed).
fn tear(ctx: &mut PaintCtx<'_>, side: Side, strength: f32) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    if !eye.is_open() {
        return;
    }
    let colour = fixed_colour(&ctx.profile.eyes.tear, FALLBACK_COLOUR);
    let top = eye.point(-TEAR_IN, -TEAR_DOWN);
    let list = ctx.list(FaceLayerId::Base);
    // The prototype's canvas offsets are y-down; here y is up.
    let at = |x: f32, y: f32| top + Vec2::new(x, -y);
    let path = list
        .path()
        .move_to(top)
        .quad_to(at(TEAR_CONTROL.x, TEAR_CONTROL.y), at(0.0, TEAR_HEIGHT))
        .quad_to(at(-TEAR_CONTROL.x, TEAR_CONTROL.y), top)
        .close()
        .finish();
    list.push(Cmd::Fill { path, paint: Paint::Solid(colour), opacity: strength });
}
