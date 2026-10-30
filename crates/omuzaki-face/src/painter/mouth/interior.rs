//! The inside of an open mouth: tongue, upper and lower teeth and fangs (`face.js` 337-362), and the fang a closed
//! smile shows (`face.js` 319-323).
//!
//! Everything is clipped to the open shape's outline ([`OpenMouth::path`]), so no pixel lands outside it, and the teeth
//! and fangs are measured from the outline's own upper edge ([`OpenMouth::top`]), so they follow the lip however it
//! curves. The inner-mouth gradient is the shape's fill, not drawn here. A closed mouth or a kiss has no interior (only
//! a closed smile shows a fang), except while a kiss gives way to the open shape: then it paints at the partial `reveal`.
//!
//! - **Tongue**: an ellipse resting on the bottom, with a small shine.
//! - **Upper teeth**: a band hanging from the upper edge, as strong as the `teeth_upper` channel, and at least half
//!   there when fangs show.
//! - **Lower teeth**: a row under the line, as strong as `teeth_lower`, with a split line when the upper teeth are
//!   clenched too, as faint as the row. The row starts below the lowest point of a flat mouth, so only a real smile,
//!   which lowers the mouth's centre, brings it into the opening.
//! - **Fangs**: a triangle hanging from the upper edge at [`FangStyle::position`] of the half-width at each corner the
//!   style's [`FangKind`] has (both, or one), as opaque as the `fangs` channel. In a mouth too narrow for the base, the
//!   outer corner stops at the mouth's corner, so the fang narrows instead of being clipped.
//! - **Sharp teeth** (`teeth_sharp`, #2406): a zigzag row of pointed teeth along the upper edge and another, offset by
//!   half a tooth, rising from the lower edge, each with a thin outline: the shark teeth of an angry anime face. As
//!   they come in, the plain teeth and the fangs give way to them.
//! - **Closed-smile fang**: one small outlined triangle on the line of a closed mouth, at the character's left corner
//!   (the right one for a [`FangKind::OneRight`] face), when the `fangs` channel is on and that corner smiles enough. As the mouth opens it fades out over the openings 0.06 to
//!   0.2 ([`OpenMouth::fade`]) while the open fangs, clipped to a lens that has hardly any depth there, take over.
//!
//! [`FangStyle::position`]: omuzaki_face_api::FangStyle::position
//! [`FangKind`]: omuzaki_face_api::FangKind
//! [`FangKind::OneRight`]: omuzaki_face_api::FangKind::OneRight

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cap, Cmd, FaceLayerId, FangKind, Join, Paint, narrow};

use super::OpenMouth;
use super::lips::{DEFAULT_LINE, DEFAULT_TEETH, DEFAULT_TEETH_LOWER, DEFAULT_TONGUE, lighten};
use super::shape::{Frame, Kind, along, solid};
use crate::painter::{PaintCtx, fixed_colour};

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
/// The tongue's groove: the mouth line's colour at this alpha, this wide (metres, times the performance scale).
const GROOVE_ALPHA: f32 = 0.45;
const GROOVE_WIDTH: f32 = 0.0005;
/// The gum strip's share of the teeth band's depth.
const GUM_SHARE: f32 = 0.22;
/// The lines between teeth: the mouth line's colour at this alpha, this wide, reaching this share down the band.
const GAP_ALPHA: f32 = 0.5;
const GAP_WIDTH: f32 = 0.00035;
const GAP_REACH: f32 = 0.85;
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
/// The closed smile's fang, scaled from the open ones: half-width and height (`0.0026` and `0.0045` against the style's
/// `0.0030` and `0.0058`), its place (`0.55` of the half-width against `0.72`) and its outline, in metres.
const CLOSED_FANG_WIDTH: f32 = 0.0026 / 0.0030;
const CLOSED_FANG_HEIGHT: f32 = 0.0045 / 0.0058;
const CLOSED_FANG_AT: f32 = 0.55 / 0.72;
const CLOSED_FANG_OUTLINE: f32 = 0.0008;
/// The closed fang needs a smile above this at its corner.
const CLOSED_FANG_SMILE: f32 = 0.3;
/// Sharp teeth show from this strength. Each row has this many teeth; a tooth's tip reaches this share of the opening
/// at its place (upper row) or rises this share (lower row), and its root sits this share in from the edge, so the row
/// reads as a band with points. The outline is the mouth line's colour at this alpha and width.
const SHARP_MIN: f32 = 0.01;
const SHARP_TEETH: u8 = 7;
const SHARP_UPPER_TIP: f32 = 0.42;
const SHARP_LOWER_TIP: f32 = 0.36;
const SHARP_ROOT: f32 = 0.08;
const SHARP_OUTLINE_ALPHA: f32 = 0.7;
const SHARP_OUTLINE_WIDTH: f32 = 0.0006;

