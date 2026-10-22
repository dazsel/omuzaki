//! The GPU rasteriser: draws a `DrawList` through Vello. [`SceneEncoder`] is the CPU half, which turns a list into a
//! `vello::Scene` and does no GPU work; [`VelloRasterizer`] rasterises that scene with Vello's compute stages (`engine`,
//! on resources kept between repaints) and resolves it into the renderer's face texture, recording both into the
//! renderer's encoder. Strokes thinner than a pixel are drawn as Skia's hairlines, in `stroke`.

pub mod clip;
mod engine;
pub mod fill;
pub mod gradient;
pub mod resolve;
mod sections;
mod snap;
mod stroke;

use std::any::Any;
use std::future::Future;
use std::ops::Range;
use std::task::{Context, Poll, Waker};

use omuzaki_core::glam::Affine2;
use omuzaki_face_api::{Cap, Cmd, DrawList, FaceError, FaceLayerId, FaceLayerSpec, Join};
use omuzaki_render_api::PixelRect;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap as KCap, Join as KJoin, Rect, Stroke};
use vello::peniko::{ColorStop, Fill};

use self::clip::{OpenStack, Transforms};
pub use self::engine::BumpCapacity;
use self::engine::{Engine, Frame, LayerTarget, Recovery};
use self::fill::{Bounds, fill_bez};
use self::gradient::brush;
use self::resolve::Resolve;
use self::sections::{LayerCache, check_tiling, fingerprint};
use self::snap::snapped_path;
use self::stroke::hairline;
use crate::layer::GpuRasterizer;
use crate::painter::LayerSections;
use crate::raster::cpu::check;

/// The most a layer may be per side, as in the CPU raster.
const MAX_SIDE: u32 = 16_384;
/// Vello evaluates a gradient at a pixel's corner, where Skia (and the canvas) evaluate it at the centre. Moving the
/// gradient half a pixel up and left puts the corner's value where the centre's would be.
const GRADIENT_SHIFT: Affine = Affine::new([1.0, 0.0, 0.0, 1.0, -0.5, -0.5]);
/// The miter limit the CPU raster and the canvas use.
const MITER_LIMIT: f64 = 10.0;
/// The side of one of Vello's bins, in pixels (16 tiles of 16).
const BIN_SIDE: u32 = 256;
/// The most bins Vello's coarse stage handles (vello#680): a layer with more draws wrong, so the CPU raster draws it.
const MAX_BINS: u32 = 256;

/// Encodes draw lists into Vello scenes. Keep one per layer: it owns the scratch buffers, so a warm encode only
/// allocates what Vello itself does.
#[derive(Default)]
pub struct SceneEncoder {
    path: BezPath,
    stops: Vec<ColorStop>,
    transforms: Transforms,
    clips: OpenStack<()>,
    bounds: Bounds,
}

/// A face-plane affine as a kurbo one.
fn affine(a: Affine2) -> Affine {
    Affine::new(a.to_cols_array().map(f64::from))
}

impl SceneEncoder {
    /// An encoder with empty buffers.
    pub fn new() -> Self {
        Self::default()
    }

    /// The pixel rectangle the last [`encode`](Self::encode) drew into, or `None` if it drew nothing. It is a
    /// conservative hull (control points, stroke reach, clips ignored), so a damage rectangle built from it is never
    /// too small.
    pub fn last_bounds(&self) -> Option<Rect> {
        self.bounds.get()
    }

    /// Appends the encoding of `list` to `scene`, which the caller owns and resets. The layer-to-pixel transform is the
    /// CPU raster's: the plane's y axis points up, the rectangle fills the `size_px` image exactly.
    ///
    /// # Errors
    /// [`FaceError::Raster`] if the rectangle or density is not positive, the image is too large, a transform overflows,
    /// or a pop has no push or a push is never popped (the message names the command's index). `scene` is then
    /// unspecified: reset it.
    pub fn encode(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        px_per_m: f32,
        scene: &mut Scene,
    ) -> Result<(), FaceError> {
        self.encode_range(spec, list, 0..list.cmds().len(), px_per_m, scene)
    }

