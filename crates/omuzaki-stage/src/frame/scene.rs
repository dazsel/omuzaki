//! Building the renderer's `Scene` each frame from the uploaded character: instances in render order, joint runs and
//! shadow flags.

use omuzaki_char_api::{BuiltCharacter, PartMesh};
use omuzaki_core::glam::Mat4;
use omuzaki_render_api::{Instance, JointRange, MaterialId, MeshId, Scene};

use crate::error::StageError;
use crate::upload::MeshLookup;

/// What one mesh needs each frame, resolved once so the frame does no lookups by name.
struct Draw {
    mesh: MeshId,
    material: MaterialId,
    casts_shadow: bool,
    order: i16,
    /// The bone behind each entry of the mesh's joint run, in `joint_bones` order. Empty for a rigid or dynamic mesh.
    bones: Box<[Option<usize>]>,
}

/// The bone behind each entry of `mesh`'s joint run: what the scene's joint palette is built from.
pub(crate) fn joint_bones(built: &BuiltCharacter, mesh: &PartMesh) -> Box<[Option<usize>]> {
    mesh.joint_bones.iter().map(|joint| built.rig.bone(joint).map(omuzaki_anim_api::BoneId::index)).collect()
}

/// A palette entry: the skinning matrix of `bone`, or the identity for a joint the skeleton lacks.
pub(crate) fn joint_matrix(bone: Option<usize>, skin: &[Mat4]) -> Mat4 {
    bone.and_then(|b| skin.get(b)).copied().unwrap_or(Mat4::IDENTITY)
}

/// The character's meshes in draw order.
pub(crate) struct ScenePlan {
    draws: Vec<Draw>,
    /// Entries the joint palette takes per frame: the sum of the draws' joint runs.
    joints: usize,
}

impl ScenePlan {
    /// Resolves every mesh of `built` against its upload and the character's skeleton, ordered by `render_order`
    /// (stable, so equal orders keep build order).
    ///
    /// # Errors
    /// [`StageError::NotUploaded`] for a mesh that was not uploaded.
    pub(crate) fn new(built: &BuiltCharacter, uploaded: &impl MeshLookup) -> Result<Self, StageError> {
        let mut keyed = Vec::with_capacity(built.meshes.len());
        for built_mesh in &built.meshes {
            let mesh = &built_mesh.mesh;
            let up = uploaded.mesh(&built_mesh.part, built_mesh.local).ok_or_else(|| StageError::NotUploaded {
                what: "mesh",
                name: format!("{}#{}", built_mesh.part, built_mesh.local),
            })?;
            let bones = joint_bones(built, mesh);
            let order =
                i16::try_from(mesh.render_order.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or_default();
            keyed.push((
                mesh.render_order,
                Draw { mesh: up.mesh, material: up.material, casts_shadow: mesh.casts_shadow, order, bones },
            ));
        }
        keyed.sort_by_key(|(render_order, _)| *render_order);
        let joints = keyed.iter().map(|(_, draw)| draw.bones.len()).sum();
        Ok(Self { draws: keyed.into_iter().map(|(_, draw)| draw).collect(), joints })
    }

    /// Grows the scene's instance and joint buffers to hold this plan's frame, so [`fill`](Self::fill) never allocates
    /// with it. Cold: called when a plan is made for a character that a later frame will fill with.
    pub(crate) fn reserve(&self, scene: &mut Scene) {
        // One more for the ground disc the stage adds after the character.
        scene.instances.reserve((self.draws.len() + 1).saturating_sub(scene.instances.len()));
        scene.joints.reserve(self.joints.saturating_sub(scene.joints.len()));
    }

    /// Replaces the scene's instances and joint palette with this frame's. Lights, camera and the rest stay as the
    /// caller set them. Hot path: the vectors keep their capacity, so after the first frame nothing allocates.
    pub(crate) fn fill(&self, scene: &mut Scene, skin: &[Mat4], root: Mat4) {
        scene.instances.clear();
        scene.joints.clear();
        for draw in &self.draws {
            let mut instance = Instance::new(draw.mesh, draw.material, root);
            instance.casts_shadow = draw.casts_shadow;
            instance.order = draw.order;
            if !draw.bones.is_empty() {
                let start = scene.joints.len();
                scene.joints.extend(draw.bones.iter().map(|bone| joint_matrix(*bone, skin)));
                instance.skin = Some(JointRange {
                    start: u32::try_from(start).unwrap_or(u32::MAX),
                    len: u32::try_from(draw.bones.len()).unwrap_or(u32::MAX),
                });
            }
            scene.instances.push(instance);
        }
    }
}
