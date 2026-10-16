//! The viewport's GPU side: the stage's frame loop on the app's own wgpu device, drawn into the panel through an
//! `egui_wgpu` paint callback.
//!
//! The renderer is built once on the device egui draws with ([`WgpuRenderer::from_device`]) and kept in egui's
//! callback resources. Each frame the panel hands the callback a [`Job`]: a newly built character to stage, the camera,
//! the size, and whether anything changed. In `prepare` the callback stages the character the way stills are staged
//! (`omuzaki_stage::ops::render::still`: the light rig, the face, the settle, one tick), and only when something
//! changed redraws the stage into its target with `Stage::redraw`; in `paint` it copies the target into the panel's
//! rect with a one-triangle blit. A frame where nothing changed renders nothing and only copies the last picture.

use std::sync::{Arc, Mutex, PoisonError};

use eframe::egui_wgpu::{self, CallbackResources, CallbackTrait, ScreenDescriptor};
use omuzaki_anim_api::{ChannelBuf, HumanoidPose};
use omuzaki_char_api::BuiltCharacter;
use omuzaki_core::glam::UVec2;
use omuzaki_render::WgpuRenderer;
use omuzaki_render_api::{AlphaEncoding, Camera, Quality, Renderer, Scene, TargetDesc, TargetId};
use omuzaki_stage::frame::face::{FaceRig, SETTLE_STEPS};
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::ops::render::still::StillLight;
use omuzaki_stage::{StageError, UploadOptions};

/// The fixed rate the stage settles at, as stills do, so the viewport's picture is a still's.
const FPS: f32 = 60.0;

/// What the panel asks of the GPU this frame, shared with the callback.
#[derive(Debug)]
pub struct Job {
    /// A newly built character to stage in place of the current one.
    pub built: Option<BuiltCharacter>,
    /// The camera to render with.
    pub camera: Camera,
    /// The target's size in pixels.
    pub size: UVec2,
    /// Something changed since the last render: draw the stage again.
    pub render: bool,
    /// How many times the stage was drawn (not counting frames that only copied the last picture).
    pub renders: u64,
    /// Why the last staging or render failed.
    pub error: Option<String>,
}

impl Default for Job {
    fn default() -> Self {
        Self { built: None, camera: Camera::default(), size: UVec2::ONE, render: true, renders: 0, error: None }
    }
}

/// A job shared between the panel and its callback.
pub type SharedJob = Arc<Mutex<Job>>;

/// The renderer, the stage and the blit, in egui's callback resources.
pub struct Gpu {
    renderer: WgpuRenderer,
    light: StillLight,
    quality: Quality,
    stage: Option<Stage>,
    /// The character as sculpted: what a still shows when nothing animates it (decision 0014 section 4).
    base: HumanoidPose,
    target: Option<(TargetId, UVec2)>,
    blit: Blit,
    /// The target's texture bound for the blit; remade when the target is.
    bound: Option<wgpu::BindGroup>,
}

impl Gpu {
    /// The renderer on `state`'s device, lit by `light` and drawing at `quality`.
    ///
    /// # Errors
    /// The device lacks what the renderer needs, or setting it up failed.
    pub fn new(state: &egui_wgpu::RenderState, light: StillLight, quality: Quality) -> Result<Self, String> {
        let renderer = WgpuRenderer::from_device(&state.adapter, state.device.clone(), state.queue.clone())
            .map_err(|e| e.to_string())?;
        let blit = Blit::new(&state.device, state.target_format);
        Ok(Self { renderer, light, quality, stage: None, base: HumanoidPose::REST, target: None, blit, bound: None })
    }

    /// Installs a `Gpu` for `state` in its callback resources, once; later calls do nothing.
    ///
    /// # Errors
    /// What [`Gpu::new`] reports.
    pub fn install(
        state: &egui_wgpu::RenderState,
        light: impl FnOnce() -> StillLight,
        quality: Quality,
    ) -> Result<(), String> {
        let mut renderer = state.renderer.write();
        if renderer.callback_resources.contains::<Mutex<Self>>() {
            return Ok(());
        }
        let gpu = Self::new(state, light(), quality)?;
        renderer.callback_resources.insert(Mutex::new(gpu));
        Ok(())
    }

    /// The renderer, for a caller that reads the target back (tests).
    pub fn renderer(&mut self) -> &mut WgpuRenderer {
        &mut self.renderer
    }

    /// The target the stage draws into.
    pub fn target(&self) -> Option<TargetId> {
        self.target.map(|(id, _)| id)
    }

    /// Does `job`: resizes the target, stages a new character, and draws the stage when something changed.
    pub fn prepare(&mut self, device: &wgpu::Device, job: &mut Job) {
        if let Err(e) = self.try_prepare(device, job) {
            job.error = Some(e);
        }
    }

