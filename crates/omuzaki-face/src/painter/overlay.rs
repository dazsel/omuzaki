//! The Overlay layer, in front of the hair: the sweat drop at the temple, the gloom's hatch over the forehead and the
//! anger vein.
//!
//! The prototype has neither (they are new designs in its drawing language: flat colours, fine lines, one soft shade),
//! so there is no parity test; the Face lead signs the look on a contact sheet. Both scale smoothly with their channel,
//! draw nothing at 0 and stay inside the face rect (every point is clamped into it), so they composite over any hair.
//!
//! - **Sweat** (`PaintParams::sweat`): a drop at the character's left temple, in the tear's colour with a thin darker
//!   outline and a small highlight. It grows from 60 % to full size as it fades in.
//! - **Gloom** (`effects.gloom`): a soft dark oval across the upper forehead, gone at its edges, and a row of fine
//!   vertical hatch lines of alternating length over it, each fading out toward its top (#2428).
//! - **Anger vein** (`PaintParams::anger_vein`, #2403): the cross-shaped popping mark at the temple, on the side the
//!   face style says ([`AngerVeinStyle`](omuzaki_face_api::AngerVeinStyle)). Four thick corner strokes in the style's
//!   red, each with a dark outline, their corners pointing at a shared centre. It pops in: its size overshoots by about
//!   12 % halfway through its fade-in and settles at full size, and it is fully opaque from 40 % on, so a fade of a few
//!   frames reads as a pop and a hard cut shows it at its final size.

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cap, Cmd, FaceLayerId, FaceRect, Join, Paint, Stops, narrow};
use std::f32::consts::PI;

use crate::painter::{PaintCtx, fixed_colour};

/// Below this a strength counts as off.
const OFF: f32 = 0.01;

// ---- sweat

/// The drop's half-width and half-height at full size, in metres, and how much of that it has when it first appears.
/// About an eye's height: the range test found a smaller drop vanishes at a small stream size (56 px), and on the head
/// it sits where the face turns away, so it needs the size to read there too.
const DROP_HALF: Vec2 = Vec2::new(0.013, 0.024);
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

/// The band's colour (`rgb(40,30,44)`, the lid shadow's) and its opacity in its middle at full strength.
const BAND_COLOUR: (u8, u8, u8) = (40, 30, 44);
const BAND_OPACITY: f32 = 0.30;
/// The shade's oval holds most of its darkness out to this share of its radius (the stop's alpha), then fades to none.
const BAND_FLAT: f32 = 0.55;
const BAND_FLAT_ALPHA: f32 = 0.75;
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
/// A hatch line is solid over this share of its length from the bottom, then fades to nothing at its top.
const HATCH_SOLID: f32 = 0.35;

// ---- anger vein

/// The gap between the four strokes' corners and the centre, and how far each arm reaches, in vein half-sizes; the
/// stroke's width as a share of the half-size, and the outline round it in metres.
const VEIN_GAP: f32 = 0.2;
const VEIN_ARM: f32 = 1.0;
const VEIN_WIDTH: f32 = 0.34;
const VEIN_OUTLINE: f32 = 0.0016;
/// The pop: the size at the start of the fade-in, the overshoot's height, and the share of the fade-in by which the
/// vein is fully opaque.
const VEIN_START: f32 = 0.6;
const VEIN_OVERSHOOT: f32 = 0.3;
const VEIN_OPAQUE_BY: f32 = 0.4;

/// The vein's size multiplier at `t` of its fade-in: from [`VEIN_START`] up past 1 and back to exactly 1 at `t = 1`.
fn vein_pop(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    VEIN_START + (1.0 - VEIN_START) * t + VEIN_OVERSHOOT * (PI * t).sin()
}

/// Paints the Overlay layer: the sweat drop, the gloom's band and hatch, and the anger vein.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let sweat = ctx.params.sweat.clamp(0.0, 1.0);
    if sweat > OFF {
        sweat_drop(ctx, sweat);
    }
    let gloom = ctx.params.effects.gloom.clamp(0.0, 1.0);
    if gloom > OFF {
        gloom_band(ctx, gloom);
    }
    let vein = ctx.params.anger_vein.clamp(0.0, 1.0);
    if vein > OFF {
        anger_vein(ctx, vein);
    }
}

