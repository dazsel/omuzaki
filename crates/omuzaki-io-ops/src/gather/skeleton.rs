//! The skeleton half of the gather: the character's bones (humanoid, hair, cloth and accessory) as nodes, and the
//! joint and inverse-bind tables that skin the meshes to them.
//!
//! Bone `i` is node `i` ([`bone_node`]): the gather puts the bones first and in the skeleton's own order, which is
//! already parents-first, so nothing downstream needs a table to find a bone's node.

use std::collections::HashSet;

use omuzaki_anim_api::{BoneDef, BoneId, HumanoidBone, Skeleton};
use omuzaki_char_api::{CharacterRig, JointRef, MeshData, PartMesh};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{HumanBoneName, Node, NodeIx, Transform};

use super::GatherError;
use crate::convert::bones;

/// The node of `bone` in a gathered scene.
pub fn bone_node(bone: BoneId) -> NodeIx {
    NodeIx(u32::from(bone.0))
}

/// The node of the bone at `index` in the skeleton's bone list. A skeleton has at most `MAX_BONES` bones, so the
/// conversion never saturates.
fn node_ix(index: usize) -> NodeIx {
    NodeIx(u32::try_from(index).unwrap_or(u32::MAX))
}

/// The nodes of the bones that have no parent, in bone order: the scene's roots before any mesh joins them.
pub(super) fn root_nodes(skeleton: &Skeleton) -> Vec<NodeIx> {
    skeleton.bones().iter().enumerate().filter(|(_, bone)| bone.parent.is_none()).map(|(i, _)| node_ix(i)).collect()
}

/// One node per bone, in bone order: the joints of the character's skin.
pub(super) fn joint_nodes(skeleton: &Skeleton) -> Vec<NodeIx> {
    (0..skeleton.len()).map(node_ix).collect()
}

/// Every humanoid role the skeleton fills, with its node, in VRM table order (parents first). This is what the avatar
/// block's humanoid map is made of: all 55 VRM 1.0 bones, the 30 finger bones included, convert and appear when the
/// skeleton has them.
///
/// # Errors
/// [`GatherError::NotHumanoid`] for a generic skeleton; [`GatherError::UnknownBone`] if the runtime has a role the file
/// contract lacks (the table test in `convert::bones` fails first).
pub fn humanoid_nodes(skeleton: &Skeleton) -> Result<Vec<(HumanBoneName, NodeIx)>, GatherError> {
    skeleton.require_humanoid()?.iter().map(|(role, bone)| Ok((bones::to_file(role)?, bone_node(bone)))).collect()
}

/// The nodes for every bone of `skeleton`, with children linked and translations multiplied by `scale`.
///
/// A humanoid role is named after its VRM bone, whatever the skeleton called it, so a re-import finds the role from the
/// node name alone; any other bone keeps its own name. Rotations are written as the skeleton has them (VRM 1.0 asks for
/// a T-pose rest, and exporters never "fix" a pose).
///
/// # Errors
/// [`GatherError::ReservedBoneName`] for a bone without a role that is named like one in a humanoid skeleton,
/// [`GatherError::DuplicateNodeName`] for two bones that end up with one name, and [`GatherError::UnknownBone`] as in
/// [`humanoid_nodes`].
pub(super) fn bone_nodes(skeleton: &Skeleton, scale: f32) -> Result<Vec<Node>, GatherError> {
    let humanoid = skeleton.is_humanoid();
    let mut nodes: Vec<Node> = Vec::with_capacity(skeleton.len());
    let mut taken = HashSet::new();
    for def in skeleton.bones() {
        let name = node_name(def, humanoid)?;
        // A role's VRM name could equal another bone's own name; the skeleton's uniqueness rule only covers the
        // names it stores, not the ones the roles are given here.
        if !taken.insert(name.clone()) {
            return Err(GatherError::DuplicateNodeName(name));
        }
        let mut node = Node::new(name);
        node.transform =
            Transform { translation: def.rest.translation * scale, rotation: def.rest.rotation, scale: Vec3::ONE };
        nodes.push(node);
    }
    // Parents come first, so by the time a bone is linked its parent is already in the list.
    for (i, def) in skeleton.bones().iter().enumerate() {
        if let Some(parent) = def.parent
            && let Some(node) = nodes.get_mut(parent.index())
        {
            node.children.push(node_ix(i));
        }
    }
    Ok(nodes)
}

/// The node name of a bone. In a humanoid skeleton, a bone with no role must not be called like a role: its node would
/// read back as that role (`convert::bones::joint_from_node_name`). A generic skeleton has no roles to confuse, so a
/// creature's `head` is just a name.
fn node_name(def: &BoneDef, humanoid_skeleton: bool) -> Result<String, GatherError> {
    match def.humanoid {
        Some(role) => Ok(bones::to_file(role)?.as_str().to_owned()),
        None if humanoid_skeleton && HumanoidBone::from_name(&def.name).is_some() => {
            Err(GatherError::ReservedBoneName(def.name.clone()))
        }
        None => Ok(def.name.clone()),
    }
}

/// How a mesh's joint reads in a message: a role's VRM name or an extra bone's own.
fn joint_name(joint: &JointRef) -> String {
    match joint {
        JointRef::Humanoid(role) => role.name().to_owned(),
        JointRef::Named(name) => name.as_str().to_owned(),
    }
}

/// The inverse bind matrices of `rig` for a scene scaled by `scale`: one per bone, in bone order.
///
/// They come from the rig's bind pose ([`CharacterRig::inverse_bind`], decision 0014), never from the rest: a character
/// that rests in the T-pose but was sculpted in an A-pose exports T nodes (VRM 1.0 wants that) with the A-pose's
/// matrices, so the mesh skins to itself at the nodes' rest. A bone's matrix has all its length in the translation
/// column (a rotation has none), so scaling the lengths of the rig scales exactly that column, bound or not.
pub(super) fn inverse_bind(rig: &CharacterRig, scale: f32) -> Vec<Mat4> {
    rig.inverse_bind()
        .into_iter()
        .map(|mut matrix| {
            matrix.w_axis = (matrix.w_axis.truncate() * scale).extend(1.0);
            matrix
        })
        .collect()
}

/// Rewrites the joint indices of a skinned `geometry` from `mesh`'s own joint list to skeleton indices (the skin's joint
/// indices), leaving the weights alone. A slot with no weight points at joint 0, whatever it pointed at before: it
/// contributes nothing, and glTF wants every index valid.
///
/// # Errors
/// [`GatherError::MissingBone`] if a joint of the mesh isn't in the skeleton, [`GatherError::JointOutOfRange`] if a
/// vertex uses a joint index its mesh doesn't list.
pub(super) fn remap_joints(rig: &CharacterRig, mesh: &PartMesh, geometry: &mut MeshData) -> Result<(), GatherError> {
    let table = mesh
        .joint_bones
        .iter()
        .map(|joint| {
            rig.bone(joint)
                .map(|bone| bone.0)
                .ok_or_else(|| GatherError::MissingBone { mesh: mesh.name.clone(), joint: joint_name(joint) })
        })
        .collect::<Result<Vec<u16>, _>>()?;
    for (joints, weights) in geometry.joints.iter_mut().zip(&geometry.weights) {
        for (joint, weight) in joints.iter_mut().zip(weights) {
            *joint = if *weight > 0.0 {
                table.get(usize::from(*joint)).copied().ok_or_else(|| GatherError::JointOutOfRange {
                    mesh: mesh.name.clone(),
                    index: *joint,
                    len: table.len(),
                })?
            } else {
                0
            };
        }
    }
    Ok(())
}
