//! The `PaintTextures` pass: painters have already written into their textures (`paint_texture` runs them at call
//! time, in the face system's turn of the frame); this pass brings the sampled picture of each texture they touched up
//! to date, over the repainted rectangle only (premultiply for straight-alpha painters, then the mip chain), so nothing
//! samples a stale level, and reports the painters' CPU time as its own.

use omuzaki_render_api::{Pass, RenderError};

use super::{FramePass, Outcome, PassCx};
use crate::shared::Shared;

/// See the module docs.
pub(crate) struct PaintTextures;

impl PaintTextures {
    pub(crate) fn new(_shared: &Shared) -> Self {
        Self
    }
}

impl FramePass for PaintTextures {
    fn pass(&self) -> Pass {
        Pass::PaintTextures
    }

    fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Outcome, RenderError> {
        if !cx.paints.painted {
            return Ok(Outcome::Skipped);
        }
        for &(id, dirty) in &cx.paints.dirty {
            // A texture released since it was painted has nothing left to rebuild, and a new texture in its slot has not
            // been painted: the lookup checks the generation.
            if let Some(texture) = cx.resources.textures.get(id.index(), id.generation()) {
                cx.shared.mips.refresh(cx.encoder, texture, dirty);
            }
        }
        cx.extra_cpu += cx.paints.cpu;
        Ok(Outcome::Ran)
    }
}
