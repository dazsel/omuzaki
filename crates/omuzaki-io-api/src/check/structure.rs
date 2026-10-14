//! The intermediate scene's own invariants (`scene.*` and `avatar.*` rules). A scene that passes can be indexed
//! without bounds worries by every consumer; that is the guarantee [`run_import`](crate::run_import) gives.

use super::graph::{Tour, components};
use crate::{Avatar, Expression, ExpressionName, Finding, IoScene, Mesh, NodeConstraint, NodeIx, Report};

/// Records an out-of-range index and returns true if it was one.
fn out_of_range(r: &mut Report, what: &str, at: &str, ix: usize, len: usize) -> bool {
    if ix < len {
        return false;
    }
    r.push(Finding::error("scene.index_out_of_range", format!("{what} {ix} doesn't exist (there are {len})")).at(at));
    true
}

/// Runs every structural check, in a fixed order.
pub(super) fn check(scene: &IoScene) -> Report {
    let mut r = Report::new();
    let parents = hierarchy(scene, &mut r);
    nodes(scene, &mut r);
    meshes(scene, &mut r);
    skins(scene, &mut r);
    skinned_vertices(scene, &mut r);
    materials(scene, &mut r);
    if let Some(ix) = scene.meta.thumbnail_image {
        out_of_range(&mut r, "image", "meta/thumbnailImage", ix.index(), scene.images.len());
    }
    if let Some(avatar) = &scene.avatar {
        avatar_layer(scene, avatar, &Tour::new(&parents), &mut r);
    }
    r
}

/// Builds the parent table and reports nodes with several parents, roots that have parents, and cycles.
fn hierarchy(scene: &IoScene, r: &mut Report) -> Vec<Option<usize>> {
    let n = scene.nodes.len();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    for (i, node) in scene.nodes.iter().enumerate() {
        for (j, child) in node.children.iter().enumerate() {
            let at = format!("nodes/{i}/children/{j}");
            if out_of_range(r, "node", &at, child.index(), n) {
                continue;
            }
            if parent[child.index()].replace(i).is_some() {
                r.push(
                    Finding::error("scene.node_multiple_parents", format!("node {} has more than one parent", child.0))
                        .at(at),
                );
            }
        }
    }
    for (j, root) in scene.roots.iter().enumerate() {
        let at = format!("roots/{j}");
        if !out_of_range(r, "node", &at, root.index(), n) && parent[root.index()].is_some() {
            r.push(
                Finding::error("scene.root_has_parent", format!("root node {} is also somebody's child", root.0))
                    .at(at),
            );
        }
    }
    if let Some(node) = find_cycle(&parent) {
        r.push(
            Finding::error("scene.node_cycle", format!("node {node} is its own ancestor")).at(format!("nodes/{node}")),
        );
    }
    parent
}

/// A node on a parent cycle, if any. Linear: each node is walked once.
fn find_cycle(parent: &[Option<usize>]) -> Option<usize> {
    // 0 = unvisited, 1 = on the path being walked, 2 = known to reach a root.
    let mut state = vec![0u8; parent.len()];
    for start in 0..parent.len() {
        let mut path = Vec::new();
        let mut cur = Some(start);
        while let Some(n) = cur {
            match state[n] {
                2 => break,
                1 => return Some(n),
                _ => {}
            }
            state[n] = 1;
            path.push(n);
            cur = parent[n];
        }
        for n in path {
            state[n] = 2;
        }
    }
    None
}

fn nodes(scene: &IoScene, r: &mut Report) {
    for (i, node) in scene.nodes.iter().enumerate() {
        let t = &node.transform;
        let finite = t.translation.is_finite() && t.rotation.is_finite() && t.scale.is_finite();
        if !finite {
            r.push(Finding::error("scene.non_finite", "a transform contains NaN or infinity").at(format!("nodes/{i}")));
        } else if (t.rotation.length() - 1.0).abs() > 1e-3 {
            r.push(
                Finding::error("scene.rotation_not_unit", format!("rotation has length {}", t.rotation.length()))
                    .at(format!("nodes/{i}/rotation"))
                    .hint("normalise the quaternion"),
            );
        }
        if let Some(m) = node.mesh {
            out_of_range(r, "mesh", &format!("nodes/{i}/mesh"), m.index(), scene.meshes.len());
        }
        if let Some(s) = node.skin {
            out_of_range(r, "skin", &format!("nodes/{i}/skin"), s.index(), scene.skins.len());
            if node.mesh.is_none() {
                r.push(
                    Finding::warning("scene.skin_without_mesh", "a skin is set on a node with no mesh")
                        .at(format!("nodes/{i}")),
                );
            }
        }
    }
}

