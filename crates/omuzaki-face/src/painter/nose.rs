//! The nose: the prototype's tiny tick with a dot of light above it (`face.js` 411-416), or a plain dot, and the
//! creases a sneer wrinkles over its bridge.
//!
//! Offsets are from the nose line (`layout.nose_y`, face plane) and in metres, the prototype's own numbers; the
//! style's `size` scales every length of the mark about `(0, nose_y)`.

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cap, Cmd, Colour, FaceLayerId, Join, NoseKind, Paint, narrow};

use crate::painter::{PaintCtx, fixed_colour};

/// The tick runs from the first point to the second, as offsets from `(0, nose_y)`.
const TICK_FROM: Vec2 = Vec2::new(0.0012, 0.0017);
const TICK_TO: Vec2 = Vec2::new(-0.0006, -0.0017);
/// Tick line width and its opacity.
const TICK_WIDTH: f32 = 0.0012;
const TICK_OPACITY: f32 = 0.8;
/// The dot of light above the tick: centre offset, radius, opacity and colour.
const LIGHT_AT: Vec2 = Vec2::new(0.0028, 0.004);
const LIGHT_RADIUS: f32 = 0.0009;
const LIGHT_OPACITY: f32 = 0.7;
const LIGHT_COLOUR: Colour = Colour::rgb(255, 250, 250);
/// The plain dot's radius and opacity.
const DOT_RADIUS: f32 = 0.0011;
const DOT_OPACITY: f32 = 0.8;
/// The sneer's creases (#2428): on each side of the bridge, two short diagonal lines above the nose, the inner one
/// higher, from the first offset to the second (the character's left side; the right mirrors it). Drawn from this
/// much sneer on, at full opacity by full sneer.
const CREASES: [(Vec2, Vec2); 2] =
    [(Vec2::new(0.0030, 0.0135), Vec2::new(0.0058, 0.0085)), (Vec2::new(0.0052, 0.0150), Vec2::new(0.0080, 0.0100))];
const CREASE_WIDTH: f32 = 0.0011;
const CREASE_OPACITY: f32 = 0.75;
const SNEER_FROM: f32 = 0.05;
/// Colour used when a palette slot reached the painter unresolved (it should not: the host resolves them).
const FALLBACK_COLOUR: Colour = Colour::rgb(0xbe, 0x78, 0x78);

/// Paints the nose on the Base layer.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let style = &ctx.profile.nose;
    let colour = fixed_colour(&style.colour, FALLBACK_COLOUR);
    let origin = Vec2::new(0.0, narrow(ctx.profile.layout.nose_y));
    let size = narrow(style.size);
    let at = |offset: Vec2| origin + offset * size;
    let list = ctx.list(FaceLayerId::Base);
    match style.kind {
        NoseKind::None => {}
        NoseKind::Tick => {
            let tick = list.path().move_to(at(TICK_FROM)).line_to(at(TICK_TO)).finish();
            list.push(Cmd::Stroke {
                path: tick,
                paint: Paint::Solid(colour),
                width: TICK_WIDTH * size,
                cap: Cap::Round,
                join: Join::Round,
                opacity: TICK_OPACITY,
            });
            let light = list.path().ellipse(at(LIGHT_AT), LIGHT_RADIUS * size, LIGHT_RADIUS * size).finish();
            list.push(Cmd::Fill { path: light, paint: Paint::Solid(LIGHT_COLOUR), opacity: LIGHT_OPACITY });
        }
        NoseKind::Dot => {
            let dot = list.path().ellipse(origin, DOT_RADIUS * size, DOT_RADIUS * size).finish();
            list.push(Cmd::Fill { path: dot, paint: Paint::Solid(colour), opacity: DOT_OPACITY });
        }
    }
    // The sneer wrinkles the bridge, whatever the nose's kind: the mark that tells disgust from a frown.
    let sneer = ctx.params.nose_sneer.clamp(0.0, 1.0);
    if sneer >= SNEER_FROM {
        let list = ctx.list(FaceLayerId::Base);
        for side in [1.0, -1.0] {
            for (from, to) in CREASES {
                let mirror = |p: Vec2| Vec2::new(p.x * side, p.y);
                let crease = list.path().move_to(at(mirror(from))).line_to(at(mirror(to))).finish();
                list.push(Cmd::Stroke {
                    path: crease,
                    paint: Paint::Solid(colour),
                    width: CREASE_WIDTH * size,
                    cap: Cap::Round,
                    join: Join::Round,
                    opacity: CREASE_OPACITY * sneer,
                });
            }
        }
    }
}
