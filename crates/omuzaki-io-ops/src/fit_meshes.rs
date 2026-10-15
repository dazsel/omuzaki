//! Joining mesh nodes that nothing binds (`fit.meshes`, `docs/design/platform-export.md` §3, P6).
//!
//! Platforms count mesh nodes, not triangles, so an avatar of one body, a hair mesh and three outfit pieces costs five
//! where two would do. [`merge_meshes`] moves the primitives of compatible mesh nodes into one mesh. Primitives are
//! moved whole and in node order, never re-packed, so every material assignment survives and the vertex and triangle
//! totals do not change.
//!
//! The pass is built to fail safe: when it is unsure whether two nodes can join, they stay apart.
//!
//! - **Never joined:** a node an expression morph bind names, a node whose mesh has morph targets or is shared by
//!   another node, and a node whose skinning data is inconsistent.
//! - **Skinned meshes** join when every joint their skins share has the same inverse bind matrix (within
//!   [`IBM_TOLERANCE`]) and their first-person annotations are equal, an unannotated mesh counting as `Auto`. Skinned
//!   vertices follow joints, not their node, so the nodes may sit anywhere. The joined skin is the union of the joints,
//!   in the target's order first, and the moved vertices are re-indexed.
//! - **Basic meshes** (no skin) follow their node. They join only under the same parent, with equal annotations, and
//!   only when neither node can move on its own: no humanoid bone, spring joint, spring centre, collider, constraint
//!   source or destination, skin joint or skeleton root. The moved vertices are baked into the target node's space,
//!   which is exact in every pose because the parent is shared. An [`IoScene`] holds no animation clips, so "animated"
//!   is judged from the nodes the scene itself marks as driven.
//!
//! Merged nodes stay in the tree without a mesh, so node indices, humanoid, spring, constraint and expression data
//! need no remapping. The target keeps its first-person annotation; the emptied nodes drop theirs. Meshes and skins
//! no node uses any more are removed from their arenas, which renumbers the survivors but not their content.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_core::glam::{Mat3, Mat4};
use omuzaki_io_api::{
    ConstraintKind, FirstPersonFlag, IoError, IoScene, MeshIx, Node, NodeIx, Primitive, Skin, SkinIx, Transform,
};

/// How far two inverse bind matrices of the same joint may differ, per element, and still be one matrix.
pub const IBM_TOLERANCE: f32 = 1e-6;

/// Below this, a transform's determinant is treated as singular and its node can't be a bake target.
const MIN_DETERMINANT: f32 = 1e-12;

/// Why two mesh nodes were kept apart.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApartReason {
    /// Their first-person annotations differ (an unannotated mesh counts as `Auto`).
    FirstPerson,
    /// Their skins name different skeleton roots.
    Skeleton,
    /// Both skins have `joint`, with different inverse bind matrices; it is the first such joint in the joining
    /// node's skin.
    InverseBind {
        /// The first joint whose matrices differ.
        joint: NodeIx,
    },
    /// The joined skin would need more joints than a `u16` vertex index can address.
    TooManyJoints,
    /// The two basic meshes have different parents.
    Parent,
    /// A basic mesh's own transform can change at run time (`node` is a humanoid bone, a spring or constraint node, or
    /// a skin joint).
    Moves {
        /// The node that can move.
        node: NodeIx,
    },
    /// The target node's transform can't be inverted, so nothing can be baked into its space.
    SingularTransform,
}

impl std::fmt::Display for ApartReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FirstPerson => f.write_str("first-person annotations differ"),
            Self::Skeleton => f.write_str("skins name different skeleton roots"),
            Self::InverseBind { joint } => {
                write!(f, "joint node {} has different inverse bind matrices in the two skins", joint.0)
            }
            Self::TooManyJoints => f.write_str("the joined skin would exceed 65536 joints"),
            Self::Parent => f.write_str("the nodes have different parents"),
            Self::Moves { node } => write!(f, "node {} can move at run time", node.0),
            Self::SingularTransform => f.write_str("the target node's transform can't be inverted"),
        }
    }
}

/// One pair of mesh nodes that stayed apart.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ApartPair {
    /// The earlier node, which a join would have kept.
    pub target: NodeIx,
    /// The later node, which a join would have emptied.
    pub other: NodeIx,
    /// Why they could not join.
    pub reason: ApartReason,
}