    /// As [`encode`](Self::encode), for the commands in `range` only (one section's slice). The slice must be balanced:
    /// every push inside it popped inside it.
    ///
    /// # Errors
    /// As [`encode`](Self::encode), and [`FaceError::Raster`] if `range` is not inside the list.
    pub fn encode_range(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        range: Range<usize>,
        px_per_m: f32,
        scene: &mut Scene,
    ) -> Result<(), FaceError> {
        let cmds = list.cmds().get(range.clone()).ok_or_else(|| {
            FaceError::Raster(format!("the command range {range:?} is outside the {} commands", list.cmds().len()))
        })?;
        let rect = spec.rect;
        if !(rect.width() > 0.0 && rect.height() > 0.0 && px_per_m.is_finite() && px_per_m > 0.0) {
            return Err(FaceError::Raster("the layer rectangle and density must be positive".to_owned()));
        }
        let (w, h) = rect.size_px(px_per_m);
        if w > MAX_SIDE || h > MAX_SIDE {
            return Err(FaceError::Raster(format!("a {w} x {h} raster is larger than the {MAX_SIDE} px limit")));
        }
        #[allow(clippy::cast_precision_loss)] // pixel counts are far below 2^24
        let (sx, sy) = (f64::from(w as f32 / rect.width()), f64::from(h as f32 / rect.height()));
        let to_px = Affine::new([sx, 0.0, 0.0, -sy, -f64::from(rect.x0) * sx, f64::from(rect.y1) * sy]);

        self.bounds.clear();
        self.transforms.clear();
        self.clips.clear();
        for (index, cmd) in cmds.iter().enumerate() {
            // The CPU raster's check, so a non-finite list is an error on both paths and not a blank or odd picture.
            check(range.start + index, cmd, list)?;
            let current = self.transforms.top().map_or(to_px, |t| *t);
            self.command(range.start + index, cmd, current, list, scene)?;
        }
        self.finish(scene)
    }

    /// Encodes command `index`, drawing under `to_px`, the plane-to-pixel transform in force.
    fn command(
        &mut self,
        index: usize,
        cmd: &Cmd,
        to_px: Affine,
        list: &DrawList,
        scene: &mut Scene,
    ) -> Result<(), FaceError> {
        match *cmd {
            Cmd::PushTransform(t) => {
                let next = to_px * affine(t);
                if !next.as_coeffs().iter().all(|c| c.is_finite()) {
                    return Err(FaceError::Raster(format!("command {index}: PushTransform overflows")));
                }
                self.transforms.push(index, next);
            }
            Cmd::PopTransform => {
                self.transforms.pop(index, "Transform")?;
            }
            Cmd::PushClip(path) => {
                snapped_path(list.elements(path), to_px, &mut self.path);
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &self.path);
                self.clips.push(index, ());
            }
            Cmd::PopClip => {
                self.clips.pop(index, "Clip")?;
                scene.pop_layer();
            }
            Cmd::Fill { path, ref paint, opacity } => {
                snapped_path(list.elements(path), to_px, &mut self.path);
                self.bounds.add(&self.path, Affine::IDENTITY, 0.0);
                // The path is in pixels already; paint coordinates stay in the command's local space.
                let paint = brush(paint, opacity, &mut self.stops);
                scene.fill(Fill::NonZero, Affine::IDENTITY, &paint, Some(GRADIENT_SHIFT * to_px), &self.path);
            }
            Cmd::Stroke { path, ref paint, width, cap, join, opacity } => {
                let els = list.elements(path);
                // A zero or negative width draws nothing on the canvas (a width that is not a number was rejected by
                // `check`).
                if width <= 0.0 {
                    return Ok(());
                }
                let round = cap == Cap::Round;
                if let Some(factor) = hairline(els, width, to_px, paint, opacity, &mut self.path) {
                    if factor <= 0.0 {
                        return Ok(());
                    }
                    // The line's own half width, and a round cap's half pixel past the end.
                    self.bounds.add(&self.path, Affine::IDENTITY, 1.0);
                    let mut style = Stroke::new(1.0);
                    style.start_cap = if round { KCap::Round } else { KCap::Butt };
                    style.end_cap = style.start_cap;
                    let brush = brush(paint, opacity * factor, &mut self.stops);
                    scene.stroke(&style, Affine::IDENTITY, &brush, Some(GRADIENT_SHIFT * to_px), &self.path);
                    return Ok(());
                }
                fill_bez(els, &mut self.path);
                let mut style = Stroke::new(f64::from(width));
                style.start_cap = if round { KCap::Round } else { KCap::Butt };
                style.end_cap = style.start_cap;
                style.join = if join == Join::Round { KJoin::Round } else { KJoin::Miter };
                style.miter_limit = MITER_LIMIT;
                let reach = if join == Join::Miter { MITER_LIMIT } else { 1.0 };
                let [a, b, c, d, ..] = to_px.as_coeffs();
                // The Frobenius norm is at least the largest singular value, so it bounds the stroke's reach under a shear
                // too, where the longest column alone would not.
                let scale = (a * a + b * b + c * c + d * d).sqrt();
                self.bounds.add(&self.path, to_px, 0.5 * f64::from(width) * scale * reach);
                scene.stroke(&style, to_px, &brush(paint, opacity, &mut self.stops), None, &self.path);
            }
        }
        Ok(())
    }

    /// Checks nothing is left open; closes clips so the scene stays valid for Vello either way.
    fn finish(&mut self, scene: &mut Scene) -> Result<(), FaceError> {
        let open_clip = self.clips.oldest_open();
        for _ in 0..self.clips.depth() {
            scene.pop_layer();
        }
        if let Some(index) = open_clip {
            return Err(FaceError::Raster(format!("command {index}: PushClip is never popped")));
        }
        match self.transforms.oldest_open() {
            Some(index) => Err(FaceError::Raster(format!("command {index}: PushTransform is never popped"))),
            None => Ok(()),
        }
    }
}

