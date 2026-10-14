//! `reduce_bones`: a synthetic rig with 15 humanoid bones, 45 helpers, a hair spring chain of 12 joints and a
//! skirt chain of 8, one skinned mesh. Nothing here is a character asset.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::collections::BTreeSet;

use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_io_api::{
    Avatar, Collider, ColliderShape, ConstraintKind, FirstPerson, FirstPersonFlag, IoScene, Mesh, MeshAnnotation,
    MeshData, Node, NodeConstraint, NodeIx, Primitive, Skin, SpringChain, SpringJoint,
};
use omuzaki_io_ops::fit_bones::{BoneBudget, reduce_bones};

/// The 15 bones VRM 1.0 requires, with the index of their parent in this list (`None` for the root hips).
const HUMANOID: [(&str, Option<usize>); 15] = [
    ("hips", None),
    ("spine", Some(0)),
    ("head", Some(1)),
    ("leftUpperLeg", Some(0)),
    ("leftLowerLeg", Some(3)),
    ("leftFoot", Some(4)),
    ("rightUpperLeg", Some(0)),
    ("rightLowerLeg", Some(6)),
    ("rightFoot", Some(7)),
    ("leftUpperArm", Some(1)),
    ("leftLowerArm", Some(9)),
    ("leftHand", Some(10)),
    ("rightUpperArm", Some(1)),
    ("rightLowerArm", Some(12)),
    ("rightHand", Some(13)),
];

struct Rig {
    scene: IoScene,
    humanoid: Vec<NodeIx>,
    hair: Vec<NodeIx>,
    skirt: Vec<NodeIx>,
    helpers: Vec<NodeIx>,
    /// The helpers no vertex uses.
    unweighted: BTreeSet<NodeIx>,
}

/// A small deterministic generator, so the rig is the same on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn unit(&mut self) -> f32 {
        self.next() as f32 / (1u32 << 31) as f32
    }
}

fn add_child(scene: &mut IoScene, parent: Option<NodeIx>, name: String, k: usize) -> NodeIx {
    // Offsets and a twist that vary per node, so world matrices are not trivially translations.
    let mut node = Node::new(name).at(Vec3::new(0.01 * (k % 5) as f32, 0.1 + 0.01 * (k % 7) as f32, 0.02));
    node.transform.rotation = Quat::from_rotation_z(0.05 * ((k % 9) as f32 - 4.0));
    let ix = scene.add_node(node);
    match parent {
        Some(p) => scene.attach(p, ix),
        None => scene.roots.push(ix),
    }
    ix
}

fn world_matrices(scene: &IoScene) -> Vec<Mat4> {
    let mut parent = vec![None; scene.nodes.len()];
    for (i, n) in scene.nodes.iter().enumerate() {
        for c in &n.children {
            parent[c.index()] = Some(i);
        }
    }
    let mut world = vec![Mat4::IDENTITY; scene.nodes.len()];
    // Parents are always added before their children, so one forward pass is enough.
    for (i, n) in scene.nodes.iter().enumerate() {
        let t = n.transform;
        let local = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        world[i] = parent[i].map_or(local, |p| world[p] * local);
    }
    world
}

