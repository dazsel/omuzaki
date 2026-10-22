//! The Overlay layer, in front of the hair: the sweat drop at the temple and the gloom's hatch over the forehead.
//!
//! The prototype has neither (they are new designs in its drawing language: flat colours, fine lines, one soft shade),
//! so there is no parity test; the Face lead signs the look on a contact sheet. Both scale smoothly with their channel,
//! draw nothing at 0 and stay inside the face rect (every point is clamped into it), so they composite over any hair.
//!
//! - **Sweat** (`PaintParams::sweat`): a drop at the character's left temple, in the tear's colour with a thin darker
//!   outline and a small highlight. It grows from 60 % to full size as it fades in.
//! - **Gloom** (`effects.gloom`): a soft dark band across the upper forehead, darkest at the top, and a row of fine
//!   vertical hatch lines of alternating length over it.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cap, Cmd, FaceLayerId, FaceRect, Join, Paint, Stops, narrow};

use crate::painter::{PaintCtx, fixed_colour};

/// Below this a strength counts as off.
const OFF: f32 = 0.01;

// ---- sweat

/// The drop's half-width and half-height at full size, in metres, and how much of that it has when it first appears.
/// About two thirds of an eye's height: the range test found a smaller drop vanishes at a small stream size (56 px).
const DROP_HALF: Vec2 = Vec2::new(0.0095, 0.0175);
const DROP_SMALLEST: f32 = 0.6;
/// The drop's place: out from the eye line's outer corner by this much, up from the eye line by this much, in metres.
const TEMPLE_OUT: f32 = 0.026;
const TEMPLE_UP: f32 = 0.045;
/// The outline's colour (`rgba(110,150,190)`), width and opacity at full strength.
const DROP_OUTLINE: Colour = Colour::rgb(110, 150, 190);
const DROP_OUTLINE_WIDTH: f32 = 0.0012;
const DROP_OUTLINE_OPACITY: f32 = 0.8;
/// The highlight on the drop: its place and radii as shares of the half-size, and its opacity.
const DROP_SHINE_AT: Vec2 = Vec2::new(-0.35, -0.30);
const DROP_SHINE_RADII: Vec2 = Vec2::new(0.22, 0.30);
const DROP_SHINE_OPACITY: f32 = 0.8;

// ---- gloom

/// The band's colour (`rgb(40,30,44)`, the lid shadow's) and its opacity at the top at full strength.
const BAND_COLOUR: (u8, u8, u8) = (40, 30, 44);
const BAND_OPACITY: f32 = 0.30;
/// The band and the hatch cover this much either side of the midline, from this far above the brow line, up to this far
/// above it (or the rect's top), in metres.
const BAND_HALF_WIDTH: f32 = 0.12;
const BAND_FROM: f32 = 0.010;
const BAND_REACH: f32 = 0.060;
/// Hatch lines: how many, their width and their opacity at full strength, and the share of the band a short one covers.
const HATCH_LINES: u8 = 9;
const HATCH_WIDTH: f32 = 0.0012;
const HATCH_OPACITY: f32 = 0.55;
const HATCH_SHORT: f32 = 0.6;

/// Paints the Overlay layer: the sweat drop and the gloom's band and hatch.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let sweat = ctx.params.sweat.clamp(0.0, 1.0);
    if sweat > OFF {
        sweat_drop(ctx, sweat);
    }
    let gloom = ctx.params.effects.gloom.clamp(0.0, 1.0);
    if gloom > OFF {
        gloom_band(ctx, gloom);
    }
}

/// The overlay layer's rectangle.
fn rect(ctx: &PaintCtx<'_>) -> FaceRect {
    ctx.specs[FaceLayerId::Overlay as usize].rect
}

