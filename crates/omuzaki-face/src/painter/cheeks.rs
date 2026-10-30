//! Cheeks: the warm wash over the lower face, the two blush patches (soft or hatched) and the shade under the fringe;
//! the prototype's `blush()` and the strip in `draw()` (`face.js` 380-409). The puffed cheek (`cheek_puff`, which the
//! prototype has no channel for) is drawn over the blush as a shaded rim along the lower outer edge of each patch and a
//! small highlight on its swell, so a pout reads even though the blush it adds is faint.
//!
//! Heights are in the face plane. The prototype writes them in its head frame, whose origin is 0.778 m lower; the
//! layout (`blush_y`) is already in the face plane, and the fringe strip, which the prototype fixes in head-frame
//! metres, is [`HEAD_TO_PLANE`] lower here.

use omuzaki_core::glam::{Affine2, Vec2};
use omuzaki_face_api::{BlushKind, Cap, CheekStyle, Cmd, Colour, FaceLayerId, FaceRect, Join, Paint, Stops, narrow};

use crate::painter::{PaintCtx, fixed_colour, scaled};

/// Radius of the warm wash (the prototype's `0.17`).
const WASH_RADIUS: f32 = 0.17;
/// The wash is centred this far below the blush height.
const WASH_DROP: f32 = 0.005;
/// The wash colour (`rgba(244,166,146)`); only its opacity is a style number.
const WASH_COLOUR: [u8; 3] = [244, 166, 146];
/// Middle stop of a soft patch: opacity relative to the centre.
const MID_STOP_ALPHA: f32 = 0.6;

/// The fringe shade's colour, `rgb(214,150,150)`, and its peak opacity. Fixed in the prototype and not a style number:
/// it is what makes the bangs sit on the forehead, whatever the blush.
const FRINGE_COLOUR: [u8; 3] = [214, 150, 150];
const FRINGE_ALPHA: f32 = 0.30;
/// Where the fringe shade starts to fade out and where it is gone, in the prototype's head frame (`0.86`, `0.80`).
const FRINGE_TOP_HEAD: f32 = 0.86;
const FRINGE_BOTTOM_HEAD: f32 = 0.80;
/// Add to a head-frame height for the face plane (the prototype's eye line, `0.680`, is `-0.098` on the sheet).
const HEAD_TO_PLANE: f32 = -0.778;

/// Segments along the puffed cheek's rim, a half ellipse.
const PUFF_SEGMENTS: usize = 16;
/// The puffed cheek bulges outward: its centre sits this share of the patch's half-width further from the midline.
const PUFF_OUT: f32 = 0.10;
/// The rim's radii, as shares of the patch's: a little taller than the patch, so the rim clears the blush.
const PUFF_RIM: Vec2 = Vec2::new(1.0, 1.1);
/// The rim is the lower half of the ellipse, from this angle (radians, measured from the outer side, downwards)
/// round to its mirror image on the inner side: a little more than half, so its ends turn up.
const PUFF_SWEEP: f32 = 3.4;
/// The soft shade under the rim: its width in metres and its opacity at a full puff.
const PUFF_SHADE_WIDTH: f32 = 0.006;
const PUFF_SHADE_ALPHA: f32 = 0.18;
/// The crisp line on the rim: width in metres and opacity at a full puff. Its colour is the blush's, darkened.
const PUFF_LINE_WIDTH: f32 = 0.0012;
const PUFF_LINE_ALPHA: f32 = 0.45;
/// How much the line's colour is the blush's colour (the rest is darkness).
const PUFF_LINE_DARKEN: f32 = 0.7;
/// The swell's highlight: its centre's place inside the patch (shares of the half-width and half-height, towards the
/// midline and up), its radii (shares of the patch's), and its opacity at a full puff.
const PUFF_HIGHLIGHT_AT: Vec2 = Vec2::new(-0.25, 0.35);
const PUFF_HIGHLIGHT_RADII: Vec2 = Vec2::new(0.30, 0.22);
const PUFF_HIGHLIGHT_ALPHA: f32 = 0.35;
/// The highlight's colour, `rgb(255,244,240)`.
const PUFF_HIGHLIGHT_COLOUR: [u8; 3] = [255, 244, 240];
/// A puff below this paints nothing.
const PUFF_MIN: f32 = 0.005;

