//! Meshes, their skins and their morph targets (design D1, D4, D8; BY10).
//!
//! **One runtime mesh per primitive per drawing node.** A primitive draws with one material, so each becomes its own
//! [`AvatarMesh`]; a mesh that several nodes draw becomes one set per node, so each copy deforms and is morphed on its
//! own. [`RetainedMesh`] records which runtime meshes each source mesh's primitives became at each node, so export
//! writes every source mesh once.
//!
//! **Skins stay with their meshes.** A skinned primitive keeps its skin's joints as its `joint_bones` and the skin's
//! inverse bind matrices as its own (ADR 0014 §2: per mesh, because glTF skins may give one joint different matrices in
//! different skins). A primitive drawn by a node without a skin becomes a one-joint skin on that node's bone, so a rigid
//! accessory follows its node. A scaled node's scale is folded into the matrices ([`Rig::scale_fix`]); an unscaled
//! file's matrices pass through bit for bit.
//!
//! **Morph targets go sparse**: each dense target keeps exactly the vertices it moves
//! ([`MorphSet::from_dense`] with no tolerance), so the deltas are reproduced exactly.

use std::collections::BTreeSet;

use omuzaki_avatar_api::avatar::{AvatarMesh, ImportedAvatar, MAX_INDEXED};
use omuzaki_avatar_api::retained::{RetainedMesh, RetainedPrimitive};
use omuzaki_char_api::{BoneName, JointRef, PartMesh};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_io_api::{Finding, FirstPersonFlag, IoScene, MeshData, NodeIx, Primitive, Report};
use omuzaki_render_api::MorphSet;

use super::materials::MaterialNames;
use super::skeleton::Rig;

/// How far a vertex's weights may sum away from 1 before they are renormalised (`PartMesh::validate`'s tolerance).
const WEIGHT_TOLERANCE: f32 = 1e-3;

/// Fills `avatar.meshes`, `avatar.first_person` and `avatar.retained.meshes` from the scene's drawing nodes, in node
/// order and then primitive order.
pub fn convert(scene: &IoScene, rig: &Rig, names: &MaterialNames, avatar: &mut ImportedAvatar, report: &mut Report) {
    let mut sources: Vec<RetainedMesh> = scene
        .meshes
        .iter()
        .map(|m| {
            let primitives = m.primitives.iter().map(|p| retained_primitive(p.material.map(|i| i.0))).collect();
            RetainedMesh::new(m.name.clone(), primitives).with_morph_target_names(m.morph_target_names.clone())
        })
        .collect();
    let flags = first_person_flags(scene);
    for (n, node) in scene.nodes.iter().enumerate() {
        let (Some(mesh_ix), Ok(node_ix)) = (node.mesh, u32::try_from(n)) else { continue };
        let Some(mesh) = scene.meshes.get(mesh_ix.index()) else { continue };
        let at = |p: usize| format!("meshes/{}/primitives/{p}", mesh_ix.0);
        let mut made = Vec::with_capacity(mesh.primitives.len());
        for (p, primitive) in mesh.primitives.iter().enumerate() {
            // `MAX_INDEXED` is what a `u16` can name.
            let Ok(index) = u16::try_from(avatar.meshes.len()) else {
                let msg = format!("the model draws more than {MAX_INDEXED} meshes; the rest were left out");
                report.push(Finding::error("avatar.too_many_meshes", msg).at(at(p)));
                return;
            };
            let Some(runtime) =
                runtime_mesh(scene, rig, names, NodeIx(node_ix), primitive, &mesh.morph_target_names, &at(p), report)
            else {
                continue;
            };
            avatar.meshes.push(runtime);
            avatar.first_person.push(flags.get(n).copied().unwrap_or(FirstPersonFlag::Auto));
            made.push(index);
        }
        if let Some(source) = sources.get_mut(mesh_ix.index())
            && made.len() == source.primitives.len()
        {
            *source = std::mem::take(source).with_instance(node_ix, made);
        }
    }
    avatar.retained.meshes = sources;
}

/// The retained record of a primitive drawing material `material`.
fn retained_primitive(material: Option<u32>) -> RetainedPrimitive {
    material.map_or_else(RetainedPrimitive::new, |m| RetainedPrimitive::new().with_material(m))
}

/// The first-person flag of each node, by node index (the file's annotations; `Auto` where it has none).
fn first_person_flags(scene: &IoScene) -> Vec<FirstPersonFlag> {
    let mut flags = vec![FirstPersonFlag::Auto; scene.nodes.len()];
    let annotations = scene.avatar.iter().filter_map(|a| a.first_person.as_ref()).flat_map(|f| &f.annotations);
    for annotation in annotations {
        if let Some(slot) = flags.get_mut(annotation.node.index()) {
            *slot = annotation.flag;
        }
    }
    flags
}