/// What [`merge_meshes`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeshMergeReport {
    /// Skinned mesh nodes `(before, after)`.
    pub skinned: (usize, usize),
    /// Basic (unskinned) mesh nodes `(before, after)`.
    pub basic: (usize, usize),
    /// The nodes whose mesh moved into another node, skinned ones first, each group in node order. They are still in
    /// the tree, without a mesh or skin.
    pub merged: Vec<NodeIx>,
    /// Every pair of candidate nodes that could not join, for each candidate that ended up joined to nothing and each
    /// earlier target that refused it. Nodes excluded outright (bound, morphing, shared) are not pairs.
    pub apart: Vec<ApartPair>,
}

/// Joins the mesh nodes of `scene` that nothing binds, as the module documentation describes.
///
/// Deterministic: candidates are taken in node order and each joins the first target that accepts it.
///
/// # Errors
/// [`IoError::Rejected`] when the scene breaks its own invariants (`IoScene::check`); nothing is changed then.
pub fn merge_meshes(scene: &mut IoScene) -> Result<MeshMergeReport, IoError> {
    let checked = scene.check();
    if checked.has_errors() {
        return Err(IoError::Rejected(checked));
    }
    let facts = Facts::gather(scene);
    let mut report = MeshMergeReport {
        skinned: (count_nodes(scene, true), 0),
        basic: (count_nodes(scene, false), 0),
        ..MeshMergeReport::default()
    };

    let skinned = plan_skinned(scene, &facts, &mut report.apart);
    let basic = plan_basic(scene, &facts, &mut report.apart);
    let mut orphaned_meshes = BTreeSet::new();
    let mut orphaned_skins = BTreeSet::new();
    for plan in &skinned {
        apply_skinned(scene, plan, &mut report.merged, &mut orphaned_meshes, &mut orphaned_skins);
    }
    for plan in &basic {
        apply_basic(scene, plan, &mut report.merged, &mut orphaned_meshes);
    }
    drop_annotations(scene, &report.merged);
    compact(scene, &orphaned_meshes, &orphaned_skins);
    report.skinned.1 = count_nodes(scene, true);
    report.basic.1 = count_nodes(scene, false);
    Ok(report)
}

/// How many nodes carry a mesh with (`skinned`) or without a skin.
fn count_nodes(scene: &IoScene, skinned: bool) -> usize {
    scene.nodes.iter().filter(|n| n.mesh.is_some() && n.skin.is_some() == skinned).count()
}

/// What the scene says about each node, read once.
struct Facts {
    parents: Vec<Option<NodeIx>>,
    /// How many nodes use each mesh.
    mesh_users: Vec<usize>,
    /// Meshes an expression morph bind reaches through its node.
    bound_meshes: BTreeSet<MeshIx>,
    flags: BTreeMap<NodeIx, FirstPersonFlag>,
    /// Nodes whose own transform can change at run time.
    moving: BTreeSet<NodeIx>,
}

impl Facts {
    fn gather(scene: &IoScene) -> Self {
        let mut parents = vec![None; scene.nodes.len()];
        for (i, node) in scene.nodes.iter().enumerate() {
            for child in &node.children {
                if let Some(slot) = parents.get_mut(child.index()) {
                    *slot = Some(node_ix(i));
                }
            }
        }
        let mut mesh_users = vec![0; scene.meshes.len()];
        for node in &scene.nodes {
            if let Some(n) = node.mesh.and_then(|m| mesh_users.get_mut(m.index())) {
                *n += 1;
            }
        }
        let mut bound_meshes = BTreeSet::new();
        let mut flags = BTreeMap::new();
        let mut moving: BTreeSet<NodeIx> = BTreeSet::new();
        for skin in &scene.skins {
            moving.extend(skin.joints.iter().copied());
            moving.extend(skin.skeleton);
        }
        if let Some(avatar) = &scene.avatar {
            for bind in avatar.expressions.values().flat_map(|e| &e.morph_binds) {
                bound_meshes.extend(scene.nodes.get(bind.node.index()).and_then(|n| n.mesh));
            }
            for a in avatar.first_person.iter().flat_map(|fp| &fp.annotations) {
                flags.entry(a.node).or_insert(a.flag);
            }
            moving.extend(avatar.humanoid.bones.values().copied());
            let springs = &avatar.spring_bones;
            moving.extend(springs.colliders.iter().map(|c| c.node));
            for chain in &springs.chains {
                moving.extend(chain.joints.iter().map(|j| j.node));
                moving.extend(chain.center);
            }
            for c in &avatar.constraints {
                moving.insert(c.node);
                let (ConstraintKind::Roll { source, .. }
                | ConstraintKind::Aim { source, .. }
                | ConstraintKind::Rotation { source }) = c.kind;
                moving.insert(source);
            }
        }
        Self { parents, mesh_users, bound_meshes, flags, moving }
    }

