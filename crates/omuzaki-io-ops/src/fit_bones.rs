//! Cutting a skin's bone count to a budget (`fit.bones`, `docs/design/platform-export.md` §3).
//!
//! `VRChat` counts a skin's joints as bones and Roblox holds a body to a small fixed rig, while a model from a
//! modelling tool carries helper bones. [`reduce_bones`] removes joints from the *skins*, never nodes from the tree, so
//! humanoid, spring, constraint, look-at and expression data that name nodes by index need no remapping (the rule of
//!). A removed joint's vertex weights move to its nearest surviving ancestor, which at the bind pose leaves every
//! vertex where it was as long as that ancestor's inverse bind matrix still undoes its rest pose.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_io_api::{ConstraintKind, IoScene, MeshIx, NodeIx, SkinIx};

/// How many distinct joints, over all skins, the scene may keep.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BoneBudget {
    /// The most bones allowed.
    pub max_bones: usize,
}

/// What [`reduce_bones`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoneReport {
    /// Distinct joints over all skins before.
    pub before: usize,
    /// Distinct joints over all skins after.
    pub after: usize,
    /// Every removed joint as `(from, into)`, in removal order. `into` is the ancestor that took the weights (the
    /// final one, when it was removed later in turn); a joint with no weight and no ancestor joint is dropped and
    /// listed as `(from, from)`.
    pub merged: Vec<(NodeIx, NodeIx)>,
    /// How many of the joints are protected: humanoid, spring, collider, constraint, first-person and skeleton-root
    /// nodes that are skin joints.
    pub protected: usize,
    /// True if `after` is within the budget.
    pub reached: bool,
}

/// Reduces the distinct joints over all skins to `budget.max_bones`.
///
/// Protected nodes (every humanoid bone, which includes the look-at head; every spring joint, collider node and spring
/// centre; every constraint source and destination; first-person annotation nodes; each skin's skeleton root) are never
/// merged. The other joints go smallest total weight first, ties by node index: a joint with no weight is dropped
/// for free, one with weight is merged into its nearest surviving ancestor joint (it must be in every skin that
/// carries weights of the joint). Each vertex then keeps its four strongest influences, renormalised to 1.
///
/// Nothing changes, and `reached` is false, when the protected joints alone exceed the budget or the skinning data is
/// malformed (a joint index past its skin, a mesh used with two skins). When only some joints cannot be merged (no
/// usable ancestor), the rest are still merged and `reached` is false.
pub fn reduce_bones(scene: &mut IoScene, budget: &BoneBudget) -> BoneReport {
    let joints: BTreeSet<NodeIx> = scene.skins.iter().flat_map(|s| s.joints.iter().copied()).collect();
    let before = joints.len();
    let protected: BTreeSet<NodeIx> = protected_nodes(scene).intersection(&joints).copied().collect();
    let report = |after: usize, merged: Vec<(NodeIx, NodeIx)>| BoneReport {
        before,
        after,
        merged,
        protected: protected.len(),
        reached: after <= budget.max_bones,
    };
    if before <= budget.max_bones {
        return report(before, Vec::new());
    }
    if protected.len() > budget.max_bones {
        return report(before, Vec::new());
    }
    let Some(mesh_skin) = mesh_skins(scene) else { return report(before, Vec::new()) };

    let mut weight = joint_weights(scene, &mesh_skin, &joints);
    let parents = parent_table(scene);
    let used_skins: BTreeSet<SkinIx> = mesh_skin.values().copied().collect();
    let skin_sets: BTreeMap<SkinIx, BTreeSet<NodeIx>> =
        used_skins.iter().map(|&s| (s, scene.skins[s.index()].joints.iter().copied().collect())).collect();

    let mut alive = joints;
    let mut into: BTreeMap<NodeIx, Option<NodeIx>> = BTreeMap::new();
    let mut order = Vec::new();
    let mut stuck: BTreeSet<NodeIx> = BTreeSet::new();
    while alive.len() > budget.max_bones {
        let candidate = alive
            .iter()
            .filter(|j| !protected.contains(j) && !stuck.contains(j))
            .min_by(|a, b| weight[a].total_cmp(&weight[b]).then(a.cmp(b)))
            .copied();
        let Some(from) = candidate else { break };
        let target = nearest_ancestor(&parents, from, &alive);
        let w = weight[&from];
        if w > 0.0 {
            // Weight can only move to a joint every weighted skin that has `from` also has.
            let fits = target.is_some_and(|t| skin_sets.values().all(|s| !s.contains(&from) || s.contains(&t)));
            if !fits {
                stuck.insert(from);
                continue;
            }
        }
        if let Some(tw) = target.and_then(|t| weight.get_mut(&t)) {
            *tw += w;
        }
        alive.remove(&from);
        into.insert(from, target);
        order.push(from);
        stuck.clear();
    }

    let merged: Vec<(NodeIx, NodeIx)> =
        order.iter().map(|&from| (from, resolve(&into, from).unwrap_or(from))).collect();
    apply(scene, &mesh_skin, &into, &alive);
    report(alive.len(), merged)
}