fn meshes(scene: &IoScene, r: &mut Report) {
    for (mi, mesh) in scene.meshes.iter().enumerate() {
        for (pi, prim) in mesh.primitives.iter().enumerate() {
            primitive(scene, mi, pi, prim, r);
        }
        let count = mesh.morph_target_count();
        if mesh.primitives.iter().any(|p| !p.morph_targets.is_empty() && p.morph_targets.len() != count) {
            let msg = "primitives of one mesh must define the same number of morph targets (glTF 2.0, Morph Targets)";
            r.push(
                Finding::error("scene.morph_target_mismatch", msg)
                    .at(format!("meshes/{mi}"))
                    .hint("pad the others with zero deltas"),
            );
        }
        if !mesh.morph_target_names.is_empty() && mesh.morph_target_names.len() != count {
            let msg = format!("{} target names for {count} targets", mesh.morph_target_names.len());
            r.push(Finding::error("scene.morph_target_mismatch", msg).at(format!("meshes/{mi}/morphTargetNames")));
        }
    }
}

fn primitive(scene: &IoScene, mi: usize, pi: usize, prim: &crate::Primitive, r: &mut Report) {
    let at = format!("meshes/{mi}/primitives/{pi}");
    let m = &prim.mesh;
    let n = m.positions.len();
    let errors_before = r.errors().count();
    if !m.positions.iter().chain(&m.normals).all(|v| v.is_finite()) {
        r.push(
            Finding::error("scene.non_finite", "a vertex position or normal is NaN or infinite")
                .at(format!("{at}/positions")),
        );
    }
    // Normals are required (the importer generates them when the file has none); every other attribute may be absent.
    let lengths = [
        ("normals", m.normals.len(), true),
        ("uvs", m.uvs.len(), false),
        ("colours", m.colours.len(), false),
        ("joints", m.joints.len(), false),
        ("weights", m.weights.len(), false),
    ];
    for (name, len, required) in lengths {
        if len != n && (required || len != 0) {
            r.push(
                Finding::error("scene.attribute_length_mismatch", format!("{name} has {len} entries for {n} vertices"))
                    .at(format!("{at}/{name}")),
            );
        }
    }
    if m.joints.is_empty() != m.weights.is_empty() {
        r.push(Finding::error("scene.skin_attributes_incomplete", "joints and weights must come together").at(&at));
    }
    if m.weights.iter().any(|w| (w.iter().sum::<f32>() - 1.0).abs() > 1e-3) {
        let msg = "skinning weights don't sum to 1 for some vertex";
        r.push(Finding::warning("scene.weights_not_normalised", msg).at(format!("{at}/weights")));
    }
    if n == 0 || m.indices.is_empty() {
        r.push(Finding::error("scene.indices_invalid", "a primitive needs at least one triangle").at(&at));
    }
    if !m.indices.len().is_multiple_of(3) {
        let msg = format!("{} indices don't make whole triangles", m.indices.len());
        r.push(Finding::error("scene.indices_invalid", msg).at(format!("{at}/indices")));
    }
    if m.indices.iter().any(|&i| i as usize >= n) {
        let msg = format!("an index is past the {n} vertices");
        r.push(Finding::error("scene.indices_invalid", msg).at(format!("{at}/indices")));
    }
    if let Some(mat) = prim.material {
        out_of_range(r, "material", &format!("{at}/material"), mat.index(), scene.materials.len());
    }
    // The shared mesh type owns the definition of a valid mesh; anything it rejects that the finer rules above missed
    // (a malformed extra attribute, or a rule added to it later) still surfaces here.
    if r.errors().count() == errors_before
        && let Err(e) = m.validate()
    {
        r.push(Finding::error("scene.mesh_invalid", e.to_string()).at(&at));
    }
    for (ti, target) in prim.morph_targets.iter().enumerate() {
        let bad = target.positions.len() != n || (!target.normals.is_empty() && target.normals.len() != n);
        if bad {
            let msg = format!("morph target {ti} doesn't have {n} deltas");
            r.push(Finding::error("scene.attribute_length_mismatch", msg).at(format!("{at}/targets/{ti}")));
        }
    }
}