    fn flag(&self, node: NodeIx) -> FirstPersonFlag {
        self.flags.get(&node).copied().unwrap_or_default()
    }

    /// Whether the mesh at `node` may take part at all: used by this node alone, bound by no expression, without
    /// morph targets (a joined mesh would need padded targets).
    fn mesh_is_free(&self, scene: &IoScene, node: &Node) -> bool {
        let Some(mesh) = node.mesh else { return false };
        let Some(data) = scene.meshes.get(mesh.index()) else { return false };
        self.mesh_users.get(mesh.index()) == Some(&1)
            && !self.bound_meshes.contains(&mesh)
            && data.morph_target_count() == 0
            && data.primitives.iter().all(|p| p.morph_targets.is_empty())
            && !data.primitives.is_empty()
    }
}

/// A position as a scene index; a scene with more than `u32::MAX` items can't exist (`IoScene` indices are `u32`).
fn ix32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

fn node_ix(i: usize) -> NodeIx {
    NodeIx(ix32(i))
}

/// The skin joined meshes will use, grown as members are accepted.
struct Union {
    joints: Vec<NodeIx>,
    inverse_bind: Vec<Mat4>,
    skeleton: Option<NodeIx>,
}

/// One target skinned node and the nodes that move into it.
struct SkinnedPlan {
    target: NodeIx,
    union: Union,
    /// `(node, joint index in the target's joined skin for each joint of the node's own skin)`.
    members: Vec<(NodeIx, Vec<u16>)>,
}

/// The skin of `node` if it can join anything: one matrix per joint, each joint once, every vertex joint in range.
fn usable_skin<'a>(scene: &'a IoScene, facts: &Facts, node: NodeIx) -> Option<&'a Skin> {
    let n = scene.nodes.get(node.index())?;
    let skin = scene.skins.get(n.skin?.index())?;
    let distinct: BTreeSet<NodeIx> = skin.joints.iter().copied().collect();
    let sound = skin.inverse_bind.len() == skin.joints.len() && distinct.len() == skin.joints.len();
    let mesh = scene.meshes.get(n.mesh?.index())?;
    let vertices_ok = mesh.primitives.iter().all(|p| {
        let m = &p.mesh;
        m.joints.len() == m.weights.len()
            && !m.joints.is_empty()
            && m.joints.iter().flatten().all(|&j| usize::from(j) < skin.joints.len())
    });
    (sound && vertices_ok && facts.mesh_is_free(scene, n)).then_some(skin)
}

fn plan_skinned(scene: &IoScene, facts: &Facts, apart: &mut Vec<ApartPair>) -> Vec<SkinnedPlan> {
    let mut plans: Vec<SkinnedPlan> = Vec::new();
    for (i, node) in scene.nodes.iter().enumerate() {
        let ix = node_ix(i);
        if node.skin.is_none() {
            continue;
        }
        let Some(skin) = usable_skin(scene, facts, ix) else { continue };
        let mut refused = Vec::new();
        let mut joined = false;
        for plan in &mut plans {
            match try_join(&plan.union, facts.flag(plan.target) == facts.flag(ix), skin) {
                Ok(remap) => {
                    absorb(&mut plan.union, skin);
                    plan.members.push((ix, remap));
                    joined = true;
                    break;
                }
                Err(reason) => refused.push(ApartPair { target: plan.target, other: ix, reason }),
            }
        }
        if !joined {
            apart.extend(refused);
            let union =
                Union { joints: skin.joints.clone(), inverse_bind: skin.inverse_bind.clone(), skeleton: skin.skeleton };
            plans.push(SkinnedPlan { target: ix, union, members: Vec::new() });
        }
    }
    plans.retain(|p| !p.members.is_empty());
    plans
}

/// Whether `skin` can join `union`, and if so where each of its joints lands in the joined skin.
fn try_join(union: &Union, same_flag: bool, skin: &Skin) -> Result<Vec<u16>, ApartReason> {
    if !same_flag {
        return Err(ApartReason::FirstPerson);
    }
    if union.skeleton.is_some() && skin.skeleton.is_some() && union.skeleton != skin.skeleton {
        return Err(ApartReason::Skeleton);
    }
    let position: BTreeMap<NodeIx, usize> = union.joints.iter().enumerate().map(|(k, &j)| (j, k)).collect();
    let mut remap = Vec::with_capacity(skin.joints.len());
    let mut added = 0usize;
    for (&joint, matrix) in skin.joints.iter().zip(&skin.inverse_bind) {
        let slot = if let Some(&k) = position.get(&joint) {
            if !same_matrix(&union.inverse_bind[k], matrix) {
                return Err(ApartReason::InverseBind { joint });
            }
            k
        } else {
            added += 1;
            union.joints.len() + added - 1
        };
        remap.push(u16::try_from(slot).map_err(|_| ApartReason::TooManyJoints)?);
    }
    Ok(remap)
}