/// A hatched patch is cut by this many gaps, so every radius gets the same look at its own scale.
const HATCH_GAPS: u8 = 12;
/// Hatch line width as a share of the gap between lines.
const HATCH_WIDTH: f32 = 0.35;
/// The sweep: each patch moves this far out and up (in patch radii), grows by these factors and turns this far toward
/// the temple (radians).
const SWEEP_OUT: f32 = 0.35;
const SWEEP_UP: f32 = 0.55;
const SWEEP_RADII: Vec2 = Vec2::new(1.2, 0.8);
const SWEEP_TURN: f32 = 0.38;
/// The bridge band's height, as a share of a patch's.
const BRIDGE_HEIGHT: f32 = 0.75;
/// The contour: how far out (patch half-widths) and down (patch half-heights) from the blush, its radii as factors of
/// the patch's, its turn toward the jaw (radians) and its opacity at full strength.
const CONTOUR_OUT: f32 = 1.05;
const CONTOUR_DOWN: f32 = 1.3;
const CONTOUR_RADII: Vec2 = Vec2::new(0.55, 1.5);
const CONTOUR_TURN: f32 = 0.6;
const CONTOUR_ALPHA: f32 = 0.5;
/// Colour used when a palette slot reached the painter unresolved (it should not: the host resolves them).
const FALLBACK_COLOUR: Colour = Colour::rgb(0xf0, 0x8c, 0x84);

/// Paints the wash, the blush patches and the fringe shade on the Base layer.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let rect = ctx.specs[FaceLayerId::Base as usize].rect;
    let style = &ctx.profile.cheeks;
    let layout = &ctx.profile.layout;
    let (blush_y, spacing) = (narrow(layout.blush_y), narrow(layout.blush_spacing));
    wash(ctx, &rect, narrow(style.wash), blush_y);
    contour(ctx, style, spacing, blush_y);
    let channel = narrow(style.strength) * settled(ctx.params.blush);
    // Makeup blush shows whatever the channel says; the channel's blush builds on it.
    let base = narrow(style.base).clamp(0.0, 1.0);
    let strength = channel + base * (1.0 - channel.min(1.0));
    if style.kind == BlushKind::Bridge {
        bridge(ctx, style, strength, spacing, blush_y);
    } else {
        for side in [1.0, -1.0] {
            patch(ctx, style, strength, side, Vec2::new(side * spacing, blush_y));
        }
    }
    let puff = ctx.params.cheek_puff.clamp(0.0, 1.0);
    let puff = if puff < PUFF_DEAD { 0.0 } else { settled(puff) };
    if puff > PUFF_MIN {
        for side in [1.0, -1.0] {
            puffed_cheek(ctx, style, puff, side, Vec2::new(side * spacing, blush_y));
        }
    }
    fringe_shade(ctx, &rect);
}

/// Steps the blush takes: a tracked face's cheek channels jitter by a hundredth or two every frame, which would change
/// the blush by a fraction of a colour level and still repaint both patches, the largest part of a CPU repaint (#2443).
/// In 32nds a step is under 5 levels of the patch's colour at full strength (a soft wash, where that doesn't show), and
/// the noise crosses one far less often.
const BLUSH_STEPS: f32 = 32.0;
/// The puff a tracker reports at rest (its noise) counts as none; above it the puff is in 32nds, as the blush.
const PUFF_DEAD: f32 = 0.04;

/// `value` rounded to [`BLUSH_STEPS`].
fn settled(value: f32) -> f32 {
    (value * BLUSH_STEPS).round() / BLUSH_STEPS
}

/// A colour with `alpha` (clamped to `0..=1`) as its coverage.
fn with_alpha(rgb: [u8; 3], alpha: f32) -> Colour {
    Colour::rgba(rgb[0], rgb[1], rgb[2], omuzaki_core::colour::to_byte(alpha))
}

/// The prototype's `fillRect` of a gradient is clipped to `rect`; a gradient is transparent past its radius, so only
/// the square around the disc needs filling.
fn wash(ctx: &mut PaintCtx<'_>, rect: &FaceRect, opacity: f32, blush_y: f32) {
    if opacity <= 0.0 {
        return;
    }
    let centre = Vec2::new(0.0, blush_y - WASH_DROP);
    let stops = Stops::new(&[(0.0, with_alpha(WASH_COLOUR, opacity)), (1.0, with_alpha(WASH_COLOUR, 0.0))]);
    let (lo, hi) = (centre - Vec2::splat(WASH_RADIUS), centre + Vec2::splat(WASH_RADIUS));
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&corners(rect, lo, hi)).close().finish();
    list.fill(path, Paint::Radial { centre, radius: WASH_RADIUS, stops });
}

/// The four corners of the box `lo..hi` clipped to `rect`.
fn corners(rect: &FaceRect, lo: Vec2, hi: Vec2) -> [Vec2; 4] {
    let (x0, x1) = (lo.x.max(rect.x0), hi.x.min(rect.x1));
    let (y0, y1) = (lo.y.max(rect.y0), hi.y.min(rect.y1));
    [Vec2::new(x0, y0), Vec2::new(x1, y0), Vec2::new(x1, y1), Vec2::new(x0, y1)]
}