fn skins(scene: &IoScene, r: &mut Report) {
    let node_count = scene.nodes.len();
    for (i, skin) in scene.skins.iter().enumerate() {
        for (j, joint) in skin.joints.iter().enumerate() {
            out_of_range(r, "node", &format!("skins/{i}/joints/{j}"), joint.index(), node_count);
        }
        if skin.inverse_bind.len() != skin.joints.len() {
            let msg = format!("{} inverse bind matrices for {} joints", skin.inverse_bind.len(), skin.joints.len());
            r.push(Finding::error("scene.skin_matrices", msg).at(format!("skins/{i}/inverseBindMatrices")));
        }
        if let Some(s) = skin.skeleton {
            out_of_range(r, "node", &format!("skins/{i}/skeleton"), s.index(), node_count);
        }
    }
}

/// Every `JOINTS_0` value must name a joint of the skin that deforms the mesh.
fn skinned_vertices(scene: &IoScene, r: &mut Report) {
    for (i, node) in scene.nodes.iter().enumerate() {
        let (Some(mesh), Some(skin)) =
            (node.mesh.and_then(|m| scene.meshes.get(m.index())), node.skin.and_then(|s| scene.skins.get(s.index())))
        else {
            continue;
        };
        let joints = skin.joints.len();
        let bad = mesh.primitives.iter().any(|p| p.mesh.joints.iter().flatten().any(|&j| usize::from(j) >= joints));
        if bad {
            r.push(
                Finding::error("scene.joint_out_of_range", format!("a vertex uses a joint past the skin's {joints}"))
                    .at(format!("nodes/{i}")),
            );
        }
    }
}

fn materials(scene: &IoScene, r: &mut Report) {
    for (i, tex) in scene.textures.iter().enumerate() {
        out_of_range(r, "image", &format!("textures/{i}/source"), tex.image.index(), scene.images.len());
    }
    for (i, mat) in scene.materials.iter().enumerate() {
        for (slot, t) in mat.texture_refs() {
            out_of_range(r, "texture", &format!("materials/{i}/{slot}"), t.texture.index(), scene.textures.len());
            if t.uv_set != 0 {
                let msg = format!("{slot} reads TEXCOORD_{}, but only TEXCOORD_0 is kept", t.uv_set);
                r.push(Finding::warning("scene.uv_set_dropped", msg).at(format!("materials/{i}/{slot}")));
            }
        }
    }
}

fn avatar_layer(scene: &IoScene, avatar: &Avatar, tour: &Tour, r: &mut Report) {
    let nodes = scene.nodes.len();
    let mut seen = std::collections::BTreeMap::new();
    for (bone, node) in &avatar.humanoid.bones {
        let at = format!("avatar/humanoid/{bone}");
        if !out_of_range(r, "node", &at, node.index(), nodes)
            && let Some(other) = seen.insert(node.0, *bone)
        {
            r.push(
                Finding::error("avatar.humanoid_duplicate_node", format!("node {} is both {other} and {bone}", node.0))
                    .at(at),
            );
        }
    }
    for (name, expr) in &avatar.expressions {
        expression(scene, name, expr, r);
    }
    spring_bones(scene, avatar, tour, r);
    constraints(scene, avatar, r);
    constraint_cycles(scene, avatar, r);
    if let Some(look) = &avatar.look_at {
        let finite = look.offset_from_head_bone.is_finite();
        if !finite {
            r.push(Finding::error("scene.non_finite", "lookAt offset is NaN or infinite").at("avatar/lookAt"));
        }
    }
    for (i, a) in avatar.first_person.iter().flat_map(|f| f.annotations.iter()).enumerate() {
        out_of_range(r, "node", &format!("avatar/firstPerson/meshAnnotations/{i}"), a.node.index(), nodes);
    }
}

