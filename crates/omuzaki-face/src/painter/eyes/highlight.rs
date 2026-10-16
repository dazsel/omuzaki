//! Catchlights and the wet-eye shimmer (the `eye()` body of `face.js`, lines 162-186, and the heart of 235-240), drawn
//! over the iris inside the aperture clip.
//!
//! # From the JS constants to the profile
//!
//! | Profile | Replaces (JS) |
//! |---|---|
//! | `highlights.shape` | the glossy blob; the `heart` and `star` effects crossfade the heart and the four-point star in from any nonzero strength, the profile's own shape giving way as they grow (the prototype swapped the heart in above 0.5, which pops) |
//! | `highlights.size`, `offset` | the blob's `0.30` radius and its `(0.28, 0.22)` place, in iris radii |
//! | `highlights.second` | the small spark's opacity (`0.65`) |
//! | `highlights.shimmer` | `P.refl` as the strength of the band and the two stars |
//! | `highlights.view_motion` | `P.refl` as the factor on the reflection offset (`reflV`), which is [`FaceView::highlight_shift`](omuzaki_face_api::FaceView) |
//! | `effects.sparkle` | `P.spark`, the surprised dot |
//!
//! The prototype slid only its heart with the view; here every shape of the main catchlight slides (the second spark
//! stays put, as in the prototype). The shimmer's band and stars glide the other way, as they did.
//!
//! # The shimmer is a function of the shot clock
//!
//! The band glides across the iris once every `1 / 0.23` seconds and the stars twinkle at 7 rad/s, both from
//! [`PaintCtx::time`] (the prototype's `twT`) and never the wall clock, so a render replays exactly. The two eyes are
//! out of step by their side. Like the prototype's `%`, the band's phase keeps the sign of its operand, so the right
//! eye's band waits just off the iris's left edge for its first 0.74 s.

use libm::{cosf, sin, sinf};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{CatchlightShape, Cmd, DrawList, FaceLayerId, Paint, PathRef, Stops};

use super::Side;
use super::aperture::Eye;
use super::iris::Iris;
use crate::painter::PaintCtx;

// ---- the prototype's constants that the profile does not parametrise

/// The main catchlight's opacity (`rgba(255,255,255,0.95)`).
const MAIN_OPACITY: f32 = 0.95;
/// The blob's height as a fraction of its width (`0.13 / 0.30` of the iris radii).
const BLOB_FLATNESS: f32 = 0.13 / 0.30;
/// The blob's tilt, counter-clockwise on the face plane (the prototype's `-0.1` on a y-down canvas).
const BLOB_TILT: f32 = 0.1;
/// The heart is smaller than the blob (`0.26` against `0.30`) and sits a little nearer the iris centre.
const HEART_SCALE: f32 = 0.26 / 0.30;
/// How far the heart sits toward the centre of the iris, in iris radii, against the blob's place `(0.28, 0.22)`.
const HEART_PULL: Vec2 = Vec2::new(0.04, 0.02);
/// The star catchlight's reach (tip to centre) as a fraction of `size` times the iris half-width.
const STAR_REACH: f32 = 1.0;
/// The second spark: place, radii (fractions of the iris radii), in iris radii with `y` up.
const SECOND_PLACE: Vec2 = Vec2::new(-0.38, -0.42);
const SECOND_RADII: Vec2 = Vec2::new(0.12, 0.07);
/// The surprised dot: place and radius (of the iris half-width).
const SPARK_PLACE: Vec2 = Vec2::new(-0.1, 0.5);
const SPARK_RADIUS: f32 = 0.16;

/// The band of shimmer: its opacity at full strength, its radius (of the iris half-width), how far across the iris it
/// glides (from `-0.6` to `0.6` iris half-widths), where it sits (`y`, iris half-heights), the speed of its glide, and
/// each side's head start.
const BAND_OPACITY: f32 = 0.30;
const BAND_RADIUS: f32 = 0.55;
const BAND_SPAN: (f32, f32) = (-0.6, 1.2);
const BAND_HEIGHT: f32 = -0.45;
const BAND_SPEED: f32 = 0.23;
const BAND_SIDE_PHASE: f32 = 0.17;
/// The twinkling stars: `(x, y, phase, size)` with the place in iris radii (`y` up) and the size of the iris half-width.
const STARS: [(f32, f32, f32, f32); 2] = [(-0.42, -0.38, 0.0, 0.11), (0.40, -0.52, 2.1, 0.07)];
/// A star's opacity at full strength, its twinkle's speed (rad/s), and the reach of its points in radii.
const STAR_OPACITY: f32 = 0.9;
const TWINKLE_SPEED: f32 = 7.0;
const TWINKLE_RANGE: (f32, f32) = (0.55, 0.45);
const STAR_POINT: f32 = 1.6;
/// How far the view moves the stars, against the band's whole offset.
const STAR_SLIDE: f32 = 0.6;
/// Below this a strength counts as off.
const OFF: f32 = 0.01;

