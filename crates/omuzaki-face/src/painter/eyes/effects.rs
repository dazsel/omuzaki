//! Eye effects: star eyes, gloom, shock, spiral, squeeze, tears, and the reference bar's symbol eyes (blank, dot, stare,
//! pill) and deep gloom (#2404, #2405, #2408). The heart and star catchlights, the sparkle dot and the shimmer are
//! [`super::highlight`]'s; the sweat drop, the gloom hatch and the anger vein are [`crate::painter::overlay`]'s.
//!
//! The prototype has three effects (the heart catchlight, the sparkle and the tear drop); the other five are new designs
//! in its drawing language: flat colours, fine dark lines, one soft shade. Every effect scales smoothly from 0 to 1, by
//! opacity or by shape, never by a switch, and draws nothing at 0, so effects compose (a heart with tears and blush) and
//! nothing pops.
//!
//! | Effect | What it draws |
//! |---|---|
//! | `heart` | a pink heart over the iris, with the iris ring's outline (and the heart catchlight, in `highlight`) |
//! | `star` | a five-point star over the iris, lighter than it, with the iris ring's outline (and the star catchlight, in `highlight`) |
//! | `gloom` | the iris darkened to one flat shade; the highlights lose their light (`highlight`); the hatch on the Overlay layer |
//! | `shock` | the iris shrinks to a small disc with a white dot for a pupil, the eye white showing round it |
//! | `spiral` | a two-turn spiral over the iris, in the ring's dark |
//! | `squeeze` | the lid closes and the eye becomes a `>` or `<` pointing at the nose |
//! | `tears` | the prototype's tear drop under the eye (`tear`, `face.js` 257-263) |
//! | `blank` | the whole opening flat white, no iris and no catchlight, outlined boldly in the lash colour |
//! | `dot` | the eye gives way to a small solid dot in the lash colour |
//! | `stare` | the iris shrinks to a small pinned disc (`iris`), the lids open wide (the mapping) and a heavy shadow hangs under the eye |
//! | `pill` | the eye gives way to a flat capsule, the iris colour on its outer half and white on its inner half, outlined |
//! | deep `gloom` | above 0.6 a dark band falls over the upper face, eyes included, and the pupils glow through it |
//!
//! The iris effects ([`paint_iris`]) draw inside the aperture clip, so they are cut by the lids; the chevron and the tear
//! ([`paint`]) draw outside it, after the lash. The blank eye's white ([`paint_cover`]) is the last thing inside the clip,
//! so it hides the catchlights too. The dot and the pill take the eye's place: the drawn eye fades out by them
//! ([`Eye::open_fade`]) while they fade in. Deep gloom ([`paint_deep_gloom`]) runs once, after both eyes.
//!
//! # Which effects follow the eye, and which do not
//!
//! An eye can shut (a blink, an arc, a squeeze: see [`Eye::shut`]), and every effect drawn ON the open eye goes with it,
//! so none outlives or pops against the open pieces: the iris effects, the catchlights and the sparkle are painted among
//! the open pieces, which fade together by [`Eye::open_fade`]; the tear, which is painted after them, multiplies its own
//! opacity by it. The squeeze chevron is the exception that takes the eye's place (it fades in by the squeeze itself). The
//! effects that live OFF the eye keep their own timing: the sweat drop and the gloom band and hatch are on the Overlay
//! layer, on the forehead and temple, and do not depend on the lids.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cap, Cmd, FaceLayerId, Join, Paint, Stops};

use super::aperture::{self, Eye, smooth01};
use super::highlight::heart;
use super::iris::{Iris, colours, lighten};
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

// ---- heart

/// The heart over the iris: its size as a share of the iris half-width (the heart path is about twice that wide), its
/// colour, its opacity at full strength, and the outline's. The catchlight heart of the prototype alone is too small to
/// read at stream size, so heart eyes paint the iris over, as star eyes do.
const HEART_IRIS: f32 = 0.92;
const HEART_COLOUR: Colour = Colour::rgb(0xff, 0x6f, 0x9a);
const HEART_OPACITY: f32 = 0.94;
const HEART_OUTLINE_WIDTH: f32 = 0.0008;
const HEART_OUTLINE_OPACITY: f32 = 0.7;
/// The heart sits this far below the iris centre, in iris half-heights, so its lobes are not cut by the upper lid.
const HEART_DROP: f32 = 0.12;

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