/// Element-wise within [`IBM_TOLERANCE`]; a non-finite element never matches.
fn same_matrix(a: &Mat4, b: &Mat4) -> bool {
    a.to_cols_array().iter().zip(b.to_cols_array()).all(|(x, y)| (x - y).abs() <= IBM_TOLERANCE)
}

/// Adds the joints of `skin` that `union` lacks, in the skin's order.
fn absorb(union: &mut Union, skin: &Skin) {
    for (&joint, &matrix) in skin.joints.iter().zip(&skin.inverse_bind) {
        if !union.joints.contains(&joint) {
            union.joints.push(joint);
            union.inverse_bind.push(matrix);
        }
    }
    union.skeleton = union.skeleton.or(skin.skeleton);
}

fn apply_skinned(
    scene: &mut IoScene,
    plan: &SkinnedPlan,
    merged: &mut Vec<NodeIx>,
    orphaned_meshes: &mut BTreeSet<MeshIx>,
    orphaned_skins: &mut BTreeSet<SkinIx>,
) {
    let Some(target_mesh) = scene.nodes[plan.target.index()].mesh else { return };
    let mut moved: Vec<Primitive> = Vec::new();
    for (node, remap) in &plan.members {
        let source = &mut scene.nodes[node.index()];
        let (mesh, skin) = (source.mesh.take(), source.skin.take());
        let Some(mesh) = mesh else { continue };
        let mut prims = std::mem::take(&mut scene.meshes[mesh.index()].primitives);
        for prim in &mut prims {
            for slot in prim.mesh.joints.iter_mut().flatten() {
                *slot = remap[usize::from(*slot)];
            }
        }
        moved.append(&mut prims);
        orphaned_meshes.insert(mesh);
        orphaned_skins.extend(skin);
        merged.push(*node);
    }
    scene.meshes[target_mesh.index()].primitives.append(&mut moved);

    let target = &mut scene.nodes[plan.target.index()];
    let Some(old_skin) = target.skin else { return };
    let mut joined = Skin::new(
        scene.skins[old_skin.index()].name.clone(),
        plan.union.joints.clone(),
        plan.union.inverse_bind.clone(),
    );
    joined.skeleton = plan.union.skeleton;
    // A skin that other nodes also use keeps its own joints; the joined one is a new skin.
    if skin_is_shared(scene, old_skin, plan) {
        let new = SkinIx(ix32(scene.skins.len()));
        scene.skins.push(joined);
        scene.nodes[plan.target.index()].skin = Some(new);
        orphaned_skins.insert(old_skin);
    } else {
        scene.skins[old_skin.index()] = joined;
    }
}

/// Whether a node outside the plan still uses `skin`. Counted on the scene as it stands: members have let go already.
fn skin_is_shared(scene: &IoScene, skin: SkinIx, plan: &SkinnedPlan) -> bool {
    scene.nodes.iter().enumerate().any(|(i, n)| n.skin == Some(skin) && node_ix(i) != plan.target)
}

/// One target basic node and the nodes baked into it.
struct BasicPlan {
    target: NodeIx,
    members: Vec<NodeIx>,
}

fn plan_basic(scene: &IoScene, facts: &Facts, apart: &mut Vec<ApartPair>) -> Vec<BasicPlan> {
    let mut plans: Vec<BasicPlan> = Vec::new();
    for (i, node) in scene.nodes.iter().enumerate() {
        let ix = node_ix(i);
        if node.skin.is_some() || !facts.mesh_is_free(scene, node) {
            continue;
        }
        let mut refused = Vec::new();
        let mut joined = false;
        for plan in &mut plans {
            match basic_pair(scene, facts, plan.target, ix) {
                Ok(()) => {
                    plan.members.push(ix);
                    joined = true;
                    break;
                }
                Err(reason) => refused.push(ApartPair { target: plan.target, other: ix, reason }),
            }
        }
        if !joined {
            apart.extend(refused);
            plans.push(BasicPlan { target: ix, members: Vec::new() });
        }
    }
    plans.retain(|p| !p.members.is_empty());
    plans
}

