//! [`RealPainter`]: the production [`FacePainter`], dispatching to the sections.

use std::ops::Range;

use omuzaki_face_api::{FaceDrawing, FaceError, FaceInput, FaceLayerId, FaceLayerSpec, FacePainter, FaceProjection};

use super::{MouthPiece, PaintCtx, Section};

/// The command range each section wrote into one layer's draw list, in [`Section::ALL`] order. A section that wrote
/// nothing there has an empty range, and the ranges that are not empty tile the list in order.
pub type LayerSections = [Range<usize>; Section::ALL.len()];

/// Where every section's commands sit in every layer's draw list, as of the last paint (Q2 of #1433). A fixed-size array,
/// so recording it allocates nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SectionRanges([LayerSections; FaceLayerId::COUNT]);

impl SectionRanges {
    /// The ranges in `layer`'s draw list.
    pub fn layer(&self, layer: FaceLayerId) -> &LayerSections {
        &self.0[layer as usize]
    }
}

/// The production face painter. Pure and deterministic: the same [`FaceInput`] always fills the same lists.
#[derive(Clone, Debug)]
pub struct RealPainter {
    layers: [FaceLayerSpec; FaceLayerId::COUNT],
    sections: SectionRanges,
}

impl RealPainter {
    /// A painter for a face projected onto `projection`, with the base layer at `base_px_per_m` (see
    /// [`DEFAULT_PX_PER_M`](omuzaki_face_api::DEFAULT_PX_PER_M)).
    pub fn new(projection: FaceProjection, base_px_per_m: f32) -> Self {
        Self {
            layers: FaceLayerId::ALL.map(|id| FaceLayerSpec::for_layer(id, &projection, base_px_per_m)),
            sections: SectionRanges::default(),
        }
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

    /// The command range each section wrote in the last paint, for the GPU rasteriser's per-section cache.
    pub fn sections(&self) -> &SectionRanges {
        &self.sections
    }

    fn run(&mut self, input: &FaceInput<'_>, out: &mut FaceDrawing, only: Option<Section>) {
        out.clear();
        let mut ctx = PaintCtx::new(input, &self.layers, out, only);
        let len = |ctx: &mut PaintCtx<'_>, layer: FaceLayerId| ctx.list(layer).cmds().len();
        for (at, section) in Section::ALL.into_iter().enumerate() {
            let before = FaceLayerId::ALL.map(|layer| len(&mut ctx, layer));
            if ctx.runs(section) {
                section.paint(&mut ctx);
            }
            for layer in FaceLayerId::ALL {
                self.sections.0[layer as usize][at] = before[layer as usize]..len(&mut ctx, layer);
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