fn rig() -> Rig {
    let mut scene = IoScene::new();
    let mut k = 0;
    let mut node = |scene: &mut IoScene, parent: Option<NodeIx>, name: String| {
        k += 1;
        add_child(scene, parent, name, k)
    };
    let armature = node(&mut scene, None, "armature".into());
    let mut humanoid: Vec<NodeIx> = Vec::new();
    let mut avatar = Avatar::new();
    for (name, parent) in HUMANOID {
        let p = parent.map_or(armature, |i| humanoid[i]);
        let ix = node(&mut scene, Some(p), name.into());
        avatar.humanoid.assign(name, ix).unwrap();
        humanoid.push(ix);
    }
    let head = humanoid[2];
    let mut chain_nodes = |scene: &mut IoScene, root: NodeIx, prefix: &str, n: usize| -> Vec<NodeIx> {
        let mut out: Vec<NodeIx> = Vec::new();
        for i in 0..n {
            let p = out.last().copied().unwrap_or(root);
            out.push(node(scene, Some(p), format!("{prefix}{i}")));
        }
        out
    };
    let hair = chain_nodes(&mut scene, head, "hair", 12);
    let skirt = chain_nodes(&mut scene, humanoid[0], "skirt", 8);
    // Helpers hang off the humanoid bones in runs of three; every ninth run of four is unweighted.
    let mut helpers: Vec<NodeIx> = Vec::new();
    for h in 0..45 {
        let p = if h % 3 == 0 { humanoid[(h / 3) % 15] } else { helpers[h - 1] };
        helpers.push(node(&mut scene, Some(p), format!("helper{h}")));
    }
    let unweighted: BTreeSet<NodeIx> =
        helpers.iter().enumerate().filter(|(h, _)| (3..7).contains(&(h % 9))).map(|(_, n)| *n).collect();

    let springs = &mut avatar.spring_bones;
    for (name, nodes, center) in [("hair", &hair, Some(humanoid[0])), ("skirt", &skirt, None)] {
        springs.chains.push(SpringChain {
            name: name.into(),
            joints: nodes.iter().map(|&n| SpringJoint::new(n)).collect(),
            center,
            ..SpringChain::default()
        });
    }
    scene.avatar = Some(avatar);

    // Skin joints: the armature root, the humanoid bones, the hair chain, the first two skirt joints, every helper.
    // The other six skirt joints are spring-only nodes: protected, but not bones.
    let joints: Vec<NodeIx> = [armature]
        .into_iter()
        .chain(humanoid.iter().copied())
        .chain(hair.iter().copied())
        .chain(skirt[..2].iter().copied())
        .chain(helpers.iter().copied())
        .collect();
    let world = world_matrices(&scene);
    let inverse_bind = joints.iter().map(|j| world[j.index()].inverse()).collect();
    let mut skin = Skin::new("body", joints.clone(), inverse_bind);
    skin.skeleton = Some(armature);
    let skin_ix = scene.add_skin(skin);

    let pool: Vec<u16> =
        joints.iter().enumerate().filter(|(_, j)| !unweighted.contains(j)).map(|(i, _)| i as u16).collect();
    let mut rng = Lcg(7);
    let mut mesh = MeshData::default();
    for v in 0..600 {
        mesh.positions.push(Vec3::new(rng.unit() - 0.5, rng.unit() * 1.8, rng.unit() - 0.5));
        mesh.normals.push(Vec3::Z);
        // The first influence cycles through the pool so every weighted joint has weight; the rest are random.
        let mut js = [pool[v % pool.len()], 0, 0, 0];
        for slot in 1..4 {
            loop {
                let c = pool[rng.next() as usize % pool.len()];
                if !js[..slot].contains(&c) {
                    js[slot] = c;
                    break;
                }
            }
        }
        let raw = [0.2 + rng.unit(), rng.unit(), rng.unit() * 0.5, rng.unit() * 0.1];
        let sum: f32 = raw.iter().sum();
        mesh.joints.push(js);
        mesh.weights.push(raw.map(|w| w / sum));
    }
    mesh.indices = (0..597).collect();
    let mesh_ix = scene.add_mesh({
        let mut m = Mesh::new("body");
        m.primitives.push(Primitive::new(mesh));
        m
    });
    let mut body = Node::new("body");
    body.mesh = Some(mesh_ix);
    body.skin = Some(skin_ix);
    let body_ix = scene.add_node(body);
    scene.roots.push(body_ix);
    Rig { scene, humanoid, hair, skirt, helpers, unweighted }
}

fn joint_set(scene: &IoScene) -> BTreeSet<NodeIx> {
    scene.skins.iter().flat_map(|s| s.joints.iter().copied()).collect()
}

