//! The eye white and the pink-lilac shade under the lid (the `eye()` prologue, `face.js` lines 126-132).

use omuzaki_face_api::{Cmd, FaceLayerId, Paint, PathRef, Stops};

use super::aperture::Eye;
use super::fixed_colour;
use crate::painter::PaintCtx;

/// Fills the aperture `outline` with the three-stop eye white: `sclera.top` under the upper lid, `mid` half way,
/// `bottom` at the lower lid. The gradient runs straight down from the upper lid to the lower lid at the eye's centre
/// (as the prototype's does), so a tilted lid still shades from where the lid is. Draws one command, before the
/// caller's clip to `outline` starts.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, eye: &Eye, outline: PathRef) {
    let style = &ctx.profile.eyes.sclera;
    let stops = Stops::new(&[
        (0.0, fixed_colour(&style.top)),
        (0.5, fixed_colour(&style.mid)),
        (1.0, fixed_colour(&style.bottom)),
    ]);
    let paint = Paint::Linear { from: eye.point(0.0, eye.top(0.0)), to: eye.point(0.0, eye.bottom(0.0)), stops };
    ctx.list(FaceLayerId::Base).push(Cmd::Fill { path: outline, paint, opacity: 1.0 });
}