/// The runtime mesh of one primitive drawn at `node`, or `None` (reported) when it cannot be drawn.
#[allow(clippy::too_many_arguments)] // the scene's tables a primitive draws on; grouping them would only rename them
fn runtime_mesh(
    scene: &IoScene,
    rig: &Rig,
    names: &MaterialNames,
    node: NodeIx,
    primitive: &Primitive,
    target_names: &[String],
    at: &str,
    report: &mut Report,
) -> Option<AvatarMesh> {
    let mut geometry = primitive.mesh.clone();
    let skin = scene.nodes.get(node.index()).and_then(|n| n.skin).and_then(|s| scene.skins.get(s.index()));
    let (joint_bones, inverse_bind) = if let Some(skin) = skin {
        let mut bones = Vec::with_capacity(skin.joints.len());
        let mut matrices = Vec::with_capacity(skin.joints.len());
        for (j, &joint) in skin.joints.iter().enumerate() {
            let Some(name) = rig.bone_name(joint).and_then(|n| BoneName::new(n).ok()) else {
                let msg = format!("joint {j} of the skin is node {}, which is not a bone", joint.0);
                report.push(Finding::error("avatar.skin_invalid", msg).at(at));
                return None;
            };
            let matrix = skin.inverse_bind.get(j).copied().unwrap_or(Mat4::IDENTITY);
            bones.push(JointRef::Named(name));
            matrices.push(rig.scale_fix(joint).map_or(matrix, |fix| fix * matrix));
        }
        normalise_weights(&mut geometry, at, report);
        (bones, matrices)
    } else {
        let Some(name) = rig.bone_name(node).and_then(|n| BoneName::new(n).ok()) else {
            debug_assert!(false, "a node that draws a mesh without a skin is always a bone");
            return None;
        };
        let count = geometry.positions.len();
        geometry.joints = vec![[0; 4]; count];
        geometry.weights = vec![[1.0, 0.0, 0.0, 0.0]; count];
        (vec![JointRef::Named(name)], vec![rig.scale_fix(node).unwrap_or(Mat4::IDENTITY)])
    };
    let material = names.of(primitive.material.map(|m| m.0))?;
    let name = scene.nodes.get(node.index()).map_or_else(String::new, |n| n.name.clone());
    let mesh = PartMesh { name, geometry, joint_bones, material, casts_shadow: true, render_order: 0, dynamic: false };
    let mut runtime = AvatarMesh::new(mesh).with_inverse_bind(inverse_bind);
    if !primitive.morph_targets.is_empty() {
        match sparse_morphs(primitive, target_names) {
            Ok(morphs) => runtime = runtime.with_morphs(morphs),
            Err(why) => {
                let msg = format!("the morph targets could not be read ({why}); this mesh's expressions won't move it");
                report.push(Finding::warning("avatar.morphs_dropped", msg).at(at));
            }
        }
    }
    Some(runtime)
}

/// The primitive's dense targets, sparse, named after the file's target names made into unique slugs (`t<i>` where the
/// file names none).
fn sparse_morphs(primitive: &Primitive, target_names: &[String]) -> Result<MorphSet, omuzaki_render_api::RenderError> {
    let mut taken = BTreeSet::new();
    let names: Vec<Slug> = (0..primitive.morph_targets.len())
        .map(|i| target_slug(target_names.get(i).map(String::as_str), i, &mut taken))
        .collect();
    let dense: Vec<(Slug, &[Vec3], &[Vec3])> = primitive
        .morph_targets
        .iter()
        .zip(names)
        .map(|(t, name)| (name, t.positions.as_slice(), t.normals.as_slice()))
        .collect();
    MorphSet::from_dense(primitive.mesh.positions.len(), &dense, 0.0)
}

/// A unique slug for target `index` called `name` in the file.
fn target_slug(name: Option<&str>, index: usize, taken: &mut BTreeSet<String>) -> Slug {
    let cleaned = super::skeleton::slug_stem(name.unwrap_or_default());
    let stem = if cleaned.is_empty() { format!("t{index}") } else { cleaned };
    super::skeleton::unique_slug(&stem, taken)
}

/// Renormalises the weights of vertices whose weights do not sum to 1 within [`WEIGHT_TOLERANCE`] (files store
/// quantised weights), and gives a vertex with no weight at all to its first joint; one finding says how many changed.
fn normalise_weights(geometry: &mut MeshData, at: &str, report: &mut Report) {
    let mut changed = 0usize;
    for weights in &mut geometry.weights {
        let sum: f32 = weights.iter().filter(|w| w.is_finite() && **w > 0.0).sum();
        if (sum - 1.0).abs() <= WEIGHT_TOLERANCE && weights.iter().all(|w| w.is_finite() && *w >= 0.0) {
            continue;
        }
        changed += 1;
        *weights = if sum > 0.0 {
            weights.map(|w| if w.is_finite() && w > 0.0 { w / sum } else { 0.0 })
        } else {
            [1.0, 0.0, 0.0, 0.0]
        };
    }
    if changed > 0 {
        let msg = format!("{changed} vertices had skin weights that did not add up to 1; they were rescaled");
        report.push(Finding::info("avatar.weights_normalised", msg).at(at));
    }
}
