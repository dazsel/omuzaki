//! Painters: how the face system (and anything else that draws a dynamic texture) reaches a texture.
//!
//! [`GpuPaintTarget`] is what a [`Painter`](omuzaki_render_api::Painter) receives. Its portable `write_rgba` stages the
//! pixels and applies them only if the painter succeeds, so a painter that fails halfway leaves the old picture (the
//! contract's rule). Its `native()` is the GPU interop convention of `docs/design/render-api.md` (decision D9, revised
//! by #2074): a [`NativePaintTarget`] a GPU painter downcasts and records its commands into. The renderer submits the
//! encoder when the painter returns, inside the paint's error scope and between two GPU timestamps, so the painter's GPU
//! time is reported under `PaintTextures` and the paint is ordered before the next render. The painter reports the
//! rectangle it changed with `mark_damage`, and the `PaintTextures` pass rebuilds the mip chain over that rectangle only.

use std::any::Any;
use std::ops::Range;
use std::time::Duration;

use omuzaki_render_api::{PaintTarget, PixelRect, RenderError, TextureDesc, TextureId};

use crate::frame::timing::GpuTimer;
use crate::gpu::Gpu;
use crate::resource::texture::GpuTexture;

/// What `native()` hands a GPU painter: cheap clones of the renderer's device, queue and texture, and a command encoder
/// the painter records into. The painter may write buffers and textures of its own through the queue (those writes land
/// before the encoder runs) but never submits: the renderer submits the encoder once the painter returns `Ok`, and drops
/// it unsubmitted if the painter fails. Pixels the same painter writes with `write_rgba` land before the encoder's
/// commands. The texture is the one painters write (`Rgba8Unorm` with `STORAGE_BINDING | RENDER_ATTACHMENT | COPY_DST`):
/// the painter writes its level 0 only, and the renderer rebuilds the sampled picture (premultiply, mips) from it.
pub type NativePaintTarget = (wgpu::Device, wgpu::Queue, wgpu::Texture, wgpu::CommandEncoder);

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
    /// Times the native commands, when the device has timestamps.
    timer: Option<&'a mut GpuTimer>,
    /// Made by the first `native()` call: the painter's handles and the encoder it records into.
    native: Option<NativePaintTarget>,
    /// The query the end of the native commands is timed into, if they are timed.
    timed: Option<u32>,
    /// Whether the painter reported what it changed through `native()` (`mark_damage`).
    marked: bool,
    /// The union of what it reported; `None` for nothing.
    damage: Option<PixelRect>,
}

impl<'a> GpuPaintTarget<'a> {
    pub(crate) fn new(
        gpu: &'a Gpu,
        texture: &'a GpuTexture,
        staging: &'a mut Staging,
        timer: Option<&'a mut GpuTimer>,
    ) -> Self {
        staging.bytes.clear();
        staging.writes.clear();
        Self { gpu, texture, staging, timer, native: None, timed: None, marked: false, damage: None }
    }

    /// Applies the staged writes, then submits what the painter recorded through `native()`, and returns the rectangle
    /// that changed (`None` if nothing did) and the timestamp pair the submitted commands write (its end query), which
    /// the caller gives back if the submit turns out invalid. Call only when the painter succeeded.
    pub(crate) fn commit(mut self) -> (Option<PixelRect>, Option<u32>) {
        let mut changed = None;
        for (rect, range) in &self.staging.writes {
            self.texture.write(self.gpu, *rect, &self.staging.bytes[range.clone()]);
            changed = Some(changed.map_or(*rect, |c| union(c, *rect)));
        }
        let Some((_, _, _, mut encoder)) = self.native.take() else { return (changed, None) };
        // The span is the submitted commands' now: dropping the target must not give it back.
        let timed = self.timed.take();
        if let (Some(timer), Some(end)) = (self.timer.as_deref(), timed) {
            timer.end_paint(&mut encoder, end);
        }
        self.gpu.queue.submit([encoder.finish()]);
        // A painter that drew without saying where may have drawn anywhere.
        let drawn = if self.marked { self.damage } else { Some(PixelRect::full(self.texture.desc.size)) };
        let changed = match (changed, drawn) {
            (Some(a), Some(b)) => Some(union(a, b)),
            (a, b) => a.or(b),
        };
        (changed, timed)
    }
}

impl Drop for GpuPaintTarget<'_> {
    /// A target dropped without [`commit`](GpuPaintTarget::commit) (the painter failed, or the paint's error scope
    /// did) drops what the painter recorded unsubmitted, so its timestamp pair is given back: nothing will write it, and
    /// `PaintTextures` must not count it.
    fn drop(&mut self) {
        if let (Some(timer), Some(end)) = (self.timer.as_deref_mut(), self.timed.take()) {
            timer.cancel_paint(end);
        }
    }
}

