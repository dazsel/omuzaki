//! The inside of an open mouth: tongue, upper and lower teeth and fangs (`face.js` 337-362).
//!
//! Everything is clipped to the open shape's outline ([`OpenMouth::path`]), so no pixel lands outside it, and the teeth
//! and fangs are measured from the outline's own upper edge ([`OpenMouth::top`]), so they follow the lip however it
//! curves. The inner-mouth gradient is the shape's fill, not drawn here. A closed mouth or a kiss has no interior.
//!
//! - **Tongue**: an ellipse resting on the bottom, with a small shine.
//! - **Upper teeth**: a band hanging from the upper edge, as strong as the `teeth_upper` channel, and at least half
//!   there when fangs show.
//! - **Lower teeth**: a row under the line, as strong as `teeth_lower`, with a split line when the upper teeth are
//!   clenched too. The row starts below the lowest point of a flat mouth, so only a real smile, which lowers the
//!   mouth's centre, brings it into the opening.
//! - **Fangs**: two triangles hanging from the upper edge at [`FangStyle::position`] of the half-width, as opaque as
//!   the `fangs` channel. In a mouth too narrow for the base, the outer corner stops at the mouth's corner, so the fang
//!   narrows instead of being clipped.
//!
//! [`FangStyle::position`]: omuzaki_face_api::FangStyle::position

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cmd, FaceLayerId, Paint, narrow};

use super::OpenMouth;
use super::lips::{DEFAULT_LINE, DEFAULT_TEETH, DEFAULT_TEETH_LOWER, DEFAULT_TONGUE, along, lighten, resolve, solid};
use crate::painter::PaintCtx;

/// Samples along the teeth band, as along the open shape.
const BAND_SEGMENTS: usize = 28;
/// The tongue ellipse: horizontal radius as a share of the half-width, vertical radius as a share of the depth, and its
/// centre's depth below the line as a share of the depth.
const TONGUE_RX: f32 = 0.72;
const TONGUE_RY: f32 = 0.5;
const TONGUE_AT: f32 = 0.98;
/// The tongue's shine: the tongue colour this share of the way to white, at this alpha, left of and above the middle.
const SHINE_WHITE: f32 = 0.6;
const SHINE_ALPHA: f32 = 0.35;
const SHINE_X: f32 = -0.15;
const SHINE_AT: f32 = 0.72;
const SHINE_RX: f32 = 0.22;
const SHINE_RY: f32 = 0.12;
/// Upper teeth show from this strength.
const BAND_MIN: f32 = 0.01;
/// Fangs make the band at least this strong, once the channel is above [`FANG_BAND_ABOVE`].
const FANG_BAND: f32 = 0.55;
/// See [`FANG_BAND`].
const FANG_BAND_ABOVE: f32 = 0.5;
/// The band's depth, as a share of the opening: without and with lower teeth showing.
const BAND_SHARE: f32 = 0.30;
const BAND_SHARE_LOW: f32 = 0.62;
/// The band never hangs lower than this, in metres, times the performance scale.
const BAND_MAX: f32 = 0.0032;
/// Lower teeth show from this strength.
const LOWER_MIN: f32 = 0.01;
/// The lower row starts this many depths below the line, is half a depth tall, and at least this many prototype pixels.
const LOWER_AT: f32 = 1.05;
const LOWER_SHARE: f32 = 0.5;
const LOWER_MIN_PX: f32 = 2.0;
/// The split line of clenched teeth: the mouth line colour at this alpha, this wide, this share of the depth below the
/// line.
const SPLIT_ALPHA: f32 = 0.55;
const SPLIT_WIDTH: f32 = 0.0007;
const SPLIT_AT: f32 = 0.42;
/// Teeth count as clenched above this strength.
const CLENCHED_ABOVE: f32 = 0.5;
/// Fangs show from this strength.
const FANG_MIN: f32 = 0.01;
/// Fang length is capped at this share of the depth; the tip leans outward by [`FANG_LEAN`] metres; the base sits
/// [`FANG_DROP`] metres above the upper edge.
const FANG_MAX_SHARE: f32 = 0.62;
const FANG_LEAN: f32 = 0.0008;
const FANG_DROP: f32 = 0.0005;

/// Paints the inside of an open mouth.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let Some(open) = ctx.open_mouth else { return };
    ctx.list(FaceLayerId::Base).push(Cmd::PushClip(open.path));
    tongue(ctx, &open);
    upper_teeth(ctx, &open);
    lower_teeth(ctx, &open);
    fangs(ctx, &open);
    ctx.list(FaceLayerId::Base).push(Cmd::PopClip);
}