/// The sweat drop at the character's left temple.
fn sweat_drop(ctx: &mut PaintCtx<'_>, strength: f32) {
    let layout = &ctx.profile.layout;
    let bounds = rect(ctx);
    // The drop shrinks to fit a small rectangle, so the clamps below always have room (min <= max).
    let fit =
        ((bounds.x1 - bounds.x0) / (2.0 * DROP_HALF.x)).min((bounds.y1 - bounds.y0) / (2.0 * DROP_HALF.y)).min(1.0);
    let size = DROP_HALF * (fit.max(0.0) * (DROP_SMALLEST + (1.0 - DROP_SMALLEST) * strength));
    // The outer corner of the eye on this side, out and up; kept clear of the rect's edges by the drop's own size.
    let outer = narrow(layout.eye_spacing + layout.eye_width * ctx.profile.eyes.scale);
    let at = Vec2::new(
        (outer + TEMPLE_OUT).clamp(bounds.x0 + size.x, bounds.x1 - size.x),
        (narrow(layout.eye_y) + TEMPLE_UP).clamp(bounds.y0 + size.y, bounds.y1 - size.y),
    );
    let colour = fixed_colour(&ctx.profile.eyes.tear, Colour::rgba(190, 225, 255, 217));
    let list = ctx.list(FaceLayerId::Overlay);
    // A drop standing on its round end, pointed up: two cubics from the tip to the bottom.
    let (tip, bottom) = (at + Vec2::new(0.0, size.y), at - Vec2::new(0.0, 0.7 * size.y));
    let drop = list
        .path()
        .move_to(tip)
        .cubic_to(at + Vec2::new(0.9 * size.x, 0.15 * size.y), at + Vec2::new(size.x, -0.7 * size.y), bottom)
        .cubic_to(at + Vec2::new(-size.x, -0.7 * size.y), at + Vec2::new(-0.9 * size.x, 0.15 * size.y), tip)
        .close()
        .finish();
    list.push(Cmd::Fill { path: drop, paint: Paint::Solid(colour), opacity: strength });
    list.push(Cmd::Stroke {
        path: drop,
        paint: Paint::Solid(DROP_OUTLINE),
        width: DROP_OUTLINE_WIDTH,
        cap: Cap::Round,
        join: Join::Round,
        opacity: DROP_OUTLINE_OPACITY * strength,
    });
    let shine_at = at + Vec2::new(DROP_SHINE_AT.x * size.x, DROP_SHINE_AT.y * size.y);
    let shine = list.path().ellipse(shine_at, DROP_SHINE_RADII.x * size.x, DROP_SHINE_RADII.y * size.y).finish();
    list.push(Cmd::Fill {
        path: shine,
        paint: Paint::Solid(Colour::rgb(255, 255, 255)),
        opacity: DROP_SHINE_OPACITY * strength,
    });
}

/// The gloom's soft band and fine vertical hatch over the upper forehead.
fn gloom_band(ctx: &mut PaintCtx<'_>, strength: f32) {
    let bounds = rect(ctx);
    let layout = &ctx.profile.layout;
    let brow = narrow(layout.eye_y + layout.brow_gap);
    let edge = |y: f32| y.clamp(bounds.y0, bounds.y1);
    let (bottom, top) = (edge(brow + BAND_FROM), edge(brow + BAND_REACH));
    if top <= bottom {
        return;
    }
    // The band is centred on the face's midline and cut to the rect on each side separately, so a rect that does not
    // straddle the midline still gets its part of the band (or none, if it misses it).
    let (left, right) = ((-BAND_HALF_WIDTH).max(bounds.x0), BAND_HALF_WIDTH.min(bounds.x1));
    if right <= left {
        return;
    }
    let (r, g, b) = BAND_COLOUR;
    let stops = Stops::new(&[(0.0, Colour::rgba(r, g, b, 0)), (1.0, Colour::rgb(r, g, b))]);
    let list = ctx.list(FaceLayerId::Overlay);
    let band = list
        .path()
        .polyline(&[Vec2::new(left, bottom), Vec2::new(right, bottom), Vec2::new(right, top), Vec2::new(left, top)])
        .close()
        .finish();
    list.push(Cmd::Fill {
        path: band,
        paint: Paint::Linear { from: Vec2::new(left, bottom), to: Vec2::new(left, top), stops },
        opacity: BAND_OPACITY * strength,
    });
    let lines = f32::from(HATCH_LINES);
    for i in 0..HATCH_LINES {
        // The lines keep their places over the band's full width; those outside the rect are left out, not
        // re-spread over what is left.
        let x = -BAND_HALF_WIDTH + 2.0 * BAND_HALF_WIDTH * (f32::from(i) + 0.5) / lines;
        if x < left || x > right {
            continue;
        }
        // Every other line is short, from the top down, so the hatch reads as hand-drawn rather than as a fence.
        let low = if i % 2 == 0 { bottom } else { top - (top - bottom) * HATCH_SHORT };
        let line = list.path().move_to(Vec2::new(x, low)).line_to(Vec2::new(x, top)).finish();
        list.push(Cmd::Stroke {
            path: line,
            paint: Paint::Solid(Colour::rgb(r, g, b)),
            width: HATCH_WIDTH,
            cap: Cap::Round,
            join: Join::Round,
            opacity: HATCH_OPACITY * strength,
        });
    }
}