const WHITE: Colour = Colour::rgb(255, 255, 255);

/// `+1` for the character's left eye, `-1` for the right (the prototype's `side`).
fn sign(side: Side) -> f32 {
    match side {
        Side::Left => 1.0,
        Side::Right => -1.0,
    }
}

/// A closed heart centred at `c`, about `2 r` wide, point down (`heartPath`).
pub(super) fn heart(list: &mut DrawList, c: Vec2, r: f32) -> PathRef {
    // The prototype's offsets are y-down; `p` takes them as written.
    let p = |x: f32, y: f32| c + Vec2::new(x * r, -y * r);
    list.path()
        .move_to(p(0.0, 0.75))
        .cubic_to(p(-1.1, 0.05), p(-0.75, -0.8), p(0.0, -0.28))
        .cubic_to(p(0.75, -0.8), p(1.1, 0.05), p(0.0, 0.75))
        .close()
        .finish()
}

/// A four-point star centred at `c` whose points reach `reach` from it, with concave sides.
fn star(list: &mut DrawList, c: Vec2, reach: f32) -> PathRef {
    let (up, right) = (Vec2::new(0.0, reach), Vec2::new(reach, 0.0));
    list.path()
        .move_to(c + up)
        .quad_to(c, c + right)
        .quad_to(c, c - up)
        .quad_to(c, c - right)
        .quad_to(c, c + up)
        .close()
        .finish()
}

/// An ellipse centred at `c`, turned `angle` radians counter-clockwise.
fn tilted_ellipse(list: &mut DrawList, c: Vec2, rx: f32, ry: f32, angle: f32) -> PathRef {
    const KAPPA: f32 = 0.552_284_8;
    let (sin, cos) = (sinf(angle), cosf(angle));
    let p = |x: f32, y: f32| c + Vec2::new(cos * x - sin * y, sin * x + cos * y);
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    list.path()
        .move_to(p(rx, 0.0))
        .cubic_to(p(rx, ky), p(kx, ry), p(0.0, ry))
        .cubic_to(p(-kx, ry), p(-rx, ky), p(-rx, 0.0))
        .cubic_to(p(-rx, -ky), p(-kx, -ry), p(0.0, -ry))
        .cubic_to(p(kx, -ry), p(rx, -ky), p(rx, 0.0))
        .close()
        .finish()
}

/// Paints the catchlights, the second spark, the shimmer and the surprised dot of the eye on `side`. Draws nothing for
/// a shut eye.
#[allow(clippy::cast_possible_truncation)] // the profile is f64 for the file's sake; the painter works in f32
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let (profile, params) = (ctx.profile, ctx.params);
    let eye = Eye::new(profile, params, side);
    if !eye.is_open() {
        return;
    }
    let iris = Iris::new(profile, params, &eye);
    let style = &profile.eyes.highlights;
    let glide = ctx.view.highlight_shift * style.view_motion as f32;

    // A gloomy eye loses its light: every highlight fades with the effect.
    let keep = 1.0 - params.effects.gloom.clamp(0.0, 1.0);
    catchlight(ctx, &iris, glide, keep);
    if style.second > 0.0 {
        let list = ctx.list(FaceLayerId::Base);
        let path = list
            .path()
            .ellipse(iris.at(SECOND_PLACE.x, SECOND_PLACE.y), iris.rx * SECOND_RADII.x, iris.ry * SECOND_RADII.y)
            .finish();
        list.push(Cmd::Fill { path, paint: Paint::Solid(WHITE), opacity: style.second as f32 * keep });
    }
    let shimmer = style.shimmer as f32 * keep;
    if shimmer > OFF {
        shimmer_band(ctx, &iris, side, glide, shimmer);
        twinkles(ctx, &iris, side, glide, shimmer);
    }
    let spark = params.effects.sparkle;
    if spark * keep > OFF {
        let list = ctx.list(FaceLayerId::Base);
        let r = iris.rx * SPARK_RADIUS;
        let path = list.path().ellipse(iris.at(SPARK_PLACE.x, SPARK_PLACE.y), r, r).finish();
        list.push(Cmd::Fill { path, paint: Paint::Solid(WHITE), opacity: spark.min(1.0) * keep });
    }
}

