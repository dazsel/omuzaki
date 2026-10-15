//! The wgpu side: instance, surface, composite alpha mode, and a one-pass renderer for the test image.

use std::sync::Arc;

use winit::window::Window;

use crate::alpha::{self, Output, Picked};
use crate::scene_pass::{self, ScenePass};
use crate::timing::GpuTimer;

/// The shader source, shared with the unit test that validates it without a GPU.
pub const SHADER: &str = include_str!("shader.wgsl");

/// How to open the GPU and the surface.
// One bool per switch a run can turn on: a struct of switches is what an options block is.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct Options {
    /// Backends the instance may use.
    pub backends: wgpu::Backends,
    /// A requested composite alpha mode, or `None` to pick the best offered.
    pub alpha: Option<wgpu::CompositeAlphaMode>,
    /// Forced shader output convention, or `None` to follow the mode.
    pub output: Option<Output>,
    /// Use the CPU adapter (`WARP`, `llvmpipe`, `SwiftShader`) where the platform has one.
    pub fallback_adapter: bool,
    /// Windows DX12 only: build the swap chain on a `DirectComposition` visual, the only way wgpu offers alpha there.
    pub dx12_visual: bool,
    /// Move the circling disc and the frame stamp every frame (the cost run) instead of freezing the picture.
    pub animate: bool,
    /// Ask for GPU timestamp queries around the two passes, where the adapter has them.
    pub timestamps: bool,
}

/// Why opening the GPU or drawing failed.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// The surface could not be created for the window.
    #[error("creating the surface: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),
    /// No adapter can present to the window.
    #[error("no GPU adapter for this window and backend selection: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    /// The adapter refused the device.
    #[error("opening the GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
    /// The requested alpha mode is not offered.
    #[error(transparent)]
    Alpha(#[from] alpha::Unsupported),
    /// The surface reported no usable format.
    #[error("the surface offers no texture format")]
    NoFormat,
    /// The driver rejected the frame.
    #[error("the surface rejected the frame (validation error)")]
    Frame,
}

/// What the surface and adapter reported, printed so a run on a real machine can be pasted into the research page.
#[derive(Clone, Debug)]
pub struct SurfaceReport {
    /// Adapter name.
    pub adapter: String,
    /// Backend of the adapter.
    pub backend: wgpu::Backend,
    /// Device type (discrete, integrated, CPU).
    pub device_type: wgpu::DeviceType,
    /// Driver name and version as the adapter reports them.
    pub driver: String,
    /// Every composite alpha mode the surface offered.
    pub offered: Vec<wgpu::CompositeAlphaMode>,
    /// The mode in use.
    pub picked: Picked,
    /// The shader output convention in use.
    pub output: Output,
    /// Swap chain format.
    pub format: wgpu::TextureFormat,
    /// Present mode.
    pub present_mode: wgpu::PresentMode,
    /// Whether GPU timestamp queries are active.
    pub gpu_timestamps: bool,
}

/// What happened to one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Drawn and presented.
    Presented,
    /// The window cannot show a frame right now (minimised, occluded, timeout); try again later.
    Skipped,
}

/// Instance, device, surface and the two passes: the picture into an offscreen texture, then a copy to the swap chain.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    scene_pass: ScenePass,
    scene_view: wgpu::TextureView,
    blit: Blit,
    straight: bool,
    animate: bool,
    frame: u32,
    timer: Option<GpuTimer>,
    report: SurfaceReport,
}

fn instance(options: &Options) -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = options.backends;
    if options.dx12_visual {
        descriptor.backend_options.dx12.presentation_system = wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    wgpu::Instance::new(descriptor)
}

/// Lists the composite alpha modes each backend offers for `window`, without drawing. Backends that cannot open a
/// surface or adapter are reported with the reason.
pub fn probe_backends(window: &Arc<Window>, base: &Options) -> Vec<String> {
    let all = [
        ("vulkan", wgpu::Backends::VULKAN),
        ("dx12", wgpu::Backends::DX12),
        ("metal", wgpu::Backends::METAL),
        ("gl", wgpu::Backends::GL),
    ];
    let mut lines = Vec::new();
    for (name, backends) in all {
        let options = Options { backends, ..base.clone() };
        let instance = instance(&options);
        let surface = match instance.create_surface(window.clone()) {
            Ok(s) => s,
            Err(e) => {
                lines.push(format!("{name}: no surface ({e})"));
                continue;
            }
        };
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            force_fallback_adapter: options.fallback_adapter,
            ..Default::default()
        }));
        match adapter {
            Ok(adapter) => {
                let info = adapter.get_info();
                let caps = surface.get_capabilities(&adapter);
                lines.push(format!(
                    "{name}: {} ({:?}, driver {}); alpha modes {:?}; formats {:?}; present modes {:?}",
                    info.name, info.device_type, info.driver, caps.alpha_modes, caps.formats, caps.present_modes
                ));
            }
            Err(e) => lines.push(format!("{name}: no adapter ({e})")),
        }
    }
    lines
}