/// One blush patch centred on `centre`, in the style's kind; `side` is `+1` on the character's left.
fn patch(ctx: &mut PaintCtx<'_>, style: &CheekStyle, strength: f32, side: f32, centre: Vec2) {
    let colour = fixed_colour(&style.colour, FALLBACK_COLOUR);
    // Coverage saturates first (the prototype's `rgba()` clamps alpha at 1), then the colour's own alpha scales it, so
    // a translucent colour never peaks above its alpha and a transparent one paints nothing.
    let alpha = colour.alpha();
    if strength.min(1.0) * alpha <= 0.0 {
        return;
    }
    let rgb = [colour.r, colour.g, colour.b];
    let radii = Vec2::new(narrow(style.radius[0]), narrow(style.radius[1]));
    match style.kind {
        BlushKind::Soft | BlushKind::Bridge => soft_patch(ctx, rgb, strength, alpha, centre, radii, 0.0),
        BlushKind::Hatched => hatched_patch(ctx, rgb, strength.min(1.0) * alpha, centre, radii),
        BlushKind::Sweep => {
            let at = centre + Vec2::new(side * SWEEP_OUT * radii.x, SWEEP_UP * radii.y);
            soft_patch(ctx, rgb, strength, alpha, at, radii * SWEEP_RADII, side * SWEEP_TURN);
        }
    }
}

/// The bridge blush: one soft band across the nose from one patch's outer edge to the other's.
fn bridge(ctx: &mut PaintCtx<'_>, style: &CheekStyle, strength: f32, spacing: f32, blush_y: f32) {
    let colour = fixed_colour(&style.colour, FALLBACK_COLOUR);
    if strength.min(1.0) * colour.alpha() <= 0.0 {
        return;
    }
    let radii = Vec2::new(spacing + narrow(style.radius[0]), narrow(style.radius[1]) * BRIDGE_HEIGHT);
    soft_patch(ctx, [colour.r, colour.g, colour.b], strength, colour.alpha(), Vec2::new(0.0, blush_y), radii, 0.0);
}

/// Contour: a soft shade under each cheekbone, leaning toward the jaw, in the contour colour.
fn contour(ctx: &mut PaintCtx<'_>, style: &CheekStyle, spacing: f32, blush_y: f32) {
    let strength = narrow(style.contour).clamp(0.0, 1.0);
    let colour = fixed_colour(&style.contour_colour, FALLBACK_COLOUR);
    if strength * colour.alpha() <= 0.0 {
        return;
    }
    let radii = Vec2::new(narrow(style.radius[0]), narrow(style.radius[1]));
    for side in [1.0, -1.0] {
        let at = Vec2::new(side * (spacing + CONTOUR_OUT * radii.x), blush_y - CONTOUR_DOWN * radii.y);
        let rgb = [colour.r, colour.g, colour.b];
        soft_patch(ctx, rgb, strength * CONTOUR_ALPHA, colour.alpha(), at, radii * CONTOUR_RADII, -side * CONTOUR_TURN);
    }
}

/// A radial gradient on a disc of the patch's half-width, squashed to the patch's half-height by a transform, exactly
/// as the prototype does: the soft edge stays round-symmetric whatever the aspect. `turn` turns it counter-clockwise
/// (radians; the prototype's patches have none).
fn soft_patch(ctx: &mut PaintCtx<'_>, rgb: [u8; 3], strength: f32, alpha: f32, centre: Vec2, radii: Vec2, turn: f32) {
    // Each stop saturates on its own, as the prototype's per-stop `rgba()` does, before the colour's alpha.
    let stop = |coverage: f32| with_alpha(rgb, coverage.min(1.0) * alpha);
    let stops =
        Stops::new(&[(0.0, stop(strength)), (0.5, stop(strength * MID_STOP_ALPHA)), (1.0, with_alpha(rgb, 0.0))]);
    let squash = Affine2::from_translation(centre)
        * Affine2::from_angle(turn)
        * Affine2::from_scale(Vec2::new(1.0, radii.y / radii.x))
        * Affine2::from_translation(-centre);
    let list = ctx.list(FaceLayerId::Base);
    let disc = list.path().ellipse(centre, radii.x, radii.x).finish();
    list.push(Cmd::PushTransform(squash));
    list.fill(disc, Paint::Radial { centre, radius: radii.x, stops });
    list.push(Cmd::PopTransform);
}

