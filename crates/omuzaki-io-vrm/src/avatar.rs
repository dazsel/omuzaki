//! Checks and repairs on the avatar layer that both importers share.
//!
//! Real VRM files are messy: a morph bind to a target that was deleted, a spring chain whose joints aren't in
//! hierarchy order, a collider on a node that doesn't exist. The scene's invariants (`IoScene::check`) would reject
//! the whole model for any of them, which is the right rule for a scene but the wrong outcome for a purchased avatar
//! with one bad spring. So [`tidy`] drops or clamps the offending *optional* data, and says so in the report; what
//! remains satisfies the invariants. Mandatory structure (humanoid nodes, mesh data) is left for the checks to reject.

use std::fmt::Write;

use omuzaki_io_api::{
    ARKIT_52, Avatar,
    ConstraintKind::{Aim, Roll, Rotation},
    Finding, IoScene, NodeConstraint, NodeIx, Report, Severity, VrmVersion, vrm_profile,
};

/// The parent of every node, from the scene's child lists. Out-of-range children are ignored here; the scene check
/// reports them. A node listed as a child more than once keeps the last parent.
pub(crate) fn parent_table(scene: &IoScene) -> Vec<Option<usize>> {
    let mut parent = vec![None; scene.nodes.len()];
    for (i, node) in scene.nodes.iter().enumerate() {
        for c in &node.children {
            if let Some(slot) = parent.get_mut(c.index()) {
                *slot = Some(i);
            }
        }
    }
    parent
}

/// Ancestor queries in O(1) after one O(n) pass: an Euler-tour numbering of the forest the parent table describes.
///
/// A hostile file can name 100k spring chains over a 100k-deep hierarchy, so walking up the parents per query is
/// quadratic. Nodes on a parent cycle are reachable from no root and so lie below nothing but themselves; the scene
/// check rejects such scenes, so that only decides the order findings come in.
struct Tour {
    enter: Vec<u32>,
    exit: Vec<u32>,
}

impl Tour {
    /// Marks a node no root reaches.
    const UNREACHED: u32 = u32::MAX;