    fn try_prepare(&mut self, device: &wgpu::Device, job: &mut Job) -> Result<(), String> {
        let size = job.size.max(UVec2::ONE);
        let render_error = |e: omuzaki_render_api::RenderError| e.to_string();
        let stage_error = |e: StageError| e.to_string();
        let target = match self.target {
            Some((id, at)) if at == size => id,
            Some((id, _)) => {
                self.renderer.resize_target(id, size).map_err(render_error)?;
                self.target = Some((id, size));
                self.bound = None;
                job.render = true;
                id
            }
            None => {
                let id = self
                    .renderer
                    .create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight })
                    .map_err(render_error)?;
                self.target = Some((id, size));
                job.render = true;
                id
            }
        };
        if let Some(built) = job.built.take() {
            if let Some(old) = self.stage.take() {
                old.release(&mut self.renderer).map_err(stage_error)?;
            }
            let (staged, base) =
                stage(&mut self.renderer, built, &self.light, self.quality, target).map_err(stage_error)?;
            self.stage = Some(staged);
            self.base = base;
            job.render = true;
            job.error = None;
        }
        let Some(stage) = self.stage.as_mut() else { return Ok(()) };
        if std::mem::take(&mut job.render) {
            stage.scene_mut().camera = job.camera;
            let input = FrameInput { channels: ChannelBuf::NEUTRAL, pose: self.base, ..FrameInput::default() };
            stage.redraw(&mut self.renderer, &input).map_err(stage_error)?;
            job.renders += 1;
        }
        if self.bound.is_none() {
            let texture = self.renderer.target_texture(target).map_err(render_error)?;
            self.bound = Some(self.blit.bind(device, &texture));
        }
        Ok(())
    }

    /// Copies the target into the pass, if there is a picture yet.
    fn paint(&self, info: &egui::PaintCallbackInfo, pass: &mut wgpu::RenderPass<'static>) {
        let Some(bound) = &self.bound else { return };
        let viewport = info.viewport_in_pixels();
        let clip = info.clip_rect_in_pixels();
        #[allow(clippy::cast_precision_loss, reason = "pixel sizes are far below 2^24")]
        pass.set_viewport(
            viewport.left_px as f32,
            viewport.top_px as f32,
            viewport.width_px as f32,
            viewport.height_px as f32,
            0.0,
            1.0,
        );
        #[allow(clippy::cast_sign_loss, reason = "clamped at zero just before")]
        pass.set_scissor_rect(
            clip.left_px.max(0) as u32,
            clip.top_px.max(0) as u32,
            clip.width_px.max(0) as u32,
            clip.height_px.max(0) as u32,
        );
        pass.set_pipeline(&self.blit.pipeline);
        pass.set_bind_group(0, bound, &[]);
        pass.draw(0..3, 0..1);
    }
}

impl Drop for Gpu {
    /// Gives the stage's resources back to the renderer: a stage must be released, not dropped.
    fn drop(&mut self) {
        if let Some(stage) = self.stage.take()
            && let Err(e) = stage.release(&mut self.renderer)
        {
            tracing::warn!(%e, "releasing the viewport's stage failed");
        }
    }
}

/// Stages `built` on `target` as a still is staged: the light rig's scene, the face bound, the rig settled and one tick
/// run, so a redraw from a camera is the still from that camera. Also returns the base pose the redraws show.
fn stage(
    renderer: &mut dyn Renderer,
    built: BuiltCharacter,
    light: &StillLight,
    quality: Quality,
    target: TargetId,
) -> Result<(Stage, HumanoidPose), StageError> {
    let mut scene = Scene::new(Camera::default());
    scene.lights.clone_from(&light.lights);
    scene.background = light.background;
    scene.post = light.post;
    scene.outline_light = light.outline_light;
    let has_face = built.face.is_some();
    // As stills do: the sculpted pose, not the T-pose of `FrameInput::default()`.
    let mut base = HumanoidPose::REST;
    seed_base_pose(&built.rig, &mut base);
    let mut stage =
        Stage::new(renderer, built, UploadOptions::default(), scene, target, quality, StageClock::fixed(FPS))?;
    let ready = (|| {
        if has_face {
            stage.set_face(renderer, FaceRig::standard())?;
        }
        let frame = FrameInput { pose: base, ..FrameInput::default() };
        // The stage starts in the T-pose; the warm-up has to run under the base pose.
        stage.tick(renderer, &frame)?;
        stage.settle(SETTLE_STEPS);
        stage.tick(renderer, &frame)
    })();
    match ready {
        Ok(_) => Ok((stage, base)),
        Err(e) => {
            if let Err(cleanup) = stage.release(renderer) {
                tracing::warn!(%cleanup, "releasing a stage that failed to start failed too");
            }
            Err(e)
        }
    }
}

/// The blit's pipeline and sampler.
struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Blit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewport blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport blit"),
            bind_group_layouts: &[Some(&layout)],
            ..Default::default()
        });
        let fragment = if format.is_srgb() { "fs_decode" } else { "fs_copy" };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport blit"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(fragment),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // One texel per pixel: nearest keeps the copy exact.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("viewport blit"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        Self { pipeline, layout, sampler }
    }

    fn bind(&self, device: &wgpu::Device, texture: &wgpu::Texture) -> wgpu::BindGroup {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport blit"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }
}

/// The paint callback: does the frame's job in `prepare`, copies the picture in `paint`.
pub struct Callback {
    /// The job of the frame, shared with the panel.
    pub job: SharedJob,
}

impl CallbackTrait for Callback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen: &ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(gpu) = resources.get::<Mutex<Gpu>>() else { return Vec::new() };
        let mut gpu = gpu.lock().unwrap_or_else(PoisonError::into_inner);
        let mut job = self.job.lock().unwrap_or_else(PoisonError::into_inner);
        gpu.prepare(device, &mut job);
        // The renderer submits its own work on the shared queue before egui's, so the picture is ready to sample.
        Vec::new()
    }

    fn paint(
        &self,
        info: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        if let Some(gpu) = resources.get::<Mutex<Gpu>>() {
            gpu.lock().unwrap_or_else(PoisonError::into_inner).paint(&info, pass);
        }
    }
}