// ---- blank

/// The blank eye's white and the width of its bold outline, in metres.
const WHITE: Colour = Colour::rgb(255, 255, 255);
const BLANK_OUTLINE: f32 = 0.0026;

// ---- dot

/// The dot's radii in eye units and how far below the eye's centre it sits.
const DOT_RADII: Vec2 = Vec2::new(0.2, 0.26);
const DOT_DOWN: f32 = 0.05;

// ---- stare

/// The shadow under a staring eye: how far below the lower lid it starts and how deep it hangs, in eye units; how far
/// along the eye it reaches each side; its colour and its opacity at full strength. Two short hatch lines under it
/// (`u` places, length in eye units, width in metres) make it read as the anime "pinned" stare.
const STARE_FROM: f32 = 0.04;
const STARE_DEPTH: f32 = 0.42;
const STARE_REACH: f32 = 0.95;
const STARE_COLOUR: (u8, u8, u8) = (52, 28, 46);
const STARE_OPACITY: f32 = 0.62;
const STARE_HATCH: [f32; 3] = [-0.35, 0.05, 0.45];
const STARE_HATCH_LENGTH: f32 = 0.3;
const STARE_HATCH_WIDTH: f32 = 0.0011;
const STARE_HATCH_OPACITY: f32 = 0.7;

// ---- pill

/// The capsule: half its length and its end radius, in eye units, the outline's width in metres, the divider's, and its
/// glint (place on the coloured half and radius, in eye units).
const PILL_HALF: f32 = 0.92;
const PILL_RADIUS: f32 = 0.34;
const PILL_OUTLINE: f32 = 0.0024;
const PILL_DIVIDER: f32 = 0.0012;
const PILL_GLINT: Vec2 = Vec2::new(0.42, 0.13);
const PILL_GLINT_RADIUS: f32 = 0.09;

// ---- deep gloom

/// Gloom starts to darken the upper face above this strength and does so fully at 1.
const DEEP_FROM: f32 = 0.6;
/// The band's colour; its opacity at the top, at the eyes and at its soft lower edge; how far past the outer eye corners
/// it reaches (in eye half-widths) and how far below the eye line it ends (in eye half-widths).
const DEEP_COLOUR: (u8, u8, u8) = (26, 16, 34);
const DEEP_TOP: f32 = 0.88;
const DEEP_EYES: f32 = 0.74;
const DEEP_PAST: f32 = 1.8;
const DEEP_BELOW: f32 = 1.15;
/// The glowing pupil: the iris's low colour lightened this far, the glow's radius and the core's, in iris half-widths.
const GLOW_LIGHTEN: f32 = 0.45;
const GLOW_RADIUS: f32 = 0.62;
const GLOW_CORE: f32 = 0.15;

/// The iris effects of one open eye, inside its aperture clip: gloom, shock, spiral, heart and star eyes, in that order.
pub(super) fn paint_iris(ctx: &mut PaintCtx<'_>, side: Side) {
    let effects = ctx.params.effects;
    let wanted = [effects.gloom, effects.shock, effects.spiral, effects.heart, effects.star].iter().any(|e| *e > OFF);
    if !wanted {
        return;
    }
    let eye = Eye::new(ctx.profile, ctx.params, side);
    if !eye.is_open() {
        return;
    }
    let iris = Iris::new(ctx.profile, ctx.params, &eye);
    if effects.gloom > OFF {
        gloom(ctx, &iris, side, effects.gloom.min(1.0));
    }
    if effects.shock > OFF {
        shock(ctx, &iris, side, effects.shock.min(1.0));
    }
    if effects.spiral > OFF {
        spiral(ctx, &iris, side, effects.spiral.min(1.0));
    }
    if effects.heart > OFF {
        heart_iris(ctx, &iris, side, effects.heart.min(1.0));
    }
    if effects.star > OFF {
        star_iris(ctx, &iris, side, effects.star.min(1.0));
    }
}

