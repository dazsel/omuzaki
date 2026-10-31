//! The avatar's part of the renderer's `Scene` each frame: one instance per mesh in draw order, its joint run (each
//! bone's model-space matrix times the mesh's own inverse bind for that joint) and its morph weights.
//!
//! Meshes with the same joint run share it in the palette. A file's primitives usually share one skin (a `VRoid` face
//! is about ten primitives on one skin of 150 or more joints), so one run per mesh would pass the scene's 2048 joint
//! matrices on an ordinary model; one run per skin keeps the palette at the skeleton's size.

use omuzaki_anim_api::{MAX_BONES, Pose};
use omuzaki_avatar::buffers::MorphWeights;
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_char_api::JointRef;
use omuzaki_core::glam::Mat4;
use omuzaki_render_api::limits::MAX_JOINTS;
use omuzaki_render_api::{Instance, JointRange, MaterialId, MeshId, MorphRange, Scene};

use super::upload::AvatarUpload;
use crate::error::StageError;
use crate::frame::smear::Trails;

/// One joint of a mesh's run: the bone that moves it and the mesh's inverse bind for it.
#[derive(Copy, Clone, Debug)]
struct Joint {
    bone: usize,
    inverse_bind: Mat4,
}

/// What one mesh needs each frame, resolved once.
#[derive(Debug)]
struct Draw {
    mesh: MeshId,
    material: MaterialId,
    casts_shadow: bool,
    order: i16,
    /// Its run in the palette, `None` for a mesh without joints.
    skin: Option<JointRange>,
    morph: Option<MorphRange>,
}

/// The avatar's meshes in draw order, with everything a frame fills from.
#[derive(Debug)]
pub(crate) struct AvatarPlan {
    draws: Vec<Draw>,
    /// The palette per frame: every distinct run, one after the other, as the draws' ranges point into it.
    joints: Box<[Joint]>,
    /// Morph weights per frame.
    weights: usize,
}

/// The bone a mesh's joint names in `avatar`'s skeleton.
fn bone_of(avatar: &ImportedAvatar, joint: &JointRef) -> Option<usize> {
    let id = match joint {
        JointRef::Humanoid(bone) => avatar.skeleton.humanoid_bone(*bone),
        JointRef::Named(name) => avatar.skeleton.find(name.as_str()),
    }?;
    (id.index() < MAX_BONES).then_some(id.index())
}

impl AvatarPlan {
    /// Resolves every mesh of `avatar` against its upload, its skeleton and the evaluator's weight layout, ordered by
    /// `render_order` (stable, so equal orders keep file order).
    ///
    /// # Errors
    /// [`StageError::Avatar`] for a joint naming a bone the skeleton lacks or a joint run without its inverse binds
    /// (which [`ImportedAvatar::validate`] refuses), or an upload of another avatar.
    pub(crate) fn new(
        avatar: &ImportedAvatar,
        upload: &AvatarUpload,
        weights: &MorphWeights,
    ) -> Result<Self, StageError> {
        let uploaded = upload.meshes();
        if uploaded.len() != avatar.meshes.len() || weights.meshes() != avatar.meshes.len() {
            return Err(StageError::Avatar { reason: "the upload or the weights were made for another avatar".into() });
        }
        let mut keyed = Vec::with_capacity(avatar.meshes.len());
        let mut palette: Vec<Joint> = Vec::new();
        let mut runs: Vec<JointRange> = Vec::new();
        for (index, (avatar_mesh, up)) in avatar.meshes.iter().zip(uploaded).enumerate() {
            let mesh = &avatar_mesh.mesh;
            if avatar_mesh.inverse_bind.len() != mesh.joint_bones.len() {
                return Err(StageError::Avatar {
                    reason: format!(
                        "mesh {index} ('{}') has {} joints and {} inverse binds",
                        mesh.name,
                        mesh.joint_bones.len(),
                        avatar_mesh.inverse_bind.len()
                    ),
                });
            }
            let joints = mesh
                .joint_bones
                .iter()
                .zip(&avatar_mesh.inverse_bind)
                .map(|(joint, inverse_bind)| {
                    bone_of(avatar, joint).map(|bone| Joint { bone, inverse_bind: *inverse_bind }).ok_or_else(|| {
                        StageError::Avatar {
                            reason: format!(
                                "mesh {index} ('{}') is skinned to {joint:?}, which the skeleton lacks",
                                mesh.name
                            ),
                        }
                    })
                })
                .collect::<Result<Vec<Joint>, _>>()?;
            let skin = (!joints.is_empty()).then(|| share(&mut palette, &mut runs, &joints));
            let morph = weights
                .range(index)
                .filter(|range| !range.is_empty())
                .map(|range| MorphRange { start: range.start, len: range.end - range.start });
            let order =
                i16::try_from(mesh.render_order.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).unwrap_or_default();
            keyed.push((
                mesh.render_order,
                Draw { mesh: up.mesh, material: up.material, casts_shadow: mesh.casts_shadow, order, skin, morph },
            ));
        }
        keyed.sort_by_key(|(render_order, _)| *render_order);
        Ok(Self {
            draws: keyed.into_iter().map(|(_, draw)| draw).collect(),
            joints: palette.into_boxed_slice(),
            weights: weights.as_slice().len(),
        })
    }