/// Paints the inside of an open mouth, or the fang of a closed smile.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    match (ctx.open_mouth, Frame::of(ctx).kind()) {
        (Some(open), _) => {
            open_interior(ctx, &open);
            closed_fang(ctx, 1.0 - open.fade);
        }
        (None, Kind::Closed) => closed_fang(ctx, 1.0),
        (None, _) => {}
    }
}

fn open_interior(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    ctx.list(FaceLayerId::Base).push(Cmd::PushClip(open.path));
    tongue(ctx, open);
    // Sharp teeth take over from the plain ones and the fangs as they come in.
    let sharp = ctx.params.mouth.teeth_sharp.clamp(0.0, 1.0);
    let mark = ctx.list(FaceLayerId::Base).cmds().len();
    upper_teeth(ctx, open);
    lower_teeth(ctx, open);
    fangs(ctx, open);
    if sharp > SHARP_MIN {
        ctx.list(FaceLayerId::Base).scale_opacity_from(mark, 1.0 - sharp);
        sharp_teeth(ctx, open, sharp);
    }
    ctx.list(FaceLayerId::Base).push(Cmd::PopClip);
}

/// The zigzag rows of sharp teeth, upper and lower, at `strength` (see the module docs).
fn sharp_teeth(ctx: &mut PaintCtx<'_>, open: &OpenMouth, strength: f32) {
    let teeth = fixed_colour(&ctx.profile.mouth.interior.teeth, DEFAULT_TEETH);
    let line = fixed_colour(&ctx.profile.mouth.line_colour, DEFAULT_LINE);
    let opacity = strength * open.reveal;
    let n = SHARP_TEETH;
    // Points along the row: `2n + 1` of them, a root on every even index and a tip on every odd one. `lower` offsets
    // the lower row by half a tooth, so its points sit between the upper row's.
    let row = |list: &mut omuzaki_face_api::DrawList, lower: bool| {
        let edge = |u: f32| if lower { open.bottom(u) } else { open.top(u) };
        let gap = |u: f32| (open.top(u) - open.bottom(u)).max(0.0);
        let point = |k: u8| {
            let shift = if lower { 1.0 / f32::from(2 * n) } else { 0.0 };
            let u = (-1.0 + 2.0 * f32::from(k) / f32::from(2 * n) + shift).clamp(-1.0, 1.0);
            let share = if k % 2 == 1 { if lower { SHARP_LOWER_TIP } else { SHARP_UPPER_TIP } } else { SHARP_ROOT };
            let depth = gap(u) * share;
            let y = if lower { edge(u) + depth } else { edge(u) - depth };
            open.centre + Vec2::new(u * open.half_width, y)
        };
        let beyond = |u: f32| {
            open.centre + Vec2::new(u * open.half_width, edge(u) + if lower { -1.0 } else { 1.0 } * open.height)
        };
        let mut path = list.path().move_to(beyond(-1.0));
        for k in 0..=2 * n {
            path = path.line_to(point(k));
        }
        path.line_to(beyond(1.0)).close().finish()
    };
    let list = ctx.list(FaceLayerId::Base);
    for lower in [false, true] {
        let path = row(list, lower);
        list.push(Cmd::Fill { path, paint: Paint::Solid(teeth), opacity });
        list.push(Cmd::Stroke {
            path,
            paint: solid(line, SHARP_OUTLINE_ALPHA),
            width: SHARP_OUTLINE_WIDTH * open.grow,
            cap: Cap::Round,
            join: Join::Miter,
            opacity,
        });
    }
}

