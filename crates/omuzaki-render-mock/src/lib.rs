//! A deterministic CPU renderer for tests and goldens (spec v3 §39 rule 6, "a headless renderer").
//!
//! [`MockRenderer`] implements [`omuzaki_render_api::Renderer`] with a small exact rasteriser: z-buffered triangles,
//! the toon ramp with a tinted shade, rim light, skin treatment, texture overlays (so the face painter's output shows
//! up), inverted-hull outlines, particles drawn with ordered coverage (the reference for `particles::evaluate`), alpha blending, exposure and tone mapping, and integer supersampling. It does not do
//! shadows, bloom, depth of field, grain or grade (its [`Capabilities`] say so, and
//! `FrameReport` shows those passes never ran), and it shades flat per triangle.
//!
//! What it is for: any crate that *uses* a renderer (the stage, the face painter, ops, the golden harness) can build
//! and test on any machine, with identical bytes on every run, no GPU and no window. What it must never do is define
//! behaviour: [`conformance`] holds the checks that both the mock and the real GPU renderer pass, so the mock can't
//! drift from the contract.
//!
//! ```
//! use omuzaki_core::glam::{Mat4, UVec2, Vec3};
//! use omuzaki_render_api::{
//!     AlphaEncoding, Camera, Colour, Instance, Light, Material, Quality, RenderError, Renderer, RendererExt, Scene,
//! };
//! use omuzaki_render_mock::{MockRenderer, shapes};
//!
//! # fn main() -> Result<(), RenderError> {
//! let mut renderer = MockRenderer::new();
//! let mesh = renderer.create_mesh(&shapes::cuboid(Vec3::splat(0.5)))?;
//! let material = renderer.create_material(&Material::toon(Colour::hex(0x00f9_d3c5)))?;
//!
//! let camera = Camera::look_at(Vec3::new(2.0, 1.5, 3.0), Vec3::ZERO, Vec3::Y, Camera::default().projection);
//! let mut scene = Scene::new(camera);
//! scene.lights.push(Light::Directional { to_light: Vec3::new(0.5, 1.0, 0.8), colour: Colour::WHITE, intensity: 2.0, casts_shadow: false });
//! scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
//!
//! let image = renderer.render_image(UVec2::new(64, 64), AlphaEncoding::Straight, &scene, &Quality::live())?;
//! assert_eq!(image.size, UVec2::new(64, 64));
//! # Ok(()) }
//! ```
#![deny(missing_docs)]

mod arena;
pub mod conformance;
#[cfg(not(target_arch = "wasm32"))]
pub mod counting;
mod environment;
mod frame;
mod morph;
pub mod particle_sheet;
mod particles;
mod raster;
mod resources;
mod shade;
pub mod shapes;

use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    Capabilities, EnvironmentDesc, EnvironmentId, FrameReport, GpuTimings, Image, Material, MaterialId, MeshData,
    MeshId, MeshOptions, MeshUsage, MtoonSupport, Painter, ParticleSystemDesc, ParticleSystemId, Pass, PassTimes,
    Quality, RenderError, Renderer, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId, TextureDesc, TextureId,
    TextureUsage, limits,
};

use crate::arena::Arena;
use crate::environment::EnvironmentData;
use crate::frame::{Drawn, Resources, draw_frame};
pub use crate::morph::morph_vertices;
use crate::particles::SystemEntry;
pub use crate::resources::MockNative;
use crate::resources::{MeshEntry, MockPaintTarget, TextureData};

/// The largest texture or target side the mock accepts. Its buffers are plain `Vec`s, so it stays modest.
const MAX_TEXTURE_SIZE: u32 = 8192;

/// The most morph targets one mesh may have in the mock (the 52 face-tracking shapes of a VRM avatar plus the extras avatars add).
const MAX_MORPH_TARGETS: u32 = 256;

/// What a particle system holds on a GPU: one uniform block of its parameters (particles.md D2).
const PARTICLE_SYSTEM_BYTES: u64 = 256;

/// A render target: its description and the last frame drawn into it.
struct TargetEntry {
    desc: TargetDesc,
    last: Option<Image>,
}