    /// Grows the scene's buffers to hold this plan's frame, so [`fill`](Self::fill) never allocates. Cold.
    pub(crate) fn reserve(&self, scene: &mut Scene) {
        // One more for the ground disc the stage adds after the avatar, and the palette twice over for a smear's
        // trailing pose.
        scene.instances.reserve((self.draws.len() + 1).saturating_sub(scene.instances.len()));
        scene.joints.reserve((2 * self.joints.len()).saturating_sub(scene.joints.len()));
        scene.morph_weights.reserve(self.weights.saturating_sub(scene.morph_weights.len()));
    }

    /// Replaces the scene's instances, joint palette and morph weights with this frame's: `world` is the solved
    /// skeleton in model space, `weights` the evaluator's output, `root` the avatar's world matrix. Hot path: after
    /// [`reserve`](Self::reserve) nothing allocates.
    pub(crate) fn fill(&self, scene: &mut Scene, world: &Pose, weights: &MorphWeights, root: Mat4) {
        scene.instances.clear();
        scene.joints.clear();
        scene.morph_weights.clear();
        scene.morph_weights.extend_from_slice(weights.as_slice());
        scene.joints.extend(self.joints.iter().map(|j| world.local[j.bone].to_mat4() * j.inverse_bind));
        for draw in &self.draws {
            let mut instance = Instance::new(draw.mesh, draw.material, root);
            instance.casts_shadow = draw.casts_shadow;
            instance.order = draw.order;
            instance.morph = draw.morph;
            instance.skin = draw.skin;
            scene.instances.push(instance);
        }
    }

    /// Gives the skinned instances a [`fill`](Self::fill) just made a smear along `past`, the palette of an earlier
    /// frame (as long as this one's), carried into this frame's model space by `carry`: the trailing palette follows
    /// the scene's own, each instance's run at the same place in it. Left as posed when it would not fit
    /// ([`MAX_JOINTS`]). Hot path: allocation-free after [`reserve`](Self::reserve).
    pub(crate) fn smear(scene: &mut Scene, past: &[Mat4], carry: Mat4, trails: &Trails) {
        let own = scene.joints.len();
        let Ok(offset) = u32::try_from(own) else {
            return;
        };
        if past.len() != own || 2 * own > MAX_JOINTS {
            return;
        }
        let Scene { instances, joints, .. } = scene;
        joints.extend(past.iter().map(|m| carry * *m));
        for instance in instances.iter_mut() {
            if let Some(skin) = instance.skin {
                instance.smear = Some(trails.smear(JointRange { start: skin.start + offset, len: skin.len }));
            }
        }
    }
}

/// The range of `joints` in `palette`: an earlier run with the same bones and inverse binds if there is one, else
/// `joints` added at the end. `runs` holds the ranges added so far.
fn share(palette: &mut Vec<Joint>, runs: &mut Vec<JointRange>, joints: &[Joint]) -> JointRange {
    let same = |run: &&JointRange| {
        let start = run.start as usize;
        palette.get(start..start + run.len as usize).is_some_and(|earlier| {
            earlier.len() == joints.len()
                && earlier.iter().zip(joints).all(|(a, b)| a.bone == b.bone && a.inverse_bind == b.inverse_bind)
        })
    };
    if let Some(run) = runs.iter().find(same) {
        return *run;
    }
    let run = JointRange {
        start: u32::try_from(palette.len()).unwrap_or(u32::MAX),
        len: u32::try_from(joints.len()).unwrap_or(u32::MAX),
    };
    palette.extend_from_slice(joints);
    runs.push(run);
    run
}