/// Every node the avatar or a skin depends on by index. The look-at head is a humanoid bone, so it is covered.
fn protected_nodes(scene: &IoScene) -> BTreeSet<NodeIx> {
    let mut out: BTreeSet<NodeIx> = scene.skins.iter().filter_map(|s| s.skeleton).collect();
    let Some(avatar) = &scene.avatar else { return out };
    out.extend(avatar.humanoid.bones.values().copied());
    let springs = &avatar.spring_bones;
    out.extend(springs.colliders.iter().map(|c| c.node));
    for chain in &springs.chains {
        out.extend(chain.joints.iter().map(|j| j.node));
        out.extend(chain.center);
    }
    for c in &avatar.constraints {
        out.insert(c.node);
        let (ConstraintKind::Roll { source, .. }
        | ConstraintKind::Aim { source, .. }
        | ConstraintKind::Rotation { source }) = c.kind;
        out.insert(source);
    }
    if let Some(fp) = &avatar.first_person {
        out.extend(fp.annotations.iter().map(|a| a.node));
    }
    out
}

/// Which skin deforms each skinned mesh, or `None` if the data can't be remapped safely: a mesh drawn with two
/// different skins, an index out of range, or vertex data that disagrees with its skin.
fn mesh_skins(scene: &IoScene) -> Option<BTreeMap<MeshIx, SkinIx>> {
    let mut map: BTreeMap<MeshIx, SkinIx> = BTreeMap::new();
    for node in &scene.nodes {
        let (Some(mesh), Some(skin)) = (node.mesh, node.skin) else { continue };
        if *map.entry(mesh).or_insert(skin) != skin {
            return None;
        }
    }
    for skin in &scene.skins {
        if skin.inverse_bind.len() != skin.joints.len() || skin.joints.iter().any(|j| j.index() >= scene.nodes.len()) {
            return None;
        }
    }
    for (mesh, skin) in &map {
        let joint_count = scene.skins.get(skin.index())?.joints.len();
        for prim in &scene.meshes.get(mesh.index())?.primitives {
            let m = &prim.mesh;
            if m.joints.len() != m.weights.len() {
                return None;
            }
            // Every slot, weighted or not: the weight pass indexes the skin by each of them.
            let bad = m.joints.iter().any(|js| js.iter().any(|&j| usize::from(j) >= joint_count));
            if bad {
                return None;
            }
        }
    }
    Some(map)
}

/// The total skinning weight each joint carries over the vertices of every skinned mesh (each mesh counted once).
fn joint_weights(
    scene: &IoScene,
    mesh_skin: &BTreeMap<MeshIx, SkinIx>,
    joints: &BTreeSet<NodeIx>,
) -> BTreeMap<NodeIx, f64> {
    let mut total: BTreeMap<NodeIx, f64> = joints.iter().map(|&j| (j, 0.0)).collect();
    for (mesh, skin) in mesh_skin {
        let skin_joints = &scene.skins[skin.index()].joints;
        for prim in &scene.meshes[mesh.index()].primitives {
            for (js, ws) in prim.mesh.joints.iter().zip(&prim.mesh.weights) {
                for (&j, &w) in js.iter().zip(ws) {
                    if let Some(t) = total.get_mut(&skin_joints[usize::from(j)]).filter(|_| w > 0.0) {
                        *t += f64::from(w);
                    }
                }
            }
        }
    }
    total
}