fn tongue(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    let colour = resolve(&ctx.profile.mouth.interior.tongue, DEFAULT_TONGUE);
    let (c, w, h) = (open.centre, open.half_width, open.height);
    let list = ctx.list(FaceLayerId::Base);
    let body = list.path().ellipse(c - Vec2::new(0.0, h * TONGUE_AT), w * TONGUE_RX, h * TONGUE_RY).finish();
    list.fill(body, Paint::Solid(colour));
    let shine = list.path().ellipse(c + Vec2::new(w * SHINE_X, -h * SHINE_AT), w * SHINE_RX, h * SHINE_RY).finish();
    list.fill(shine, solid(lighten(colour, SHINE_WHITE), SHINE_ALPHA));
}

fn upper_teeth(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    let mouth = &ctx.params.mouth;
    let band = mouth.teeth_upper.max(if mouth.fangs > FANG_BAND_ABOVE { FANG_BAND } else { 0.0 });
    if band <= BAND_MIN {
        return;
    }
    let share = if mouth.teeth_lower > CLENCHED_ABOVE { BAND_SHARE_LOW } else { BAND_SHARE };
    let depth = (open.height * share).min(BAND_MAX * open.grow) * band;
    let colour = resolve(&ctx.profile.mouth.interior.teeth, DEFAULT_TEETH);
    let edge = |i: usize| {
        let u = along(i, BAND_SEGMENTS);
        open.centre + Vec2::new(u * open.half_width, open.top(u))
    };
    let list = ctx.list(FaceLayerId::Base);
    let mut outline = list.path();
    for i in 0..=BAND_SEGMENTS {
        outline = if i == 0 { outline.move_to(edge(i)) } else { outline.line_to(edge(i)) };
    }
    for i in (0..=BAND_SEGMENTS).rev() {
        outline = outline.line_to(edge(i) - Vec2::new(0.0, depth));
    }
    let band = outline.close().finish();
    list.fill(band, Paint::Solid(colour));
}

fn lower_teeth(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    let strength = ctx.params.mouth.teeth_lower;
    if strength <= LOWER_MIN {
        return;
    }
    let colour = resolve(&ctx.profile.mouth.interior.teeth_lower, DEFAULT_TEETH_LOWER);
    let line = resolve(&ctx.profile.mouth.line_colour, DEFAULT_LINE);
    let clenched = ctx.params.mouth.teeth_upper > CLENCHED_ABOVE;
    let row_height = (open.height * LOWER_SHARE).max(ctx.metres_from_px(LOWER_MIN_PX));
    let (c, w, h) = (open.centre, open.half_width, open.height);
    let top = c.y - h * LOWER_AT;
    let list = ctx.list(FaceLayerId::Base);
    let row = list
        .path()
        .move_to(Vec2::new(c.x - w, top))
        .line_to(Vec2::new(c.x + w, top))
        .line_to(Vec2::new(c.x + w, top - row_height))
        .line_to(Vec2::new(c.x - w, top - row_height))
        .close()
        .finish();
    list.push(Cmd::Fill { path: row, paint: Paint::Solid(colour), opacity: strength.min(1.0) });
    if clenched {
        let y = c.y - h * SPLIT_AT;
        let split = list.path().move_to(Vec2::new(c.x - w, y)).line_to(Vec2::new(c.x + w, y)).finish();
        list.stroke(split, solid(line, SPLIT_ALPHA), SPLIT_WIDTH);
    }
}

fn fangs(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    let strength = ctx.params.mouth.fangs;
    if strength <= FANG_MIN {
        return;
    }
    let style = &ctx.profile.mouth.fang;
    let (width, height, position) = (narrow(style.width), narrow(style.height), narrow(style.position));
    let colour = resolve(&ctx.profile.mouth.interior.teeth, DEFAULT_TEETH);
    let half = width / 2.0 * open.grow;
    // The distance from a base's centre to the mouth's corner on its side. `position` is validated to `0.3..=1`, so
    // this is never negative and needs no clamp.
    let room = (1.0 - position) * open.half_width;
    let length = (height * open.grow).min(open.height * FANG_MAX_SHARE);
    let list = ctx.list(FaceLayerId::Base);
    for side in [-1.0_f32, 1.0] {
        let u = side * position;
        let base = open.centre + Vec2::new(u * open.half_width, open.top(u) + FANG_DROP);
        // The base corner toward the mouth's corner (outer) stops there, so a narrow mouth narrows the fang instead of
        // clipping it; the corner toward the middle (inner) keeps its full half-width.
        let outer = half.min(room);
        let (left, right) = if side > 0.0 { (half, outer) } else { (outer, half) };
        let fang = list
            .path()
            .move_to(base - Vec2::new(left, 0.0))
            .line_to(base + Vec2::new(right, 0.0))
            .line_to(base + Vec2::new(side * FANG_LEAN, -length))
            .close()
            .finish();
        list.push(Cmd::Fill { path: fang, paint: Paint::Solid(colour), opacity: strength.min(1.0) });
    }
}