/// What the mock reports for a pass that ran. The mock never reads a clock (that would make reports differ between
/// runs), so tests set these to simulate a slow frame for the budget scheduler.
#[derive(Copy, Clone, Default)]
struct PassCost {
    cpu: Seconds,
    gpu: Seconds,
}

/// The deterministic CPU renderer. See the [crate docs](crate).
pub struct MockRenderer {
    capabilities: Capabilities,
    meshes: Arena<MeshEntry>,
    textures: Arena<TextureData>,
    materials: Arena<Material>,
    targets: Arena<TargetEntry>,
    environments: Arena<EnvironmentData>,
    particle_systems: Arena<SystemEntry>,
    costs: [PassCost; Pass::COUNT],
    /// Set by a successful paint, cleared by the next frame: that frame reports `Pass::PaintTextures`.
    painted_since_frame: bool,
}

impl Default for MockRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl MockRenderer {
    /// An empty renderer.
    pub fn new() -> Self {
        Self {
            capabilities: Capabilities {
                backend: "mock-cpu".to_owned(),
                max_texture_size: MAX_TEXTURE_SIZE,
                max_samples: limits::MAX_SAMPLES,
                passes: vec![
                    Pass::PaintTextures,
                    Pass::Opaque,
                    Pass::Outlines,
                    Pass::Particles,
                    Pass::Transparent,
                    Pass::Resolve,
                ],
                shading_programs: Vec::new(),
                gpu_timings: true,
                bit_exact: true,
                dynamic_meshes: true,
                morph_targets: true,
                max_morph_targets: MAX_MORPH_TARGETS,
                mtoon: MtoonSupport::OUTLINE_STYLE,
            },
            meshes: Arena::default(),
            textures: Arena::default(),
            materials: Arena::default(),
            targets: Arena::default(),
            environments: Arena::default(),
            particle_systems: Arena::default(),
            costs: [PassCost::default(); Pass::COUNT],
            painted_since_frame: false,
        }
    }

    /// Makes every future frame report `cpu` and `gpu` time for `pass` whenever it runs, so scheduler tests can
    /// simulate an over-budget frame without any real work being slow.
    pub fn set_pass_cost(&mut self, pass: Pass, cpu: Seconds, gpu: Seconds) {
        self.costs[pass.index()] = PassCost { cpu, gpu };
    }

    fn check_size(&self, size: UVec2, what: &'static str) -> Result<(), RenderError> {
        if size.x == 0 || size.y == 0 {
            return Err(RenderError::invalid(what, format!("size {}x{} has a zero side", size.x, size.y)));
        }
        if size.x > self.capabilities.max_texture_size || size.y > self.capabilities.max_texture_size {
            return Err(RenderError::LimitExceeded { what, limit: u64::from(self.capabilities.max_texture_size) });
        }
        Ok(())
    }

    /// Every texture the material names must be live.
    fn check_textures(&self, material: &Material) -> Result<(), RenderError> {
        material.validate()?;
        material.textures().try_for_each(|t| {
            self.textures.get(t.index(), t.generation()).map(drop).ok_or(RenderError::UnknownHandle { what: "texture" })
        })
    }

    /// The description's own rules, and a live atlas texture.
    fn check_particle_system(&self, desc: &ParticleSystemDesc) -> Result<(), RenderError> {
        desc.validate()?;
        match desc.sprite.texture() {
            Some(t) if self.textures.get(t.index(), t.generation()).is_none() => {
                Err(RenderError::UnknownHandle { what: "texture" })
            }
            _ => Ok(()),
        }
    }

    fn report(&self, frame: u64, drawn: &Drawn) -> FrameReport {
        let (mut cpu, mut gpu) = (PassTimes::default(), PassTimes::default());
        for pass in Pass::ALL.into_iter().filter(|p| drawn.ran[p.index()]) {
            let cost = self.costs[pass.index()];
            cpu.set(pass, cost.cpu);
            gpu.set(pass, cost.gpu);
        }
        FrameReport {
            frame,
            render_size: drawn.render_size,
            cpu,
            gpu: Some(GpuTimings { frame, passes: gpu, painters: None }),
            draw_calls: drawn.draw_calls,
            triangles: drawn.triangles,
            particles: drawn.particles,
        }
    }
}

