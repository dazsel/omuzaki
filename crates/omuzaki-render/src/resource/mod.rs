//! The renderer's resources: what `create_*` uploads and the handles name.
//!
//! Each kind lives in its own module and owns its GPU objects; this module owns the tables. Passes never reach into the
//! tables by handle: the frame turns a scene's handles into slot indices once ([`crate::frame`]), and passes read
//! slots through [`SceneResources`].

pub(crate) mod material;
pub(crate) mod mesh;
pub(crate) mod mips;
pub(crate) mod premultiply;
pub(crate) mod target;
pub(crate) mod texture;

use crate::arena::Arena;
use crate::environment::GpuEnvironment;
use crate::morph::MorphPool;
use material::GpuMaterial;
use mesh::GpuMesh;
use omuzaki_render_api::{ResourceStats, Scene};
use target::GpuTarget;
use texture::GpuTexture;

/// Every live resource, by kind.
#[derive(Default)]
pub(crate) struct Resources {
    pub(crate) meshes: Arena<GpuMesh>,
    pub(crate) textures: Arena<GpuTexture>,
    pub(crate) materials: Arena<GpuMaterial>,
    pub(crate) targets: Arena<GpuTarget>,
    pub(crate) environments: Arena<GpuEnvironment>,
    /// The per-instance morphed vertex streams of the frame (`morph.rs`).
    pub(crate) morphs: MorphPool,
}

/// The resources a scene draws with, read-only: meshes, textures, materials and environments. Split from the targets so a frame can
/// hold one target mutably while its passes read the rest.
#[derive(Copy, Clone)]
pub(crate) struct SceneResources<'a> {
    pub(crate) meshes: &'a Arena<GpuMesh>,
    pub(crate) textures: &'a Arena<GpuTexture>,
    pub(crate) materials: &'a Arena<GpuMaterial>,
    pub(crate) environments: &'a Arena<GpuEnvironment>,
    pub(crate) morphs: &'a MorphPool,
}

impl Resources {
    /// Borrows the scene resources and the targets separately.
    pub(crate) fn split(&mut self) -> (SceneResources<'_>, &mut Arena<GpuTarget>) {
        let scene = SceneResources {
            meshes: &self.meshes,
            textures: &self.textures,
            materials: &self.materials,
            environments: &self.environments,
            morphs: &self.morphs,
        };
        (scene, &mut self.targets)
    }

    /// Brings the outline directions and shadow bounds of every dynamic mesh `scene` draws up to date. The bounds are
    /// read by shadow fitting (casters and receivers alike) and the blended sort, the directions only by the outline
    /// pass, so a mesh that is not outlined never pays for its directions. Handles that miss are left for
    /// [`FrameState::prepare`](crate::frame::FrameState::prepare) to report.
    pub(crate) fn refresh_dynamic(&mut self, queue: &wgpu::Queue, scene: &Scene) {
        for instance in &scene.instances {
            let outlined = self
                .materials
                .get(instance.material.index(), instance.material.generation())
                .is_some_and(GpuMaterial::draws_outline);
            if let Some(mesh) = self.meshes.get_mut(instance.mesh.index(), instance.mesh.generation()) {
                mesh.refresh(queue, outlined);
            }
        }
    }

    /// Counts and estimated GPU memory of everything live.
    pub(crate) fn stats(&self) -> ResourceStats {
        let gpu_bytes = self.meshes.iter().map(|m| m.bytes).sum::<u64>()
            + self.textures.iter().map(|t| t.bytes).sum::<u64>()
            + self.materials.iter().map(|_| material::UNIFORM_BYTES).sum::<u64>()
            + self.targets.iter().map(GpuTarget::bytes).sum::<u64>()
            + self.environments.iter().map(|e| e.bytes).sum::<u64>()
            + self.morphs.bytes();
        // Counts are bounded by memory long before they reach u32::MAX.
        #[allow(clippy::cast_possible_truncation)]
        ResourceStats {
            meshes: self.meshes.len() as u32,
            textures: self.textures.len() as u32,
            materials: self.materials.len() as u32,
            targets: self.targets.len() as u32,
            environments: self.environments.len() as u32,
            gpu_bytes,
        }
    }
}
