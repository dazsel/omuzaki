//! [`WgpuRenderer`]: the contract's [`Renderer`] on wgpu.
//!
//! This file is plumbing between the contract and the modules that do the work: handles and limits are checked here,
//! resources are made in `resource/`, the frame is prepared in `frame/` and drawn by the passes in `pass/`.

use std::time::Instant;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{
    Capabilities, EnvironmentDesc, EnvironmentId, FrameReport, Image, Material, MaterialId, MeshData, MeshId,
    MtoonSupport, Painter, Quality, RenderError, Renderer, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId,
    TextureDesc, TextureId, TextureUsage,
};

use crate::caps::{self, CapabilityProfile};
use crate::environment::GpuEnvironment;
use crate::frame::attachments::Attachments;
use crate::frame::graph::FrameGraph;
use crate::frame::{FrameState, FrameView};
use crate::gpu::{Gpu, HeadlessOptions, scope_errors};
use crate::paint::{GpuPaintTarget, PaintLog, Staging};
use crate::pass::{FrameStats, PassCx};
use crate::program;
use crate::readback;
use crate::resource::Resources;
use crate::resource::material::{self, GpuMaterial};
use crate::resource::mesh::GpuMesh;
use crate::resource::target::GpuTarget;
use crate::resource::texture::GpuTexture;
use crate::shared::Shared;

/// The wgpu renderer. Create it headless for tests, goldens and tools, or on the device an application already has.
///
/// One thread owns it (`&mut self`), as the contract requires. Rendering is a pure function of the resources, the scene
/// and the quality, up to the GPU's own rounding (`Capabilities::bit_exact` is false).
pub struct WgpuRenderer {
    shared: Shared,
    capabilities: Capabilities,
    resources: Resources,
    frame: FrameState,
    graph: FrameGraph,
    /// Painters that ran since the last frame, for the `PaintTextures` pass.
    paints: PaintLog,
    /// Pixels staged by the current painter, kept between paints so repainting allocates nothing once warm.
    staging: Staging,
}

impl std::fmt::Debug for WgpuRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WgpuRenderer")
            .field("backend", &self.capabilities.backend)
            .field("resources", &self.resources.stats())
            .finish_non_exhaustive()
    }
}

impl WgpuRenderer {
    /// Opens a GPU of its own, with no window, and builds a renderer on it.
    ///
    /// The device gets the limits of the [`CapabilityProfile`] that `OMUZAKI_RENDER_PROFILE` names (`Native` if unset);
    /// use [`headless_with_profile`](Self::headless_with_profile) to choose in code.
    ///
    /// # Errors
    /// `Backend` if no adapter or device is available or the profile cannot run the renderer, `Invalid` if
    /// `OMUZAKI_RENDER_PROFILE` names no profile, `OutOfMemory` or `DeviceLost` while setting up.
    pub fn headless(options: &HeadlessOptions) -> Result<Self, RenderError> {
        Self::from_gpu(Gpu::open_headless(*options)?)
    }

    /// Like [`headless`](Self::headless), with the device opened under `profile`: exactly its limits and features,
    /// never the adapter's. A profile whose limits the renderer cannot run on (`WebGl2` today) fails here with every
    /// unmet requirement listed, not just the first one wgpu would report.
    ///
    /// # Errors
    /// `Backend` for unmet requirements, an adapter that cannot provide the profile, or no adapter or device.
    pub fn headless_with_profile(options: &HeadlessOptions, profile: CapabilityProfile) -> Result<Self, RenderError> {
        Self::from_gpu(Gpu::open_headless_with(*options, profile)?)
    }