/// Manga hatching: parallel diagonal lines clipped to the patch's ellipse. The lines run along `(ry, rx)`, steeper
/// the flatter the patch; the gap is the patch's extent across the lines over [`HATCH_GAPS`], so a patch of any size
/// gets the same number of lines.
fn hatched_patch(ctx: &mut PaintCtx<'_>, rgb: [u8; 3], alpha: f32, centre: Vec2, radii: Vec2) {
    let dir = Vec2::new(radii.y, radii.x).normalize_or_zero();
    let across = dir.perp();
    // Half the ellipse's width measured along `across`.
    let half = Vec2::new(radii.x * across.x, radii.y * across.y).length();
    let gap = 2.0 * half / f32::from(HATCH_GAPS);
    let reach = radii.x.max(radii.y);
    let colour = with_alpha(rgb, alpha);
    let list = ctx.list(FaceLayerId::Base);
    let outline = list.path().ellipse(centre, radii.x, radii.y).finish();
    list.push(Cmd::PushClip(outline));
    for i in 0..=HATCH_GAPS {
        let offset = centre + across * (-half + gap * f32::from(i));
        let line = list.path().move_to(offset - dir * reach).line_to(offset + dir * reach).finish();
        list.push(Cmd::Stroke {
            path: line,
            paint: Paint::Solid(colour),
            width: gap * HATCH_WIDTH,
            cap: Cap::Butt,
            join: Join::Miter,
            opacity: 1.0,
        });
    }
    list.push(Cmd::PopClip);
}

/// One puffed cheek: a shaded rim along the lower outer edge of the patch at `centre` and a highlight on the swell,
/// both scaled by `puff` (`0..=1`), so a half pout is half as strong everywhere and nothing pops in.
fn puffed_cheek(ctx: &mut PaintCtx<'_>, style: &CheekStyle, puff: f32, side: f32, centre: Vec2) {
    let radii = Vec2::new(narrow(style.radius[0]), narrow(style.radius[1]));
    let blush = fixed_colour(&style.colour, FALLBACK_COLOUR);
    let rim_centre = centre + Vec2::new(side * PUFF_OUT * radii.x, 0.0);
    let rim_radii = radii * PUFF_RIM;
    // The lower half (and a little more) of the ellipse, from the outer side round the bottom to the inner side.
    let mut rim = [Vec2::ZERO; PUFF_SEGMENTS + 1];
    for (i, p) in rim.iter_mut().enumerate() {
        // Indices are at most `PUFF_SEGMENTS`, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let angle = -0.2 - PUFF_SWEEP * (i as f32 / PUFF_SEGMENTS as f32);
        *p = rim_centre + Vec2::new(side * angle.cos() * rim_radii.x, angle.sin() * rim_radii.y);
    }
    let darker = |c: u8| (f32::from(c) * PUFF_LINE_DARKEN).round().clamp(0.0, 255.0);
    // Between 0 and 255 by construction.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let line = Colour::rgba(darker(blush.r) as u8, darker(blush.g) as u8, darker(blush.b) as u8, blush.a);
    let shade = with_alpha(FRINGE_COLOUR, PUFF_SHADE_ALPHA * puff);
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(&rim).finish();
    list.stroke(path, Paint::Solid(shade), PUFF_SHADE_WIDTH);
    list.stroke(path, Paint::Solid(scaled(line, PUFF_LINE_ALPHA * puff)), PUFF_LINE_WIDTH);
    let at = centre + Vec2::new(side * PUFF_HIGHLIGHT_AT.x * radii.x, PUFF_HIGHLIGHT_AT.y * radii.y);
    let spot = list.path().ellipse(at, PUFF_HIGHLIGHT_RADII.x * radii.x, PUFF_HIGHLIGHT_RADII.y * radii.y).finish();
    list.fill(spot, Paint::Solid(with_alpha(PUFF_HIGHLIGHT_COLOUR, PUFF_HIGHLIGHT_ALPHA * puff)));
}

/// The warm strip under the fringe: full across the face, from the top of the rect down to a gradient that is gone
/// at [`FRINGE_BOTTOM_HEAD`].
fn fringe_shade(ctx: &mut PaintCtx<'_>, rect: &FaceRect) {
    let (top, bottom) = (FRINGE_TOP_HEAD + HEAD_TO_PLANE, FRINGE_BOTTOM_HEAD + HEAD_TO_PLANE);
    let stops = Stops::new(&[(0.0, with_alpha(FRINGE_COLOUR, FRINGE_ALPHA)), (1.0, with_alpha(FRINGE_COLOUR, 0.0))]);
    let list = ctx.list(FaceLayerId::Base);
    let path =
        list.path().polyline(&corners(rect, Vec2::new(rect.x0, bottom), Vec2::new(rect.x1, rect.y1))).close().finish();
    list.fill(path, Paint::Linear { from: Vec2::new(0.0, top), to: Vec2::new(0.0, bottom), stops });
}
