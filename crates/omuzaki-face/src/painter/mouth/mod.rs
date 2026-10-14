//! The mouth: shape, then interior, lip rim and gloss, and tongue.
//!
//! Pieces are functions of the section context and draw nothing when they do not apply. They recompute what they share
//! from the paint parameters; the one thing only the shape can hand on, the open mouth's outline in the draw list, is
//! left in [`PaintCtx::open_mouth`] for the pieces that follow it.

mod interior;
mod lips;
mod shape;
mod tongue;

pub use shape::OpenMouth;

use crate::painter::PaintCtx;

/// One piece of the mouth, for tools that look at a piece alone (parity tests, contact sheets): see
/// [`RealPainter::paint_mouth_piece`](crate::RealPainter::paint_mouth_piece).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MouthPiece {
    /// The kiss, the closed line or the open shape with its fill.
    Shape,
    /// The inside of an open mouth: tongue, teeth and fangs.
    Interior,
    /// The lip rim and the lower-lip gloss.
    Lips,
    /// The tongue poking out.
    Tongue,
}

/// Paints the mouth, or only the piece the context asks for.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let only = ctx.mouth_only;
    let runs = |piece| only.is_none_or(|only| only == piece);
    if runs(MouthPiece::Shape) {
        shape::paint(ctx);
    }
    if runs(MouthPiece::Interior) {
        interior::paint(ctx);
    }
    if runs(MouthPiece::Lips) {
        lips::paint(ctx);
    }
    if runs(MouthPiece::Tongue) {
        tongue::paint(ctx);
    }
}