/// The anger vein at the temple (see the module docs).
fn anger_vein(ctx: &mut PaintCtx<'_>, strength: f32) {
    let style = &ctx.profile.effects.anger_vein;
    let layout = &ctx.profile.layout;
    let bounds = rect(ctx);
    let reach = VEIN_ARM + VEIN_WIDTH / 2.0;
    let half = narrow(style.size) * vein_pop(strength);
    // Never larger than the window, so the clamp below always has room.
    let half = half.min((bounds.x1 - bounds.x0) / (2.0 * reach)).min((bounds.y1 - bounds.y0) / (2.0 * reach));
    let room = half * reach;
    let corner = narrow(layout.eye_spacing + layout.eye_width * ctx.profile.eyes.scale);
    let at = Vec2::new(
        (style.side.sign() * (corner + narrow(style.out))).clamp(bounds.x0 + room, bounds.x1 - room),
        (narrow(layout.eye_y + layout.brow_gap + style.up)).clamp(bounds.y0 + room, bounds.y1 - room),
    );
    let opacity = (strength / VEIN_OPAQUE_BY).min(1.0);
    let (fill, outline) = (
        fixed_colour(&style.colour, Colour::rgb(0xe8, 0x2c, 0x34)),
        fixed_colour(&style.outline, Colour::rgb(0x4a, 0x0c, 0x14)),
    );
    let width = VEIN_WIDTH * half;
    let list = ctx.list(FaceLayerId::Overlay);
    // The four corners, one per quadrant, each a quadratic from one arm's end round the corner near the centre to the
    // other arm's end: the corner points inward, the arms flare out. All four outlines go first, so no outline cuts
    // across a neighbour's fill.
    let mut strokes = [None; 4];
    for (i, (sx, sy)) in [(1.0_f32, 1.0_f32), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)].into_iter().enumerate() {
        let p = |x: f32, y: f32| at + Vec2::new(sx * x * half, sy * y * half);
        let path =
            list.path().move_to(p(VEIN_GAP, VEIN_ARM)).quad_to(p(VEIN_GAP, VEIN_GAP), p(VEIN_ARM, VEIN_GAP)).finish();
        strokes[i] = Some(path);
    }
    for path in strokes.iter().flatten() {
        list.push(Cmd::Stroke {
            path: *path,
            paint: Paint::Solid(outline),
            width: width + 2.0 * VEIN_OUTLINE,
            cap: Cap::Round,
            join: Join::Round,
            opacity,
        });
    }
    for path in strokes.iter().flatten() {
        list.push(Cmd::Stroke {
            path: *path,
            paint: Paint::Solid(fill),
            width,
            cap: Cap::Round,
            join: Join::Round,
            opacity,
        });
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
    // The shade is a soft oval over the band, darkest in its middle and gone at every edge (#2428): a rectangle with a
    // vertical ramp read as a grey patch with a hard top where the layer's rectangle cut it. A disc of the band's
    // half-width, squashed to its half-height, so the fade is round whatever the band's shape.
    let centre = Vec2::new(0.5 * (left + right), 0.5 * (bottom + top));
    let radii = Vec2::new(0.5 * (right - left), 0.5 * (top - bottom));
    let stops = Stops::new(&[
        (0.0, Colour::rgb(r, g, b)),
        (BAND_FLAT, Colour::rgba(r, g, b, omuzaki_core::colour::to_byte(BAND_FLAT_ALPHA))),
        (1.0, Colour::rgba(r, g, b, 0)),
    ]);
    let squash = Affine2::from_translation(centre)
        * Affine2::from_scale(Vec2::new(1.0, radii.y / radii.x))
        * Affine2::from_translation(-centre);
    let list = ctx.list(FaceLayerId::Overlay);
    let band = list.path().ellipse(centre, radii.x, radii.x).finish();
    list.push(Cmd::PushTransform(squash));
    list.push(Cmd::Fill {
        path: band,
        paint: Paint::Radial { centre, radius: radii.x, stops },
        opacity: BAND_OPACITY * strength,
    });
    list.push(Cmd::PopTransform);
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
        // Each line fades out toward the top, so the hatch dissolves into the forehead instead of stopping at a
        // ruled edge.
        let line = list.path().move_to(Vec2::new(x, low)).line_to(Vec2::new(x, top)).finish();
        let fade = Stops::new(&[
            (0.0, Colour::rgb(r, g, b)),
            (HATCH_SOLID, Colour::rgb(r, g, b)),
            (1.0, Colour::rgba(r, g, b, 0)),
        ]);
        list.push(Cmd::Stroke {
            path: line,
            paint: Paint::Linear { from: Vec2::new(x, low), to: Vec2::new(x, top), stops: fade },
            width: HATCH_WIDTH,
            cap: Cap::Round,
            join: Join::Round,
            opacity: HATCH_OPACITY * strength,
        });
    }
}
