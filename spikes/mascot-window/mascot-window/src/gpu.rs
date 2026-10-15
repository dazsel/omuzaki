//! The wgpu side: instance, surface, composite alpha mode, and a one-pass renderer for the test image.

use std::sync::Arc;

use wgpu::util::DeviceExt as _;
use winit::window::Window;

use crate::alpha::{self, Output, Picked, Transparency};
use crate::scene::{Fit, Scene};

/// The shader source, shared with the unit test that validates it without a GPU.
pub const SHADER: &str = include_str!("shader.wgsl");

/// How to open the GPU and the surface.
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
}

/// What happened to one frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// Drawn and presented.
    Presented,
    /// The window cannot show a frame right now (minimised, occluded, timeout); try again later.
    Skipped,
}

/// Instance, device, surface and pipeline for the test image.
pub struct Renderer {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    image_size: (u32, u32),
    straight: bool,
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
                    "{name}: {} ({:?}); alpha modes {:?}; formats {:?}; present modes {:?}",
                    info.name, info.device_type, caps.alpha_modes, caps.formats, caps.present_modes
                ));
            }
            Err(e) => lines.push(format!("{name}: no adapter ({e})")),
        }
    }
    lines
}

impl Renderer {
    /// Opens the GPU for `window` and prepares to draw `scene`.
    ///
    /// # Errors
    /// See [`GpuError`].
    pub fn new(window: &Arc<Window>, options: &Options, scene: &Scene) -> Result<Self, GpuError> {
        let instance = instance(options);
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            compatible_surface: Some(&surface),
            force_fallback_adapter: options.fallback_adapter,
            power_preference: wgpu::PowerPreference::LowPower,
            ..Default::default()
        }))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(
            adapter.request_device(&wgpu::DeviceDescriptor { label: Some("mascot-window"), ..Default::default() }),
        )?;

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
        let (pipeline, bind_group, params) = pipeline(&device, &queue, format, scene);
        let report = SurfaceReport {
            adapter: info.name,
            backend: info.backend,
            device_type: info.device_type,
            offered: caps.alpha_modes,
            picked,
            output,
            format,
            present_mode,
        };
        Ok(Self {
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            params,
            image_size: (scene.width, scene.height),
            straight: output == Output::Straight,
            report,
        })
    }

    /// What the surface reported when it was opened.
    pub fn report(&self) -> &SurfaceReport {
        &self.report
    }

    /// Whether the window can be expected to be see-through with the picked mode.
    pub fn transparency(&self) -> Transparency {
        self.report.picked.transparency
    }

    /// Reconfigures the swap chain for a new window size.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
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
        let fit = Fit::new((self.config.width, self.config.height), self.image_size);
        self.queue.write_buffer(&self.params, 0, bytemuck::cast_slice(&params_block(&fit, opacity, self.straight)));

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mascot"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        if reconfigure {
            self.surface.configure(&self.device, &self.config);
        }
        Ok(Frame::Presented)
    }
}

/// The uniform block: view (window and image size) then mix (opacity, straight flag).
fn params_block(fit: &Fit, opacity: f32, straight: bool) -> [f32; 8] {
    [
        fit.window.0 as f32,
        fit.window.1 as f32,
        fit.image.0 as f32,
        fit.image.1 as f32,
        opacity.clamp(0.0, 1.0),
        if straight { 1.0 } else { 0.0 },
        0.0,
        0.0,
    ]
}

fn pipeline(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    scene: &Scene,
) -> (wgpu::RenderPipeline, wgpu::BindGroup, wgpu::Buffer) {
    // Rgba8Unorm, not sRGB: the bytes are display-encoded already and the compositor blends them as they are.
    let texture = device.create_texture_with_data(
        queue,
        &wgpu::TextureDescriptor {
            label: Some("mascot image"),
            size: wgpu::Extent3d { width: scene.width, height: scene.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &scene.pixels,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        ..Default::default()
    });
    let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mascot params"),
        contents: bytemuck::cast_slice(&[0.0_f32; 8]),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mascot layout"),
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
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mascot bind group"),
        layout: &layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
            wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
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
    (pipeline, bind_group, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_is_valid_wgsl() {
        let module = naga::front::wgsl::parse_str(SHADER).unwrap();
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap();
    }

    #[test]
    fn params_block_matches_the_uniform_layout() {
        let fit = Fit::new((1920, 1080), (512, 512));
        let block = params_block(&fit, 1.5, true);
        assert_eq!(block.len() * 4, 32, "two vec4<f32>");
        assert_eq!(block[..4], [1920.0, 1080.0, 512.0, 512.0]);
        assert_eq!(block[4], 1.0, "opacity is clamped");
        assert_eq!(block[5], 1.0);
    }
}
