//! The imported skeleton: which nodes become bones, their rest without scale, and the node-to-bone table (design D4).
//!
//! **Which nodes.** Every node something moves with, and its ancestors: skin joints, nodes that draw a mesh without a
//! skin (they become one-joint skins, so a rigid accessory follows its node), humanoid bones, spring joints, collider
//! nodes and chain centres, and constrained nodes and their sources. A node nothing needs (a skinned mesh's own node,
//! an unused helper leaf) stays out, so a big file stays under [`MAX_BONES`]. The needed set is closed under ancestors,
//! so a bone's parent is always its node's parent.
//!
//! **Order and names.** Bones come in depth-first order from the scene's roots (then any needed node outside the
//! scene, by index), children in file order, so parents come first as the skeleton requires. A bone is named after its
//! node, made a slug and de-duplicated (`J_Bip_L_Hand` becomes `j_bip_l_hand`; a second `Hair` becomes `hair_2`);
//! a name with nothing left (`頭`) becomes `node_<index>`.
//!
//! **Scale.** Bones carry no scale (the anim-api `Transform`), so a scaled node's scale is baked: its bone's rest is
//! its world placement without the scale, and [`Rig::scale_fix`] gives the matrix that carries a mesh bound to the
//! scaled node onto the bone. A file without scale (every `VRoid` and `UniVRM` export) keeps its rest transforms bit for
//! bit, and no mesh is touched.

use std::collections::BTreeSet;

use omuzaki_anim_api::{BoneDef, BoneId, HumanoidBone, MAX_BONES, Skeleton, Transform};
use omuzaki_core::glam::{Mat4, Quat};
use omuzaki_core::id::Slug;
use omuzaki_io_api::{Finding, IoScene, NodeIx, Report};

use crate::convert::bones::from_file;
use crate::prepare::graph;

/// The skeleton made from a scene, with the node each bone came from.
#[derive(Clone, Debug)]
pub(crate) struct Rig {
    /// The skeleton: needed nodes, parents first, rest without scale.
    pub skeleton: Skeleton,
    /// The bone each node became, by node index; `None` for a node no bone came from.
    pub bones: Vec<Option<BoneId>>,
    /// Each node's world matrix in the file, scale included, by node index.
    pub world: Vec<Mat4>,
    /// Whether a node and all its ancestors have unit scale, by node index.
    unit_scale: Vec<bool>,
}

impl Rig {
    /// The bone node `node` became.
    pub fn bone(&self, node: NodeIx) -> Option<BoneId> {
        self.bones.get(node.index()).copied().flatten()
    }

    /// The name of the bone node `node` became.
    pub fn bone_name(&self, node: NodeIx) -> Option<&str> {
        self.skeleton.get(self.bone(node)?).map(|b| b.name.as_str())
    }

    /// The matrix from node `node`'s file frame (scale included) to its bone's frame (no scale): `W'⁻¹ · W`, or `None`
    /// when they are the same frame (no scale on the node or above it), so unscaled files are never touched.
    pub fn scale_fix(&self, node: NodeIx) -> Option<Mat4> {
        let i = node.index();
        if self.unit_scale.get(i).copied().unwrap_or(true) {
            return None;
        }
        let world = *self.world.get(i)?;
        let (_, rotation, translation) = world.to_scale_rotation_translation();
        Some(Mat4::from_rotation_translation(unit(rotation), translation).inverse() * world)
    }
}

/// The nodes something moves with (see the module docs), closed under ancestors.
fn needed(scene: &IoScene, parent: &[Option<usize>]) -> Vec<bool> {
    let mut need = vec![false; scene.nodes.len()];
    let mut mark = |node: NodeIx| {
        if let Some(slot) = need.get_mut(node.index()) {
            *slot = true;
        }
    };
    scene.skins.iter().flat_map(|s| &s.joints).for_each(|&j| mark(j));
    for (i, node) in scene.nodes.iter().enumerate() {
        if node.mesh.is_some() && node.skin.is_none() {
            mark(NodeIx(u32::try_from(i).unwrap_or(u32::MAX)));
        }
    }
    if let Some(avatar) = &scene.avatar {
        avatar.humanoid.bones.values().for_each(|&n| mark(n));
        let springs = &avatar.spring_bones;
        springs.colliders.iter().for_each(|c| mark(c.node));
        for chain in &springs.chains {
            chain.joints.iter().for_each(|j| mark(j.node));
            chain.center.into_iter().for_each(&mut mark);
        }
        for constraint in &avatar.constraints {
            mark(constraint.node);
            mark(constraint_source(constraint.kind));
        }
    }
    for i in 0..need.len() {
        if need[i] {
            let mut up = parent.get(i).copied().flatten();
            for _ in 0..need.len() {
                let Some(p) = up.filter(|&p| !need[p]) else { break };
                need[p] = true;
                up = parent.get(p).copied().flatten();
            }
        }
    }
    need
}

/// The node a constraint follows.
pub(crate) fn constraint_source(kind: omuzaki_io_api::ConstraintKind) -> NodeIx {
    match kind {
        omuzaki_io_api::ConstraintKind::Roll { source, .. }
        | omuzaki_io_api::ConstraintKind::Aim { source, .. }
        | omuzaki_io_api::ConstraintKind::Rotation { source } => source,
    }
}

/// `q` made unit length, or the identity when it has no direction (a file's zero quaternion).
pub(crate) fn unit(q: Quat) -> Quat {
    let length = q.length();
    if q.is_normalized() {
        q
    } else if length.is_finite() && length > 0.0 {
        q / length
    } else {
        Quat::IDENTITY
    }
}

