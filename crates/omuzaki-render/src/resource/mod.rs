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
use crate::morph::{MorphPath, MorphPool, is_morphing};
use material::GpuMaterial;
use mesh::{GpuMesh, Needs};
use omuzaki_render_api::{AlphaMode, ResourceStats, Scene};
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

    /// Brings what each dynamic mesh `scene` draws lags behind its vertices up to date, as far as a pass reads it: the
    /// outline directions of the mesh's own stream when an outlined instance draws it, and the bounds when shadow
    /// fitting (`shadows`: this frame fits a shadow map) or the blended sort reads them. An instance that draws a morph
    /// stream instead reads neither from the mesh (`morph/mod.rs` blends its own), so a mesh that is only ever drawn
    /// morphed pays for none of this, and what it skips stays stale for a frame that does need it. The one exception is the
    /// bounds on the GPU morph path, which grows the mesh's. Handles that miss are left for
    /// [`FrameState::prepare`](crate::frame::FrameState::prepare) to report.
    ///
    /// Outline directions that a dispatch welds are queued on the morph pool, which records them with the frame's other
    /// dispatches.
    pub(crate) fn refresh_dynamic(&mut self, queue: &wgpu::Queue, scene: &Scene, shadows: bool) {
        let Self { meshes, materials, morphs, .. } = self;
        // Welds the last frame queued but never submitted did not run: their meshes lag again.
        morphs.take_welds(|index, generation| {
            if let Some(mesh) = meshes.get_mut(index, generation) {
                mesh.directions_lost();
            }
        });
        let grows_bounds = morphs.path() == MorphPath::Gpu;
        for instance in &scene.instances {
            let Some(material) = materials.get(instance.material.index(), instance.material.generation()) else {
                continue;
            };
            let drawn_morphed = is_morphing(instance, scene, meshes);
            let blended = matches!(material.material.alpha, AlphaMode::Blend);
            let shadowed = shadows && (instance.casts_shadow || material.material.receives_shadows);
            let needs = Needs {
                hull: material.draws_outline() && !drawn_morphed,
                bounds: if drawn_morphed { grows_bounds } else { blended || shadowed },
            };
            if let Some(mesh) = meshes.get_mut(instance.mesh.index(), instance.mesh.generation())
                && mesh.refresh(queue, needs)
            {
                morphs.queue_weld(instance.mesh.index(), instance.mesh.generation());
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
