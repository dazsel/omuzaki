//! [`RealPainter`]: the production [`FacePainter`], dispatching to the sections.

use omuzaki_face_api::{FaceDrawing, FaceError, FaceInput, FaceLayerId, FaceLayerSpec, FacePainter, FaceProjection};

use super::{MouthPiece, PaintCtx, Section};

/// The production face painter. Pure and deterministic: the same [`FaceInput`] always fills the same lists.
#[derive(Clone, Debug)]
pub struct RealPainter {
    layers: [FaceLayerSpec; FaceLayerId::COUNT],
}

impl RealPainter {
    /// A painter for a face projected onto `projection`, with the base layer at `base_px_per_m` (see
    /// [`DEFAULT_PX_PER_M`](omuzaki_face_api::DEFAULT_PX_PER_M)).
    pub fn new(projection: FaceProjection, base_px_per_m: f32) -> Self {
        Self { layers: FaceLayerId::ALL.map(|id| FaceLayerSpec::for_layer(id, &projection, base_px_per_m)) }
    }

    /// Paints one section alone into `out` (cleared first), for tools: parity tests, sweeps and contact sheets.
    ///
    /// # Errors
    /// As [`FacePainter::paint`].
    pub fn paint_section(
        &mut self,
        section: Section,
        input: &FaceInput<'_>,
        out: &mut FaceDrawing,
    ) -> Result<(), FaceError> {
        self.run(input, out, Some(section));
        Ok(())
    }

    /// Paints one piece of the mouth alone into `out` (cleared first): the shape without the interior, rim or tongue,
    /// so each piece can be compared with its own part of the prototype.
    ///
    /// # Errors
    /// As [`FacePainter::paint`].
    pub fn paint_mouth_piece(
        &mut self,
        piece: MouthPiece,
        input: &FaceInput<'_>,
        out: &mut FaceDrawing,
    ) -> Result<(), FaceError> {
        out.clear();
        let mut ctx = PaintCtx::new(input, &self.layers, out, Some(Section::Mouth));
        ctx.mouth_only = Some(piece);
        Section::Mouth.paint(&mut ctx);
        Ok(())
    }

    fn run(&self, input: &FaceInput<'_>, out: &mut FaceDrawing, only: Option<Section>) {
        out.clear();
        let mut ctx = PaintCtx::new(input, &self.layers, out, only);
        for section in Section::ALL {
            if ctx.runs(section) {
                section.paint(&mut ctx);
            }
        }
    }
}

impl FacePainter for RealPainter {
    fn layers(&self) -> &[FaceLayerSpec] {
        &self.layers
    }

    fn paint(&mut self, input: &FaceInput<'_>, out: &mut FaceDrawing) -> Result<(), FaceError> {
        self.run(input, out, None);
        Ok(())
    }
}