/// The last piece inside the aperture clip, after the catchlights: a blank eye's flat white over everything.
pub(super) fn paint_cover(ctx: &mut PaintCtx<'_>, side: Side) {
    let blank = ctx.params.effects.blank.clamp(0.0, 1.0);
    if blank <= OFF {
        return;
    }
    let eye = Eye::new(ctx.profile, ctx.params, side);
    if !eye.is_open() {
        return;
    }
    let list = ctx.list(FaceLayerId::Base);
    let outline = aperture::outline(list, &eye);
    list.push(Cmd::Fill { path: outline, paint: Paint::Solid(WHITE), opacity: blank });
}

/// The effects outside the clip, after the lash and the tick: the blank eye's outline, the stare's shadow, the dot and
/// the pill, the squeeze chevron, then the tear.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let effects = ctx.params.effects;
    if effects.blank > OFF {
        blank_outline(ctx, side, effects.blank.min(1.0));
    }
    if effects.stare > OFF {
        stare_shadow(ctx, side, effects.stare.min(1.0));
    }
    if effects.dot > OFF {
        dot(ctx, side, effects.dot.min(1.0));
    }
    if effects.pill > OFF {
        pill(ctx, side, effects.pill.min(1.0));
    }
    if effects.squeeze > OFF {
        squeeze(ctx, side, effects.squeeze.min(1.0));
    }
    if effects.tears > OFF {
        tear(ctx, side, effects.tears.min(1.0));
    }
}

/// The blank eye's bold outline round the whole opening, fading with the open eye.
fn blank_outline(ctx: &mut PaintCtx<'_>, side: Side, strength: f32) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let strength = strength * eye.open_fade();
    if !eye.is_open() || strength <= OFF {
        return;
    }
    let colour = fixed_colour(&ctx.profile.eyes.lash.colour, FALLBACK_COLOUR);
    let list = ctx.list(FaceLayerId::Base);
    let outline = aperture::outline(list, &eye);
    list.push(Cmd::Stroke {
        path: outline,
        paint: Paint::Solid(colour),
        width: BLANK_OUTLINE,
        cap: Cap::Round,
        join: Join::Round,
        opacity: strength,
    });
}

/// The heavy shadow under a staring eye: a dark band under the lower lid, soft at its bottom, with short hatch lines.
fn stare_shadow(ctx: &mut PaintCtx<'_>, side: Side, strength: f32) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let (r, g, b) = STARE_COLOUR;
    let steps = 16_u8;
    let along = |i: u8| -STARE_REACH + 2.0 * STARE_REACH * f32::from(i) / f32::from(steps);
    // The band follows the lower lid: its top edge just under the lid, its bottom edge a fixed depth further down, both
    // tapering to the corners so it never reads as a box.
    let taper = |u: f32| 1.0 - (u / STARE_REACH).powi(2);
    let top = |u: f32| eye.point(u, eye.bottom(u) - STARE_FROM);
    let low = |u: f32| eye.point(u, eye.bottom(u) - STARE_FROM - STARE_DEPTH * taper(u));
    let list = ctx.list(FaceLayerId::Base);
    let mut path = list.path().move_to(top(along(0)));
    for i in 1..=steps {
        path = path.line_to(top(along(i)));
    }
    for i in (0..=steps).rev() {
        path = path.line_to(low(along(i)));
    }
    let band = path.close().finish();
    let (from, to) = (eye.point(0.0, eye.bottom(0.0) - STARE_FROM), eye.point(0.0, eye.bottom(0.0) - STARE_FROM - STARE_DEPTH));
    let stops = Stops::new(&[(0.0, Colour::rgb(r, g, b)), (1.0, Colour::rgba(r, g, b, 0))]);
    list.push(Cmd::Fill {
        path: band,
        paint: Paint::Linear { from, to, stops },
        opacity: STARE_OPACITY * strength,
    });
    for u in STARE_HATCH {
        let start = eye.bottom(u) - STARE_FROM - 0.06;
        let line = list
            .path()
            .move_to(eye.point(u, start))
            .line_to(eye.point(u - 0.05, start - STARE_HATCH_LENGTH * taper(u)))
            .finish();
        list.push(Cmd::Stroke {
            path: line,
            paint: Paint::Solid(Colour::rgb(r, g, b)),
            width: STARE_HATCH_WIDTH,
            cap: Cap::Round,
            join: Join::Round,
            opacity: STARE_HATCH_OPACITY * strength,
        });
    }
}