fn expression(scene: &IoScene, name: &ExpressionName, expr: &Expression, r: &mut Report) {
    let at = format!("avatar/expressions/{name}");
    for (i, bind) in expr.morph_binds.iter().enumerate() {
        let at = format!("{at}/morphTargetBinds/{i}");
        if !(0.0..=1.0).contains(&bind.weight) {
            r.push(
                Finding::error("avatar.bind_weight_range", format!("weight {} isn't in 0..=1", bind.weight))
                    .at(&at)
                    .hint("VRM 1.0 weights are 0..1; 0.x used 0..100"),
            );
        }
        let mesh: Option<&Mesh> =
            scene.nodes.get(bind.node.index()).and_then(|n| n.mesh).and_then(|m| scene.meshes.get(m.index()));
        match mesh {
            None => r.push(
                Finding::error(
                    "avatar.morph_bind_invalid",
                    format!("node {} doesn't exist or has no mesh", bind.node.0),
                )
                .at(&at),
            ),
            Some(m) if bind.index as usize >= m.morph_target_count() => {
                let msg =
                    format!("morph target {} doesn't exist (the mesh has {})", bind.index, m.morph_target_count());
                r.push(Finding::error("avatar.morph_bind_invalid", msg).at(&at));
            }
            Some(_) => {}
        }
    }
    let materials = scene.materials.len();
    let binds = expr.color_binds.iter().map(|b| b.material).chain(expr.uv_binds.iter().map(|b| b.material));
    for (i, m) in binds.enumerate() {
        out_of_range(r, "material", &format!("{at}/materialBinds/{i}"), m.index(), materials);
    }
}

fn spring_bones(scene: &IoScene, avatar: &Avatar, tour: &Tour, r: &mut Report) {
    let sb = &avatar.spring_bones;
    let nodes = scene.nodes.len();
    for (i, c) in sb.colliders.iter().enumerate() {
        out_of_range(r, "node", &format!("avatar/springBone/colliders/{i}/node"), c.node.index(), nodes);
    }
    for (i, g) in sb.collider_groups.iter().enumerate() {
        for &c in &g.colliders {
            out_of_range(
                r,
                "collider",
                &format!("avatar/springBone/colliderGroups/{i}"),
                c as usize,
                sb.colliders.len(),
            );
        }
    }
    let mut owner = std::collections::BTreeMap::new();
    for (ci, chain) in sb.chains.iter().enumerate() {
        let at = format!("avatar/springBone/springs/{ci}");
        for &g in &chain.collider_groups {
            out_of_range(r, "collider group", &at, g as usize, sb.collider_groups.len());
        }
        let ids: Vec<usize> = chain.joints.iter().map(|j| j.node.index()).collect();
        if ids.iter().any(|&n| out_of_range(r, "node", &format!("{at}/joints"), n, nodes)) {
            continue;
        }
        for (a, b) in ids.iter().zip(ids.iter().skip(1)) {
            if a == b || !tour.is_below(*b, *a) {
                let msg = format!("joint {b} isn't a descendant of the joint {a} before it");
                r.push(
                    Finding::error("avatar.spring_chain_order", msg)
                        .at(format!("{at}/joints"))
                        .hint("each joint must be a child or deeper descendant of the previous"),
                );
            }
        }
        for n in ids {
            if let Some(first) = owner.insert(n, ci)
                && first != ci
            {
                r.push(
                    Finding::error("avatar.spring_joint_shared", format!("node {n} is in springs {first} and {ci}"))
                        .at(&at),
                );
            }
        }
        if let Some(center) = chain.center {
            let first = chain.joints.first().map(|j| j.node.index());
            let ok = first.is_some_and(|f| center.index() < nodes && tour.is_below(f, center.index()));
            if !ok {
                r.push(
                    Finding::error(
                        "avatar.spring_center",
                        "the centre must be the first joint or one of its ancestors",
                    )
                    .at(format!("{at}/center")),
                );
            }
        }
    }
}

/// The node that drives the constraint.
fn constraint_source(c: &NodeConstraint) -> NodeIx {
    use crate::ConstraintKind::{Aim, Roll, Rotation};
    match c.kind {
        Roll { source, .. } | Aim { source, .. } | Rotation { source } => source,
    }
}