/// `name` lower-cased, with every character a slug cannot hold made `_`, trimmed of leading and trailing `_` and `-`,
/// and cut to leave room for a `_<n>` suffix; empty when nothing usable is left.
pub(crate) fn slug_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' { c } else { '_' })
        .collect();
    let trimmed = cleaned.trim_matches(['_', '-']);
    if trimmed.chars().any(|c| c.is_ascii_alphanumeric()) {
        trimmed.chars().take(Slug::MAX_LEN - 8).collect()
    } else {
        String::new()
    }
}

/// The first of `stem`, `stem_2`, `stem_3`, … that is a slug and not in `taken`, which it joins. A stem that is a
/// Windows device name (`con`) is refused as a slug, so it gets a suffix too.
pub(crate) fn unique_slug(stem: &str, taken: &mut BTreeSet<String>) -> Slug {
    let mut k = 1usize;
    loop {
        let candidate = if k == 1 { stem.to_owned() } else { format!("{stem}_{k}") };
        if let Ok(slug) = Slug::new(candidate.as_str())
            && taken.insert(candidate)
        {
            return slug;
        }
        k += 1;
    }
}

/// A unique bone name for the node `name` at `index` (see the module docs).
fn bone_name(name: &str, index: usize, taken: &mut BTreeSet<String>) -> String {
    let stem = slug_stem(name);
    let stem = if stem.is_empty() { format!("node_{index}") } else { stem };
    unique_slug(&stem, taken).as_str().to_owned()
}

/// The skeleton of `scene` (see the module docs).
///
/// # Errors
/// A report with `avatar.too_many_bones` when the needed nodes are over [`MAX_BONES`], or `avatar.skeleton_invalid`
/// naming the rule the bones break (a required humanoid bone missing, a humanoid bone under the wrong parent).
pub(crate) fn build(scene: &IoScene) -> Result<Rig, Report> {
    let parent = graph::parents(scene);
    let graph::World { matrices: world, unit_scale, .. } = graph::world_of(scene);
    let need = needed(scene, &parent);
    let order: Vec<usize> = graph::hierarchy_order(scene, &parent, &|i| need.get(i).copied().unwrap_or(false))
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    if order.len() > MAX_BONES {
        let msg = format!("the model moves {} nodes; at most {MAX_BONES} can be bones", order.len());
        let hint = "remove unused bones from the model, or merge accessory bones, then import it again";
        return Err(single(Finding::error("avatar.too_many_bones", msg).at("nodes").hint(hint)));
    }
    let roles = humanoid_roles(scene);
    let mut bones = vec![None; scene.nodes.len()];
    let mut taken = BTreeSet::new();
    let mut defs = Vec::with_capacity(order.len());
    for &i in &order {
        let node = &scene.nodes[i];
        let up = parent.get(i).copied().flatten();
        let parent_bone = up.and_then(|p| bones.get(p).copied().flatten());
        let rest = if unit_scale[i] {
            Transform { translation: node.transform.translation, rotation: unit(node.transform.rotation) }
        } else {
            rigid_local(&world, up, i)
        };
        let id = BoneId(u16::try_from(defs.len()).unwrap_or(u16::MAX));
        bones[i] = Some(id);
        let humanoid = roles.get(i).copied().flatten();
        defs.push(BoneDef { name: bone_name(&node.name, i, &mut taken), parent: parent_bone, rest, humanoid });
    }
    let skeleton = Skeleton::new(defs).map_err(|e| {
        let hint = "check the humanoid map: every required bone mapped, each under its parent bone";
        single(Finding::error("avatar.skeleton_invalid", e.to_string()).at("avatar/humanoid").hint(hint))
    })?;
    Ok(Rig { skeleton, bones, world, unit_scale })
}

/// The humanoid role of each node, by node index.
fn humanoid_roles(scene: &IoScene) -> Vec<Option<HumanoidBone>> {
    let mut roles = vec![None; scene.nodes.len()];
    for (bone, node) in scene.avatar.iter().flat_map(|a| &a.humanoid.bones) {
        if let (Some(slot), Some(role)) = (roles.get_mut(node.index()), from_file(*bone)) {
            *slot = Some(role);
        }
    }
    roles
}

/// Node `i`'s placement relative to its parent with every scale baked out: both world placements without scale.
fn rigid_local(world: &[Mat4], parent: Option<usize>, i: usize) -> Transform {
    let rigid = |m: Mat4| {
        let (_, rotation, translation) = m.to_scale_rotation_translation();
        Transform { translation, rotation: unit(rotation) }
    };
    let own = rigid(world[i]);
    match parent.and_then(|p| world.get(p)) {
        Some(&p) => {
            let up = rigid(p);
            let inverse = up.rotation.conjugate();
            Transform {
                translation: inverse * (own.translation - up.translation),
                rotation: unit(inverse * own.rotation),
            }
        }
        None => own,
    }
}

/// A report holding one finding.
fn single(finding: Finding) -> Report {
    let mut report = Report::new();
    report.push(finding);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bone_names_are_unique_slugs_with_a_fallback() {
        let mut taken = BTreeSet::new();
        assert_eq!(bone_name("J_Bip_L_Hand", 0, &mut taken), "j_bip_l_hand");
        assert_eq!(bone_name("Hair", 1, &mut taken), "hair");
        assert_eq!(bone_name("hair", 2, &mut taken), "hair_2");
        assert_eq!(bone_name("頭", 3, &mut taken), "node_3");
        assert_eq!(bone_name("_Root.001", 4, &mut taken), "root_001");
        assert_eq!(bone_name("CON", 5, &mut taken), "con_2");
        let long = "a".repeat(200);
        let name = bone_name(&long, 6, &mut taken);
        assert!(Slug::new(name.as_str()).is_ok() && name.len() <= Slug::MAX_LEN, "{name}");
    }
}