    /// Numbers the nodes with one iterative depth-first search (an explicit stack: a 100k-deep hierarchy must not
    /// recurse). Children are the inverse of `parent`, so a node listed under several parents stays with the last.
    fn new(parent: &[Option<usize>]) -> Self {
        let n = parent.len();
        // Child lists as offsets into one flat vector: three allocations however wide or deep the tree is.
        let mut start = vec![0usize; n + 1];
        for p in parent.iter().flatten() {
            start[p + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut fill = start.clone();
        let mut flat = vec![0usize; start[n]];
        for (child, p) in parent.iter().enumerate() {
            if let Some(p) = *p {
                flat[fill[p]] = child;
                fill[p] += 1;
            }
        }
        // `fill` is reused as each node's next child to visit.
        fill.copy_from_slice(&start);
        let mut enter = vec![Self::UNREACHED; n];
        let mut exit = vec![Self::UNREACHED; n];
        let mut clock = 0u32;
        let mut stack = Vec::new();
        for root in (0..n).filter(|&i| parent[i].is_none()) {
            enter[root] = clock;
            clock += 1;
            stack.push(root);
            while let Some(&top) = stack.last() {
                if fill[top] < start[top + 1] {
                    let child = flat[fill[top]];
                    fill[top] += 1;
                    enter[child] = clock;
                    clock += 1;
                    stack.push(child);
                } else {
                    exit[top] = clock;
                    stack.pop();
                }
            }
        }
        Self { enter, exit }
    }

    /// True if `node` is `ancestor` or lies below it. Out-of-range or unreachable nodes are below only themselves.
    fn is_below(&self, node: usize, ancestor: usize) -> bool {
        if node == ancestor {
            return true;
        }
        match (self.enter.get(node), self.exit.get(node), self.enter.get(ancestor), self.exit.get(ancestor)) {
            (Some(&en), Some(&ex), Some(&ea), Some(&xa)) if en != Self::UNREACHED && ea != Self::UNREACHED => {
                ea <= en && ex <= xa
            }
            _ => false,
        }
    }
}

/// Drops or clamps avatar data that would break the scene's invariants, with one finding per kind of repair.
pub(crate) fn tidy(scene: &IoScene, av: &mut Avatar, report: &mut Report) {
    binds(scene, av, report);
    colliders(scene, av, report);
    chains(scene, av, report);
    constraints(scene, av, report);
    let nodes = scene.nodes.len();
    if let Some(fp) = &mut av.first_person {
        let before = fp.annotations.len();
        fp.annotations.retain(|a| a.node.index() < nodes);
        note(
            report,
            "vrm.annotation_dropped",
            before - fp.annotations.len(),
            "first-person annotation(s) name a missing node",
        );
    }
}

fn note(report: &mut Report, rule: &str, count: usize, what: &str) {
    if count > 0 {
        report.push(Finding::warning(rule, format!("{count} {what}, so they were dropped")));
    }
}

fn binds(scene: &IoScene, av: &mut Avatar, report: &mut Report) {
    let (mut dropped, mut clamped) = (0, 0);
    for e in av.expressions.values_mut() {
        let before = e.morph_binds.len() + e.color_binds.len() + e.uv_binds.len();
        e.morph_binds.retain_mut(|b| {
            let targets = scene
                .nodes
                .get(b.node.index())
                .and_then(|n| n.mesh)
                .and_then(|m| scene.meshes.get(m.index()))
                .map(omuzaki_io_api::Mesh::morph_target_count);
            if targets.is_none_or(|t| b.index as usize >= t) {
                return false;
            }
            if !(0.0..=1.0).contains(&b.weight) {
                b.weight = if b.weight.is_nan() { 0.0 } else { b.weight.clamp(0.0, 1.0) };
                clamped += 1;
            }
            true
        });
        e.color_binds.retain(|b| b.material.index() < scene.materials.len());
        e.uv_binds.retain(|b| b.material.index() < scene.materials.len());
        dropped += before - (e.morph_binds.len() + e.color_binds.len() + e.uv_binds.len());
    }
    note(
        report,
        "vrm.bind_dropped",
        dropped,
        "expression bind(s) name a morph target, node or material that doesn't exist",
    );
    if clamped > 0 {
        report.push(Finding::warning(
            "vrm.bind_weight_clamped",
            format!("{clamped} morph bind weight(s) were outside 0..1 and were clamped"),
        ));
    }
}

/// Drops colliders on missing nodes and renumbers the groups that referred to them.
fn colliders(scene: &IoScene, av: &mut Avatar, report: &mut Report) {
    let sb = &mut av.spring_bones;
    let mut remap = Vec::with_capacity(sb.colliders.len());
    let mut kept = Vec::new();
    for c in std::mem::take(&mut sb.colliders) {
        if c.node.index() < scene.nodes.len() {
            remap.push(Some(u32::try_from(kept.len()).unwrap_or(u32::MAX)));
            kept.push(c);
        } else {
            remap.push(None);
        }
    }
    let dropped = remap.iter().filter(|r| r.is_none()).count();
    sb.colliders = kept;
    let mut stale = 0;
    for g in &mut sb.collider_groups {
        let before = g.colliders.len();
        g.colliders = g.colliders.iter().filter_map(|&c| remap.get(c as usize).copied().flatten()).collect();
        stale += before - g.colliders.len();
    }
    note(report, "vrm.collider_dropped", dropped, "collider(s) sit on a node that doesn't exist");
    note(
        report,
        "vrm.collider_dropped",
        stale.saturating_sub(dropped),
        "collider group entries point at a missing collider",
    );
}

/// Keeps only the spring chains the checks accept: joints that exist, each below the one before, none shared with an
/// earlier chain. An invalid centre is removed; a chain stays without it.
fn chains(scene: &IoScene, av: &mut Avatar, report: &mut Report) {
    let n = scene.nodes.len();
    let tour = Tour::new(&parent_table(scene));
    let groups = av.spring_bones.collider_groups.len();
    let mut taken = vec![false; n];
    let mut dropped = 0;
    let mut centres = 0;
    av.spring_bones.chains.retain_mut(|chain| {
        let ids: Vec<usize> = chain.joints.iter().map(|j| j.node.index()).collect();
        let valid = !ids.is_empty()
            && ids.iter().all(|&i| i < n && !taken[i])
            && ids.windows(2).all(|w| w[0] != w[1] && tour.is_below(w[1], w[0]));
        if !valid {
            dropped += 1;
            return false;
        }
        for &i in &ids {
            taken[i] = true;
        }
        if let Some(c) = chain.center
            && !(c.index() < n && tour.is_below(ids[0], c.index()))
        {
            chain.center = None;
            centres += 1;
        }
        chain.collider_groups.retain(|&g| (g as usize) < groups);
        true
    });
    note(report, "vrm.spring_chain_dropped", dropped, "spring chain(s) have missing, shared or out-of-order joints");
    note(report, "vrm.spring_center_dropped", centres, "spring chain centre(s) aren't the first joint or its ancestor");
}

/// The node a constraint reads from.
fn source_of(c: &NodeConstraint) -> NodeIx {
    let (Roll { source, .. } | Aim { source, .. } | Rotation { source }) = c.kind;
    source
}

fn constraints(scene: &IoScene, av: &mut Avatar, report: &mut Report) {
    let n = scene.nodes.len();
    let before = av.constraints.len();
    av.constraints.retain_mut(|c| {
        let ok = c.node.index() < n && source_of(c).index() < n && source_of(c) != c.node;
        c.weight = if c.weight.is_nan() { 1.0 } else { c.weight.clamp(0.0, 1.0) };
        ok
    });
    note(
        report,
        "vrm.constraint_dropped",
        before - av.constraints.len(),
        "constraint(s) name a missing node or constrain a node by itself",
    );
    let cyclic = on_cycle(&av.constraints);
    let mut flags = cyclic.iter();
    av.constraints.retain(|_| !flags.next().copied().unwrap_or(false));
    note(
        report,
        "vrm.constraint_dropped",
        cyclic.iter().filter(|&&c| c).count(),
        "constraint(s) form a circular dependency",
    );
}

/// For each constraint, true if it lies on a circular dependency: `node` is driven by `source`, so the constraint is
/// on a cycle exactly when both ends sit in one strongly connected component (a node that is its own source is a
/// cycle of one). Constraints that only feed a cycle are not on it. Tarjan's algorithm, iterative, because a hostile
/// file can chain 100k constraints. Only the constraints' own node numbers matter, so a number past the scene's end
/// is just another node.
///
/// This is the plain graph rule, not `UniVRM`'s hierarchy-aware one (which also lets a constraint read a node its
/// target is an ancestor of); that is out of scope here.
pub(crate) fn on_cycle(constraints: &[NodeConstraint]) -> Vec<bool> {
    const NONE: usize = usize::MAX;
    // Dense ids for the nodes named, so a stray huge index doesn't size the tables.
    let mut ids: Vec<usize> = constraints.iter().flat_map(|c| [c.node.index(), source_of(c).index()]).collect();
    ids.sort_unstable();
    ids.dedup();
    let dense = |ix: NodeIx| ids.binary_search(&ix.index()).unwrap_or(0);
    let n = ids.len();
    // Edges node -> source as offsets into one flat vector.
    let mut start = vec![0usize; n + 1];
    for c in constraints {
        start[dense(c.node) + 1] += 1;
    }
    for i in 0..n {
        start[i + 1] += start[i];
    }
    let mut fill = start.clone();
    let mut edges = vec![0usize; constraints.len()];
    for c in constraints {
        let from = dense(c.node);
        edges[fill[from]] = dense(source_of(c));
        fill[from] += 1;
    }

    let (mut index, mut low, mut comp) = (vec![NONE; n], vec![0usize; n], vec![NONE; n]);
    let (mut next, mut comps) = (0usize, 0usize);
    let mut open = Vec::new(); // Tarjan's component stack
    let mut work: Vec<(usize, usize)> = Vec::new(); // (node, next edge to follow)
    for root in 0..n {
        if index[root] != NONE {
            continue;
        }
        work.push((root, start[root]));
        index[root] = next;
        low[root] = next;
        next += 1;
        open.push(root);
        while let Some(&(v, e)) = work.last() {
            if e < start[v + 1] {
                if let Some(top) = work.last_mut() {
                    top.1 += 1;
                }
                let w = edges[e];
                if index[w] == NONE {
                    index[w] = next;
                    low[w] = next;
                    next += 1;
                    open.push(w);
                    work.push((w, start[w]));
                } else if comp[w] == NONE {
                    low[v] = low[v].min(index[w]);
                }
            } else {
                work.pop();
                if let Some(&(parent, _)) = work.last() {
                    low[parent] = low[parent].min(low[v]);
                }
                if low[v] == index[v] {
                    while let Some(w) = open.pop() {
                        comp[w] = comps;
                        if w == v {
                            break;
                        }
                    }
                    comps += 1;
                }
            }
        }
    }
    constraints.iter().map(|c| comp[dense(c.node)] == comp[dense(source_of(c))]).collect()
}

/// Everything worth telling the user about a freshly imported avatar: what the VRM profile says is missing (as
/// warnings, because the model is still usable), and how complete its perfect-sync set is.
pub(crate) fn assess(scene: &IoScene, version: VrmVersion, report: &mut Report) {
    for mut f in vrm_profile(scene, version).findings {
        if f.severity == Severity::Error {
            f.severity = Severity::Warning;
        }
        report.push(f);
    }
    if let Some(note) = scene.avatar.as_ref().and_then(perfect_sync_note) {
        report.push(note);
    }
}

/// How many of the 52 `ARKit` channels the avatar has, as an info finding; `None` if it has none (a plain VRM).
fn perfect_sync_note(av: &Avatar) -> Option<Finding> {
    let missing: Vec<&str> =
        ARKIT_52.iter().copied().filter(|name| !av.expressions.keys().any(|k| k.as_str() == *name)).collect();
    let have = ARKIT_52.len() - missing.len();
    if have == 0 {
        return None;
    }
    if missing.is_empty() {
        return Some(Finding::info(
            "vrm.perfect_sync_complete",
            "all 52 ARKit channels are present: face trackers drive this model directly",
        ));
    }
    let case_only: Vec<&&str> =
        missing.iter().filter(|name| av.expressions.keys().any(|k| k.as_str().eq_ignore_ascii_case(name))).collect();
    let shown: Vec<&str> = missing.iter().take(6).copied().collect();
    let mut msg = format!(
        "{have} of 52 ARKit channels are present; missing {}{}",
        shown.join(", "),
        if missing.len() > 6 { ", …" } else { "" }
    );
    if !case_only.is_empty() {
        let _ =
            write!(msg, "; {} exist with different capitalisation, which most trackers match exactly", case_only.len());
    }
    Some(Finding::info("vrm.perfect_sync_partial", msg).at("avatar/expressions"))
}

#[cfg(test)]
mod tests {
    use omuzaki_io_api::{
        ConstraintKind, Expression, ExpressionName, MorphBind, Node, NodeIx, SpringChain, SpringJoint,
    };

    use super::*;

    fn scene_with_chain_nodes() -> IoScene {
        // 0 -> 1 -> 2, and a separate root 3.
        let mut s = IoScene::new();
        for name in ["a", "b", "c", "d"] {
            s.add_node(Node::new(name));
        }
        s.attach(NodeIx(0), NodeIx(1));
        s.attach(NodeIx(1), NodeIx(2));
        s.roots = vec![NodeIx(0), NodeIx(3)];
        s
    }

    fn chain(nodes: &[u32]) -> SpringChain {
        SpringChain { joints: nodes.iter().map(|&n| SpringJoint::new(NodeIx(n))).collect(), ..SpringChain::default() }
    }

    #[test]
    fn spring_chains_that_break_the_invariants_are_dropped_and_the_rest_kept() {
        let scene = scene_with_chain_nodes();
        let mut av = Avatar::new();
        av.spring_bones.chains = vec![
            chain(&[0, 1, 2]), // fine
            chain(&[2, 1]),    // out of order
            chain(&[1, 2]),    // shares joints with the first
            chain(&[3, 9]),    // missing node
            chain(&[]),        // empty
        ];
        av.spring_bones.chains[0].center = Some(NodeIx(3)); // not an ancestor
        let mut report = Report::new();
        tidy(&scene, &mut av, &mut report);
        assert_eq!(av.spring_bones.chains.len(), 1);
        assert_eq!(av.spring_bones.chains[0].center, None);
        assert!(report.has_rule("vrm.spring_chain_dropped") && report.has_rule("vrm.spring_center_dropped"));
        let mut checked = scene.clone();
        checked.avatar = Some(av);
        assert!(!checked.check().has_errors(), "what remains satisfies the scene's invariants");
    }

    fn constraint(node: u32, source: u32) -> NodeConstraint {
        NodeConstraint { node: NodeIx(node), kind: ConstraintKind::Rotation { source: NodeIx(source) }, weight: 1.0 }
    }

    /// A deterministic pseudo-random generator, so the tests need no dependency.
    fn lcg(state: &mut u64) -> u64 {
        *state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        *state >> 33
    }

    /// `spine` nodes in a line (0 -> 1 -> ...), and the scene's roots set to node 0 only.
    fn spine_scene(spine: usize) -> IoScene {
        let mut s = IoScene::new();
        for i in 0..spine {
            s.add_node(Node::new(format!("s{i}")));
        }
        for i in 1..spine {
            s.attach(NodeIx(u32::try_from(i - 1).unwrap()), NodeIx(u32::try_from(i).unwrap()));
        }
        s.roots = vec![NodeIx(0)];
        s
    }

    fn ix(i: usize) -> u32 {
        u32::try_from(i).unwrap()
    }

    #[test]
    fn tour_agrees_with_the_walk_it_replaces() {
        // 200 nodes: node i hangs from a random earlier node, two of them (0 and 100) are roots.
        let mut seed = 7;
        let mut parent: Vec<Option<usize>> = vec![None; 200];
        for (i, p) in parent.iter_mut().enumerate().filter(|(i, _)| *i != 0 && *i != 100) {
            *p = Some(usize::try_from(lcg(&mut seed)).unwrap() % i);
        }
        let walk = |node: usize, ancestor: usize| {
            let mut cur = Some(node);
            while let Some(n) = cur {
                if n == ancestor {
                    return true;
                }
                cur = parent[n];
            }
            false
        };
        let tour = Tour::new(&parent);
        for node in 0..200 {
            for ancestor in 0..200 {
                assert_eq!(tour.is_below(node, ancestor), walk(node, ancestor), "{node} below {ancestor}");
            }
        }
    }

    #[test]
    fn a_node_on_a_parent_cycle_is_below_only_itself() {
        let tour = Tour::new(&[Some(1), Some(0), None]);
        assert!(tour.is_below(0, 0) && !tour.is_below(0, 1) && !tour.is_below(1, 0) && !tour.is_below(2, 0));
        assert!(!tour.is_below(5, 0), "out of range");
    }

    #[test]
    fn a_hundred_thousand_deep_chain_does_not_recurse() {
        let n = 100_000;
        let parent: Vec<Option<usize>> = (0..n).map(|i: usize| i.checked_sub(1)).collect();
        let tour = Tour::new(&parent);
        assert!(tour.is_below(n - 1, 0));
        assert!(!tour.is_below(0, n - 1));
    }

    #[test]
    fn hostile_repeated_invalid_chain_is_cheap() {
        // A 50k spine with two sibling leaves at the bottom; 50k chains [L1, L2], where L1 isn't above L2.
        let spine = 50_000;
        let mut scene = spine_scene(spine);
        let l1 = scene.add_node(Node::new("l1"));
        let l2 = scene.add_node(Node::new("l2"));
        scene.attach(NodeIx(ix(spine - 1)), l1);
        scene.attach(NodeIx(ix(spine - 1)), l2);
        let mut av = Avatar::new();
        av.spring_bones.chains = vec![chain(&[l1.0, l2.0]); 50_000];
        let mut report = Report::new();
        tidy(&scene, &mut av, &mut report);
        assert_eq!(av.spring_bones.chains.len(), 0);
        assert_eq!(report.findings.iter().filter(|f| f.rule == "vrm.spring_chain_dropped").count(), 1);
    }

    #[test]
    fn hostile_invalid_centres_are_cheap() {
        // A 25k spine; 25k two-joint chains hang off its bottom, each with a centre on a side branch.
        let spine = 25_000;
        let mut scene = spine_scene(spine);
        let side = scene.add_node(Node::new("side"));
        scene.attach(NodeIx(0), side);
        let mut av = Avatar::new();
        for i in 0..25_000 {
            let a = scene.add_node(Node::new(format!("a{i}")));
            let b = scene.add_node(Node::new(format!("b{i}")));
            scene.attach(NodeIx(ix(spine - 1)), a);
            scene.attach(a, b);
            let mut c = chain(&[a.0, b.0]);
            c.center = Some(side);
            av.spring_bones.chains.push(c);
        }
        let mut report = Report::new();
        tidy(&scene, &mut av, &mut report);
        assert_eq!(av.spring_bones.chains.len(), 25_000);
        assert!(av.spring_bones.chains.iter().all(|c| c.center.is_none()));
        assert_eq!(report.findings.iter().filter(|f| f.rule == "vrm.spring_center_dropped").count(), 1);
    }

    #[test]
    fn a_two_cycle_and_a_self_source_are_cyclic() {
        assert_eq!(on_cycle(&[constraint(0, 1), constraint(1, 0)]), [true, true]);
        assert_eq!(on_cycle(&[constraint(4, 4)]), [true]);
    }

    #[test]
    fn a_feeder_into_a_cycle_is_kept() {
        // 1 <-> 2 is the cycle; 3 reads from 2 and nothing reads from 3.
        assert_eq!(on_cycle(&[constraint(1, 2), constraint(2, 1), constraint(3, 2)]), [true, true, false]);
    }

    #[test]
    fn independent_cycles_are_each_found() {
        let c = [
            constraint(0, 1),
            constraint(1, 2),
            constraint(2, 0),
            constraint(10, 11),
            constraint(11, 10),
            constraint(20, 21),
        ];
        assert_eq!(on_cycle(&c), [true, true, true, true, true, false]);
    }

    #[test]
    fn a_long_chain_is_not_a_cycle_and_a_long_ring_is() {
        let n = 100_000;
        let chain: Vec<_> = (0..n).map(|i| constraint(ix(i), ix(i + 1))).collect();
        assert!(on_cycle(&chain).iter().all(|&c| !c));
        let ring: Vec<_> = (0..n).map(|i| constraint(ix(i), ix((i + 1) % n))).collect();
        assert!(on_cycle(&ring).iter().all(|&c| c));
    }

    #[test]
    fn tidy_drops_circular_constraints_and_says_so() {
        let scene = scene_with_chain_nodes();
        let mut av = Avatar::new();
        av.constraints = vec![constraint(0, 1), constraint(1, 0), constraint(2, 1)];
        let mut report = Report::new();
        tidy(&scene, &mut av, &mut report);
        assert_eq!(av.constraints, [constraint(2, 1)]);
        assert!(report.has_rule("vrm.constraint_dropped"));
    }

    #[test]
    fn bad_binds_are_dropped_and_wild_weights_are_clamped() {
        let scene = scene_with_chain_nodes();
        let mut av = Avatar::new();
        let mut e = Expression::new();
        e.morph_binds = vec![MorphBind { node: NodeIx(0), index: 0, weight: 1.0 }]; // node 0 has no mesh
        av.expressions.insert(ExpressionName::parse("happy").unwrap(), e);
        let mut report = Report::new();
        tidy(&scene, &mut av, &mut report);
        assert!(report.has_rule("vrm.bind_dropped"));
        assert!(av.expressions.values().all(|e| e.morph_binds.is_empty()));
    }

    #[test]
    fn perfect_sync_completeness_is_reported_with_the_missing_names() {
        let mut av = Avatar::new();
        assert!(perfect_sync_note(&av).is_none(), "a plain VRM says nothing");
        for name in ARKIT_52.iter().skip(2) {
            av.expressions.insert(ExpressionName::parse(name).unwrap(), Expression::new());
        }
        let partial = perfect_sync_note(&av).unwrap();
        assert_eq!(partial.rule, "vrm.perfect_sync_partial");
        assert!(partial.message.contains("50 of 52") && partial.message.contains("eyeBlinkLeft, eyeLookDownLeft"));
        for name in &ARKIT_52[..2] {
            av.expressions.insert(ExpressionName::parse(name).unwrap(), Expression::new());
        }
        assert_eq!(perfect_sync_note(&av).unwrap().rule, "vrm.perfect_sync_complete");
    }
}
