//! The GPU rasteriser: draws a `DrawList` through Vello. [`SceneEncoder`] is the CPU half, which turns a list into a
//! `vello::Scene` and does no GPU work; [`VelloRasterizer`] renders that scene and resolves it into the renderer's face
//! texture. Strokes beyond the basic mapping (hairline parity) are [`stroke`]'s.

pub mod clip;
pub mod fill;
pub mod gradient;
pub mod resolve;
mod sections;
mod snap;
pub mod stroke;

use std::any::Any;
use std::future::Future;
use std::ops::Range;
use std::task::{Context, Poll, Waker};

use omuzaki_core::glam::Affine2;
use omuzaki_face_api::{Cap, Cmd, DrawList, FaceError, FaceLayerId, FaceLayerSpec, Join};
use vello::kurbo::{Affine, BezPath, Cap as KCap, Join as KJoin, Rect, Stroke};
use vello::peniko::Color;
use vello::peniko::{ColorStop, Fill};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

use self::clip::{OpenStack, Transforms};
use self::fill::{Bounds, fill_bez};
use self::gradient::brush;
use self::resolve::Resolve;
use self::sections::{LayerCache, check_tiling, fingerprint};
use self::snap::snapped_path;
use crate::layer::GpuRasterizer;
use crate::painter::LayerSections;

/// The most a layer may be per side, as in the CPU raster.
const MAX_SIDE: u32 = 16_384;
/// Vello evaluates a gradient at a pixel's corner, where Skia (and the canvas) evaluate it at the centre. Moving the
/// gradient half a pixel up and left puts the corner's value where the centre's would be.
const GRADIENT_SHIFT: Affine = Affine::new([1.0, 0.0, 0.0, 1.0, -0.5, -0.5]);
/// The miter limit the CPU raster and the canvas use.
const MITER_LIMIT: f64 = 10.0;

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
                fill_bez(list.elements(path), &mut self.path);
                let mut style = Stroke::new(f64::from(width));
                style.start_cap = if cap == Cap::Round { KCap::Round } else { KCap::Butt };
                style.end_cap = style.start_cap;
                style.join = if join == Join::Round { KJoin::Round } else { KJoin::Miter };
                style.miter_limit = MITER_LIMIT;
                let reach = if join == Join::Miter { MITER_LIMIT } else { 1.0 };
                let [a, b, c, d, ..] = to_px.as_coeffs();
                let scale = a.hypot(b).max(c.hypot(d));
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
type Native = (wgpu::Device, wgpu::Queue, wgpu::Texture);

/// Vello's private render target for one layer, with the resolve's bind group and origin buffer for it. Kept until the
/// layer changes size.
struct Private {
    size: (u32, u32),
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
    origin: wgpu::Buffer,
}

/// Everything one layer keeps between repaints: its sections' scenes, its private texture, the last target it drew into
/// and whether that target holds the picture the scenes describe.
#[derive(Default)]
struct LayerState {
    sections: LayerCache,
    private: Option<Private>,
    /// The last target texture and its level-0 view, kept because making a view allocates.
    target: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// The target shows what the cached scenes encode, so only changed sections need drawing again.
    painted: bool,
}

/// Everything that needs a device. Created on the first paint, because Vello compiles its pipelines when its renderer is
/// created.
struct Gpu {
    /// The device the renderer, the resolve pipeline and the layers' private textures belong to.
    device: wgpu::Device,
    renderer: Renderer,
    resolve: Resolve,
}

/// The Vello rasteriser: encodes a list with [`SceneEncoder`], renders it with Vello into a private straight-alpha
/// texture, and resolves that into the renderer's face texture (level 0 only; the renderer rebuilds the mips).
///
/// Given a list's section ranges ([`rasterize_native_sections`](GpuRasterizer::rasterize_native_sections)) it keeps one
/// encoded scene per section of every layer and, on a repaint, encodes only the sections whose slice of the list changed,
/// then renders and resolves only the rectangle those sections cover now or covered before (decision 7.2). A repaint of an
/// unchanged layer into the same texture does no work at all. [`rasterize_native`](GpuRasterizer::rasterize_native) treats
/// the whole list as one section.
///
/// The native convention: `native` downcasts to `(wgpu::Device, wgpu::Queue, wgpu::Texture)`, which is
/// `omuzaki_render::NativePaintTarget` (this crate can't name that alias, so it names the tuple). The texture is the
/// renderer's `Rgba8Unorm` face texture holding premultiplied sRGB with `STORAGE_BINDING | RENDER_ATTACHMENT | COPY_DST`
/// usages; only level 0 is written, and the renderer rebuilds the mips. The rasteriser submits on that queue.
///
/// Use one for all of a system's layers. The Vello renderer belongs to the device of the paint that made it; a paint with
/// another device (the renderer was rebuilt after a device loss) drops that state and starts over with whole repaints.
///
/// It returns `Ok(false)`, so the CPU raster draws, when `native` is not the renderer's `(Device, Queue, Texture)` (the
/// mock renderer's targets, another backend) and when Vello can't run on the device: that is logged once and never
/// retried. Don't register it on a software adapter such as lavapipe: those check correctness only and are far slower
/// than the CPU raster (decision 7.3). Registering it is the stage's job.
#[derive(Default)]
pub struct VelloRasterizer {
    encoder: SceneEncoder,
    /// The scene of the rectangle being drawn: the cached sections appended in order.
    scene: Scene,
    gpu: Option<Gpu>,
    layers: [LayerState; FaceLayerId::COUNT],
    /// Vello can't run on the device: every paint declines.
    unavailable: bool,
    /// How many sections the last paint encoded.
    encoded: usize,
}