/// Every vertex's position under the bind pose: `sum(w * world(joint) * inverse_bind(joint)) * p`.
fn skinned_positions(scene: &IoScene) -> Vec<Vec3> {
    let world = world_matrices(scene);
    let skin = &scene.skins[0];
    let m = &scene.meshes[0].primitives[0].mesh;
    m.positions
        .iter()
        .zip(m.joints.iter().zip(&m.weights))
        .map(|(p, (js, ws))| {
            let blended: Vec3 = js
                .iter()
                .zip(ws)
                .map(|(&j, &w)| {
                    w * (world[skin.joints[usize::from(j)].index()] * skin.inverse_bind[usize::from(j)])
                        .transform_point3(*p)
                })
                .sum();
            blended
        })
        .collect()
}

fn protected_joints(rig: &Rig) -> Vec<NodeIx> {
    let mut out = vec![NodeIx(0)];
    out.extend(&rig.humanoid);
    out.extend(&rig.hair);
    out.extend(&rig.skirt[..2]);
    out
}

#[test]
fn the_fixture_is_valid_and_counts_what_the_issue_says() {
    let r = rig();
    assert!(!r.scene.check().has_errors(), "{:?}", r.scene.check());
    assert_eq!((r.humanoid.len(), r.helpers.len(), r.hair.len(), r.skirt.len()), (15, 45, 12, 8));
    assert_eq!(r.unweighted.len(), 20);
    assert_eq!(joint_set(&r.scene).len(), 75);
}

#[test]
fn a_budget_of_30_keeps_every_protected_joint_and_the_rest_pose() {
    let mut r = rig();
    let before = skinned_positions(&r.scene);
    let report = reduce_bones(&mut r.scene, &BoneBudget { max_bones: 30 });

    assert!(report.reached);
    assert_eq!((report.before, report.after, report.protected), (75, 30, 30));
    assert_eq!(report.merged.len(), 45, "all 45 helpers go; protected joints fill the budget exactly");
    assert!(!r.scene.check().has_errors(), "{:?}", r.scene.check());

    let kept = joint_set(&r.scene);
    assert_eq!(kept.len(), 30);
    for node in protected_joints(&r) {
        assert!(kept.contains(&node), "{node:?} was protected");
    }
    let parent_of = |n: NodeIx| r.scene.parent_of(n);
    for &(from, into) in &report.merged {
        let mut cur = from;
        let mut found = false;
        while let Some(p) = parent_of(cur) {
            if p == into {
                found = true;
                break;
            }
            cur = p;
        }
        assert!(found, "{into:?} is an ancestor of {from:?}");
        assert!(kept.contains(&into) && !kept.contains(&from));
    }

    let after = skinned_positions(&r.scene);
    for (a, b) in before.iter().zip(&after) {
        assert!((*a - *b).length() < 1e-5, "{a:?} moved to {b:?}");
    }
    let skin = &r.scene.skins[0];
    assert_eq!(skin.joints.len(), skin.inverse_bind.len());
    let m = &r.scene.meshes[0].primitives[0].mesh;
    for (js, ws) in m.joints.iter().zip(&m.weights) {
        assert!((ws.iter().sum::<f32>() - 1.0).abs() < 1e-5, "{ws:?}");
        assert!(js.iter().zip(ws).all(|(&j, &w)| w == 0.0 || usize::from(j) < skin.joints.len()));
        let live: Vec<u16> = js.iter().zip(ws).filter(|&(_, &w)| w > 0.0).map(|(&j, _)| j).collect();
        let distinct: BTreeSet<_> = live.iter().collect();
        assert!(live.len() <= 4 && distinct.len() == live.len(), "{js:?} {ws:?}");
    }
}

#[test]
fn unweighted_joints_are_dropped_before_any_weighted_one_is_merged() {
    let mut r = rig();
    // 75 joints, 30 protected: a budget of 45 drops 30 helpers, and 20 of them carry no weight.
    let report = reduce_bones(&mut r.scene, &BoneBudget { max_bones: 45 });
    assert!(report.reached && report.after == 45);
    let (first, rest) = report.merged.split_at(20);
    let dropped: BTreeSet<NodeIx> = first.iter().map(|m| m.0).collect();
    assert_eq!(dropped, r.unweighted, "the 20 unweighted helpers go first");
    assert_eq!(rest.len(), 10);
    assert!(rest.iter().all(|(from, _)| !r.unweighted.contains(from)));
    assert!(!r.scene.check().has_errors());
}

