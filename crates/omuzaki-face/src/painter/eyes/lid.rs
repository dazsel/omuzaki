//! The lower lid and the smiling lids (the `eye()` body of `face.js`, lines 188-210): drawn over the eye white, for an
//! open eye only.
//!
//! - **Lower lid**: a fine warm line along the outer half of the lower lid (`u` 0.05 to 1.03), as strong as
//!   [`LowerLidStyle::line`].
//! - **Smiling lids**: when the style has [`LowerLidStyle::cheek_line`] and the cheek raises the lower lid, a full curved
//!   line along the lid (inner to middle) with a soft light rim just under it. Both fade in as the lower lid rises
//!   from 0.15 to 0.45, so the lid does not pop.
//!
//! [`LowerLidStyle::line`]: omuzaki_face_api::LowerLidStyle::line
//! [`LowerLidStyle::cheek_line`]: omuzaki_face_api::LowerLidStyle::cheek_line

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Colour, ColourExt, FaceLayerId, Paint, narrow};

use super::aperture::Eye;
use super::{FALLBACK_COLOUR, Side};
use crate::painter::PaintCtx;
use crate::painter::fixed_colour;

/// Points on each of the lines (`i = 0..=24`).
const POINTS: usize = 25;
/// The lower lid line: the outer half, `u` from [`LINE_FROM`] over [`LINE_SPAN`]; 1.1 mm wide.
const LINE_FROM: f32 = 0.05;
const LINE_SPAN: f32 = 0.98;
const LINE_WIDTH: f32 = 0.0011;
/// The smiling lids need a raised lower lid above this, and reach full strength [`SMILE_RANGE`] higher.
const SMILE_FROM: f32 = 0.15;
const SMILE_RANGE: f32 = 0.30;
/// The smiling lid's line: `u` from -0.8 over 0.88 (to 0.08), at 0.6 of the fade, 1.2 mm wide.
const SMILE_LINE_FROM: f32 = -0.8;
const SMILE_LINE_SPAN: f32 = 0.88;
const SMILE_LINE_ALPHA: f32 = 0.6;
const SMILE_LINE_WIDTH: f32 = 0.0012;
/// The soft rim under it: `u` from -0.7 over 1.6 (to 0.9), `rgba(255,236,232)` at 0.55 of the fade, 1.6 mm wide, 0.09
/// eye units below the lid.
const RIM_FROM: f32 = -0.7;
const RIM_SPAN: f32 = 1.6;
const RIM_COLOUR: Colour = Colour::rgb(255, 236, 232);
const RIM_ALPHA: f32 = 0.55;
const RIM_WIDTH: f32 = 0.0016;
const RIM_BELOW: f32 = 0.09;

/// Paints the lower lid and, when the style and the cheek call for it, the smiling lid, of one open eye.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    if !eye.is_open() {
        return;
    }
    let style = &ctx.profile.eyes.lower_lid;
    let colour = fixed_colour(&style.colour, FALLBACK_COLOUR);
    let line_alpha = narrow(style.line);
    if line_alpha > 0.0 {
        let points = along(&eye, LINE_FROM, LINE_SPAN, 0.0);
        stroke(ctx, &points, colour.faded(line_alpha), LINE_WIDTH);
    }
    let strength = ((eye.lower_lid() - SMILE_FROM) / SMILE_RANGE).clamp(0.0, 1.0);
    if style.cheek_line && eye.lower_lid() > SMILE_FROM && strength > 0.0 {
        let line = along(&eye, SMILE_LINE_FROM, SMILE_LINE_SPAN, 0.0);
        stroke(ctx, &line, colour.faded(SMILE_LINE_ALPHA * strength), SMILE_LINE_WIDTH);
        let rim = along(&eye, RIM_FROM, RIM_SPAN, -RIM_BELOW);
        stroke(ctx, &rim, RIM_COLOUR.faded(RIM_ALPHA * strength), RIM_WIDTH);
    }
}

/// The lower lid at [`POINTS`] steps from `from` over `span` (eye units), shifted by `shift` eye units.
fn along(eye: &Eye, from: f32, span: f32, shift: f32) -> [Vec2; POINTS] {
    let mut points = [Vec2::ZERO; POINTS];
    for (i, p) in points.iter_mut().enumerate() {
        // `i` is at most 24, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let u = from + span * i as f32 / (POINTS - 1) as f32;
        *p = eye.point(u, eye.bottom(u) + shift);
    }
    points
}

fn stroke(ctx: &mut PaintCtx<'_>, points: &[Vec2], colour: Colour, width: f32) {
    let list = ctx.list(FaceLayerId::Base);
    let path = list.path().polyline(points).finish();
    list.stroke(path, Paint::Solid(colour), width);
}