/// The output of `future` if it is ready now, which wgpu's error scopes are on native targets; `None` if it is not.
fn ready<T>(future: impl Future<Output = T>) -> Option<T> {
    match std::pin::pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// What the native handle of a face layer downcasts to: the renderer's `NativePaintTarget`, which this crate can't name.
type Native = (wgpu::Device, wgpu::Queue, wgpu::Texture, wgpu::CommandEncoder);

/// The private straight-alpha texture the fine stage writes for one layer, with the resolve's bind group and origin
/// buffer for it. Kept until the layer changes size.
struct Private {
    size: (u32, u32),
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
    origin: wgpu::Buffer,
}

/// Everything one layer keeps between repaints: its sections' scenes, its GPU resources, the last target it drew into
/// and whether that target holds the picture the scenes describe.
#[derive(Default)]
struct LayerState {
    sections: LayerCache,
    private: Option<Private>,
    /// The layer's uploads and bind groups in the engine; made with `private`.
    vello: Option<LayerTarget>,
    /// The last target texture and its level-0 view, kept because making a view allocates.
    target: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// The target shows what the cached scenes encode, so only changed sections need drawing again.
    painted: bool,
}

/// Everything that needs a device. Created on the first paint, because the engine compiles Vello's pipelines.
struct Gpu {
    /// The device the engine, the resolve pipeline and the layers' textures belong to.
    device: wgpu::Device,
    engine: Engine,
    resolve: Resolve,
}

/// The Vello rasteriser: encodes a list with [`SceneEncoder`], rasterises it with Vello's compute stages into a private
/// straight-alpha texture, and resolves that into the renderer's face texture (level 0 only; the renderer rebuilds the
/// mips).
///
/// Given a list's section ranges ([`rasterize_native_sections`](GpuRasterizer::rasterize_native_sections)) it keeps one
/// encoded scene per section of every layer and, on a repaint, encodes only the sections whose slice of the list changed,
/// then rasterises and resolves only the rectangle those sections cover now or covered before (decision 7.2). A repaint of
/// an unchanged layer into the same texture records nothing. [`rasterize_native`](GpuRasterizer::rasterize_native) treats
/// the whole list as one section. Every buffer, texture and bind group is kept between repaints (`engine`), so a warm
/// repaint allocates nothing in this crate; what wgpu allocates per recorded pass and queue write is wgpu's.
///
/// The native convention: `native` downcasts to `(wgpu::Device, wgpu::Queue, wgpu::Texture, wgpu::CommandEncoder)`, which
/// is `omuzaki_render::NativePaintTarget` (this crate can't name that alias, so it names the tuple). The texture is the
/// renderer's `Rgba8Unorm` face texture holding premultiplied sRGB with `STORAGE_BINDING | RENDER_ATTACHMENT | COPY_DST`
/// usages; only level 0 is written, and the renderer rebuilds the mips over the rectangle
/// [`last_damage`](GpuRasterizer::last_damage) reports. The rasteriser records into the encoder and writes its uploads
/// through the queue; it never submits: the renderer does, right after the painter returns, so the face's GPU time is
/// the renderer's to measure.
///
/// Use one for all of a system's layers. The engine belongs to the device of the paint that made it; a paint with another
/// device drops that state and starts over with whole repaints. A rebuilt renderer may not look like another device (see
/// [`reset`](Self::reset)), so the stage calls [`GpuRasterizer::device_lost`] when it rebuilds one.
///
/// It returns `Ok(false)`, so the CPU raster draws, when `native` is not the renderer's tuple (the mock renderer's
/// targets, another backend), when Vello's stages can't run on the device, and, for a rasteriser made with
/// [`hardware_only`](Self::hardware_only), on a software adapter such as lavapipe (those check correctness only and are
/// far slower than the CPU raster, decision 7.3). A decline is logged once and never retried on that device (a paint
/// with another device tries again).
#[derive(Default)]
pub struct VelloRasterizer {
    encoder: SceneEncoder,
    /// The scene of the rectangle being drawn: the cached sections appended in order.
    scene: Scene,
    gpu: Option<Gpu>,
    layers: [LayerState; FaceLayerId::COUNT],
    /// The device the rasteriser declines on: paints on it decline, and a paint on another device tries again.
    unavailable: Option<wgpu::Device>,
    /// Decline software adapters (the stage's choice); tests that check pixels on lavapipe leave it off.
    hardware_only: bool,
    /// How many sections the last paint encoded.
    encoded: usize,
    /// The rectangle the last paint changed, `[x, y, w, h]` (`[0, 0, 0, 0]` when it changed nothing).
    damage: [u32; 4],
    /// Where the next paint writes the GPU timestamps of its passes, for the timing probe.
    pass_timestamps: Option<(wgpu::QuerySet, u32)>,
    /// The sizes Vello's bump-allocated buffers start at, instead of Vello's own: for tests of the overflow path.
    bump_capacity: Option<BumpCapacity>,
    /// What has been logged once: a scene outgrowing Vello's buffers, a layer too large for its bins.
    logged_overflow: bool,
    logged_too_large: bool,
}

impl VelloRasterizer {
    /// A rasteriser with nothing allocated on the GPU yet. It draws on any adapter, software ones included.
    pub fn new() -> Self {
        Self::default()
    }

    /// A rasteriser that declines software adapters (`wgpu::DeviceType::Cpu`, such as lavapipe or WARP), so the CPU
    /// raster draws there: the live stage's choice (decision 7.3).
    pub fn hardware_only() -> Self {
        Self { hardware_only: true, ..Self::default() }
    }

    /// How many sections the last paint encoded again: for tests and tools that check what a repaint redid.
    pub fn sections_encoded(&self) -> usize {
        self.encoded
    }

    /// Starts Vello's bump-allocated buffers (lines, bins, tiles, segment counts, segments, blend spill, command lists)
    /// at `capacity` instead of Vello's sizes, so a test can make a scene outgrow them. However they start, a repaint that
    /// outgrows any of them is found from its counters a frame or two later, without waiting on the GPU (on a later
    /// paint, or when the stage asks [`repaint_needed`](GpuRasterizer::repaint_needed)); the buffers grow and the layer
    /// is repainted whole.
    #[must_use]
    pub fn with_bump_capacity(mut self, capacity: BumpCapacity) -> Self {
        self.bump_capacity = Some(capacity);
        self
    }

    /// Makes the next paints write GPU timestamps around their two passes into `queries`, from index `at`: the
    /// rasterisation's begin and end (`at`, `at + 1`), then the resolve's (`at + 2`, `at + 3`). For the timing probe
    /// (`omuzaki-test-face`'s `gpu_face_timing` example), which needs `wgpu::Features::TIMESTAMP_QUERY`; `None` stops
    /// it. A paint that draws nothing writes none.
    pub fn time_passes(&mut self, queries: Option<(wgpu::QuerySet, u32)>) {
        self.pass_timestamps = queries;
    }
}

impl Gpu {
    /// The device state for targets of `format`. Compiles Vello's stages, so it runs once per rasteriser.
    ///
    /// wgpu reports a device too weak for Vello (too few storage buffers per stage, say) as a validation error, not as an
    /// `Err`, so the creation runs in an error scope and an error counts as a failure.
    fn create(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        capacity: Option<BumpCapacity>,
    ) -> Result<Self, String> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let engine = Engine::new(device, capacity);
        let gpu = Self { device: device.clone(), engine, resolve: Resolve::new(device, format) };
        // A scope that is still pending can't be read as "no error": count it as a failure, as `omuzaki-render` does.
        match ready(scope.pop()) {
            Some(None) => Ok(gpu),
            Some(Some(error)) => Err(error.to_string()),
            None => Err("the device did not report whether Vello's pipelines are valid".to_owned()),
        }
    }
}