/// `rect` cut to a texture of `size`; `None` if nothing of it is inside.
fn clip(rect: PixelRect, size: omuzaki_core::glam::UVec2) -> Option<PixelRect> {
    let (x1, y1) = (rect.x.saturating_add(rect.width).min(size.x), rect.y.saturating_add(rect.height).min(size.y));
    (x1 > rect.x && y1 > rect.y).then(|| PixelRect { x: rect.x, y: rect.y, width: x1 - rect.x, height: y1 - rect.y })
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
        let Self { gpu, texture, timer, timed, native, .. } = self;
        native.get_or_insert_with(|| {
            let mut encoder =
                gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("texture paint") });
            *timed = timer.as_deref_mut().and_then(|timer| timer.begin_paint(&mut encoder));
            (gpu.device.clone(), gpu.queue.clone(), texture.texture.clone(), encoder)
        })
    }

    fn mark_damage(&mut self, rect: PixelRect) {
        self.marked = true;
        if let Some(rect) = clip(rect, self.texture.desc.size) {
            self.damage = Some(self.damage.map_or(rect, |r| union(r, rect)));
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::UVec2;
    use omuzaki_render_api::{AlphaEncoding, TextureDesc};

    use super::*;
    use crate::frame::timing::GpuTimer;
    use crate::gpu::{AdapterPreference, HeadlessOptions};
    use crate::shared::Shared;

    const SIDE: u32 = 16;

    /// A renderer's shared state on the software adapter and one dynamic premultiplied texture.
    fn setup() -> (Shared, GpuTexture) {
        let gpu =
            Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).expect("a software adapter");
        let shared = Shared::new(gpu);
        let desc =
            TextureDesc { alpha: AlphaEncoding::Premultiplied, ..TextureDesc::colour(UVec2::splat(SIDE)).dynamic() };
        let texture = GpuTexture::create(&shared.gpu, &shared.mips, &desc, None);
        (shared, texture)
    }

    /// What a paint that runs `paint` against the texture reports as changed.
    fn changed(paint: impl FnOnce(&mut GpuPaintTarget<'_>)) -> Option<PixelRect> {
        let (shared, texture) = setup();
        let mut staging = Staging::default();
        let mut target = GpuPaintTarget::new(&shared.gpu, &texture, &mut staging, None);
        paint(&mut target);
        target.commit().0
    }

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect { x, y, width, height }
    }

    #[test]
    fn a_native_paint_that_reports_nothing_changes_the_whole_texture() {
        let whole = changed(|t| {
            t.native();
        });
        assert_eq!(whole, Some(PixelRect::full(UVec2::splat(SIDE))));
    }

    #[test]
    fn a_native_paint_changes_only_what_it_marks_clipped_to_the_texture() {
        assert_eq!(
            changed(|t| {
                t.native();
                t.mark_damage(rect(2, 3, 4, 5));
                t.mark_damage(rect(10, 1, 20, 2));
            }),
            Some(rect(2, 1, SIDE - 2, 7)),
            "the union of both, the second cut at the texture's edge"
        );
        assert_eq!(
            changed(|t| {
                t.native();
                t.mark_damage(rect(0, 0, 0, 0));
            }),
            None,
            "an empty rectangle reports that nothing changed"
        );
        assert_eq!(
            changed(|t| {
                t.native();
                t.mark_damage(rect(SIDE, 0, 4, 4));
            }),
            None,
            "a rectangle outside the texture changes nothing"
        );
    }

    #[test]
    fn a_native_paint_that_is_dropped_gives_its_timestamp_pair_back_and_a_committed_one_keeps_it() {
        let (shared, texture) = setup();
        let Some(mut timer) = GpuTimer::new(&shared.gpu.device, &shared.gpu.queue, shared.gpu.timestamps) else {
            println!("skipped: this adapter has no timestamps inside encoders");
            return;
        };
        let mut staging = Staging::default();
        let mut target = GpuPaintTarget::new(&shared.gpu, &texture, &mut staging, Some(&mut timer));
        target.native();
        drop(target);
        assert_eq!(timer.paints_timed(), 0, "the painter failed: its commands were never submitted, so not timed");
        let mut target = GpuPaintTarget::new(&shared.gpu, &texture, &mut staging, Some(&mut timer));
        target.native();
        let (_, submitted) = target.commit();
        assert!(submitted.is_some(), "the submitted paint's pair is reported");
        assert_eq!(timer.paints_timed(), 1, "and counted");
    }

    #[test]
    fn marked_damage_and_written_pixels_add_up() {
        let both = changed(|t| {
            t.write_rgba(rect(0, 0, 2, 2), &[0; 16]).expect("the rectangle fits");
            t.native();
            t.mark_damage(rect(8, 8, 2, 2));
        });
        assert_eq!(both, Some(rect(0, 0, 10, 10)));
        let marked_only = changed(|t| t.mark_damage(rect(8, 8, 2, 2)));
        assert_eq!(marked_only, None, "damage without a native paint is not a change: nothing was drawn");
    }
}
