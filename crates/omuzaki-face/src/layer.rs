//! [`FaceLayerPainter`]: one face layer as the renderer's `Painter`, and the [`GpuRasterizer`] seam.

use std::any::Any;

use omuzaki_core::glam::UVec2;
use omuzaki_face_api::{DrawList, FaceError, FaceLayerId, FaceLayerSpec, FaceRasterizer};
use omuzaki_render_api::{PaintTarget, Painter, PixelRect, RenderError};

use crate::painter::LayerSections;
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

    /// As [`rasterize_native`](Self::rasterize_native), for a list whose sections' command ranges are known
    /// (`sections`, from [`RealPainter`](crate::RealPainter)), so a rasteriser can keep a cache per section and redraw only
    /// what changed. The default ignores the ranges.
    ///
    /// # Errors
    /// As [`rasterize_native`](Self::rasterize_native).
    fn rasterize_native_sections(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        sections: &LayerSections,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        let _ = sections;
        self.rasterize_native(spec, list, native)
    }

    /// A new frame begins: the face system calls it, then [`stage`](Self::stage) for every layer whose list changed,
    /// before any of the frame's paints. The default does nothing.
    fn begin_frame(&mut self) {}

    /// The changed `list` of a layer the frame is about to paint, with its sections, before any of the frame's paints. A
    /// rasteriser may encode it now and, on the frame's first paint, rasterise every staged layer together, leaving each
    /// layer's own paint only the copy into its texture. A layer that is not staged is painted as before. The default
    /// does nothing.
    ///
    /// # Errors
    /// As [`rasterize_native`](Self::rasterize_native): a list it can't draw. The layer is then painted on its own,
    /// where the same error is reported.
    fn stage(&mut self, spec: &FaceLayerSpec, list: &DrawList, sections: &LayerSections) -> Result<(), FaceError> {
        let _ = (spec, list, sections);
        Ok(())
    }

    /// The renderer's device was lost or rebuilt: drop everything made on the old one (renderers, pipelines, textures,
    /// caches) and decide afresh on the next paint. The stage calls this when it rebuilds the renderer, because a
    /// rasteriser can't always tell the new device from the old one by comparing them (wgpu numbers its resources per
    /// instance, and a rebuilt renderer is a new instance). The default has nothing to drop.
    fn device_lost(&mut self) {}

    /// The rectangle of the layer the last call that returned `Ok(true)` changed, in pixels, which the painter reports
    /// to the renderer (`PaintTarget::mark_damage`) so it refreshes only that part of the texture. An empty rectangle
    /// means nothing changed. The default, `None`, doesn't know: the renderer then counts the whole texture.
    fn last_damage(&self) -> Option<PixelRect> {
        None
    }

    /// Whether `layer` must be painted again although its draw list did not change: a paint this rasteriser recorded
    /// earlier turned out to have drawn it wrong (it outgrew a GPU buffer, say), which a GPU rasteriser learns a frame
    /// or two later without waiting on the GPU. The rasteriser has already arranged for the next paint of `layer` to
    /// redraw it whole (or to decline, so the CPU raster draws it). The stage asks once a frame for every layer, so a
    /// face that stops changing is still repaired; it must not allocate or wait. The default never asks.
    fn repaint_needed(&mut self, layer: FaceLayerId) -> bool {
        let _ = layer;
        false
    }

    /// The renderer refused the last paint of `layer` after this rasteriser recorded it (`Renderer::paint_texture`
    /// returned an error), so the texture shows what it showed before: the next paint must redraw the layer whole, not
    /// only what changed since. The stage calls it. The default keeps nothing to forget.
    fn paint_failed(&mut self, layer: FaceLayerId) {
        let _ = layer;
    }
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
    sections: Option<&'a LayerSections>,
    slot: &'a mut LayerSlot,
    cpu: &'a mut dyn FaceRasterizer,
    gpu: Option<&'a mut dyn GpuRasterizer>,
}

impl<'a> FaceLayerPainter<'a> {
    pub(crate) fn new(
        spec: FaceLayerSpec,
        list: &'a DrawList,
        sections: Option<&'a LayerSections>,
        slot: &'a mut LayerSlot,
        cpu: &'a mut dyn FaceRasterizer,
        gpu: Option<&'a mut dyn GpuRasterizer>,
    ) -> Self {
        Self { spec, list, sections, slot, cpu, gpu }
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
        if let Some(gpu) = self.gpu.as_mut() {
            let native = target.native();
            let drawn = match self.sections {
                Some(sections) => gpu.rasterize_native_sections(&self.spec, self.list, sections, native),
                None => gpu.rasterize_native(&self.spec, self.list, native),
            };
            if drawn.map_err(|e| render_error(&e))? {
                if let Some(damage) = gpu.last_damage() {
                    target.mark_damage(damage);
                }
                return Ok(());
            }
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
