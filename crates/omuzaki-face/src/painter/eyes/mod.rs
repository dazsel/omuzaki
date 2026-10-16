//! Eyes: both eyes, drawn the way the prototype's `eye()` did (left, then right), one piece after another.
//!
//! Each piece is a function of the section context and the side, and draws nothing when it does not apply (the shut-eye
//! pieces only for a shut eye, the open-eye pieces only for an open one). Pieces recompute what they share from the
//! paint parameters (the aperture is a pure function: [`aperture::Eye`]), so they never depend on each other's
//! signatures.
//!
//! # The clip
//!
//! The prototype clips the eye white, the iris and the highlights to the aperture (`g.save(); g.clip(path)` ... `g.restore()`).
//! This module owns that bracket, so the pieces inside it just draw: [`iris`] and [`highlight`] run between a `PushClip`
//! of the aperture outline and its `PopClip`, and only for an open eye. The eye white is filled with the aperture
//! itself just before the clip starts, so its edge is anti-aliased once (clipping to a path and then filling the same
//! path weights the edge pixels twice, which the prototype's canvas does not). Everything else (lid line, crease, lash,
//! closed eye, effects) draws outside the bracket.

mod aperture;
mod closed;
mod crease;
mod effects;
mod highlight;
mod iris;
mod lash;
mod lid;
mod sclera;

use omuzaki_core::colour::Colour;
use omuzaki_face_api::{Cmd, FaceLayerId};

use crate::painter::PaintCtx;
use aperture::Eye;

/// Which eye. The left eye is the character's left (+x), as everywhere in the face contract.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Side {
    /// The character's left (+x).
    Left,
    /// The character's right (-x).
    Right,
}

/// The colour an eye piece paints for a palette slot nobody resolved: mid-grey, so the stage's bug shows in the picture.
const FALLBACK_COLOUR: Colour = Colour::rgb(128, 128, 128);

/// Paints both eyes.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    for side in [Side::Left, Side::Right] {
        let eye = Eye::new(ctx.profile, ctx.params, side);
        // The open pieces start here; a squeeze that is shutting the eye fades them all together (see `open_fade`).
        let mark = ctx.list(FaceLayerId::Base).cmds().len();
        if eye.is_open() {
            let outline = aperture::outline(ctx.list(FaceLayerId::Base), &eye);
            sclera::paint(ctx, &eye, outline);
            ctx.list(FaceLayerId::Base).push(Cmd::PushClip(outline));
            iris::paint(ctx, side);
            effects::paint_iris(ctx, side);
            highlight::paint(ctx, side);
            ctx.list(FaceLayerId::Base).push(Cmd::PopClip);
        }
        lid::paint(ctx, side);
        crease::paint(ctx, side);
        lash::paint(ctx, side);
        let fade = eye.open_fade(ctx.params.effects.squeeze);
        if fade < 1.0 {
            ctx.list(FaceLayerId::Base).scale_opacity_from(mark, fade);
        }
        closed::paint(ctx, side);
        effects::paint(ctx, side);
    }
}