fn tongue(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    let colour = fixed_colour(&ctx.profile.mouth.interior.tongue, DEFAULT_TONGUE);
    let shape = &ctx.profile.mouth.tongue;
    let (wide, tall, groove) = (narrow(shape.width), narrow(shape.height), narrow(shape.groove).clamp(0.0, 1.0));
    let (c, w, h) = (open.centre, open.half_width, open.height);
    let line = fixed_colour(&ctx.profile.mouth.line_colour, DEFAULT_LINE);
    let list = ctx.list(FaceLayerId::Base);
    let centre = c - Vec2::new(0.0, h * TONGUE_AT);
    let (rx, ry) = (w * TONGUE_RX * wide, h * TONGUE_RY * tall);
    let body = list.path().ellipse(centre, rx, ry).finish();
    list.push(Cmd::Fill { path: body, paint: Paint::Solid(colour), opacity: open.reveal });
    if groove > 0.0 {
        // The groove down the middle, from the tongue's top toward its centre.
        let path =
            list.path().move_to(centre + Vec2::new(0.0, ry)).line_to(centre + Vec2::new(0.0, ry * 0.15)).finish();
        list.push(Cmd::Stroke {
            path,
            paint: solid(line, GROOVE_ALPHA),
            width: GROOVE_WIDTH * open.grow,
            cap: Cap::Round,
            join: Join::Round,
            opacity: groove * open.reveal,
        });
    }
    let shine = list.path().ellipse(c + Vec2::new(w * SHINE_X, -h * SHINE_AT), w * SHINE_RX, h * SHINE_RY).finish();
    list.push(Cmd::Fill { path: shine, paint: solid(lighten(colour, SHINE_WHITE), SHINE_ALPHA), opacity: open.reveal });
}

fn upper_teeth(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    let mouth = &ctx.params.mouth;
    let fanged = ctx.profile.has_fangs() && mouth.fangs > FANG_BAND_ABOVE;
    let band = mouth.teeth_upper.max(if fanged { FANG_BAND } else { 0.0 });
    if band <= BAND_MIN {
        return;
    }
    let share = if mouth.teeth_lower > CLENCHED_ABOVE { BAND_SHARE_LOW } else { BAND_SHARE };
    let teeth = &ctx.profile.mouth.teeth;
    let depth = (open.height * share).min(BAND_MAX * open.grow) * band * narrow(teeth.height);
    let (gaps, gum, count) =
        (narrow(teeth.gaps).clamp(0.0, 1.0), narrow(teeth.gum).clamp(0.0, 1.0), teeth.count.max(2));
    let gum_colour = fixed_colour(&teeth.gum_colour, DEFAULT_TONGUE);
    let line = fixed_colour(&ctx.profile.mouth.line_colour, DEFAULT_LINE);
    let colour = fixed_colour(&ctx.profile.mouth.interior.teeth, DEFAULT_TEETH);
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
    list.push(Cmd::Fill { path: band, paint: Paint::Solid(colour), opacity: open.reveal });
    if gum > 0.0 {
        // A strip of gum along the top of the band, its share of the band's depth.
        let mut strip = list.path();
        for i in 0..=BAND_SEGMENTS {
            strip = if i == 0 { strip.move_to(edge(i)) } else { strip.line_to(edge(i)) };
        }
        for i in (0..=BAND_SEGMENTS).rev() {
            strip = strip.line_to(edge(i) - Vec2::new(0.0, depth * GUM_SHARE));
        }
        let strip = strip.close().finish();
        list.push(Cmd::Fill { path: strip, paint: Paint::Solid(gum_colour), opacity: gum * open.reveal });
    }
    if gaps > 0.0 {
        // The lines between teeth, short of the band's bottom so the row's edge stays clean; one stroke for all.
        let mut path = list.path();
        for k in 1..count {
            // At most 16 teeth (validated), exact in f32.
            #[allow(clippy::cast_precision_loss)]
            let u = -1.0 + 2.0 * k as f32 / count as f32;
            let top = open.centre + Vec2::new(u * open.half_width, open.top(u));
            path = path.move_to(top).line_to(top - Vec2::new(0.0, depth * GAP_REACH));
        }
        let path = path.finish();
        list.push(Cmd::Stroke {
            path,
            paint: solid(line, GAP_ALPHA),
            width: GAP_WIDTH * open.grow,
            cap: Cap::Round,
            join: Join::Round,
            opacity: gaps * open.reveal,
        });
    }
}