/// A dot eye: a small solid dot in the lash colour where the eye was.
fn dot(ctx: &mut PaintCtx<'_>, side: Side, strength: f32) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let colour = fixed_colour(&ctx.profile.eyes.lash.colour, FALLBACK_COLOUR);
    let (rx, ry) = (DOT_RADII.x * eye.width(), DOT_RADII.y * eye.width());
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().ellipse(eye.point(0.0, -DOT_DOWN), rx, ry).finish();
    list.push(Cmd::Fill { path, paint: Paint::Solid(colour), opacity: strength });
}

/// A pill eye: a flat capsule where the eye was, its outer half in the iris colour (this eye's own, so heterochromia
/// gives one red and one blue pill), its inner half white, divided and outlined in the lash colour, with a glint.
fn pill(ctx: &mut PaintCtx<'_>, side: Side, strength: f32) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let iris = colours(ctx.profile, side);
    let (mid, top) = (fixed_colour(iris.mid, FALLBACK_COLOUR), fixed_colour(iris.top, FALLBACK_COLOUR));
    let line = fixed_colour(&ctx.profile.eyes.lash.colour, FALLBACK_COLOUR);
    let (half, r) = (PILL_HALF, PILL_RADIUS);
    let p = |u: f32, v: f32| eye.point(u, v);
    // Quarter circles as cubics, in eye units (eye.point is affine, so the curves map exactly).
    const K: f32 = 0.552_284_8;
    let straight = half - r;
    let list = ctx.list(FaceLayerId::Base);
    let capsule = list
        .path()
        .move_to(p(-straight, r))
        .line_to(p(straight, r))
        .cubic_to(p(straight + K * r, r), p(half, K * r), p(half, 0.0))
        .cubic_to(p(half, -K * r), p(straight + K * r, -r), p(straight, -r))
        .line_to(p(-straight, -r))
        .cubic_to(p(-straight - K * r, -r), p(-half, -K * r), p(-half, 0.0))
        .cubic_to(p(-half, K * r), p(-straight - K * r, r), p(-straight, r))
        .close()
        .finish();
    list.push(Cmd::Fill { path: capsule, paint: Paint::Solid(WHITE), opacity: strength });
    list.push(Cmd::PushClip(capsule));
    let outer = list
        .path()
        .polyline(&[p(0.0, r * 1.2), p(half * 1.1, r * 1.2), p(half * 1.1, -r * 1.2), p(0.0, -r * 1.2)])
        .close()
        .finish();
    let stops = Stops::new(&[(0.0, top), (1.0, mid)]);
    list.push(Cmd::Fill {
        path: outer,
        paint: Paint::Linear { from: p(0.0, r), to: p(0.0, -r), stops },
        opacity: strength,
    });
    let glint = list.path().ellipse(p(PILL_GLINT.x, PILL_GLINT.y), PILL_GLINT_RADIUS * eye.width() * 1.6, PILL_GLINT_RADIUS * eye.width()).finish();
    list.push(Cmd::Fill { path: glint, paint: Paint::Solid(WHITE), opacity: 0.85 * strength });
    list.push(Cmd::PopClip);
    let divider = list.path().move_to(p(0.0, r)).line_to(p(0.0, -r)).finish();
    list.push(Cmd::Stroke {
        path: divider,
        paint: Paint::Solid(line),
        width: PILL_DIVIDER,
        cap: Cap::Butt,
        join: Join::Round,
        opacity: strength,
    });
    list.push(Cmd::Stroke {
        path: capsule,
        paint: Paint::Solid(line),
        width: PILL_OUTLINE,
        cap: Cap::Round,
        join: Join::Round,
        opacity: strength,
    });
}