#[test]
fn a_budget_below_the_protected_count_changes_nothing() {
    let mut r = rig();
    let original = r.scene.clone();
    let report = reduce_bones(&mut r.scene, &BoneBudget { max_bones: 10 });
    assert!(!report.reached);
    assert_eq!((report.before, report.after, report.protected), (75, 75, 30));
    assert_eq!(report.merged, []);
    assert_eq!(r.scene, original);
}

#[test]
fn a_scene_already_within_budget_is_left_alone() {
    let mut r = rig();
    let original = r.scene.clone();
    let report = reduce_bones(&mut r.scene, &BoneBudget { max_bones: 75 });
    assert!(report.reached && report.merged.is_empty() && report.after == 75);
    assert_eq!(r.scene, original);
}

#[test]
fn two_runs_are_identical() {
    let (mut a, mut b) = (rig(), rig());
    let ra = reduce_bones(&mut a.scene, &BoneBudget { max_bones: 36 });
    let rb = reduce_bones(&mut b.scene, &BoneBudget { max_bones: 36 });
    assert_eq!(ra, rb);
    assert_eq!(a.scene, b.scene);
}

#[test]
fn colliders_constraints_and_first_person_nodes_are_protected_too() {
    let mut r = rig();
    let [collider, constrained, source, annotated] = [r.helpers[0], r.helpers[1], r.helpers[2], r.helpers[10]];
    let avatar = r.scene.avatar.as_mut().unwrap();
    avatar
        .spring_bones
        .colliders
        .push(Collider { node: collider, shape: ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.1 } });
    avatar.constraints.push(NodeConstraint {
        node: constrained,
        kind: ConstraintKind::Rotation { source },
        weight: 1.0,
    });
    avatar.first_person =
        Some(FirstPerson { annotations: vec![MeshAnnotation { node: annotated, flag: FirstPersonFlag::Both }] });

    let report = reduce_bones(&mut r.scene, &BoneBudget { max_bones: 34 });
    assert_eq!((report.protected, report.after, report.reached), (34, 34, true));
    let kept = joint_set(&r.scene);
    for node in [collider, constrained, source, annotated] {
        assert!(kept.contains(&node));
    }
    assert!(!r.scene.check().has_errors(), "{:?}", r.scene.check());
}

#[test]
fn a_protected_node_outside_every_skin_is_not_counted_and_is_left_alone() {
    let mut r = rig();
    let original = r.scene.clone();
    let spring_only = &r.skirt[2..];
    assert!(spring_only.iter().all(|n| !joint_set(&r.scene).contains(n)), "the fixture keeps them out of the skin");

    let report = reduce_bones(&mut r.scene, &BoneBudget { max_bones: 30 });
    // 30 protected skin joints fill the budget exactly; the 6 spring-only skirt nodes add nothing to either count.
    assert_eq!((report.before, report.after, report.protected, report.reached), (75, 30, 30, true));
    assert!(report.merged.iter().all(|(from, into)| !spring_only.contains(from) && !spring_only.contains(into)));
    assert!(spring_only.iter().all(|n| !joint_set(&r.scene).contains(n)), "none was pulled into a skin");
    // Still present, unmerged and wired up exactly as before: nodes, hierarchy and the spring chain are untouched.
    assert_eq!(r.scene.nodes, original.nodes);
    assert_eq!(r.scene.avatar, original.avatar);
}

#[test]
fn an_unweighted_slot_past_the_skin_is_refused_without_a_panic() {
    let mut r = rig();
    // Slot 3 of vertex 0 is far past the 75 joints but carries no weight, which a weight-only check would let through.
    let m = &mut r.scene.meshes[0].primitives[0].mesh;
    m.joints[0][3] = 9_999;
    m.weights[0][3] = 0.0;
    let original = r.scene.clone();
    let report = reduce_bones(&mut r.scene, &BoneBudget { max_bones: 30 });
    assert!(!report.reached);
    assert_eq!((report.before, report.after), (75, 75));
    assert_eq!(r.scene, original);
}