fn lower_teeth(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    let strength = ctx.params.mouth.teeth_lower;
    if strength <= LOWER_MIN {
        return;
    }
    let colour = fixed_colour(&ctx.profile.mouth.interior.teeth_lower, DEFAULT_TEETH_LOWER);
    let line = fixed_colour(&ctx.profile.mouth.line_colour, DEFAULT_LINE);
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
    list.push(Cmd::Fill { path: row, paint: Paint::Solid(colour), opacity: strength.min(1.0) * open.reveal });
    if clenched {
        let y = c.y - h * SPLIT_AT;
        let split = list.path().move_to(Vec2::new(c.x - w, y)).line_to(Vec2::new(c.x + w, y)).finish();
        // The row's own opacity, so the line fades in with the teeth instead of appearing at its full alpha.
        list.push(Cmd::Stroke {
            path: split,
            paint: solid(line, SPLIT_ALPHA),
            width: SPLIT_WIDTH,
            cap: Cap::Round,
            join: Join::Round,
            opacity: strength.min(1.0) * open.reveal,
        });
    }
}

fn fangs(ctx: &mut PaintCtx<'_>, open: &OpenMouth) {
    let strength = ctx.params.mouth.fangs;
    // A character with no fangs paints none, whatever the channel says (a score or a person can still write it).
    if strength <= FANG_MIN || !ctx.profile.has_fangs() {
        return;
    }
    let style = &ctx.profile.mouth.fang;
    let (width, height, position) = (narrow(style.width), narrow(style.height), narrow(style.position));
    let colour = fixed_colour(&ctx.profile.mouth.interior.teeth, DEFAULT_TEETH);
    let half = width / 2.0 * open.grow;
    // The distance from a base's centre to the mouth's corner on its side. `position` is validated to `0.3..=1`, so
    // this is never negative and needs no clamp.
    let room = (1.0 - position) * open.half_width;
    let length = (height * open.grow).min(open.height * FANG_MAX_SHARE);
    let list = ctx.list(FaceLayerId::Base);
    for &side in style.kind.corners() {
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
        list.push(Cmd::Fill { path: fang, paint: Paint::Solid(colour), opacity: strength.min(1.0) * open.reveal });
    }
}

/// The fang a closed smile shows on the corner that smiles (`u` > 0): a small outlined triangle standing on the line,
/// drawn at `opacity` (less than 1 while the mouth opens and the open fangs take over).
fn closed_fang(ctx: &mut PaintCtx<'_>, opacity: f32) {
    let frame = Frame::of(ctx);
    // The character's left corner, unless the only fang is on the right.
    let side = if ctx.profile.mouth.fang.kind == FangKind::OneRight { -1.0 } else { 1.0 };
    let smile = if side > 0.0 { frame.smile[0] } else { frame.smile[1] };
    if !ctx.profile.has_fangs()
        || ctx.params.mouth.fangs <= FANG_BAND_ABOVE
        || smile <= CLOSED_FANG_SMILE
        || opacity <= 0.0
    {
        return;
    }
    let style = &ctx.profile.mouth;
    let teeth = fixed_colour(&style.interior.teeth, DEFAULT_TEETH);
    let outline = fixed_colour(&style.line_colour, DEFAULT_LINE);
    let (half, height) =
        (narrow(style.fang.width) / 2.0 * CLOSED_FANG_WIDTH, narrow(style.fang.height) * CLOSED_FANG_HEIGHT);
    let offset = frame.half_width * narrow(style.fang.position) * CLOSED_FANG_AT;
    // The base stands on the closed line, at the same height `closed_line` draws it at this far along.
    let u = offset / frame.half_width;
    let y = frame.centre.y + smile * narrow(style.corner_curve) * u * u * frame.grow;
    let x = frame.centre.x + side * offset;
    // As for the open fangs, the outer base corner stops at the line's end instead of hanging past it.
    let outer = half.min(frame.half_width - offset);
    let list = ctx.list(FaceLayerId::Base);
    let (inner_x, outer_x) = (x - side * half, x + side * outer);
    let fang = list
        .path()
        .move_to(Vec2::new(inner_x.min(outer_x), y))
        .line_to(Vec2::new(inner_x.max(outer_x), y))
        .line_to(Vec2::new(x, y - height))
        .close()
        .finish();
    let (fill, stroke) = (Paint::Solid(teeth), Paint::Solid(outline));
    list.push(Cmd::Fill { path: fang, paint: fill, opacity });
    list.push(Cmd::Stroke {
        path: fang,
        paint: stroke,
        width: CLOSED_FANG_OUTLINE,
        cap: Cap::Round,
        join: Join::Round,
        opacity,
    });
}