impl LayerState {
    /// Makes the private texture (and the layer's engine resources) for a `width` x `height` layer if there is none of
    /// that size; a new one forgets that the target was painted.
    fn ensure_private(&mut self, device: &wgpu::Device, resolve: &Resolve, width: u32, height: u32) {
        if self.private.as_ref().is_none_or(|p| p.size != (width, height)) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("face-vello"),
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let origin = Resolve::origin_buffer(device);
            let bind = resolve.bind(device, &view, &origin);
            self.private = Some(Private { size: (width, height), view, bind, origin });
            self.vello.get_or_insert_with(|| LayerTarget::new(device)).out_changed();
            self.painted = false;
        }
    }

    /// Makes the level-0 view of `target`; a texture other than the last one has not been painted.
    fn ensure_target(&mut self, target: &wgpu::Texture) {
        if self.target.as_ref().is_none_or(|(texture, _)| texture != target) {
            self.target = Some((target.clone(), Resolve::level_zero(target)));
            self.painted = false;
        }
    }

    /// Encodes the sections whose slice of `list` changed, and returns how many, and the rectangle they covered or cover
    /// now (`None` if nothing changed).
    fn encode_changed(
        &mut self,
        encoder: &mut SceneEncoder,
        spec: &FaceLayerSpec,
        list: &DrawList,
        sections: &LayerSections,
    ) -> Result<(usize, Option<Rect>), FaceError> {
        let (mut count, mut damage) = (0, None::<Rect>);
        for (cache, range) in self.sections.iter_mut().zip(sections) {
            let key = fingerprint(list, range.clone(), spec)?;
            if cache.key == Some(key) {
                continue;
            }
            // A failed encode leaves a half-built scene: forget its key so the content that built it is encoded again, and
            // keep `bounds` so the next damage still covers the area it last drew.
            cache.key = None;
            cache.scene.reset();
            encoder.encode_range(spec, list, range.clone(), spec.px_per_m, &mut cache.scene)?;
            let now = encoder.last_bounds();
            for bounds in [cache.bounds, now].into_iter().flatten() {
                damage = Some(damage.map_or(bounds, |d| d.union(bounds)));
            }
            (cache.bounds, cache.key) = (now, Some(key));
            count += 1;
        }
        Ok((count, damage))
    }
}

