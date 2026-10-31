//! The nose: the prototype's tiny tick with a dot of light above it (`face.js` 411-416), or a plain dot; the toon
//! noses of Western animation and comics (a cel-shadow wedge, an inked line, a button); and the creases a sneer
//! wrinkles over its bridge.
//!
//! Offsets are from the nose line (`layout.nose_y`, face plane) and in metres, the prototype's own numbers; the
//! style's `size` scales every length of the mark about `(0, nose_y)`.

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{Cap, Cmd, Colour, DrawList, FaceLayerId, Join, NoseKind, Paint, Stops, narrow};

use crate::painter::{PaintCtx, fixed_colour, scaled};

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
/// The contour (#2472): a soft shade down the bridge on the side away from the light (the tick's light is on the
/// character's left, so the shade is on the right), as offsets from the nose line: its centre and radii, and its
/// opacity at full strength.
const SHADE_AT: Vec2 = Vec2::new(-0.0032, 0.0085);
const SHADE_RADII: Vec2 = Vec2::new(0.0028, 0.011);
const SHADE_ALPHA: f32 = 0.7;
/// The highlight: a soft streak on the bridge and a spot on the tip.
const STREAK_AT: Vec2 = Vec2::new(0.0012, 0.011);
const STREAK_RADII: Vec2 = Vec2::new(0.0011, 0.0062);
const TIP_AT: Vec2 = Vec2::new(0.0012, 0.0012);
const TIP_RADIUS: f32 = 0.0016;
const HIGHLIGHT_ALPHA: f32 = 0.6;
/// Colour used when a palette slot reached the painter unresolved (it should not: the host resolves them).
const FALLBACK_COLOUR: Colour = Colour::rgb(0xbe, 0x78, 0x78);
/// How far a toon nose (wedge or line) reaches up from the nose line at `length` 1, metres.
const BRIDGE_RISE: f32 = 0.012;
/// The wedge: a cel shadow on the shadow side (the character's right, -x), as offsets from the nose line. `y` of the
/// top two points scales with the length; the rest is the tip. Its opacity, and the nostril hook drawn over it.
const WEDGE: [Vec2; 5] = [
    Vec2::new(-0.0006, 1.0),
    Vec2::new(-0.0024, 1.0),
    Vec2::new(-0.0046, 0.0012),
    Vec2::new(-0.0016, -0.0010),
    Vec2::new(-0.0008, 0.0030),
];
const WEDGE_OPACITY: f32 = 0.75;
const NOSTRIL: [Vec2; 3] = [Vec2::new(-0.0042, 0.0006), Vec2::new(-0.0030, -0.0016), Vec2::new(-0.0010, -0.0012)];
const NOSTRIL_LIGHT: [Vec2; 3] = [Vec2::new(0.0032, 0.0004), Vec2::new(0.0024, -0.0012), Vec2::new(0.0010, -0.0012)];
const NOSTRIL_OPACITY: f32 = 0.9;
const NOSTRIL_LIGHT_OPACITY: f32 = 0.45;
/// The line: down the shadow side of the bridge (top `y` scales with the length), round the tip and back under it.
const LINE: [Vec2; 4] =
    [Vec2::new(-0.0010, 1.0), Vec2::new(-0.0034, 0.0024), Vec2::new(-0.0016, -0.0016), Vec2::new(0.0014, -0.0008)];
/// The button: an arc under a round tip, from one side to the other through the bottom, and two nostril dots.
const BUTTON_RADIUS: f32 = 0.0040;
const BUTTON_DOT: Vec2 = Vec2::new(0.0017, -0.0003);
const BUTTON_DOT_RADIUS: f32 = 0.0006;

