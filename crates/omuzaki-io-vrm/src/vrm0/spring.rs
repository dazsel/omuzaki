//! VRM 0.x spring bones (`VRM.secondaryAnimation`) to and from the 1.0 model of chains, colliders and groups.
//!
//! A 0.x *bone group* names root bones and gives one set of parameters; the runtime simulates **every node below each
//! root**, each with the next child as its tail (or an implicit tail 7 cm out for a leaf). 1.0 describes the same
//! thing as linear chains whose joints each descend from the one before, with the last joint serving only as a tail
//! (`VRMC_springBone-1.0/README.md`, "For `vrm0`"). So the importer:
//! - walks each root's subtree and cuts it into chains: a node continues its chain into its *first* child, and every
//!   other child starts a chain of its own (the spec says branches are separate chains);
//! - gives every joint the group's parameters;
//! - makes the implicit 7 cm tail explicit by adding a childless node `<leaf>_end` under each leaf, at 7 cm in the
//!   direction the leaf sits from its parent, as `UniVRM`'s migrator does.
//!
//! Going the other way, one 1.0 chain becomes one bone group rooted at its first joint, with the chain's parameters
//! averaged (0.x has one set per group) and its colliders regrouped by node (a 0.x collider group belongs to a single
//! node and holds spheres only, so capsules become a row of spheres).

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    Avatar, Collider, ColliderGroup, ColliderShape, Finding, IoScene, Node, NodeIx, Report, SpringChain, SpringJoint,
};

use super::wire::{WireBoneGroup, WireColliderGroup0, WireSecondary, WireSphere0, WireVec3};
use crate::ix32;

/// How far past a leaf the 0.x runtime puts the implicit tail.
const TAIL_LENGTH: f32 = 0.07;

/// A valid node index from a 0.x `i64` (negative means "none").
fn node_of(scene: &IoScene, raw: i64) -> Option<usize> {
    usize::try_from(raw).ok().filter(|&n| n < scene.nodes.len())
}

/// Cuts the subtree under `root` into chains of nodes (see the module docs). Iterative, so a hostile hierarchy can't
/// overflow the stack, and `visited` keeps a malformed one (a cycle, a shared child) from looping or repeating a node.
fn chains_under(scene: &IoScene, root: usize, visited: &mut [bool]) -> Vec<Vec<usize>> {
    let mut chains = Vec::new();
    let mut starts = vec![root];
    while let Some(start) = starts.pop() {
        let mut path = Vec::new();
        let mut cur = Some(start);
        while let Some(n) = cur {
            if visited.get(n).copied().unwrap_or(true) {
                break;
            }
            visited[n] = true;
            path.push(n);
            let kids: Vec<usize> =
                scene.nodes[n].children.iter().map(|c| c.index()).filter(|&c| c < scene.nodes.len()).collect();
            cur = kids.first().copied();
            starts.extend(kids.iter().skip(1).rev());
        }
        if !path.is_empty() {
            chains.push(path);
        }
    }
    chains
}

/// Adds the explicit tail node under `leaf` and returns it. The direction is the leaf's offset from its parent seen in
/// the leaf's own frame, which is what the 0.x runtime extends.
fn add_tail(scene: &mut IoScene, leaf: usize) -> NodeIx {
    let node = &scene.nodes[leaf];
    let t = node.transform;
    let dir = t.rotation.inverse() * t.translation.try_normalize().unwrap_or(Vec3::NEG_Y);
    let tail = scene.add_node(Node::new(format!("{}_end", scene.nodes[leaf].name)).at(dir * TAIL_LENGTH));
    scene.attach(NodeIx(ix32(leaf)), tail);
    tail
}

/// Converts `secondaryAnimation` into `av.spring_bones`, adding tail nodes to `scene`.
pub(crate) fn read(sec: &WireSecondary, scene: &mut IoScene, av: &mut Avatar) {
    let sb = &mut av.spring_bones;
    for g in &sec.collider_groups {
        let mut group = ColliderGroup::default();
        if let Some(node) = node_of(scene, g.node) {
            group.name.clone_from(&scene.nodes[node].name);
            for c in &g.colliders {
                group.colliders.push(ix32(sb.colliders.len()));
                sb.colliders.push(Collider {
                    node: NodeIx(ix32(node)),
                    shape: ColliderShape::Sphere { offset: Vec3::from_array(c.offset.to_array()), radius: c.radius },
                });
            }
        }
        // An unusable group stays as an empty one: chains refer to groups by index.
        sb.collider_groups.push(group);
    }
    let groups: Vec<(&WireBoneGroup, Vec<Vec<usize>>)> = sec
        .bone_groups
        .iter()
        .map(|g| {
            let mut visited = vec![false; scene.nodes.len()];
            let paths = g
                .bones
                .iter()
                .filter_map(|&b| node_of(scene, b))
                .flat_map(|root| chains_under(scene, root, &mut visited))
                .collect();
            (g, paths)
        })
        .collect();
    for (g, paths) in groups {
        for path in paths {
            sb.chains.push(chain(scene, g, &path));
        }
    }
}