impl Renderer {
    /// Opens the GPU for `window` and prepares to draw the picture at the window's size.
    ///
    /// # Errors
    /// See [`GpuError`].
    pub fn new(window: &Arc<Window>, options: &Options) -> Result<Self, GpuError> {
        let instance = instance(options);
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            force_fallback_adapter: options.fallback_adapter,
            power_preference: wgpu::PowerPreference::LowPower,
            ..Default::default()
        }))?;
        let info = adapter.get_info();
        let timestamps = options.timestamps && adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("mascot-window"),
            required_features: if timestamps { wgpu::Features::TIMESTAMP_QUERY } else { wgpu::Features::empty() },
            ..Default::default()
        }))?;

        let caps = surface.get_capabilities(&adapter);
        let picked = alpha::pick(&caps.alpha_modes, options.alpha)?;
        // The picture is already display-encoded, so a non-sRGB format passes the bytes through unchanged.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .or_else(|| caps.formats.first().copied())
            .ok_or(GpuError::NoFormat)?;
        let size = window.inner_size();
        let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Fifo) {
            wgpu::PresentMode::Fifo
        } else {
            caps.present_modes.first().copied().unwrap_or(wgpu::PresentMode::Fifo)
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: picked.mode,
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        let output = alpha::output_for(picked.mode, options.output);
        let scene_pass = ScenePass::new(&device);
        let scene_texture = scene_pass::target(&device, (config.width, config.height));
        let scene_view = scene_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let blit = Blit::new(&device, format, &scene_view);
        let report = SurfaceReport {
            adapter: info.name,
            backend: info.backend,
            device_type: info.device_type,
            driver: info.driver,
            offered: caps.alpha_modes,
            picked,
            output,
            format,
            present_mode,
            gpu_timestamps: timestamps,
        };
        let timer = timestamps.then(|| GpuTimer::new(&device, &queue));
        Ok(Self {
            surface,
            device,
            queue,
            config,
            scene_pass,
            scene_view,
            blit,
            straight: output == Output::Straight,
            animate: options.animate,
            frame: 0,
            timer,
            report,
        })
    }

    /// What the surface reported when it was opened.
    pub fn report(&self) -> &SurfaceReport {
        &self.report
    }

    /// Takes the GPU time of the frames measured since the last call (the two passes, in order, start to end).
    pub fn take_gpu_times(&mut self) -> Vec<f64> {
        self.timer.as_mut().map(GpuTimer::take).unwrap_or_default()
    }

    /// Reconfigures the swap chain for a new window size and redraws the picture at that size.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        let texture = scene_pass::target(&self.device, (width, height));
        self.scene_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.blit.rebind(&self.device, &self.scene_view);
    }

    /// Draws and presents one frame at `opacity` (0..=1).
    ///
    /// # Errors
    /// [`GpuError::Frame`] if the driver reports a validation failure.
    pub fn render(&mut self, opacity: f32) -> Result<Frame, GpuError> {
        let (frame, reconfigure) = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => (frame, false),
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => (frame, true),
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return Ok(Frame::Skipped),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(Frame::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Validation => return Err(GpuError::Frame),
        };
        let size = (self.config.width, self.config.height);
        // Frozen unless animating: the click-through mask was read from the frozen picture.
        let motion = if self.animate { (self.frame as f32 * 0.05, self.frame) } else { (0.0, 0) };
        self.frame = self.frame.wrapping_add(1);
        self.blit.set_params(&self.queue, opacity, self.straight);

        let slot = self.timer.as_mut().and_then(|t| t.begin(&self.device));
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let writes = |end: bool| {
            let (timer, slot) = (self.timer.as_ref()?, slot?);
            Some(timer.writes(slot, end))
        };
        self.scene_pass.record(&self.queue, &mut encoder, &self.scene_view, size, motion, writes(false));
        self.blit.record(&mut encoder, &view, writes(true));
        if let (Some(timer), Some(slot)) = (self.timer.as_mut(), slot) {
            timer.resolve(&mut encoder, slot);
        }
        self.queue.submit([encoder.finish()]);
        if let (Some(timer), Some(slot)) = (self.timer.as_mut(), slot) {
            timer.map(slot);
        }
        self.queue.present(frame);
        if reconfigure {
            self.surface.configure(&self.device, &self.config);
        }
        Ok(Frame::Presented)
    }
}

/// The pass that copies the offscreen picture to the swap chain with the opacity applied.
struct Blit {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
}

impl Blit {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, image: &wgpu::TextureView) -> Self {
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mascot params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mascot layout"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
            ],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mascot shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mascot pipeline layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("mascot pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // No blending: the shader output replaces the (transparent) clear, and the compositor does the blending.
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let bind_group = bind_group(device, &layout, image, &params);
        Self { pipeline, layout, bind_group, params }
    }

    fn rebind(&mut self, device: &wgpu::Device, image: &wgpu::TextureView) {
        self.bind_group = bind_group(device, &self.layout, image, &self.params);
    }

    fn set_params(&self, queue: &wgpu::Queue, opacity: f32, straight: bool) {
        queue.write_buffer(&self.params, 0, bytemuck::cast_slice(&params_block(opacity, straight)));
    }

    fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        timestamps: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mascot"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            timestamp_writes: timestamps,
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

fn bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    image: &wgpu::TextureView,
    params: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mascot bind group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(image) },
            wgpu::BindGroupEntry { binding: 1, resource: params.as_entire_binding() },
        ],
    })
}

/// The uniform block: opacity, then the straight-colour flag, padded to a `vec4<f32>`.
fn params_block(opacity: f32, straight: bool) -> [f32; 4] {
    [opacity.clamp(0.0, 1.0), if straight { 1.0 } else { 0.0 }, 0.0, 0.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blit_shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(SHADER).unwrap();
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap();
    }

    #[test]
    fn params_block_matches_the_uniform_layout() {
        let block = params_block(1.5, true);
        assert_eq!(block.len() * 4, 16, "one vec4<f32>");
        assert_eq!(block, [1.0, 1.0, 0.0, 0.0], "opacity is clamped");
    }
}