/// Paints the nose on the Base layer.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let style = &ctx.profile.nose;
    let colour = fixed_colour(&style.colour, FALLBACK_COLOUR);
    let origin = Vec2::new(0.0, narrow(ctx.profile.layout.nose_y));
    let size = narrow(style.size);
    let at = |offset: Vec2| origin + offset * size;
    let (shade, light) = (narrow(style.shadow).clamp(0.0, 1.0), narrow(style.highlight).clamp(0.0, 1.0));
    let list = ctx.list(FaceLayerId::Base);
    if shade > 0.0 {
        soft(list, at(SHADE_AT), SHADE_RADII * size, scaled(colour, SHADE_ALPHA * shade));
    }
    if light > 0.0 {
        let white = scaled(LIGHT_COLOUR, HIGHLIGHT_ALPHA * light);
        soft(list, at(STREAK_AT), STREAK_RADII * size, white);
        soft(list, at(TIP_AT), Vec2::splat(TIP_RADIUS * size), white);
    }
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
        NoseKind::Wedge | NoseKind::Line | NoseKind::Button => {
            let length = narrow(style.length).clamp(0.5, 3.0);
            let width = narrow(style.line_width).max(0.0) * size;
            toon(list, style.kind, &at, (colour, width, length), size);
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

/// A toon nose of `kind`: `at` places an offset from the nose line, `ink` is the colour, line width and length.
fn toon(list: &mut DrawList, kind: NoseKind, at: &dyn Fn(Vec2) -> Vec2, ink: (Colour, f32, f32), size: f32) {
    let (colour, width, length) = ink;
    // Points whose `y` is 1.0 are the bridge top: that far up at the nose's length.
    let rise = |p: Vec2| if p.y >= 1.0 { Vec2::new(p.x, BRIDGE_RISE * length) } else { p };
    let stroke = |list: &mut DrawList, path, opacity| {
        list.push(Cmd::Stroke {
            path,
            paint: Paint::Solid(colour),
            width,
            cap: Cap::Round,
            join: Join::Round,
            opacity,
        });
    };
    match kind {
        NoseKind::Wedge => {
            let p = WEDGE.map(|p| at(rise(p)));
            let wedge =
                list.path().move_to(p[0]).line_to(p[1]).quad_to(p[2], p[3]).quad_to(p[4], p[0]).close().finish();
            list.push(Cmd::Fill { path: wedge, paint: Paint::Solid(colour), opacity: WEDGE_OPACITY });
            let n = NOSTRIL.map(at);
            let hook = list.path().move_to(n[0]).quad_to(n[1], n[2]).finish();
            stroke(list, hook, NOSTRIL_OPACITY);
            let n = NOSTRIL_LIGHT.map(at);
            let hook = list.path().move_to(n[0]).quad_to(n[1], n[2]).finish();
            stroke(list, hook, NOSTRIL_LIGHT_OPACITY);
        }
        NoseKind::Line => {
            let p = LINE.map(|p| at(rise(p)));
            let line = list.path().move_to(p[0]).quad_to(p[1], p[2]).line_to(p[3]).finish();
            stroke(list, line, NOSTRIL_OPACITY);
        }
        NoseKind::Button => {
            let r = BUTTON_RADIUS * size;
            let c = at(Vec2::ZERO);
            let arc = list
                .path()
                .move_to(c + Vec2::new(-r, 0.4 * r))
                .cubic_to(c + Vec2::new(-r, -0.9 * r), c + Vec2::new(r, -0.9 * r), c + Vec2::new(r, 0.4 * r))
                .finish();
            stroke(list, arc, NOSTRIL_OPACITY);
            for side in [1.0, -1.0] {
                let dot = at(Vec2::new(BUTTON_DOT.x * side, BUTTON_DOT.y));
                let r = BUTTON_DOT_RADIUS * size;
                let dot = list.path().ellipse(dot, r, r).finish();
                list.push(Cmd::Fill { path: dot, paint: Paint::Solid(colour), opacity: NOSTRIL_OPACITY });
            }
        }
        NoseKind::None | NoseKind::Dot | NoseKind::Tick => {}
    }
}

/// A soft elliptical spot of `colour` fading to nothing at its rim.
fn soft(list: &mut DrawList, centre: Vec2, radii: Vec2, colour: Colour) {
    let stops = Stops::new(&[(0.0, colour), (0.5, scaled(colour, 0.6)), (1.0, scaled(colour, 0.0))]);
    let squash = Affine2::from_translation(centre)
        * Affine2::from_scale(Vec2::new(1.0, radii.y / radii.x))
        * Affine2::from_translation(-centre);
    let disc = list.path().ellipse(centre, radii.x, radii.x).finish();
    list.push(Cmd::PushTransform(squash));
    list.fill(disc, Paint::Radial { centre, radius: radii.x, stops });
    list.push(Cmd::PopTransform);
}