/// Deep gloom: above [`DEEP_FROM`], a dark band over the upper face, from the top of the face window down to just
/// under the eyes, darkest at the top; then each open eye's pupil glows through it in the iris's own light colour.
/// Painted once, after both eyes, on the Base layer, so the hair stays in front of it.
pub(super) fn paint_deep_gloom(ctx: &mut PaintCtx<'_>) {
    let deep = smooth01((ctx.params.effects.gloom - DEEP_FROM) / (1.0 - DEEP_FROM));
    if deep <= OFF {
        return;
    }
    let (left, right) = (Eye::new(ctx.profile, ctx.params, Side::Left), Eye::new(ctx.profile, ctx.params, Side::Right));
    let bounds = ctx.specs[FaceLayerId::Base as usize].rect;
    let reach = left.point(1.0, 0.0).x + DEEP_PAST * left.width();
    let (x0, x1) = ((-reach).max(bounds.x0), reach.min(bounds.x1));
    let eye_y = f32::midpoint(left.centre().y, right.centre().y);
    let bottom = (eye_y - DEEP_BELOW * left.width()).max(bounds.y0);
    let top = bounds.y1;
    if x1 <= x0 || top <= bottom {
        return;
    }
    let (r, g, b) = DEEP_COLOUR;
    let alpha = |a: f32| Colour::rgba(r, g, b, omuzaki_core::colour::to_byte(a));
    // Where the eye line sits along the band's gradient, from its bottom edge (0) to its top (1).
    let eye_stop = ((eye_y - bottom) / (top - bottom)).clamp(0.05, 0.9);
    let stops = Stops::new(&[(0.0, alpha(0.0)), (eye_stop, alpha(DEEP_EYES)), (1.0, alpha(DEEP_TOP))]);
    let list = ctx.list(FaceLayerId::Base);
    let band = list
        .path()
        .polyline(&[Vec2::new(x0, bottom), Vec2::new(x1, bottom), Vec2::new(x1, top), Vec2::new(x0, top)])
        .close()
        .finish();
    list.push(Cmd::Fill {
        path: band,
        paint: Paint::Linear { from: Vec2::new(0.0, bottom), to: Vec2::new(0.0, top), stops },
        opacity: deep,
    });
    for (side, eye) in [(Side::Left, left), (Side::Right, right)] {
        let shown = deep * eye.open_fade();
        if !eye.is_open() || shown <= OFF {
            continue;
        }
        let iris = Iris::new(ctx.profile, ctx.params, &eye);
        let light = lighten(fixed_colour(colours(ctx.profile, side).low, FALLBACK_COLOUR), GLOW_LIGHTEN);
        let centre = iris.at(0.0, 0.15);
        let radius = GLOW_RADIUS * iris.rx;
        let glow_stops = Stops::new(&[
            (0.0, light),
            (0.3, Colour { a: 210, ..light }),
            (1.0, Colour { a: 0, ..light }),
        ]);
        let list = ctx.list(FaceLayerId::Base);
        // Inside the aperture, so the glow goes shut with the eye as the other open pieces do.
        let clip = aperture::outline(list, &eye);
        list.push(Cmd::PushClip(clip));
        let glow = list.path().ellipse(centre, radius, radius).finish();
        list.push(Cmd::Fill { path: glow, paint: Paint::Radial { centre, radius, stops: glow_stops }, opacity: shown });
        let core = list.path().ellipse(centre, GLOW_CORE * iris.rx, GLOW_CORE * iris.rx * 1.3).finish();
        list.push(Cmd::Fill { path: core, paint: Paint::Solid(lighten(light, 0.7)), opacity: shown });
        list.push(Cmd::PopClip);
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
fn gloom(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side, strength: f32) {
    let shade = darken(fixed_colour(colours(ctx.profile, side).mid, FALLBACK_COLOUR), GLOOM_DARKEN);
    let list = ctx.list(FaceLayerId::Base);
    let path = iris.outline(list);
    list.push(Cmd::Fill { path, paint: Paint::Solid(shade), opacity: GLOOM_OPACITY * strength });
}

/// The iris covered by the eye white and replaced by a small disc with a white dot for a pupil.
fn shock(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side, strength: f32) {
    let eyes = &ctx.profile.eyes;
    let (white, disc) =
        (fixed_colour(&eyes.sclera.mid, FALLBACK_COLOUR), fixed_colour(colours(ctx.profile, side).mid, FALLBACK_COLOUR));
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
fn spiral(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side, strength: f32) {
    let colour = fixed_colour(colours(ctx.profile, side).ring, FALLBACK_COLOUR);
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
        paint: Paint::Solid(colour),
        width: SPIRAL_WIDTH,
        cap: Cap::Round,
        join: Join::Round,
        opacity: SPIRAL_OPACITY * strength,
    });
}

/// A five-point star over the iris, lighter than it, outlined like the iris.
fn star_iris(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side, strength: f32) {
    let eyes = colours(ctx.profile, side);
    let fill = lighten(fixed_colour(eyes.low, FALLBACK_COLOUR), STAR_LIGHTEN);
    let ring = fixed_colour(eyes.ring, FALLBACK_COLOUR);
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

/// Heart eyes: a pink heart over the iris, fading in with the effect, outlined in the ring's dark.
fn heart_iris(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side, strength: f32) {
    let ring = fixed_colour(colours(ctx.profile, side).ring, FALLBACK_COLOUR);
    let centre = iris.at(0.0, -HEART_DROP);
    let list = ctx.list(FaceLayerId::Base);
    let path = heart(list, centre, iris.rx * HEART_IRIS);
    list.push(Cmd::Fill { path, paint: Paint::Solid(HEART_COLOUR), opacity: HEART_OPACITY * strength });
    list.push(Cmd::Stroke {
        path,
        paint: Paint::Solid(ring),
        width: HEART_OUTLINE_WIDTH,
        cap: Cap::Round,
        join: Join::Round,
        opacity: HEART_OUTLINE_OPACITY * strength,
    });
}

/// The `>` or `<` of a squeezed eye: two arms from the vertex toward the nose out to the outer corner region, in the
/// lash colour. The lid closes with the effect (see [`Eye::new`]), so the chevron grows over a closing eye: the arms
/// start at the vertex, where the lids meet, their length rises with the effect, and while the open eye shows they
/// stop at its outline instead of reaching over the lid.
fn squeeze(ctx: &mut PaintCtx<'_>, side: Side, strength: f32) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let colour = fixed_colour(&ctx.profile.eyes.lash.colour, FALLBACK_COLOUR);
    let (arm, vertex) = (SQUEEZE_ARM, SQUEEZE_VERTEX);
    // How far the open eye has given way: `0` while its pieces show, `1` once they are gone (see `Eye::shut`).
    let gone = eye.shut();
    let reach = strength.clamp(0.0, 1.0);
    // The vertex sits where the lids meet at its place along the eye: the middle of the narrowing aperture while the open
    // eye shows, and its own place once the eye is gone.
    let meet = Vec2::new(vertex.x, vertex.y + (eye.midline(vertex.x) - vertex.y) * (1.0 - gone));
    let end = |sign: f32| {
        let end = meet + (Vec2::new(arm.x, sign * arm.y) - meet) * reach;
        let inside = end.y.clamp(eye.bottom(end.x), eye.top(end.x));
        eye.point(end.x, inside + (end.y - inside) * gone)
    };
    let points = [end(1.0), eye.point(meet.x, meet.y), end(-1.0)];
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

/// The tear drop under an open eye: a pointed drop toward the nose, fading in with the effect (its size is fixed) and
/// out with the open eye ([`Eye::open_fade`]), like every effect drawn on the eye.
fn tear(ctx: &mut PaintCtx<'_>, side: Side, strength: f32) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let strength = strength * eye.open_fade();
    if strength <= 0.0 {
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