fn parent_table(scene: &IoScene) -> Vec<Option<NodeIx>> {
    let mut parents = vec![None; scene.nodes.len()];
    for (i, node) in scene.nodes.iter().enumerate() {
        for child in &node.children {
            if let Some(slot) = parents.get_mut(child.index()) {
                *slot = Some(NodeIx(u32::try_from(i).unwrap_or(u32::MAX)));
            }
        }
    }
    parents
}

/// The closest ancestor of `node` that is still a joint. Bounded by the node count, so a cyclic graph can't loop.
fn nearest_ancestor(parents: &[Option<NodeIx>], node: NodeIx, alive: &BTreeSet<NodeIx>) -> Option<NodeIx> {
    let mut cur = node;
    for _ in 0..parents.len() {
        cur = (*parents.get(cur.index())?)?;
        if alive.contains(&cur) {
            return Some(cur);
        }
    }
    None
}

/// Follows removals to the joint that finally holds a removed joint's weight.
fn resolve(into: &BTreeMap<NodeIx, Option<NodeIx>>, from: NodeIx) -> Option<NodeIx> {
    let mut cur = from;
    for _ in 0..=into.len() {
        match into.get(&cur) {
            Some(Some(next)) => cur = *next,
            Some(None) => return None,
            None => return Some(cur),
        }
    }
    None
}

/// Rewrites the skins and the vertex joints and weights for the joints that remain in `alive`.
fn apply(
    scene: &mut IoScene,
    mesh_skin: &BTreeMap<MeshIx, SkinIx>,
    into: &BTreeMap<NodeIx, Option<NodeIx>>,
    alive: &BTreeSet<NodeIx>,
) {
    // Old joint index -> new index per used skin; `None` when the joint's weight went nowhere (it had none).
    let mut remaps: BTreeMap<SkinIx, Vec<Option<u16>>> = BTreeMap::new();
    for &skin_ix in mesh_skin.values().collect::<BTreeSet<_>>() {
        let skin = &scene.skins[skin_ix.index()];
        let new_index: BTreeMap<NodeIx, u16> = skin
            .joints
            .iter()
            .filter(|j| alive.contains(j))
            .enumerate()
            .map(|(i, &j)| (j, u16::try_from(i).unwrap_or(u16::MAX)))
            .collect();
        let remap = skin.joints.iter().map(|&j| resolve(into, j).and_then(|f| new_index.get(&f).copied())).collect();
        remaps.insert(skin_ix, remap);
    }
    for (mesh, skin) in mesh_skin {
        let Some(remap) = remaps.get(skin) else { continue };
        for prim in &mut scene.meshes[mesh.index()].primitives {
            let m = &mut prim.mesh;
            for (js, ws) in m.joints.iter_mut().zip(m.weights.iter_mut()) {
                (*js, *ws) = strongest_four(*js, *ws, remap);
            }
        }
    }
    for skin in &mut scene.skins {
        let keep: Vec<bool> = skin.joints.iter().map(|j| alive.contains(j)).collect();
        let mut flags = keep.iter();
        skin.joints.retain(|_| flags.next().copied().unwrap_or(false));
        let mut flags = keep.iter();
        skin.inverse_bind.retain(|_| flags.next().copied().unwrap_or(false));
    }
}

/// One vertex's influences moved through `remap`: duplicates add up, the four strongest stay (ties to the lower joint
/// index), the weights sum to 1. A vertex with no weight stays without any.
fn strongest_four(joints: [u16; 4], weights: [f32; 4], remap: &[Option<u16>]) -> ([u16; 4], [f32; 4]) {
    let mut acc: Vec<(u16, f32)> = Vec::with_capacity(4);
    for (j, w) in joints.into_iter().zip(weights) {
        let Some(Some(new)) = remap.get(usize::from(j)).filter(|_| w > 0.0) else { continue };
        match acc.iter_mut().find(|(k, _)| k == new) {
            Some((_, sum)) => *sum += w,
            None => acc.push((*new, w)),
        }
    }
    acc.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    acc.truncate(4);
    let sum: f32 = acc.iter().map(|a| a.1).sum();
    let (mut js, mut ws) = ([0u16; 4], [0.0f32; 4]);
    if sum > 0.0 {
        for (slot, (j, w)) in acc.into_iter().enumerate() {
            js[slot] = j;
            ws[slot] = w / sum;
        }
    }
    (js, ws)
}
