//! Brows, on both the Base layer and the half-density Brows layer: the prototype's `brow()` (`face.js` 264-277), one
//! filled polygon per brow.
//!
//! The Brows layer is drawn with the same geometry as the Base layer, command for command, so hair can composite it
//! over the bangs and the brows read through them (CB-21: brows are always drawn over the hair). Only the raster
//! density differs, and that is the layer's business, not this section's.

use std::f32::consts::PI;

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{BrowParams, BrowStyle, Colour, FaceLayerId, FaceRect, Paint, narrow};

use crate::painter::{PaintCtx, fixed_colour};

/// Segments along each brow (the prototype's `N`).
const SEGMENTS: usize = 22;
/// Stations along one edge of a brow: the top and the bottom outline share them.
const STATIONS: usize = SEGMENTS + 1;
/// Points in a brow's closed outline: the top edge, then the bottom edge back to the inner end.
const OUTLINE: usize = 2 * STATIONS;

/// Height change per unit of [`BrowParams::height`] at the inner end, the middle and the outer end, in metres, for a
/// raise and for a lower. A brow does not move rigidly: the inner end leads (about 4x the outer end on a raise, about
/// 9x on a lower) and the outer end hardly moves. Measured on the CC0 reference rig the Director published on #1863
/// (comment 6197276885: 10.7 / 7.8 / 2.6 % and 18.2 / 16.9 / 2.1 % of the eye-to-chin height) and scaled to this
/// face (0.17 m); the prototype's own travel is a uniform 0.012. A height of 0 is the prototype's brow either way.
const HEIGHT_RAISE: [f32; 3] = [0.018, 0.013, 0.004];
/// See [`HEIGHT_RAISE`].
const HEIGHT_LOWER: [f32; 3] = [0.031, 0.029, 0.004];
/// The arch grows by this much at a height of +1 (the reference measures 9.1 % of the brow's length at rest and 11.3 %
/// raised) and is gone at -1 (it measures 0.0 % lowered): the brow flattens as it comes down.
const ARCH_RAISED: f32 = 0.25;
/// Tilt change across the brow's length, per unit of [`BrowParams::tilt`].
const TILT_STEP: f32 = 0.016;
/// Slight rise along the brow, so the outer end sits a little higher than the inner.
const RISE: f32 = 0.001;
/// Half-thickness at the two ends. The middle swells to [`BrowStyle::thickness`].
const TIP_HALF_THICKNESS: f32 = 0.0006;
/// The colour the prototype's `brow` uses (`rgba(168,112,96,0.85)`), the default of [`BrowStyle`]. It is only the
/// fallback for a palette slot that was never resolved, which a painter should never see.
const FALLBACK_COLOUR: Colour = Colour::rgba(168, 112, 96, 217);

/// Paints both brows on the Base and the Brows layers.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let style = Style::new(&ctx.profile.brows);
    let rect = ctx.specs[FaceLayerId::Base as usize].rect;
    // The profile's gap above the eye line; the prototype's is 0.762 - 0.680 = 0.082 (brow height minus `eyeY`, in its
    // head frame, while the profile's `eye_y` is in the face plane).
    let brow_y = narrow(ctx.profile.layout.eye_y + ctx.profile.layout.brow_gap);
    let arch = ctx.params.brow_arch;
    let brows = [(1.0, ctx.params.left_brow), (-1.0, ctx.params.right_brow)];
    for (side, params) in brows {
        let outline = outline(&style, &rect, brow_y, arch, side, params);
        for layer in [FaceLayerId::Base, FaceLayerId::Brows] {
            let path = ctx.list(layer).path().polyline(&outline).close().finish();
            ctx.list(layer).fill(path, Paint::Solid(style.colour));
        }
    }
}

/// The numbers of [`BrowStyle`] the outline needs, as `f32` and resolved to a colour.
struct Style {
    inner: f32,
    outer: f32,
    thickness: f32,
    arch: f32,
    colour: Colour,
}

impl Style {
    fn new(brows: &BrowStyle) -> Self {
        let colour = fixed_colour(&brows.colour, FALLBACK_COLOUR);
        Self {
            inner: narrow(brows.inner_x),
            outer: narrow(brows.outer_x),
            thickness: narrow(brows.thickness),
            arch: narrow(brows.arch),
            colour,
        }
    }
}

/// One brow's closed outline in face-plane metres: the top edge from the inner end to the outer end, then the bottom
/// edge back. Every point is clamped into `rect`, so a brow stays on the face whatever its style and parameters.
fn outline(style: &Style, rect: &FaceRect, brow_y: f32, arch: f32, side: f32, brow: BrowParams) -> [Vec2; OUTLINE] {
    let lo = Vec2::new(rect.x0, rect.y0);
    let hi = Vec2::new(rect.x1, rect.y1);
    let arch_scale = arch_scale(brow.height);
    let mut points = [Vec2::ZERO; OUTLINE];
    for i in 0..STATIONS {
        let t = station(i);
        let x = side * (style.inner + (style.outer - style.inner) * t);
        let y = brow_y
            + height_shift(brow.height, t)
            + style.arch * arch_scale * (PI * t).sin() * (1.0 + arch)
            + RISE * t
            + brow.tilt * (t - 0.5) * TILT_STEP;
        let half = half_thickness(style.thickness, t);
        points[i] = Vec2::new(x, y + half).clamp(lo, hi);
        points[OUTLINE - 1 - i] = Vec2::new(x, y - half).clamp(lo, hi);
    }
    points
}

/// How far a brow of `height` has moved at `t` along it, in metres: the height times the travel per unit there, which
/// runs from the inner end through the middle to the outer end as the parabola through the three measured values.
fn height_shift(height: f32, t: f32) -> f32 {
    let [inner, middle, outer] = if height >= 0.0 { HEIGHT_RAISE } else { HEIGHT_LOWER };
    let bend = 4.0 * (middle - inner) - (outer - inner);
    let curve = (outer - inner) - bend;
    height * (inner + bend * t + curve * t * t)
}

/// The arch's scale for a brow of `height`: 1 at rest, [`ARCH_RAISED`] more raised by one unit, none left lowered by
/// one unit, and held at those beyond.
fn arch_scale(height: f32) -> f32 {
    if height >= 0.0 { 1.0 + ARCH_RAISED * height.min(1.0) } else { (1.0 + height).max(0.0) }
}

/// The position along the brow of station `i`, `0..=1`.
fn station(i: usize) -> f32 {
    // Station indices are a few dozen at most, exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let (i, n) = (i as f32, SEGMENTS as f32);
    i / n
}

/// Half the brow's thickness at `t`: the tips taper to [`TIP_HALF_THICKNESS`] and the middle swells to `thickness`.
/// The prototype's profile, `0.0006 + 0.0012 * sin^0.7`, is this with `thickness` at its default `0.0018`. A thickness
/// below the tip size stays a hairline at both ends.
fn half_thickness(thickness: f32, t: f32) -> f32 {
    let tip = TIP_HALF_THICKNESS.min(thickness);
    let swell = (PI * (t * 0.95 + 0.06).min(1.0)).sin().max(0.0).powf(0.7);
    tip + (thickness - tip) * swell
}