fn basic_pair(scene: &IoScene, facts: &Facts, target: NodeIx, other: NodeIx) -> Result<(), ApartReason> {
    if facts.parents[target.index()] != facts.parents[other.index()] {
        return Err(ApartReason::Parent);
    }
    if let Some(&node) = [target, other].iter().find(|n| facts.moving.contains(n)) {
        return Err(ApartReason::Moves { node });
    }
    if facts.flag(target) != facts.flag(other) {
        return Err(ApartReason::FirstPerson);
    }
    if local_matrix(&scene.nodes[target.index()].transform).determinant().abs() < MIN_DETERMINANT {
        return Err(ApartReason::SingularTransform);
    }
    Ok(())
}

fn local_matrix(t: &Transform) -> Mat4 {
    Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation)
}

fn apply_basic(
    scene: &mut IoScene,
    plan: &BasicPlan,
    merged: &mut Vec<NodeIx>,
    orphaned_meshes: &mut BTreeSet<MeshIx>,
) {
    let Some(target_mesh) = scene.nodes[plan.target.index()].mesh else { return };
    let target_transform = scene.nodes[plan.target.index()].transform;
    let into_target = local_matrix(&target_transform).inverse();
    let mut moved: Vec<Primitive> = Vec::new();
    for &node in &plan.members {
        let source = &mut scene.nodes[node.index()];
        let transform = source.transform;
        let Some(mesh) = source.mesh.take() else { continue };
        let mut prims = std::mem::take(&mut scene.meshes[mesh.index()].primitives);
        // The same placement needs no arithmetic, so the vertices stay bit-identical.
        if transform != target_transform {
            let bake = into_target * local_matrix(&transform);
            for prim in &mut prims {
                bake_primitive(prim, &bake);
            }
        }
        moved.append(&mut prims);
        orphaned_meshes.insert(mesh);
        merged.push(node);
    }
    scene.meshes[target_mesh.index()].primitives.append(&mut moved);
}

/// Moves a primitive's vertices through `bake`: positions as points, normals by the inverse transpose. A mirroring
/// bake turns every triangle inside out, so its winding is flipped back.
fn bake_primitive(prim: &mut Primitive, bake: &Mat4) {
    let linear = Mat3::from_mat4(*bake);
    let normal_matrix = linear.inverse().transpose();
    let m = &mut prim.mesh;
    for p in &mut m.positions {
        *p = bake.transform_point3(*p);
    }
    for n in &mut m.normals {
        let moved = normal_matrix * *n;
        *n = moved.try_normalize().unwrap_or(*n);
    }
    if bake.determinant() < 0.0 {
        for tri in m.indices.as_chunks_mut::<3>().0 {
            tri.swap(1, 2);
        }
    }
}

/// Removes the first-person annotations of nodes that no longer hold a mesh.
fn drop_annotations(scene: &mut IoScene, emptied: &[NodeIx]) {
    let Some(fp) = scene.avatar.as_mut().and_then(|a| a.first_person.as_mut()) else { return };
    fp.annotations.retain(|a| !emptied.contains(&a.node));
}

/// Removes the meshes and skins the merge emptied and renumbers the references to the survivors. A skin that was
/// already unused before the merge is left alone.
fn compact(scene: &mut IoScene, orphaned_meshes: &BTreeSet<MeshIx>, orphaned_skins: &BTreeSet<SkinIx>) {
    let still_used: BTreeSet<SkinIx> = scene.nodes.iter().filter_map(|n| n.skin).collect();
    let mesh_map = survivors(scene.meshes.len(), |i| orphaned_meshes.contains(&MeshIx(ix32(i))));
    let skin_map = survivors(scene.skins.len(), |i| {
        orphaned_skins.contains(&SkinIx(ix32(i))) && !still_used.contains(&SkinIx(ix32(i)))
    });
    retain_by(&mut scene.meshes, &mesh_map);
    retain_by(&mut scene.skins, &skin_map);
    for node in &mut scene.nodes {
        node.mesh = node.mesh.and_then(|m| mesh_map.get(m.index()).copied().flatten().map(MeshIx));
        node.skin = node.skin.and_then(|s| skin_map.get(s.index()).copied().flatten().map(SkinIx));
    }
}

/// For each old position, its new one, or `None` when `removed`.
fn survivors(len: usize, removed: impl Fn(usize) -> bool) -> Vec<Option<u32>> {
    let mut next = 0u32;
    (0..len)
        .map(|i| {
            if removed(i) {
                None
            } else {
                next += 1;
                Some(next - 1)
            }
        })
        .collect()
}

fn retain_by<T>(items: &mut Vec<T>, map: &[Option<u32>]) {
    let mut keep = map.iter();
    items.retain(|_| keep.next().is_some_and(Option::is_some));
}
