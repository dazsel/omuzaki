//! Render targets: the texture a frame ends up in, plus the internal attachments it is drawn with.

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::TargetDesc;

use crate::frame::attachments::Attachments;
use crate::gpu::OUTPUT_FORMAT;
use crate::shared::Shared;

/// A render target.
pub(crate) struct GpuTarget {
    pub(crate) desc: TargetDesc,
    /// The finished picture: sRGB-encoded RGBA8 at the target's size. Shareable with egui or an output.
    pub(crate) output: wgpu::Texture,
    pub(crate) output_view: wgpu::TextureView,
    /// Built on the first frame and whenever the quality changes the internal size or sample count.
    pub(crate) attachments: Option<Attachments>,
    /// Whether a frame has been drawn into the output since it was created or resized.
    pub(crate) has_frame: bool,
}

impl GpuTarget {
    pub(crate) fn new(device: &wgpu::Device, desc: TargetDesc) -> Self {
        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("target"),
            size: wgpu::Extent3d { width: desc.size.x, height: desc.size.y, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: OUTPUT_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Self {
            desc,
            output_view: output.create_view(&wgpu::TextureViewDescriptor::default()),
            output,
            attachments: None,
            has_frame: false,
        }
    }

    /// The attachments for a frame at `size` and `samples`, rebuilt only if they no longer fit.
    pub(crate) fn attachments(&mut self, shared: &Shared, size: UVec2, samples: u32) -> &mut Attachments {
        if self.attachments.as_ref().is_some_and(|a| !a.fits(size, samples)) {
            self.attachments = None;
        }
        self.attachments.get_or_insert_with(|| Attachments::new(shared, size, samples))
    }

    /// Estimated GPU memory: the output plus the internal attachments.
    pub(crate) fn bytes(&self) -> u64 {
        u64::from(self.desc.size.x) * u64::from(self.desc.size.y) * 4 + self.attachments.as_ref().map_or(0, |a| a.bytes)
    }
}
