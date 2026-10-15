//! The mouth: lip gloss, shape, interior, lip rim and tongue.
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
    /// The inside of an open mouth: tongue, teeth and fangs, clipped to the open shape.
    Interior,
    /// The glossy lower lip under the mouth.
    Gloss,
    /// The rim round an open mouth.
    Rim,
    /// The tongue poking out.
    Tongue,
}

/// Paints the mouth, or only the piece the context asks for.
///
/// The prototype's order: the lower-lip gloss, the shape, the interior (clipped to the shape), the rim, the tongue. The
/// open shape's outline is recorded first, without painting, because the gloss is sized by it and a piece asked for
/// alone still needs it to clip against.
pub(super) fn paint(ctx: &mut PaintCtx<'_>) {
    let only = ctx.mouth_only;
    let runs = |piece| only.is_none_or(|only| only == piece);
    shape::record_outline(ctx);
    if runs(MouthPiece::Gloss) {
        lips::paint_gloss(ctx);
    }
    if runs(MouthPiece::Shape) {
        shape::paint(ctx);
    }
    if runs(MouthPiece::Interior) {
        interior::paint(ctx);
    }
    if runs(MouthPiece::Rim) {
        lips::paint_rim(ctx);
    }
    if runs(MouthPiece::Tongue) {
        tongue::paint(ctx);
    }
}
