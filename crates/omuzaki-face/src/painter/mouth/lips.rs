//! The lips: the glossy lower lip under an open mouth and the rim round it (`face.js` 278-289 and 363-366), and the
//! colour helpers the other mouth pieces share.
//!
//! The gloss ([`paint_gloss`]) is drawn **before** the mouth shape, as the prototype does. The rim ([`paint_rim`]) is
//! drawn after the interior, over the clipped teeth and tongue. Both follow the lip strength of the profile
//! ([`Lips::strength`]): none on the flat sheet look, full on a close-up. Every colour comes from the profile: the lip
//! tint, the mouth line colour and the interior colours, lightened or darkened where the prototype shades them.
//!
//! [`Lips::strength`]: omuzaki_face_api::Lips::strength

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cmd, Colour, ColourExt, ColourRef, FaceLayerId, Paint, Stops, narrow};

use crate::painter::PaintCtx;

/// The lip tint of an unset palette slot (which a painter should never see): the default's.
const DEFAULT_TINT: Colour = Colour::rgb(0xee, 0x8e, 0x92);
/// The mouth line, tongue and teeth colours of an unset palette slot: the defaults'.
pub(super) const DEFAULT_LINE: Colour = Colour::rgb(0xa8, 0x5a, 0x55);
pub(super) const DEFAULT_TONGUE: Colour = Colour::rgb(0xe0, 0x78, 0x7a);
pub(super) const DEFAULT_TEETH: Colour = Colour::rgb(0xff, 0xfa, 0xf6);
pub(super) const DEFAULT_TEETH_LOWER: Colour = Colour::rgb(0xf4, 0xec, 0xe8);
/// Below this lip strength there is no gloss.
const GLOSS_MIN: f32 = 0.01;
/// The gloss ellipse sits this far under the mouth, plus [`GLOSS_DROP_PUCKER`] per unit of pucker, in metres.
const GLOSS_DROP: f32 = 0.0042;
/// See [`GLOSS_DROP`].
const GLOSS_DROP_PUCKER: f32 = 0.0022;
/// The gloss ellipse's half-width as a fraction of the mouth's, plus [`GLOSS_WIDTH_PUCKER`] per unit of pucker.
const GLOSS_WIDTH: f32 = 0.62;
/// See [`GLOSS_WIDTH`].
const GLOSS_WIDTH_PUCKER: f32 = 0.3;
/// The gloss ellipse's half-height in metres, plus [`GLOSS_HEIGHT_PUCKER`] per unit of pucker.
const GLOSS_HEIGHT: f32 = 0.0034;
/// See [`GLOSS_HEIGHT`].
const GLOSS_HEIGHT_PUCKER: f32 = 0.0024;
/// Alpha of the gloss at its middle and at 60 % of its radius, times the lip strength.
const GLOSS_ALPHA: [f32; 2] = [0.42, 0.22];
/// The highlight: the tint, nearly white, at this alpha times the lip strength, tilted by [`HIGHLIGHT_TILT`] radians.
const HIGHLIGHT_WHITE: f32 = 0.95;
const HIGHLIGHT_ALPHA: f32 = 0.75;
const HIGHLIGHT_TILT: f32 = 0.1;
/// The dark rim edge is the tint scaled per channel by these shares, at [`RIM_DARK_ALPHA`], [`RIM_DARK_WIDTH`] wide.
pub(super) const DARKEN: [f32; 3] = [0.59, 0.42, 0.49];
const RIM_DARK_ALPHA: f32 = 0.75;
const RIM_DARK_WIDTH: f32 = 0.0024;
/// Width of the plain outline of a lip-less mouth, in metres.
const RIM_PLAIN_WIDTH: f32 = 0.0013;
/// The light rim: the tint this share of the way to white, at [`RIM_LIGHT_ALPHA`] times the lip strength.
const RIM_LIGHT_WHITE: f32 = 0.88;
const RIM_LIGHT_ALPHA: f32 = 0.9;
const RIM_LIGHT_WIDTH: f32 = 0.0012;
/// Lips stronger than this get the two-tone rim.
const RIM_LIPS_ABOVE: f32 = 0.3;

/// A style colour, or `fallback` for a palette slot nobody resolved (which a painter should never see).
pub(super) fn resolve(colour: &ColourRef, fallback: Colour) -> Colour {
    match colour {
        ColourRef::Fixed(c) => *c,
        ColourRef::Slot(_) => fallback,
    }
}

/// `colour` at `alpha` as a solid paint.
pub(super) fn solid(colour: Colour, alpha: f32) -> Paint {
    Paint::Solid(colour.faded(alpha))
}