fn constraints(scene: &IoScene, avatar: &Avatar, r: &mut Report) {
    let nodes = scene.nodes.len();
    for (i, c) in avatar.constraints.iter().enumerate() {
        let at = format!("avatar/constraints/{i}");
        let source = constraint_source(c);
        let bad =
            out_of_range(r, "node", &at, c.node.index(), nodes) | out_of_range(r, "node", &at, source.index(), nodes);
        if !bad && source == c.node {
            r.push(Finding::error("avatar.constraint_invalid", "a node can't be its own constraint source").at(&at));
        }
        if !(0.0..=1.0).contains(&c.weight) {
            r.push(Finding::error("avatar.constraint_invalid", format!("weight {} isn't in 0..=1", c.weight)).at(&at));
        }
    }
}

/// How many node numbers a cycle finding lists before summing up the rest.
const CYCLE_NODES_SHOWN: usize = 8;

/// `VRMC_node_constraint` 1.0: "The source must not make a circular dependency between two or more constraints."
/// Edges run from each constrained node to its source. One error per strongly connected component of two or more
/// nodes, located at its first constraint; constraints that only feed a cycle are fine. A node that is its own source
/// is a cycle of one, which `avatar.constraint_invalid` already reports.
fn constraint_cycles(scene: &IoScene, avatar: &Avatar, r: &mut Report) {
    let nodes = scene.nodes.len();
    // Constraints naming a missing node were reported as out of range and have no place in the graph.
    let edges: Vec<(usize, usize, usize)> = avatar
        .constraints
        .iter()
        .enumerate()
        .map(|(i, c)| (i, c.node.index(), constraint_source(c).index()))
        .filter(|&(_, from, to)| from < nodes && to < nodes)
        .collect();
    let comp = components(nodes, edges.iter().map(|&(_, from, to)| (from, to)));
    // One pass for every component's size and its first few nodes, so the work stays linear however many cycles
    // there are.
    let mut size = vec![0usize; nodes];
    let mut shown: Vec<Vec<usize>> = vec![Vec::new(); nodes];
    for (n, &c) in comp.iter().enumerate() {
        size[c] += 1;
        if shown[c].len() < CYCLE_NODES_SHOWN {
            shown[c].push(n);
        }
    }
    let mut reported = std::collections::BTreeSet::new();
    for &(i, from, to) in &edges {
        let c = comp[from];
        if comp[to] != c || size[c] < 2 || !reported.insert(c) {
            continue;
        }
        let members: Vec<String> = shown[c].iter().map(ToString::to_string).collect();
        let more = size[c] - members.len();
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        let msg = format!("constraints form a circular dependency through nodes {}{tail}", members.join(", "));
        r.push(
            Finding::error("avatar.constraint_cycle", msg)
                .at(format!("avatar/constraints/{i}"))
                .hint("a constraint's source must not itself depend on the constrained node"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConstraintKind, SpringChain, SpringJoint};

    /// A scene whose node `i` is the parent of node `i + 1`, with an avatar.
    fn spine(n: u32) -> IoScene {
        let mut s = IoScene::default();
        for i in 0..n {
            let mut node = crate::Node::new(format!("n{i}"));
            if i + 1 < n {
                node.children.push(NodeIx(i + 1));
            }
            s.nodes.push(node);
        }
        s.roots.push(NodeIx(0));
        s.avatar = Some(Avatar::new());
        s
    }

    fn constrain(s: &mut IoScene, node: u32, source: u32) {
        let kind = ConstraintKind::Rotation { source: NodeIx(source) };
        s.avatar.as_mut().unwrap().constraints.push(NodeConstraint { node: NodeIx(node), kind, weight: 1.0 });
    }

    fn cycle_findings(s: &IoScene) -> Vec<Finding> {
        s.check().findings.into_iter().filter(|f| f.rule == "avatar.constraint_cycle").collect()
    }

    #[test]
    fn a_two_cycle_is_one_error_at_the_first_constraint() {
        let mut s = spine(3);
        constrain(&mut s, 2, 0);
        constrain(&mut s, 0, 1);
        constrain(&mut s, 1, 0);
        let found = cycle_findings(&s);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].location.as_deref(), Some("avatar/constraints/1"));
        assert!(found[0].message.contains("nodes 0, 1"), "{}", found[0].message);
        assert_eq!(s.check().errors().count(), 1);
    }

    #[test]
    fn a_three_cycle_with_feeders_is_reported_once_and_the_feeders_are_fine() {
        let mut s = spine(6);
        constrain(&mut s, 0, 1);
        constrain(&mut s, 1, 2);
        constrain(&mut s, 2, 0);
        constrain(&mut s, 3, 0); // reads the cycle
        constrain(&mut s, 2, 4); // the cycle reads this
        constrain(&mut s, 4, 5);
        let found = cycle_findings(&s);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_eq!(found[0].location.as_deref(), Some("avatar/constraints/0"));
        assert!(found[0].message.contains("nodes 0, 1, 2") && !found[0].message.contains("more"));
        assert_eq!(s.check().errors().count(), 1);
    }

    #[test]
    fn separate_cycles_get_separate_errors_and_a_self_source_stays_with_its_own_rule() {
        let mut s = spine(6);
        constrain(&mut s, 0, 1);
        constrain(&mut s, 1, 0);
        constrain(&mut s, 2, 3);
        constrain(&mut s, 3, 2);
        constrain(&mut s, 4, 4);
        let report = s.check();
        assert_eq!(report.findings.iter().filter(|f| f.rule == "avatar.constraint_cycle").count(), 2);
        assert_eq!(report.findings.iter().filter(|f| f.rule == "avatar.constraint_invalid").count(), 1);
    }

    #[test]
    fn a_hundred_thousand_ring_neither_overflows_the_stack_nor_floods_the_message() {
        const N: u32 = 100_000;
        let mut s = spine(N);
        for i in 0..N {
            constrain(&mut s, i, (i + 1) % N);
        }
        let found = cycle_findings(&s);
        assert_eq!(found.len(), 1);
        assert!(found[0].message.contains("and 99992 more"), "{}", found[0].message);
    }

    #[test]
    fn fifty_thousand_disjoint_cycles_each_get_one_error_in_linear_time() {
        const PAIRS: u32 = 50_000;
        let mut s = spine(2 * PAIRS);
        for k in 0..PAIRS {
            constrain(&mut s, 2 * k, 2 * k + 1);
            constrain(&mut s, 2 * k + 1, 2 * k);
        }
        let found = cycle_findings(&s);
        assert_eq!(found.len(), PAIRS as usize);
        assert_eq!(found[7].location.as_deref(), Some("avatar/constraints/14"));
        assert!(found[7].message.ends_with("nodes 14, 15"), "{}", found[7].message);
    }

    #[test]
    fn constraints_naming_missing_nodes_are_left_to_the_range_rule() {
        let mut s = spine(2);
        constrain(&mut s, 0, 99);
        constrain(&mut s, 99, 0);
        let report = s.check();
        assert!(report.has_rule("scene.index_out_of_range") && !report.has_rule("avatar.constraint_cycle"));
    }

    /// 50k nodes deep with a two-joint chain from node `i` to node `N - i` for each `i`: every chain is valid, and
    /// walking up from each lower joint would take about N² / 4 steps in all (over 6e8).
    #[test]
    fn a_deep_spine_with_many_spring_chains_is_checked_in_linear_time() {
        const N: u32 = 50_000;
        let mut s = spine(N);
        let chain = |a: u32, b: u32| SpringChain {
            joints: vec![SpringJoint::new(NodeIx(a)), SpringJoint::new(NodeIx(b))],
            center: Some(NodeIx(a)),
            ..SpringChain::default()
        };
        s.avatar.as_mut().unwrap().spring_bones.chains = (1..N / 2).map(|i| chain(i, N - i)).collect();
        let report = s.check();
        assert_eq!(report.errors().count(), 0, "{:#?}", report.findings.first());
    }

    #[test]
    fn ancestors_follow_the_hierarchy_and_a_parent_cycle_is_below_nothing() {
        let tour = Tour::new(&[None, Some(0), Some(1), Some(0), Some(5), Some(4)]);
        assert!(tour.is_below(2, 0) && tour.is_below(2, 1) && tour.is_below(3, 0) && tour.is_below(2, 2));
        assert!(!tour.is_below(0, 2) && !tour.is_below(3, 1) && !tour.is_below(2, 3));
        assert!(tour.is_below(4, 4) && !tour.is_below(4, 5) && !tour.is_below(2, 4) && !tour.is_below(9, 0));
    }
}