impl VelloRasterizer {
    /// A rasteriser with nothing allocated on the GPU yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many sections the last paint encoded again: for tests and tools that check what a repaint redid.
    pub fn sections_encoded(&self) -> usize {
        self.encoded
    }
}

impl Gpu {
    /// The device state for targets of `format`. Compiles Vello's pipelines, so it runs once per rasteriser.
    ///
    /// wgpu reports a device too weak for Vello (too few storage buffers per stage, say) as a validation error, not as an
    /// `Err` from `Renderer::new`, so the creation runs in an error scope and either counts as a failure.
    fn create(device: &wgpu::Device, format: wgpu::TextureFormat) -> Result<Self, String> {
        let options = RendererOptions {
            use_cpu: false,
            antialiasing_support: AaSupport::area_only(),
            num_init_threads: std::num::NonZeroUsize::new(1),
            pipeline_cache: None,
        };
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let made = Renderer::new(device, options).map(|renderer| Self {
            device: device.clone(),
            renderer,
            resolve: Resolve::new(device, format),
        });
        // A scope that is still pending can't be read as "no error": count it as a failure, as `omuzaki-render` does.
        let rejected = ready(scope.pop());
        match (made, rejected) {
            (Ok(_), Some(Some(error))) => Err(error.to_string()),
            (Ok(_), None) => Err("the device did not report whether Vello's pipelines are valid".to_owned()),
            (Ok(gpu), Some(None)) => Ok(Self { device: device.clone(), ..gpu }),
            (Err(error), _) => Err(error.to_string()),
        }
    }
}

impl LayerState {
    /// Makes the private texture for a `width` x `height` layer if there is none of that size; a new one forgets that
    /// the target was painted.
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
    /// Draws `list` into `native`, redoing only what the sections' ranges say changed.
    fn paint(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        sections: &LayerSections,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        let Some((device, queue, target)) = native.downcast_mut::<Native>().map(|n| (&n.0, &n.1, &n.2)) else {
            return Ok(false);
        };
        if self.unavailable {
            return Ok(false);
        }
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
        // The renderer is rebuilt when the device is lost, but this rasteriser survives it: everything made on the old
        // device (the Vello renderer, the resolve pipeline, the private textures) is dropped with it.
        if self.gpu.as_ref().is_some_and(|gpu| &gpu.device != device) {
            self.gpu = None;
            self.layers = Default::default();
        }
        if self.gpu.is_none() {
            match Gpu::create(device, target.format()) {
                Ok(gpu) => self.gpu = Some(gpu),
                Err(e) => {
                    tracing::warn!("the face falls back to the CPU raster: Vello can't run on this device ({e})");
                    self.unavailable = true;
                    return Ok(false);
                }
            }
        }
        let Some(gpu) = self.gpu.as_mut() else {
            return Err(FaceError::Raster("the Vello state is missing after it was made".to_owned()));
        };
        if gpu.resolve.format() != target.format() {
            gpu.resolve = Resolve::new(device, target.format());
            self.layers = Default::default();
        }
        let layer = &mut self.layers[spec.id as usize];
        layer.ensure_private(device, &gpu.resolve, width, height);
        layer.ensure_target(target);

        // From here the target no longer shows the cached scenes until the repaint below finishes.
        let was_painted = std::mem::replace(&mut layer.painted, false);
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
        let (Some(private), Some((_, level_zero))) = (layer.private.as_ref(), layer.target.as_ref()) else {
            return Err(FaceError::Raster(
                "the private texture or the target view is missing after it was made".to_owned(),
            ));
        };
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: rect[2],
            height: rect[3],
            antialiasing_method: AaConfig::Area,
        };
        gpu.renderer
            .render_to_texture(device, queue, &self.scene, &private.view, &params)
            .map_err(|e| FaceError::Raster(format!("Vello could not render the layer: {e}")))?;
        Resolve::set_origin(queue, &private.origin, rect[0], rect[1]);
        let mut commands =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("face-resolve") });
        gpu.resolve.encode(&mut commands, &private.bind, level_zero, rect);
        queue.submit([commands.finish()]);
        layer.painted = true;
        Ok(true)
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

    fn rasterize_native_sections(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
        sections: &LayerSections,
        native: &mut dyn Any,
    ) -> Result<bool, FaceError> {
        self.paint(spec, list, sections, native)
    }
}