/// `damage` as whole pixels inside a `width` x `height` layer, one pixel wider on every side for anti-aliasing, as
/// `[x, y, w, h]`; `None` if it is empty there.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped into the layer first
fn pixel_rect(damage: Rect, width: u32, height: u32) -> Option<[u32; 4]> {
    let (w, h) = (f64::from(width), f64::from(height));
    let (x0, y0) = ((damage.x0.floor() - 1.0).clamp(0.0, w), (damage.y0.floor() - 1.0).clamp(0.0, h));
    let (x1, y1) = ((damage.x1.ceil() + 1.0).clamp(0.0, w), (damage.y1.ceil() + 1.0).clamp(0.0, h));
    (x1 > x0 && y1 > y0).then_some([x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32])
}

impl VelloRasterizer {
    /// Drops everything made on the old device: the engine, the resolve pipeline, the layers' textures, the section
    /// caches and a remembered decline. The next paint starts over with a whole repaint. Comparing devices is not enough
    /// to notice a rebuilt renderer: wgpu numbers resources per instance, and the first device of a new instance can
    /// compare equal to the first device of the old one.
    pub fn reset(&mut self) {
        self.gpu = None;
        self.layers = Default::default();
        self.unavailable = None;
        self.encoded = 0;
        self.damage = [0; 4];
    }

