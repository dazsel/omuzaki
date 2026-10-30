//! Eyeshadow (#2472): a wash of colour on the upper lid, fading through the crease colour toward the brow and sweeping
//! past the outer corner by its `wing`, and a touch under the outer lower lid. Drawn first for each eye, under
//! everything else of it, and outside the open-eye fade: it follows the lid as the eye shuts, so a closed eye keeps it.

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cmd, Colour, FaceLayerId, Paint, Stops, narrow};

use super::aperture::{Eye, smooth01};
use super::{FALLBACK_COLOUR, Side};
use crate::painter::{PaintCtx, fixed_colour, scaled};

/// Samples along the lower lid.
const POINTS: usize = 24;
/// Soft patches along the upper lid.
const PATCHES: u16 = 8;
/// The first patch's centre (eye units from the eye's middle, toward the outer corner), and the last one's on a
/// wingless eye.
const START: f32 = -0.62;
const OUTER_FROM: f32 = 0.86;
/// Each patch's half-width as a multiple of the spacing, its half-height and its centre's height as shares of the
/// wash's height there, and its opacity at full strength (they overlap, so the wash is denser than one patch).
const PATCH_WIDTH: f32 = 1.7;
const PATCH_HEIGHT: f32 = 1.1;
const PATCH_LIFT: f32 = 0.2;
const PATCH_ALPHA: f32 = 0.45;
/// The wing's tip patch keeps at least this share of the wash's height.
const MIN_TAPER: f32 = 0.3;
/// Past the outer corner, a full wing reaches this far (eye units) and lifts this much per unit of reach.
const WING_REACH: f32 = 0.42;
const WING_LIFT: f32 = 0.55;
/// The wash's height at the inner end, as a share of its tallest (toward the outer corner).
const INNER_SHARE: f32 = 0.55;
/// The lower touch: from this far along the lid to the outer corner, this deep below the lid (eye units).
const LOWER_FROM: f32 = 0.15;
const LOWER_DEPTH: f32 = 0.14;

/// Paints the eyeshadow of the eye on `side`; nothing at strength 0.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let style = &ctx.profile.eyes.shadow;
    let strength = narrow(style.strength).clamp(0.0, 1.0);
    if strength <= 0.0 {
        return;
    }
    let eye = Eye::new(ctx.profile, ctx.params, side);
    let (lid, crease) =
        (fixed_colour(&style.colour, FALLBACK_COLOUR), fixed_colour(&style.crease_colour, FALLBACK_COLOUR));
    let (height, wing, lower) =
        (narrow(style.height), narrow(style.wing).clamp(0.0, 1.0), narrow(style.lower).clamp(0.0, 1.0));
    upper(ctx, &eye, height, wing, (lid, crease), strength);
    if lower > 0.0 {
        under(ctx, &eye, lid, strength * lower);
    }
}

/// The wash on the upper lid: [`PATCHES`] soft patches along the lid, each `lid` at its heart fading through `crease`
/// to nothing, so the wash has no edge anywhere. Each sits a little above the lid, turned with it, and reaches about
/// `height` eye units above it; the eye covers the part below the lid. Past the outer corner the patches follow the
/// wing up and shrink to its tip.
fn upper(ctx: &mut PaintCtx<'_>, eye: &Eye, height: f32, wing: f32, (lid, crease): (Colour, Colour), strength: f32) {
    let end = OUTER_FROM + WING_REACH * wing;
    let spacing = (end - START) / f32::from(PATCHES - 1);
    // The lid line, carried on past the outer corner along the wing.
    let line = |u: f32| eye.top(u.min(1.0)) + WING_LIFT * (u - 1.0).max(0.0);
    let stops = Stops::new(&[(0.0, lid), (0.5, scaled(crease, 0.7)), (1.0, scaled(crease, 0.0))]);
    let list = ctx.list(FaceLayerId::Base);
    for i in 0..PATCHES {
        let u = START + spacing * f32::from(i);
        let rise = INNER_SHARE + (1.0 - INNER_SHARE) * smooth01((u + 1.0) / 1.6);
        // Past the corner the wing narrows to its tip.
        let past = (u - 1.0).max(0.0);
        let taper = if wing > 0.0 { (1.0 - past / (WING_REACH * wing + 0.05)).max(MIN_TAPER) } else { 1.0 };
        let h = height * rise * taper;
        let centre = eye.point(u, line(u) + PATCH_LIFT * h);
        let along = eye.point(u + 0.05, line(u + 0.05)) - eye.point(u - 0.05, line(u - 0.05));
        let (rx, ry) = (PATCH_WIDTH * spacing * taper.sqrt() * eye.width(), PATCH_HEIGHT * h * eye.width());
        if rx <= f32::EPSILON || ry <= f32::EPSILON {
            continue;
        }
        let turn = Affine2::from_translation(centre)
            * Affine2::from_angle(along.y.atan2(along.x))
            * Affine2::from_scale(Vec2::new(1.0, ry / rx))
            * Affine2::from_translation(-centre);
        let disc = list.path().ellipse(centre, rx, rx).finish();
        list.push(Cmd::PushTransform(turn));
        list.push(Cmd::Fill {
            path: disc,
            paint: Paint::Radial { centre, radius: rx, stops },
            opacity: strength * PATCH_ALPHA,
        });
        list.push(Cmd::PopTransform);
    }
}

/// The touch under the outer lower lid, deepest midway along its run.
fn under(ctx: &mut PaintCtx<'_>, eye: &Eye, colour: Colour, opacity: f32) {
    let list = ctx.list(FaceLayerId::Base);
    let mut path = list.path();
    let at = |i: usize| {
        #[allow(clippy::cast_precision_loss)] // at most 24
        let t = i as f32 / POINTS as f32;
        LOWER_FROM + (1.0 - LOWER_FROM) * t
    };
    for i in 0..=POINTS {
        let u = at(i);
        let p = eye.point(u, eye.bottom(u));
        path = if i == 0 { path.move_to(p) } else { path.line_to(p) };
    }
    for i in (0..=POINTS).rev() {
        let u = at(i);
        // Deepest in the middle of the run, nothing at its ends.
        let t = (u - LOWER_FROM) / (1.0 - LOWER_FROM);
        let depth = LOWER_DEPTH * (4.0 * t * (1.0 - t)).max(0.0);
        path = path.line_to(eye.point(u, eye.bottom(u) - depth));
    }
    let path = path.close().finish();
    let mid = f32::midpoint(1.0, LOWER_FROM);
    let from = eye.point(mid, eye.bottom(mid));
    let to = eye.point(mid, eye.bottom(mid) - LOWER_DEPTH);
    let stops = Stops::new(&[(0.0, colour), (1.0, scaled(colour, 0.0))]);
    list.push(Cmd::Fill { path, paint: Paint::Linear { from, to, stops }, opacity });
}
