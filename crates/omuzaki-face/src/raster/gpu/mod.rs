//! The GPU rasteriser: draws a `DrawList` through Vello. [`SceneEncoder`] is the CPU half, which turns a list into a
//! `vello::Scene` and does no GPU work; [`VelloRasterizer`] renders that scene and resolves it into the renderer's face
//! texture. Strokes beyond the basic mapping (hairline parity) are [`stroke`]'s.

pub mod clip;
pub mod fill;
pub mod gradient;
pub mod resolve;
mod snap;
pub mod stroke;

use std::any::Any;
use std::future::Future;
use std::task::{Context, Poll, Waker};

use omuzaki_core::glam::Affine2;
use omuzaki_face_api::{Cap, Cmd, DrawList, FaceError, FaceLayerSpec, Join};
use vello::kurbo::{Affine, BezPath, Cap as KCap, Join as KJoin, Rect, Stroke};
use vello::peniko::Color;
use vello::peniko::{ColorStop, Fill};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

use self::clip::{OpenStack, Transforms};
use self::fill::{Bounds, fill_bez};
use self::gradient::brush;
use self::resolve::Resolve;
use self::snap::snapped_path;
use crate::layer::GpuRasterizer;

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
        for (index, cmd) in list.cmds().iter().enumerate() {
            let current = self.transforms.top().map_or(to_px, |t| *t);
            self.command(index, cmd, current, list, scene)?;
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

/// Vello's private render target, with the resolve's bind group for it. Reused until the layer changes size.
struct Private {
    size: (u32, u32),
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

/// Everything that needs a device. Created on the first paint, because Vello compiles its pipelines when its renderer is
/// created.
struct Gpu {
    renderer: Renderer,
    resolve: Resolve,
    private: Option<Private>,
}

/// The Vello rasteriser: encodes a list with [`SceneEncoder`], renders it with Vello into a private straight-alpha
/// texture, and resolves that into the renderer's face texture (level 0 only; the renderer rebuilds the mips).
///
/// Keep one per layer, and use it with one device only: the Vello renderer and the private texture belong to the device
/// of the first paint.
///
/// It returns `Ok(false)`, so the CPU raster draws, when `native` is not the renderer's `(Device, Queue, Texture)` (the
/// mock renderer's targets, another backend) and when Vello's renderer can't be created on the device: that is logged once
/// and never retried. Don't register it on a software adapter such as lavapipe: those check correctness only and are far
/// slower than the CPU raster (decision 7.3). Registering it is the stage's job.
#[derive(Default)]
pub struct VelloRasterizer {
    encoder: SceneEncoder,
    scene: Scene,
    gpu: Option<Gpu>,
    /// Vello's renderer could not be created: every paint declines.
    unavailable: bool,
}

impl VelloRasterizer {
    /// A rasteriser with nothing allocated on the GPU yet.
    pub fn new() -> Self {
        Self::default()
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
            renderer,
            resolve: Resolve::new(device, format),
            private: None,
        });
        let rejected = ready(scope.pop()).flatten();
        match (made, rejected) {
            (Ok(_), Some(error)) => Err(error.to_string()),
            (Ok(gpu), None) => Ok(gpu),
            (Err(error), _) => Err(error.to_string()),
        }
    }

    /// Rebuilds the resolve if the target's format changed.
    fn fit_format(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        if self.resolve.format() != format {
            self.resolve = Resolve::new(device, format);
            self.private = None;
        }
    }

    /// Makes the private texture for a `width` x `height` layer if there is none of that size.
    fn ensure_private(&mut self, device: &wgpu::Device, width: u32, height: u32) {
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
            let bind = self.resolve.bind(device, &view);
            self.private = Some(Private { size: (width, height), view, bind });
        }
    }
}

impl GpuRasterizer for VelloRasterizer {
    fn rasterize_native(
        &mut self,
        spec: &FaceLayerSpec,
        list: &DrawList,
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
        self.scene.reset();
        self.encoder.encode(spec, list, spec.px_per_m, &mut self.scene)?;
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
        gpu.fit_format(device, target.format());
        gpu.ensure_private(device, width, height);
        let Gpu { renderer, resolve, private } = gpu;
        let Some(private) = private.as_ref() else {
            return Err(FaceError::Raster("the private texture is missing after it was made".to_owned()));
        };
        let params =
            RenderParams { base_color: Color::TRANSPARENT, width, height, antialiasing_method: AaConfig::Area };
        renderer
            .render_to_texture(device, queue, &self.scene, &private.view, &params)
            .map_err(|e| FaceError::Raster(format!("Vello could not render the layer: {e}")))?;
        let mut commands =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("face-resolve") });
        resolve.encode(&mut commands, &private.bind, target);
        queue.submit([commands.finish()]);
        Ok(true)
    }
}
