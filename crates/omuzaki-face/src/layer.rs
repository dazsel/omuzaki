//! [`FaceLayerPainter`]: one face layer as the renderer's `Painter`, and the [`GpuRasterizer`] seam.

use std::any::Any;

use omuzaki_core::glam::UVec2;
use omuzaki_face_api::{DrawList, FaceError, FaceLayerSpec, FaceRasterizer};
use omuzaki_render_api::{PaintTarget, Painter, PixelRect, RenderError};

use crate::system::LayerSlot;

/// A rasteriser that draws a [`DrawList`] straight into the renderer's own texture, with no CPU round trip: the face
/// painter's 0.5 ms budget needs this (`raster::gpu`).
pub trait GpuRasterizer: Send {
    /// Draws `list` over `spec.rect` into `native`, the renderer's handle from `PaintTarget::native()`. `Ok(true)`
    /// means the layer is drawn; `Ok(false)` means `native` is not a handle this rasteriser knows (another backend, or
    /// the mock renderer) and nothing was drawn, so the caller falls back to the CPU raster.
    ///
    /// # Errors
    /// [`FaceError::Raster`] if the list uses something this rasteriser can't draw.
    fn rasterize_native(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError>;
}

/// The renderer's [`Painter`] for one face layer, from [`FaceSystem::layer_painter`](crate::FaceSystem::layer_painter).
///
/// GPU first: if a [`GpuRasterizer`] is registered and recognises the target's native handle, it draws there.
/// Otherwise the CPU raster is written with `write_rgba`, rasterised now if the system skipped it because a GPU
/// rasteriser was registered. Pixels are premultiplied sRGB, which is what the layer textures hold
/// (`omuzaki_char_api::face_texture_desc`).
pub struct FaceLayerPainter<'a> {
    spec: FaceLayerSpec,
    list: &'a DrawList,
    slot: &'a mut LayerSlot,
    cpu: &'a mut dyn FaceRasterizer,
    gpu: Option<&'a mut dyn GpuRasterizer>,
}

impl<'a> FaceLayerPainter<'a> {
    pub(crate) fn new(
        spec: FaceLayerSpec,
        list: &'a DrawList,
        slot: &'a mut LayerSlot,
        cpu: &'a mut dyn FaceRasterizer,
        gpu: Option<&'a mut dyn GpuRasterizer>,
    ) -> Self {
        Self { spec, list, slot, cpu, gpu }
    }
}

fn render_error(e: &FaceError) -> RenderError {
    RenderError::invalid("face layer", e.to_string())
}

impl Painter for FaceLayerPainter<'_> {
    fn paint(&mut self, target: &mut dyn PaintTarget) -> Result<(), RenderError> {
        let (w, h) = self.spec.size_px();
        let size = UVec2::new(w, h);
        if target.desc().size != size {
            return Err(RenderError::invalid(
                "face layer",
                format!(
                    "the {:?} layer is {w}x{h} px but its texture is {}x{}",
                    self.spec.id,
                    target.desc().size.x,
                    target.desc().size.y
                ),
            ));
        }
        if let Some(gpu) = self.gpu.as_mut()
            && gpu.rasterize_native(&self.spec, self.list, target.native()).map_err(|e| render_error(&e))?
        {
            return Ok(());
        }
        if self.slot.stale {
            self.cpu
                .rasterize(&self.spec, self.list, self.spec.px_per_m, &mut self.slot.raster)
                .map_err(|e| render_error(&e))?;
            self.slot.stale = false;
        }
        let raster = &self.slot.raster;
        if (raster.width, raster.height) != (w, h) {
            return Err(RenderError::invalid(
                "face layer",
                format!("the rasteriser made {}x{} px for a {w}x{h} px layer", raster.width, raster.height),
            ));
        }
        target.write_rgba(PixelRect::full(size), &raster.pixels)
    }
}
