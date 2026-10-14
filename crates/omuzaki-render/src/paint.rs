//! Painters: how the face system (and anything else that draws a dynamic texture) reaches a texture.
//!
//! [`GpuPaintTarget`] is what a [`Painter`](omuzaki_render_api::Painter) receives. Its portable `write_rgba` stages the
//! pixels and applies them only if the painter succeeds, so a painter that fails halfway leaves the old picture (the
//! contract's rule). Its `native()` is the GPU interop convention of `docs/design/render-api.md` (decision D9): a
//! `(wgpu::Device, wgpu::Queue, wgpu::Texture)` a GPU painter downcasts, draws into and submits on the same queue, so
//! the paint is ordered before the next render. The mip chain is rebuilt afterwards by the `PaintTextures` pass.

use std::any::Any;
use std::ops::Range;
use std::time::Duration;

use omuzaki_render_api::{PaintTarget, PixelRect, RenderError, TextureDesc, TextureId};

use crate::gpu::Gpu;
use crate::resource::texture::GpuTexture;

/// What `native()` hands a GPU painter: cheap clones of the renderer's own handles.
pub type NativePaintTarget = (wgpu::Device, wgpu::Queue, wgpu::Texture);

/// Reusable storage for the writes of one paint: kept between paints so a repaint allocates nothing once warm.
#[derive(Default)]
pub(crate) struct Staging {
    bytes: Vec<u8>,
    writes: Vec<(PixelRect, Range<usize>)>,
}

/// Paints since the last frame, for the `PaintTextures` pass to pick up.
#[derive(Default)]
pub(crate) struct PaintLog {
    /// Whether any painter ran (even one that wrote nothing): the pass reports it.
    pub(crate) painted: bool,
    /// Handles of the textures painted (a reused slot has a new generation, so damage never reaches a successor) and the rectangle of each that changed, whose sampled pictures need refreshing.
    pub(crate) dirty: Vec<(TextureId, PixelRect)>,
    /// CPU time the painters took, charged to the pass.
    pub(crate) cpu: Duration,
}

impl PaintLog {
    /// Records a finished paint of `texture` that changed `changed` (`None`: nothing).
    pub(crate) fn record(&mut self, texture: TextureId, changed: Option<PixelRect>, cpu: Duration) {
        self.painted = true;
        self.cpu += cpu;
        let Some(changed) = changed else { return };
        match self.dirty.iter_mut().find(|(t, _)| *t == texture) {
            Some((_, rect)) => *rect = union(*rect, changed),
            None => self.dirty.push((texture, changed)),
        }
    }

    /// Forgets everything once the frame that consumed it has been submitted.
    pub(crate) fn clear(&mut self) {
        self.painted = false;
        self.dirty.clear();
        self.cpu = Duration::ZERO;
    }
}

/// The smallest rectangle holding both.
fn union(a: PixelRect, b: PixelRect) -> PixelRect {
    let (x, y) = (a.x.min(b.x), a.y.min(b.y));
    let (right, bottom) = ((a.x + a.width).max(b.x + b.width), (a.y + a.height).max(b.y + b.height));
    PixelRect { x, y, width: right - x, height: bottom - y }
}

/// The target of one `paint_texture` call.
pub(crate) struct GpuPaintTarget<'a> {
    gpu: &'a Gpu,
    texture: &'a GpuTexture,
    staging: &'a mut Staging,
    native: NativePaintTarget,
    /// Whether the painter asked for `native()`: it can then draw anywhere, so the whole texture counts as changed.
    native_used: bool,
}

impl<'a> GpuPaintTarget<'a> {
    pub(crate) fn new(gpu: &'a Gpu, texture: &'a GpuTexture, staging: &'a mut Staging) -> Self {
        staging.bytes.clear();
        staging.writes.clear();
        let native = (gpu.device.clone(), gpu.queue.clone(), texture.texture.clone());
        Self { gpu, texture, staging, native, native_used: false }
    }

    /// Applies the staged writes and returns the rectangle that changed (`None` if nothing did). Call only when the
    /// painter succeeded.
    pub(crate) fn commit(self) -> Option<PixelRect> {
        let mut changed = None;
        for (rect, range) in &self.staging.writes {
            self.texture.write(self.gpu, *rect, &self.staging.bytes[range.clone()]);
            changed = Some(changed.map_or(*rect, |c| union(c, *rect)));
        }
        if self.native_used { Some(PixelRect::full(self.texture.desc.size)) } else { changed }
    }
}

impl PaintTarget for GpuPaintTarget<'_> {
    fn desc(&self) -> &TextureDesc {
        &self.texture.desc
    }

    fn write_rgba(&mut self, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError> {
        let size = self.texture.desc.size;
        if !rect.fits(size) {
            return Err(RenderError::invalid(
                "paint rectangle",
                format!("{rect:?} doesn't fit a {}x{} texture", size.x, size.y),
            ));
        }
        if rgba.len() != rect.byte_len() {
            return Err(RenderError::invalid(
                "paint pixels",
                format!("{} bytes for {rect:?} (needs {})", rgba.len(), rect.byte_len()),
            ));
        }
        if rect.width == 0 || rect.height == 0 {
            return Ok(()); // nothing to write; the GPU API would reject a zero-sized copy
        }
        let start = self.staging.bytes.len();
        self.staging.bytes.extend_from_slice(rgba);
        self.staging.writes.push((rect, start..self.staging.bytes.len()));
        Ok(())
    }

    fn native(&mut self) -> &mut dyn Any {
        self.native_used = true;
        &mut self.native
    }
}
