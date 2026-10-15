//! The tongue poking out under an open mouth (`face.js` 369-379): a rounded tab with a centre groove and a shine.
//!
//! The tab starts just above the open shape's lowest edge and hangs below it, never above, growing with the `tongue`
//! channel. Not for a closed mouth or a kiss. Colours come from the resting tongue's colour in the profile and the lip
//! tint: the tab is a touch lighter than the tongue, outlined in the rim's dark.

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{FaceLayerId, Paint};

use super::lips::{DARKEN, DEFAULT_TONGUE, darken, lighten, resolve, solid, tint};
use crate::painter::PaintCtx;

/// Shows from this strength.
const MIN: f32 = 0.01;
/// The tab is at most this share of the mouth's half-width wide, and never wider than [`MAX_HALF_WIDTH`] metres times
/// the performance scale.
const WIDTH_SHARE: f32 = 0.62;
const MAX_HALF_WIDTH: f32 = 0.0052;
/// The tab's height in metres: [`HEIGHT`] plus [`HEIGHT_PER_UNIT`] per unit of tongue, times the performance scale.
const HEIGHT: f32 = 0.0028;
const HEIGHT_PER_UNIT: f32 = 0.0042;
/// The curve's control points hang this many tab heights below its start, so the belly is round.
const BELLY: f32 = 1.25;
/// The tab starts this far above the open shape's lowest point, in metres.
const ABOVE_EDGE: f32 = 0.0012;
/// The tab's colour: the tongue's, this share of the way to white.
const LIGHTER: f32 = 0.12;
/// The outline: the rim's dark tint at this alpha, this wide.
const OUTLINE_ALPHA: f32 = 0.8;
const OUTLINE_WIDTH: f32 = 0.0010;
/// The centre groove: the tongue colour darkened by this share per channel, at this alpha, this wide, from just under
/// the start to [`GROOVE_TO`] of the tab's height.
const GROOVE_DARKEN: f32 = 0.72;
const GROOVE_ALPHA: f32 = 0.55;
const GROOVE_WIDTH: f32 = 0.0008;
const GROOVE_FROM: f32 = 0.0006;
const GROOVE_TO: f32 = 0.55;
/// The shine, left of the groove: the tongue colour this share of the way to white, at this alpha, sized as shares of
/// the tab.
const SHINE_WHITE: f32 = 0.75;
const SHINE_ALPHA: f32 = 0.6;
const SHINE_X: f32 = -0.35;
const SHINE_Y: f32 = -0.1;
const SHINE_RX: f32 = 0.22;
const SHINE_RY: f32 = 0.18;

/// Paints the tongue tab, if the tongue is out.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let strength = ctx.params.mouth.tongue;
    let Some(open) = ctx.open_mouth else { return };
    if strength <= MIN {
        return;
    }
    let y0 = open.centre.y + open.bottom(0.0) + ABOVE_EDGE;
    let x = open.centre.x;
    let half = (open.half_width * WIDTH_SHARE).min(MAX_HALF_WIDTH * open.grow);
    let height = (HEIGHT + HEIGHT_PER_UNIT * strength) * open.grow;
    let groove_to = y0 - height * GROOVE_TO;
    let base = resolve(&ctx.profile.mouth.interior.tongue, DEFAULT_TONGUE);
    let outline = darken(tint(ctx), DARKEN);

    let list = ctx.list(FaceLayerId::Base);
    let belly = y0 - height * BELLY;
    // One open path serves both: the fill closes it, the outline leaves the top edge bare, as the canvas does.
    let tab = list
        .path()
        .move_to(Vec2::new(x - half, y0))
        .cubic_to(Vec2::new(x - half, belly), Vec2::new(x + half, belly), Vec2::new(x + half, y0))
        .finish();
    list.fill(tab, Paint::Solid(lighten(base, LIGHTER)));
    list.stroke(tab, solid(outline, OUTLINE_ALPHA), OUTLINE_WIDTH);
    let groove = list.path().move_to(Vec2::new(x, y0 - GROOVE_FROM)).line_to(Vec2::new(x, groove_to)).finish();
    list.stroke(groove, solid(darken(base, [GROOVE_DARKEN; 3]), GROOVE_ALPHA), GROOVE_WIDTH);
    let shine_at = Vec2::new(x + half * SHINE_X, groove_to + height * SHINE_Y);
    let shine = list.path().ellipse(shine_at, half * SHINE_RX, height * SHINE_RY).finish();
    list.fill(shine, solid(lighten(base, SHINE_WHITE), SHINE_ALPHA));
}