impl Renderer for MockRenderer {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn stats(&self) -> ResourceStats {
        let mips = |d: &TextureDesc| if d.mips { d.byte_len() as u64 * 4 / 3 } else { d.byte_len() as u64 };
        let gpu_bytes = self.meshes.iter().map(MeshEntry::gpu_bytes).sum::<u64>()
            + self.textures.iter().map(|t| mips(&t.desc)).sum::<u64>()
            + self.targets.iter().map(|t| u64::from(t.desc.size.x) * u64::from(t.desc.size.y) * 8).sum::<u64>()
            + self.environments.iter().map(|e| e.gpu_bytes).sum::<u64>()
            + self.particle_systems.len() as u64 * PARTICLE_SYSTEM_BYTES;
        // Counts are bounded by memory long before they reach u32::MAX.
        #[allow(clippy::cast_possible_truncation)]
        ResourceStats {
            meshes: self.meshes.len() as u32,
            textures: self.textures.len() as u32,
            materials: self.materials.len() as u32,
            targets: self.targets.len() as u32,
            environments: self.environments.len() as u32,
            particle_systems: self.particle_systems.len() as u32,
            gpu_bytes,
        }
    }

    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        self.create_mesh_with(mesh, &MeshOptions::default())
    }

    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        mesh.validate()?;
        if let Some(morphs) = &options.morphs {
            morphs.validate(mesh.positions.len())?;
            if morphs.len() > MAX_MORPH_TARGETS as usize {
                return Err(RenderError::LimitExceeded {
                    what: "morph targets on a mesh",
                    limit: u64::from(MAX_MORPH_TARGETS),
                });
            }
        }
        let entry = MeshEntry::new(mesh.clone(), options.usage, options.morphs.clone());
        let (index, generation) = self.meshes.insert(entry);
        Ok(MeshId::from_parts(index, generation))
    }

    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        let entry =
            self.meshes.get_mut(mesh.index(), mesh.generation()).ok_or(RenderError::UnknownHandle { what: "mesh" })?;
        if entry.usage != MeshUsage::Dynamic {
            return Err(RenderError::invalid("vertex update", "the mesh is static; create it with MeshUsage::Dynamic"));
        }
        let count = entry.data.positions.len();
        if positions.len() != count || normals.len() != count {
            let reason =
                format!("{} positions and {} normals for a mesh of {count} vertices", positions.len(), normals.len());
            return Err(RenderError::invalid("vertex update", reason));
        }
        if positions.iter().chain(normals).any(|v| !v.is_finite()) {
            return Err(RenderError::invalid("vertex update", "positions and normals must be finite"));
        }
        entry.replace_vertices(positions, normals);
        Ok(())
    }

    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        desc.validate()?;
        self.check_size(desc.size, "texture size")?;
        let rgba = match rgba {
            None => vec![0; desc.byte_len()],
            Some(bytes) if bytes.len() == desc.byte_len() => bytes.to_vec(),
            Some(bytes) => {
                let reason = format!(
                    "{} bytes for a {}x{} texture (needs {})",
                    bytes.len(),
                    desc.size.x,
                    desc.size.y,
                    desc.byte_len()
                );
                return Err(RenderError::invalid("texture pixels", reason));
            }
        };
        let (index, generation) = self.textures.insert(TextureData { desc: *desc, rgba });
        Ok(TextureId::from_parts(index, generation))
    }

    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        let data = self
            .textures
            .get(texture.index(), texture.generation())
            .ok_or(RenderError::UnknownHandle { what: "texture" })?;
        if data.desc.usage != TextureUsage::Dynamic {
            return Err(RenderError::invalid(
                "paint target",
                "only dynamic textures can be painted; create it with TextureUsage::Dynamic",
            ));
        }
        let mut target = MockPaintTarget::new(data);
        painter.paint(&mut target)?;
        if let Some(data) = self.textures.get_mut(texture.index(), texture.generation()) {
            data.rgba = target.rgba;
        }
        self.painted_since_frame = true;
        Ok(())
    }

    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        self.check_textures(material)?;
        let (index, generation) = self.materials.insert(material.clone());
        Ok(MaterialId::from_parts(index, generation))
    }

    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.check_textures(material)?;
        let slot = self
            .materials
            .get_mut(id.index(), id.generation())
            .ok_or(RenderError::UnknownHandle { what: "material" })?;
        *slot = material.clone();
        Ok(())
    }

    fn create_environment(&mut self, desc: &EnvironmentDesc) -> Result<EnvironmentId, RenderError> {
        let data = EnvironmentData::new(desc, self.capabilities.max_texture_size)?;
        let (index, generation) = self.environments.insert(data);
        Ok(EnvironmentId::from_parts(index, generation))
    }

    fn create_particle_system(&mut self, desc: &ParticleSystemDesc) -> Result<ParticleSystemId, RenderError> {
        self.check_particle_system(desc)?;
        let (index, generation) = self.particle_systems.insert(SystemEntry::new(desc.clone()));
        Ok(ParticleSystemId::from_parts(index, generation))
    }

    fn update_particle_system(&mut self, id: ParticleSystemId, desc: &ParticleSystemDesc) -> Result<(), RenderError> {
        self.check_particle_system(desc)?;
        let slot = self
            .particle_systems
            .get_mut(id.index(), id.generation())
            .ok_or(RenderError::UnknownHandle { what: "particle system" })?;
        *slot = SystemEntry::new(desc.clone());
        Ok(())
    }

    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
        self.check_size(desc.size, "target size")?;
        let (index, generation) = self.targets.insert(TargetEntry { desc: *desc, last: None });
        Ok(TargetId::from_parts(index, generation))
    }

    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError> {
        self.check_size(size, "target size")?;
        let entry = self
            .targets
            .get_mut(target.index(), target.generation())
            .ok_or(RenderError::UnknownHandle { what: "target" })?;
        entry.desc.size = size;
        entry.last = None;
        Ok(())
    }

    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        let unknown = || RenderError::UnknownHandle { what: resource.kind() };
        match resource {
            ResourceRef::Mesh(id) => self.meshes.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown),
            ResourceRef::Material(id) => {
                self.materials.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown)
            }
            ResourceRef::Target(id) => self.targets.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown),
            ResourceRef::Environment(id) => {
                self.environments.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown)
            }
            ResourceRef::ParticleSystem(id) => {
                self.particle_systems.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown)
            }
            ResourceRef::Texture(id) => {
                if self.textures.get(id.index(), id.generation()).is_none() {
                    return Err(unknown());
                }
                if self.materials.iter().any(|m| m.textures().any(|t| t == id)) {
                    return Err(RenderError::InUse { what: "texture", by: "material" });
                }
                if self.particle_systems.iter().any(|s| s.desc.sprite.texture() == Some(id)) {
                    return Err(RenderError::InUse { what: "texture", by: "particle system" });
                }
                self.textures.remove(id.index(), id.generation()).map(drop).ok_or_else(unknown)
            }
        }
    }

    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        scene.validate()?;
        quality.validate()?;
        let desc = self
            .targets
            .get(target.index(), target.generation())
            .ok_or(RenderError::UnknownHandle { what: "target" })?
            .desc;
        let resources = Resources {
            meshes: &self.meshes,
            textures: &self.textures,
            materials: &self.materials,
            environments: &self.environments,
            particle_systems: &self.particle_systems,
        };
        let drawn = draw_frame(&resources, &desc, scene, quality, self.painted_since_frame)?;
        let report = self.report(scene.frame.index, &drawn);
        self.painted_since_frame = false;
        if let Some(entry) = self.targets.get_mut(target.index(), target.generation()) {
            entry.last = Some(drawn.image);
        }
        Ok(report)
    }

    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        let entry = self
            .targets
            .get(target.index(), target.generation())
            .ok_or(RenderError::UnknownHandle { what: "target" })?;
        entry.last.clone().ok_or_else(|| RenderError::invalid("target", "nothing has been rendered into it yet"))
    }
}