    /// Why this rasteriser declines `device`, if it does: Vello's stages can't be made there, or it is a software
    /// adapter and the rasteriser is [`hardware_only`](Self::hardware_only).
    fn make_gpu(&self, device: &wgpu::Device, format: wgpu::TextureFormat) -> Result<Gpu, String> {
        if self.hardware_only && device.adapter_info().device_type == wgpu::DeviceType::Cpu {
            return Err("it is a software adapter".to_owned());
        }
        Gpu::create(device, format, self.bump_capacity)
    }

    /// Records the drawing of `list` into `native`'s encoder, redoing only what the sections' ranges say changed.
    fn paint(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        sections: &LayerSections,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        let Some((device, queue, target, commands)) =
            native.downcast_mut::<Native>().map(|n| (&n.0, &n.1, &n.2, &mut n.3))
        else {
            return Ok(false);
        };
        if self.unavailable.as_ref() == Some(device) {
            return Ok(false);
        }
        self.unavailable = None;
        let (width, height) = spec.size_px();
        if (target.width(), target.height()) != (width, height) {
            return Err(FaceError::Raster(format!(
                "the {:?} layer is {width}x{height} px but its texture is {}x{}",
                spec.id,
                target.width(),
                target.height()
            )));
        }
        check_tiling(sections, list.cmds().len())?;
        if self.too_many_bins(width, height) {
            return Ok(false);
        }
        let index = spec.id as usize;
        if !self.prepare(device, target, index, (width, height))? {
            return Ok(false);
        }
        let (Some(gpu), layer) = (self.gpu.as_mut(), &mut self.layers[index]) else {
            return Err(FaceError::Raster("the GPU state is missing after it was made".to_owned()));
        };

        // From here the target no longer shows the cached scenes until the repaint below finishes.
        let was_painted = std::mem::replace(&mut layer.painted, false);
        self.damage = [0; 4];
        let (count, damage) = layer.encode_changed(&mut self.encoder, spec, list, sections)?;
        self.encoded = count;
        let damage = if was_painted {
            let Some(damage) = damage else {
                // Nothing changed and the target already shows it.
                layer.painted = true;
                return Ok(true);
            };
            damage
        } else {
            Rect::new(0.0, 0.0, f64::from(width), f64::from(height))
        };
        let Some(rect) = pixel_rect(damage, width, height) else {
            layer.painted = true;
            return Ok(true);
        };

        self.scene.reset();
        let shift = Affine::translate((-f64::from(rect[0]), -f64::from(rect[1])));
        for cache in &layer.sections {
            self.scene.append(&cache.scene, Some(shift));
        }
        let (Some(private), Some(vello), Some((_, level_zero))) =
            (layer.private.as_ref(), layer.vello.as_mut(), layer.target.as_ref())
        else {
            return Err(FaceError::Raster(
                "the private texture or the target view is missing after it was made".to_owned(),
            ));
        };
        let stamps = self.pass_timestamps.as_ref().map(|(queries, at)| (queries, *at));
        let frame =
            Frame { scene: &self.scene, width: rect[2], height: rect[3], out: &private.view, timestamps: stamps };
        gpu.engine
            .record(device, queue, commands, vello, &frame)
            .map_err(|e| FaceError::Raster(format!("Vello could not rasterise the layer: {e}")))?;
        Resolve::set_origin(queue, &private.origin, rect[0], rect[1]);
        gpu.resolve.encode(commands, &private.bind, level_zero, rect, stamps.map(|(queries, at)| (queries, at + 2)));
        layer.painted = true;
        self.damage = rect;
        Ok(true)
    }
}