fn chain(scene: &mut IoScene, g: &WireBoneGroup, path: &[usize]) -> SpringChain {
    let params = |node: NodeIx| {
        let mut j = SpringJoint::new(node);
        j.hit_radius = g.hit_radius.unwrap_or(j.hit_radius);
        j.stiffness = g.stiffiness.unwrap_or(j.stiffness);
        j.gravity_power = g.gravity_power.unwrap_or(j.gravity_power);
        j.gravity_dir = g.gravity_dir.map_or(j.gravity_dir, |d| Vec3::from_array(d.to_array()));
        j.drag_force = g.drag_force.unwrap_or(j.drag_force);
        j
    };
    let mut joints: Vec<SpringJoint> = path.iter().map(|&n| params(NodeIx(ix32(n)))).collect();
    let last = path[path.len() - 1];
    if scene.nodes[last].children.is_empty() {
        joints.push(SpringJoint::new(add_tail(scene, last)));
    }
    SpringChain {
        name: if g.comment.is_empty() { scene.nodes[path[0]].name.clone() } else { g.comment.clone() },
        joints,
        collider_groups: g.collider_groups.iter().filter_map(|&c| u32::try_from(c).ok()).collect(),
        center: g.center.and_then(|c| node_of(scene, c)).map(|c| NodeIx(ix32(c))),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Writing

/// Spheres that cover a capsule: at least its two ends, with spacing no wider than the radius (at most 16).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // 1..=15 steps, clamped
fn spheres_along(offset: Vec3, radius: f32, tail: Vec3) -> Vec<WireSphere0> {
    let length = (tail - offset).length();
    let steps = if radius > 0.0 { (length / radius).ceil().clamp(1.0, 15.0) } else { 1.0 };
    let n = steps as u32;
    (0..=n)
        .map(|i| {
            let at = offset.lerp(tail, i as f32 / steps);
            WireSphere0 { offset: WireVec3::from_array(at.to_array()), radius }
        })
        .collect()
}

/// `secondaryAnimation` for the scene's chains and colliders. Chains with fewer than two joints simulate nothing in
/// 1.0 and are skipped. Findings say what 0.x cannot hold.
pub(crate) fn write(scene: &IoScene, av: &Avatar, report: &mut Report) -> WireSecondary {
    let sb = &av.spring_bones;
    let mut out = WireSecondary::default();
    // 1.0 groups -> 0.x groups: split by node, since a 0.x group sits on one node.
    let mut group_map: Vec<Vec<i64>> = Vec::new();
    let mut capsules = false;
    for g in &sb.collider_groups {
        let mut ids = Vec::new();
        let mut by_node: Vec<(NodeIx, Vec<WireSphere0>)> = Vec::new();
        for c in g.colliders.iter().filter_map(|&c| sb.colliders.get(c as usize)) {
            let spheres = match c.shape {
                ColliderShape::Sphere { offset, radius } => {
                    vec![WireSphere0 { offset: WireVec3::from_array(offset.to_array()), radius }]
                }
                ColliderShape::Capsule { offset, radius, tail } => {
                    capsules = true;
                    spheres_along(offset, radius, tail)
                }
            };
            match by_node.iter_mut().find(|(n, _)| *n == c.node) {
                Some((_, list)) => list.extend(spheres),
                None => by_node.push((c.node, spheres)),
            }
        }
        for (node, colliders) in by_node {
            ids.push(i64::try_from(out.collider_groups.len()).unwrap_or(i64::MAX));
            out.collider_groups.push(WireColliderGroup0 { node: i64::from(node.0), colliders });
        }
        group_map.push(ids);
    }
    if capsules {
        report.push(
            Finding::warning(
                "vrm0.collider_capsule",
                "0.x colliders are spheres; capsules were written as rows of spheres",
            )
            .at("avatar/springBone/colliders"),
        );
    }
    let mut approximated = false;
    for chain in sb.chains.iter().filter(|c| c.joints.len() >= 2) {
        let (group, differs) = bone_group(scene, chain, &group_map);
        approximated |= differs;
        out.bone_groups.push(group);
    }
    if approximated {
        report.push(
            Finding::warning(
                "vrm0.spring_approximated",
                "0.x spring bone groups have one set of parameters and move every node below their root; chains whose joints differ were averaged",
            )
            .at("avatar/springBone/springs"),
        );
    }
    out
}

/// One chain as a 0.x bone group, and whether averaging changed anything (joints that differ, or nodes below the root
/// that the chain doesn't list and 0.x will move too).
fn bone_group(scene: &IoScene, chain: &SpringChain, group_map: &[Vec<i64>]) -> (WireBoneGroup, bool) {
    // The last joint is only a tail in 1.0; its own parameters are unused.
    let moving = &chain.joints[..chain.joints.len() - 1];
    let first = &moving[0];
    let same = moving.iter().all(|j| SpringJoint { node: first.node, ..*j } == *first);
    #[allow(clippy::cast_precision_loss)] // a chain has a handful of joints
    let n = moving.len() as f32;
    // Identical joints give their own value back exactly; a mean of equal floats can be a rounding step off.
    let mean = |f: &dyn Fn(&SpringJoint) -> f32| if same { f(first) } else { moving.iter().map(f).sum::<f32>() / n };
    let gravity = if same { first.gravity_dir } else { moving.iter().map(|j| j.gravity_dir).sum::<Vec3>() / n };
    let root = chain.joints[0].node;
    let below = count_below(scene, root.index());
    let group = WireBoneGroup {
        comment: chain.name.clone(),
        stiffiness: Some(mean(&|j| j.stiffness)),
        gravity_power: Some(mean(&|j| j.gravity_power)),
        gravity_dir: Some(WireVec3::from_array(gravity.try_normalize().unwrap_or(first.gravity_dir).to_array())),
        drag_force: Some(mean(&|j| j.drag_force)),
        center: Some(chain.center.map_or(-1, |c| i64::from(c.0))),
        hit_radius: Some(mean(&|j| j.hit_radius)),
        bones: vec![i64::from(root.0)],
        collider_groups: chain
            .collider_groups
            .iter()
            .filter_map(|&g| group_map.get(g as usize))
            .flatten()
            .copied()
            .collect(),
    };
    (group, !same || below > chain.joints.len())
}

/// The nodes in the subtree of `root`, itself included.
fn count_below(scene: &IoScene, root: usize) -> usize {
    let mut seen = vec![false; scene.nodes.len()];
    let mut stack = vec![root];
    let mut count = 0;
    while let Some(n) = stack.pop() {
        if seen.get(n).copied().unwrap_or(true) {
            continue;
        }
        seen[n] = true;
        count += 1;
        stack.extend(scene.nodes[n].children.iter().map(|c| c.index()));
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small tree: 0 -> 1 -> 2 -> 3, with 1 also having a second child 4 -> 5.
    fn tree() -> IoScene {
        let mut s = IoScene::new();
        for i in 0..6 {
            s.add_node(Node::new(format!("n{i}")).at(Vec3::new(0.0, -0.1, 0.0)));
        }
        for (p, c) in [(0, 1), (1, 2), (2, 3), (1, 4), (4, 5)] {
            s.attach(NodeIx(p), NodeIx(c));
        }
        s
    }

    #[test]
    fn a_subtree_is_cut_into_chains_that_each_descend() {
        let s = tree();
        let paths = chains_under(&s, 0, &mut [false; 6]);
        assert_eq!(paths, [vec![0, 1, 2, 3], vec![4, 5]], "the first child continues, the other child starts a chain");
        // A cycle or a shared child can't loop or repeat a node.
        let mut cyclic = tree();
        cyclic.attach(NodeIx(3), NodeIx(0));
        let paths = chains_under(&cyclic, 0, &mut [false; 6]);
        assert_eq!(paths.iter().map(Vec::len).sum::<usize>(), 6);
    }

    #[test]
    fn leaves_get_an_explicit_tail_seven_centimetres_along_their_own_direction() {
        let mut s = tree();
        let tail = add_tail(&mut s, 3);
        assert_eq!(s.nodes[3].children, [tail]);
        assert_eq!(s.nodes[tail.index()].name, "n3_end");
        let t = s.nodes[tail.index()].transform.translation;
        assert!((t - Vec3::new(0.0, -0.07, 0.0)).length() < 1e-6, "{t:?}");
    }

    #[test]
    fn capsules_become_rows_of_spheres_covering_both_ends() {
        let rows = spheres_along(Vec3::ZERO, 0.5, Vec3::new(0.0, 2.0, 0.0));
        assert_eq!(rows.len(), 5);
        assert_eq!((rows[0].offset.y, rows[4].offset.y), (0.0, 2.0));
        assert_eq!(spheres_along(Vec3::ZERO, 0.0, Vec3::Y).len(), 2);
    }
}