/// Position along a path of sample `i` out of `n`, from `-1` to `1`.
pub(super) fn along(i: usize, n: usize) -> f32 {
    // Sample counts are a few dozen at most, exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let (i, n) = (i as f32, n as f32);
    -1.0 + 2.0 * i / n
}

/// A byte from a channel value that is within 0 to 255 by construction.
fn byte(v: f32) -> u8 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let b = v.round().clamp(0.0, 255.0) as u8;
    b
}

/// `colour` moved `t` of the way to white.
pub(super) fn lighten(colour: Colour, t: f32) -> Colour {
    let up = |c: u8| byte(f32::from(c) + (255.0 - f32::from(c)) * t);
    Colour { r: up(colour.r), g: up(colour.g), b: up(colour.b), a: colour.a }
}

/// `colour` with each channel scaled by the matching share of `by`.
pub(super) fn darken(colour: Colour, by: [f32; 3]) -> Colour {
    let down = |c: u8, k: f32| byte(f32::from(c) * k);
    Colour { r: down(colour.r, by[0]), g: down(colour.g, by[1]), b: down(colour.b, by[2]), a: colour.a }
}

/// The lip tint from the profile.
pub(super) fn tint(ctx: &PaintCtx<'_>) -> Colour {
    resolve(&ctx.profile.mouth.lips.colour, DEFAULT_TINT)
}

/// Paints the glossy lower lip under an open mouth: a soft swell of the lip tint with a small highlight. Nothing on a
/// flat face, and nothing for a closed mouth or a kiss (they leave no outline to size it by).
pub(super) fn paint_gloss(ctx: &mut PaintCtx<'_>) {
    let Some(open) = ctx.open_mouth else { return };
    let strength = narrow(ctx.profile.mouth.lips.strength);
    if strength < GLOSS_MIN {
        return;
    }
    let tint = tint(ctx);
    let pucker = ctx.params.mouth.pucker.clamp(0.0, 1.0);
    let centre = Vec2::new(open.centre.x, open.centre.y - open.height - GLOSS_DROP - GLOSS_DROP_PUCKER * pucker);
    let rx = open.half_width * (GLOSS_WIDTH + GLOSS_WIDTH_PUCKER * pucker);
    let ry = GLOSS_HEIGHT + GLOSS_HEIGHT_PUCKER * pucker;

    // A circle squashed to the ellipse by a transform, so the radial gradient squashes with it.
    let squash = Affine2::from_translation(centre)
        * Affine2::from_scale(Vec2::new(1.0, ry / rx))
        * Affine2::from_translation(-centre);
    let swell = Paint::Radial {
        centre,
        radius: rx,
        stops: Stops::new(&[
            (0.0, tint.faded(GLOSS_ALPHA[0] * strength)),
            (0.6, tint.faded(GLOSS_ALPHA[1] * strength)),
            (1.0, tint.faded(0.0)),
        ]),
    };
    let list = ctx.list(FaceLayerId::Base);
    let disc = list.path().ellipse(centre, rx, rx).finish();
    list.push(Cmd::PushTransform(squash));
    list.fill(disc, swell);
    list.push(Cmd::PopTransform);

    let spot = centre + Vec2::new(open.half_width * 0.12, ry * 0.25);
    let tilt = Affine2::from_translation(spot) * Affine2::from_angle(HIGHLIGHT_TILT) * Affine2::from_translation(-spot);
    let highlight = list.path().ellipse(spot, open.half_width * 0.18, ry * 0.28).finish();
    list.push(Cmd::PushTransform(tilt));
    list.fill(highlight, solid(lighten(tint, HIGHLIGHT_WHITE), HIGHLIGHT_ALPHA * strength));
    list.push(Cmd::PopTransform);
}

/// Paints the rim round an open mouth: a dark outer edge and, on a lip face, a light inner rim over it. A flat face
/// gets the plain mouth line. Nothing for a closed mouth or a kiss.
pub(super) fn paint_rim(ctx: &mut PaintCtx<'_>) {
    let Some(open) = ctx.open_mouth else { return };
    let lips = narrow(ctx.profile.mouth.lips.strength);
    let line = resolve(&ctx.profile.mouth.line_colour, DEFAULT_LINE);
    let tint = tint(ctx);
    let list = ctx.list(FaceLayerId::Base);
    if lips > RIM_LIPS_ABOVE {
        list.stroke(open.path, solid(darken(tint, DARKEN), RIM_DARK_ALPHA), RIM_DARK_WIDTH);
        list.stroke(open.path, solid(lighten(tint, RIM_LIGHT_WHITE), RIM_LIGHT_ALPHA * lips), RIM_LIGHT_WIDTH);
    } else {
        list.stroke(open.path, Paint::Solid(line), RIM_PLAIN_WIDTH);
    }
}