/// The main catchlight, slid by `glide` iris radii, with the effects' shapes crossfaded in: the profile's own shape gives
/// way to the heart or the star as `effects.heart` or `effects.star` grows (the prototype switched at 0.5, which pops),
/// and every shape loses its light with `keep` (one less the gloom).
fn catchlight(ctx: &mut PaintCtx<'_>, iris: &Iris, glide: Vec2, keep: f32) {
    let effects = ctx.params.effects;
    let (heart, star) = (effects.heart.clamp(0.0, 1.0), effects.star.clamp(0.0, 1.0));
    let own = 1.0 - heart.max(star);
    if own > 0.0 {
        let shape = ctx.profile.eyes.highlights.shape;
        draw_shape(ctx, iris, glide, shape, MAIN_OPACITY * own * keep);
    }
    if heart > OFF {
        draw_shape(ctx, iris, glide, CatchlightShape::Heart, MAIN_OPACITY * heart * keep);
    }
    if star > OFF {
        draw_shape(ctx, iris, glide, CatchlightShape::Star, MAIN_OPACITY * star * keep);
    }
}

/// One catchlight of `shape` at `opacity`.
#[allow(clippy::cast_possible_truncation)] // the profile is f64 for the file's sake; the painter works in f32
fn draw_shape(ctx: &mut PaintCtx<'_>, iris: &Iris, glide: Vec2, shape: CatchlightShape, opacity: f32) {
    let style = &ctx.profile.eyes.highlights;
    let (size, offset) = (style.size as f32, Vec2::new(style.offset[0] as f32, style.offset[1] as f32));
    let list = ctx.list(FaceLayerId::Base);
    let place = |offset: Vec2| iris.at(offset.x + glide.x, offset.y + glide.y);
    let path = match shape {
        CatchlightShape::None => return,
        CatchlightShape::Blob => {
            let rx = iris.rx * size;
            tilted_ellipse(list, place(offset), rx, iris.ry * size * BLOB_FLATNESS, BLOB_TILT)
        }
        CatchlightShape::Heart => heart(list, place(offset - HEART_PULL), iris.rx * size * HEART_SCALE),
        CatchlightShape::Star => star(list, place(offset), iris.rx * size * STAR_REACH),
    };
    list.push(Cmd::Fill { path, paint: Paint::Solid(WHITE), opacity });
}

/// The soft band of light gliding across the lower iris, cut by the iris.
fn shimmer_band(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side, glide: Vec2, strength: f32) {
    // `%` keeps the sign of its left operand, as JS does (see the module docs).
    // Computed in f64: a long shot's clock would lose the phase in f32.
    let phase = (ctx.time.0 * f64::from(BAND_SPEED) + f64::from(sign(side) * BAND_SIDE_PHASE)) % 1.0;
    #[allow(clippy::cast_possible_truncation)] // a phase in -1..1
    let x = BAND_SPAN.0 + BAND_SPAN.1 * phase as f32 - glide.x;
    let stops = Stops::new(&[(0.0, WHITE), (1.0, Colour::rgba(255, 255, 255, 0))]);
    let list = ctx.list(FaceLayerId::Base);
    let paint = Paint::Radial { centre: iris.at(x, BAND_HEIGHT), radius: iris.rx * BAND_RADIUS, stops };
    let outline = iris.outline(list);
    list.push(Cmd::Fill { path: outline, paint, opacity: BAND_OPACITY * strength });
}

/// The two four-point glints, twinkling on the shot clock.
fn twinkles(ctx: &mut PaintCtx<'_>, iris: &Iris, side: Side, glide: Vec2, strength: f32) {
    for (x, y, phase, size) in STARS {
        // In f64 for the same reason as the band's phase.
        #[allow(clippy::cast_possible_truncation)] // a sine
        let wave = sin(ctx.time.0 * f64::from(TWINKLE_SPEED) + f64::from(phase + sign(side))) as f32;
        let twinkle = TWINKLE_RANGE.0 + TWINKLE_RANGE.1 * wave;
        let reach = iris.rx * size * twinkle * STAR_POINT;
        let list = ctx.list(FaceLayerId::Base);
        let path = star(list, iris.at(x - glide.x * STAR_SLIDE, y + glide.y * STAR_SLIDE), reach);
        list.push(Cmd::Fill { path, paint: Paint::Solid(WHITE), opacity: STAR_OPACITY * strength });
    }
}
