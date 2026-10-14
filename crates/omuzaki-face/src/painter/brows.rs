//! Brows, on both the Base layer and the half-density Brows layer: the prototype's `brow()` (`face.js` 264-277), one
//! filled polygon per brow.
//!
//! The Brows layer is drawn with the same geometry as the Base layer, command for command, so hair can composite it
//! over the bangs and the brows read through them (CB-21: brows are always drawn over the hair). Only the raster
//! density differs, and that is the layer's business, not this section's.

use std::f32::consts::PI;

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{BrowParams, BrowStyle, Colour, ColourRef, FaceLayerId, FaceRect, Paint, narrow};

use crate::painter::PaintCtx;

/// Segments along each brow (the prototype's `N`).
const SEGMENTS: usize = 22;
/// Stations along one edge of a brow: the top and the bottom outline share them.
const STATIONS: usize = SEGMENTS + 1;
/// Points in a brow's closed outline: the top edge, then the bottom edge back to the inner end.
const OUTLINE: usize = 2 * STATIONS;

/// Height change per unit of [`BrowParams::height`].
const HEIGHT_STEP: f32 = 0.012;
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
        let colour = match &brows.colour {
            ColourRef::Fixed(c) => *c,
            ColourRef::Slot(_) => FALLBACK_COLOUR,
        };
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
    let base = brow_y + brow.height * HEIGHT_STEP;
    let mut points = [Vec2::ZERO; OUTLINE];
    for i in 0..STATIONS {
        let t = station(i);
        let x = side * (style.inner + (style.outer - style.inner) * t);
        let y = base + style.arch * (PI * t).sin() * (1.0 + arch) + RISE * t + brow.tilt * (t - 0.5) * TILT_STEP;
        let half = half_thickness(style.thickness, t);
        points[i] = Vec2::new(x, y + half).clamp(lo, hi);
        points[OUTLINE - 1 - i] = Vec2::new(x, y - half).clamp(lo, hi);
    }
    points
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