    /// Builds a renderer on a device the caller owns (the egui app's). `adapter` supplies the name and the format
    /// support; the renderer does not take over the device's error or device-lost handlers, so a lost shared device
    /// shows up as the errors every later call returns.
    ///
    /// # Errors
    /// `OutOfMemory` or a `Backend` error while setting up.
    pub fn from_device(adapter: &wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Result<Self, RenderError> {
        caps::require("the device", &device.limits())?;
        Self::from_gpu(Gpu::from_parts(adapter, device, queue))
    }

    fn from_gpu(gpu: Gpu) -> Result<Self, RenderError> {
        // A broken registration (a duplicate name, say) fails creation here rather than at the first draw.
        program::Registry::global()?;
        let device = gpu.device.clone();
        let (shared, frame, graph) = scope_errors(&device, "the renderer", move || {
            let shared = Shared::new(gpu);
            let frame = FrameState::new(&shared.gpu.device, &shared.layouts);
            let graph = FrameGraph::new(&shared);
            Ok((shared, frame, graph))
        })?;
        let capabilities = Capabilities {
            backend: shared.gpu.backend_name.clone(),
            max_texture_size: shared.gpu.max_texture_size,
            max_samples: shared.gpu.max_samples(),
            passes: graph.supported(),
            shading_programs: material::known_programs(),
            gpu_timings: graph.measures_gpu(),
            bit_exact: false,
            dynamic_meshes: false,
            morph_targets: false,
            max_morph_targets: 0,
            mtoon: MtoonSupport::NONE,
        };
        Ok(Self {
            shared,
            capabilities,
            resources: Resources::default(),
            frame,
            graph,
            paints: PaintLog::default(),
            staging: Staging::default(),
        })
    }

    /// The target's output texture: sRGB-encoded RGBA8 at the target's size, valid after a frame has been rendered.
    /// An application can show it directly (egui) or hand it to a live output without a copy. The texture is replaced
    /// by [`resize_target`](Renderer::resize_target), so re-fetch it after one.
    ///
    /// # Errors
    /// `UnknownHandle` if the target is not live.
    pub fn target_texture(&self, target: TargetId) -> Result<wgpu::Texture, RenderError> {
        self.resources
            .targets
            .get(target.index(), target.generation())
            .map(|t| t.output.clone())
            .ok_or(RenderError::UnknownHandle { what: "target" })
    }

    /// Blocks until the GPU has finished everything submitted so far, or for two minutes at most. For tests and
    /// benchmarks; a frame loop never needs it. (A lost device ends the wait without an error here: the next call
    /// reports it.)
    ///
    /// # Errors
    /// `Backend` if the GPU is still busy after the time limit.
    pub fn wait_idle(&self) -> Result<(), RenderError> {
        self.shared.gpu.wait_idle(crate::gpu::WAIT_LIMIT)
    }

    fn check_size(&self, size: UVec2, what: &'static str) -> Result<(), RenderError> {
        if size.x == 0 || size.y == 0 {
            return Err(RenderError::invalid(what, format!("size {}x{} has a zero side", size.x, size.y)));
        }
        let limit = self.capabilities.max_texture_size;
        if size.x > limit || size.y > limit {
            return Err(RenderError::LimitExceeded { what, limit: u64::from(limit) });
        }
        Ok(())
    }

    /// Every texture the material names must be live.
    fn check_material(&self, material: &Material) -> Result<(), RenderError> {
        material.validate()?;
        // A program's parameters must match its declaration; an unknown program is fine (it draws as toon).
        program::Registry::global()?.resolve(&material.shading)?;
        material.textures().try_for_each(|t| {
            self.resources
                .textures
                .get(t.index(), t.generation())
                .map(drop)
                .ok_or(RenderError::UnknownHandle { what: "texture" })
        })
    }
}

/// The internal render size: the target scaled by `render_scale`, never below one pixel.
fn internal_size(target: UVec2, render_scale: f32) -> UVec2 {
    // Validated render scales are 0.25..=4 and targets at most a few thousand pixels, so this is exact enough.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    let scale = |side: u32| ((side as f32 * render_scale).round() as u32).max(1);
    UVec2::new(scale(target.x), scale(target.y))
}

impl Renderer for WgpuRenderer {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn stats(&self) -> ResourceStats {
        self.resources.stats()
    }

    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        mesh.validate()?;
        let gpu = &self.shared.gpu;
        let uploaded = gpu.scoped("a mesh", || GpuMesh::upload(gpu, mesh))?;
        let (index, generation) = self.resources.meshes.insert(uploaded);
        Ok(MeshId::from_parts(index, generation))
    }

    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        desc.validate()?;
        self.check_size(desc.size, "texture size")?;
        if let Some(bytes) = rgba.filter(|b| b.len() != desc.byte_len()) {
            let reason = format!(
                "{} bytes for a {}x{} texture (needs {})",
                bytes.len(),
                desc.size.x,
                desc.size.y,
                desc.byte_len()
            );
            return Err(RenderError::invalid("texture pixels", reason));
        }
        let shared = &self.shared;
        let texture =
            shared.gpu.scoped("a texture", || Ok(GpuTexture::create(&shared.gpu, &shared.mips, desc, rgba)))?;
        let (index, generation) = self.resources.textures.insert(texture);
        Ok(TextureId::from_parts(index, generation))
    }

    fn create_environment(&mut self, desc: &EnvironmentDesc) -> Result<EnvironmentId, RenderError> {
        desc.validate()?;
        if let EnvironmentDesc::Hdri { size, .. } = desc {
            self.check_size(*size, "environment size")?;
        }
        let shared = &self.shared;
        let baked = shared
            .gpu
            .scoped("an environment", || GpuEnvironment::create(&shared.gpu, &shared.layouts.environment, desc))?;
        let (index, generation) = self.resources.environments.insert(baked);
        Ok(EnvironmentId::from_parts(index, generation))
    }

    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        self.shared.gpu.check_alive()?;
        let start = Instant::now();
        let Self { shared, resources, paints, staging, .. } = self;
        let entry = resources
            .textures
            .get(texture.index(), texture.generation())
            .ok_or(RenderError::UnknownHandle { what: "texture" })?;
        if entry.desc.usage != TextureUsage::Dynamic {
            return Err(RenderError::invalid(
                "paint target",
                "only dynamic textures can be painted; create it with TextureUsage::Dynamic",
            ));
        }
        let mut target = GpuPaintTarget::new(&shared.gpu, entry, staging);
        painter.paint(&mut target)?;
        let changed = shared.gpu.scoped("a texture paint", || Ok(target.commit()))?;
        paints.record(texture, changed, start.elapsed());
        Ok(())
    }

    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        self.check_material(material)?;
        let shared = &self.shared;
        let uploaded =
            shared.gpu.scoped("a material", || Ok(GpuMaterial::create(shared, &self.resources.textures, material)))?;
        let (index, generation) = self.resources.materials.insert(uploaded);
        Ok(MaterialId::from_parts(index, generation))
    }

    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.check_material(material)?;
        let Self { shared, resources, .. } = self;
        let Resources { textures, materials, .. } = resources;
        let slot =
            materials.get_mut(id.index(), id.generation()).ok_or(RenderError::UnknownHandle { what: "material" })?;
        shared.gpu.scoped("a material", || {
            slot.update(shared, textures, material);
            Ok(())
        })
    }

    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
        self.check_size(desc.size, "target size")?;
        let gpu = &self.shared.gpu;
        let target = gpu.scoped("a render target", || Ok(GpuTarget::new(&gpu.device, *desc)))?;
        let (index, generation) = self.resources.targets.insert(target);
        Ok(TargetId::from_parts(index, generation))
    }

    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError> {
        self.check_size(size, "target size")?;
        let gpu = &self.shared.gpu;
        let entry = self
            .resources
            .targets
            .get_mut(target.index(), target.generation())
            .ok_or(RenderError::UnknownHandle { what: "target" })?;
        let desc = TargetDesc { size, alpha: entry.desc.alpha };
        *entry = gpu.scoped("a render target", || Ok(GpuTarget::new(&gpu.device, desc)))?;
        Ok(())
    }

    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        let unknown = || RenderError::UnknownHandle { what: resource.kind() };
        let resources = &mut self.resources;
        // The handle dies now. Dropping the wgpu objects frees them once the queue is done with them, which is the
        // deferred release the contract describes: a frame in flight keeps what it uses alive.
        match resource {
            ResourceRef::Mesh(id) => resources.meshes.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown),
            ResourceRef::Material(id) => {
                resources.materials.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown)
            }
            ResourceRef::Target(id) => {
                resources.targets.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown)
            }
            ResourceRef::Texture(id) => {
                if resources.textures.get(id.index(), id.generation()).is_none() {
                    return Err(unknown());
                }
                if resources.materials.iter().any(|m| m.material.textures().any(|t| t == id)) {
                    return Err(RenderError::InUse { what: "texture", by: "material" });
                }
                resources.textures.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown)
            }
            ResourceRef::Environment(id) => {
                resources.environments.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown)
            }
        }
    }

    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        scene.validate()?;
        quality.validate()?;
        let Self { shared, capabilities, resources, frame, graph, paints, .. } = self;
        let (scene_resources, targets) = resources.split();
        let entry = targets
            .get_mut(target.index(), target.generation())
            .ok_or(RenderError::UnknownHandle { what: "target" })?;
        let desc = entry.desc;
        let internal = internal_size(desc.size, quality.render_scale);
        if internal.x > capabilities.max_texture_size || internal.y > capabilities.max_texture_size {
            return Err(RenderError::LimitExceeded {
                what: "internal render size",
                limit: u64::from(capabilities.max_texture_size),
            });
        }
        let samples = shared.gpu.samples_for(quality.samples);

        let gpu = &shared.gpu;
        gpu.scoped("a frame", || {
            // Everything that can fail on the caller's data happens before the GPU is touched, so on error the target
            // keeps its previous frame.
            frame.prepare(&gpu.device, &gpu.queue, &shared.layouts, scene, scene_resources, internal)?;
            let output = entry.output_view.clone();
            let attachments: &mut Attachments = entry.attachments(shared, internal, samples);
            attachments.begin_frame();

            let view = FrameView {
                scene,
                quality,
                internal_size: internal,
                target_size: desc.size,
                samples,
                bind_group: frame.bind_group(),
                draws: &frame.draws,
            };
            let mut encoder =
                gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
            let mut cx = PassCx {
                shared,
                resources: scene_resources,
                frame: &view,
                attachments,
                output: &output,
                output_desc: desc,
                paints,
                encoder: &mut encoder,
                stats: FrameStats::default(),
                extra_cpu: std::time::Duration::ZERO,
            };
            let recorded = graph.record(&mut cx)?;
            let stats = cx.stats;
            graph.resolve_timings(&mut encoder, scene.frame.index, recorded.ran);
            gpu.queue.submit([encoder.finish()]);
            entry.has_frame = true;
            paints.clear();

            // Map callbacks only run while the device is polled; this also collects timings of earlier frames.
            let _ = gpu.device.poll(wgpu::PollType::Poll);
            let gpu_timings = graph.collect_timings();
            Ok(FrameReport {
                frame: scene.frame.index,
                render_size: internal,
                cpu: recorded.cpu,
                gpu: gpu_timings,
                draw_calls: stats.draw_calls,
                triangles: stats.triangles,
            })
        })
    }

    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        let gpu = &self.shared.gpu;
        gpu.check_alive()?;
        let entry = self
            .resources
            .targets
            .get(target.index(), target.generation())
            .ok_or(RenderError::UnknownHandle { what: "target" })?;
        if !entry.has_frame {
            return Err(RenderError::invalid("target", "nothing has been rendered into it yet"));
        }
        gpu.scoped("a read-back", || readback::read_rgba8(gpu, &entry.output, 0, entry.desc.size, entry.desc.alpha))
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_render_api::{AlphaEncoding, Camera, Colour, Instance};

    use super::*;
    use crate::gpu::AdapterPreference;

    fn renderer() -> WgpuRenderer {
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).unwrap()
    }

    #[test]
    fn internal_size_scales_and_never_reaches_zero() {
        assert_eq!(internal_size(UVec2::new(1920, 1080), 1.5), UVec2::new(2880, 1620));
        assert_eq!(internal_size(UVec2::new(64, 64), 0.25), UVec2::splat(16));
        assert_eq!(internal_size(UVec2::new(1, 1), 0.25), UVec2::ONE, "a one-pixel target still renders one pixel");
    }

    #[test]
    fn a_lost_device_is_device_lost_for_every_call_and_never_a_panic() {
        let mut r = renderer();
        let mesh = r.create_mesh(&omuzaki_render_mock::shapes::quad(omuzaki_core::glam::Vec2::ONE)).unwrap();
        let material = r.create_material(&Material::unlit(Colour::WHITE)).unwrap();
        let target = r.create_target(&TargetDesc { size: UVec2::splat(8), alpha: AlphaEncoding::Straight }).unwrap();

        r.shared.gpu.device.destroy();
        // The loss is reported asynchronously; polling delivers it.
        r.wait_idle().unwrap();

        let mut scene = Scene::new(Camera::default());
        scene.instances.push(Instance::new(mesh, material, omuzaki_core::glam::Mat4::IDENTITY));
        assert_eq!(r.render(target, &scene, &Quality::live()).unwrap_err(), RenderError::DeviceLost);
        assert_eq!(
            r.create_target(&TargetDesc { size: UVec2::splat(8), alpha: AlphaEncoding::Straight }),
            Err(RenderError::DeviceLost)
        );
        assert_eq!(r.read_target(target).unwrap_err(), RenderError::DeviceLost);
    }
}