impl VelloRasterizer {
    /// Makes what layer `at` is drawn with on `device` into `target` (a `width` x `height` texture), if it is not made
    /// yet, and reads back what earlier repaints of it found. `false` when the rasteriser declines the device (Vello's
    /// stages can't run there, it is a software adapter and the rasteriser is hardware-only, or the scene outgrew what
    /// the device's buffers hold).
    fn prepare(
        &mut self,
        device: &wgpu::Device,
        target: &wgpu::Texture,
        at: usize,
        (width, height): (u32, u32),
    ) -> Result<bool, FaceError> {
        // The renderer is rebuilt when the device is lost, but this rasteriser survives it: everything made on the old
        // device (the engine, the resolve pipeline, the textures) is dropped with it.
        if self.gpu.as_ref().is_some_and(|gpu| &gpu.device != device) {
            self.gpu = None;
            self.layers = Default::default();
        }
        if self.gpu.is_none() {
            match self.make_gpu(device, target.format()) {
                Ok(gpu) => self.gpu = Some(gpu),
                Err(e) => {
                    tracing::warn!(
                        "the face falls back to the CPU raster: the GPU rasteriser declines this device ({e})"
                    );
                    self.unavailable = Some(device.clone());
                    return Ok(false);
                }
            }
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return Err(FaceError::Raster("the GPU state is missing after it was made".to_owned()));
        };
        if gpu.resolve.format() != target.format() {
            gpu.resolve = Resolve::new(device, target.format());
            self.layers = Default::default();
        }
        let layer = &mut self.layers[at];
        layer.ensure_private(device, &gpu.resolve, width, height);
        layer.ensure_target(target);
        // A caller that paints without asking `repaint_needed` first still gets an earlier repaint's overflow repaired.
        self.check_layer(at);
        Ok(self.unavailable.as_ref() != Some(device))
    }

    /// Reads the counters of layer `at`'s earlier repaints that have come back, never waiting, and returns whether one
    /// of them left the layer's picture wrong: it outgrew Vello's buffers (or went unchecked). The buffers then grow and
    /// the layer is marked for a whole repaint, or, when the device can't hold the scene, the rasteriser declines the
    /// device, so the repaint falls to the CPU raster.
    fn check_layer(&mut self, at: usize) -> bool {
        let (Some(gpu), Some(layer)) = (self.gpu.as_mut(), self.layers.get_mut(at)) else { return false };
        let Some(recovery) = layer.vello.as_mut().and_then(|vello| gpu.engine.check_overflow(vello)) else {
            return false;
        };
        layer.painted = false;
        match recovery {
            Recovery::Repaint => {
                if !std::mem::replace(&mut self.logged_overflow, true) {
                    tracing::warn!(
                        "a face repaint outgrew Vello's buffers (or went unchecked); they grow and the layer is drawn \
                         again whole"
                    );
                }
            }
            Recovery::Exhausted => {
                tracing::warn!(
                    "the face falls back to the CPU raster: its scene needs more than the device's buffers hold"
                );
                self.unavailable = Some(gpu.device.clone());
            }
        }
        true
    }

    /// Whether a `width` x `height` layer has more bins than Vello's coarse stage draws right (logged once).
    fn too_many_bins(&mut self, width: u32, height: u32) -> bool {
        let too_many = width.div_ceil(BIN_SIDE) * height.div_ceil(BIN_SIDE) > MAX_BINS;
        if too_many && !std::mem::replace(&mut self.logged_too_large, true) {
            tracing::warn!(
                "the face's {width}x{height} px layer has more bins than Vello draws right: the CPU raster draws it"
            );
        }
        too_many
    }
}

impl GpuRasterizer for VelloRasterizer {
    fn rasterize_native(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        let len = list.cmds().len();
        let whole: LayerSections = std::array::from_fn(|at| if at == 0 { 0..len } else { len..len });
        self.paint(spec, list, &whole, native)
    }

    fn device_lost(&mut self) {
        self.reset();
    }

    fn rasterize_native_sections(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        sections: &LayerSections,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        self.paint(spec, list, sections, native)
    }

    fn last_damage(&self) -> Option<PixelRect> {
        let [x, y, width, height] = self.damage;
        Some(PixelRect { x, y, width, height })
    }

    fn repaint_needed(&mut self, layer: FaceLayerId) -> bool {
        self.check_layer(layer as usize)
    }

    fn paint_failed(&mut self, layer: FaceLayerId) {
        // The renderer dropped what the last paint recorded: the target shows whatever it showed before, so the next paint
        // redraws the layer whole.
        if let Some(state) = self.layers.get_mut(layer as usize) {
            state.painted = false;
            if let Some(vello) = state.vello.as_mut() {
                vello.paint_dropped();
            }
        }
    }
}
