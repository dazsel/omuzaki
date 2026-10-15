//! `merge_meshes`: synthetic avatars built here (a humanoid, a body with 52 bound morph targets, a hair and an outfit
//! mesh skinned to it, and a few props). Nothing here is a character asset.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::collections::BTreeMap;

use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_io_api::{
    Avatar, Expression, ExpressionName, FirstPerson, FirstPersonFlag, IoError, IoScene, Material, Mesh, MeshAnnotation,
    MeshData, MorphTarget, Node, NodeIx, Primitive, Shading, Skin, SpringChain, SpringJoint, VRM_LICENSE_URLS, VrmMeta,
};
use omuzaki_io_ops::fit_meshes::{ApartReason, merge_meshes};
use omuzaki_store_api::{Commercial, Provenance, Source};

/// A small deterministic generator, so every fixture is the same on every run.
struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as f32 / (1u32 << 31) as f32
    }
}

/// The humanoid of the fixture: bone, parent index in this list, local position.
const BONES: [(&str, Option<usize>, [f32; 3]); 21] = [
    ("hips", None, [0.0, 0.9, 0.0]),
    ("spine", Some(0), [0.0, 0.1, 0.0]),
    ("chest", Some(1), [0.0, 0.15, 0.0]),
    ("neck", Some(2), [0.0, 0.25, 0.0]),
    ("head", Some(3), [0.0, 0.1, 0.0]),
    ("leftUpperArm", Some(2), [0.12, 0.2, 0.0]),
    ("leftLowerArm", Some(5), [0.25, 0.0, 0.0]),
    ("leftHand", Some(6), [0.25, 0.0, 0.0]),
    ("rightUpperArm", Some(2), [-0.12, 0.2, 0.0]),
    ("rightLowerArm", Some(8), [-0.25, 0.0, 0.0]),
    ("rightHand", Some(9), [-0.25, 0.0, 0.0]),
    ("leftUpperLeg", Some(0), [0.09, -0.05, 0.0]),
    ("leftLowerLeg", Some(11), [0.0, -0.4, 0.0]),
    ("leftFoot", Some(12), [0.0, -0.4, 0.0]),
    ("leftToes", Some(13), [0.0, -0.05, 0.12]),
    ("rightUpperLeg", Some(0), [-0.09, -0.05, 0.0]),
    ("rightLowerLeg", Some(15), [0.0, -0.4, 0.0]),
    ("rightFoot", Some(16), [0.0, -0.4, 0.0]),
    ("rightToes", Some(17), [0.0, -0.05, 0.12]),
    ("hairA", Some(4), [0.0, 0.12, -0.05]),
    ("skirt", Some(0), [0.0, -0.1, 0.0]),
];

struct Rig {
    scene: IoScene,
    nodes: BTreeMap<&'static str, NodeIx>,
    root: NodeIx,
    body: NodeIx,
    hair: NodeIx,
    outfit: NodeIx,
}

fn world_matrices(scene: &IoScene) -> Vec<Mat4> {
    let mut parent = vec![None; scene.nodes.len()];
    for (i, n) in scene.nodes.iter().enumerate() {
        for c in &n.children {
            parent[c.index()] = Some(i);
        }
    }
    let mut world = vec![Mat4::IDENTITY; scene.nodes.len()];
    // Parents come before their children in every fixture, so one forward pass is enough.
    for (i, n) in scene.nodes.iter().enumerate() {
        let t = n.transform;
        let local = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        world[i] = parent[i].map_or(local, |p| world[p] * local);
    }
    world
}

/// A strip of `quads` quads with two-bone skinning over `joints` skin slots, deterministic in `seed`.
fn strip(seed: u64, quads: u32, joints: u16) -> MeshData {
    let mut rng = Lcg(seed);
    let mut m = MeshData::default();
    for k in 0..=quads {
        for row in 0..2u32 {
            m.positions.push(Vec3::new(rng.unit() * 0.2 - 0.1, 0.1 * k as f32, 0.02 * row as f32 + rng.unit() * 0.01));
            m.normals.push(Vec3::Z);
            let j = u16::try_from(k + row).unwrap() % joints;
            let w = 0.25 + 0.5 * rng.unit();
            m.joints.push([j, (j + 1) % joints, 0, 0]);
            m.weights.push([w, 1.0 - w, 0.0, 0.0]);
        }
    }
    for k in 0..quads {
        let a = 2 * k;
        m.indices.extend([a, a + 1, a + 2, a + 1, a + 3, a + 2]);
    }
    m
}

/// A quad with normals and no skinning.
fn quad(seed: u64) -> MeshData {
    let mut rng = Lcg(seed);
    let mut m = MeshData::default();
    for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
        m.positions.push(Vec3::new(x * 0.1, y * 0.1, rng.unit() * 0.01));
        m.normals.push(Vec3::Z);
    }
    m.indices = vec![0, 1, 2, 1, 3, 2];
    m
}

fn unlit(scene: &mut IoScene, name: &str) -> omuzaki_io_api::MaterialIx {
    scene.add_material(Material::new(name, Shading::Unlit))
}

/// A skinned mesh node over the named joints; inverse binds undo the rest pose, so the bind pose is the rest pose.
fn add_skinned(
    scene: &mut IoScene,
    nodes: &BTreeMap<&'static str, NodeIx>,
    root: NodeIx,
    name: &str,
    joint_names: &[&str],
    seed: u64,
) -> NodeIx {
    let world = world_matrices(scene);
    let joints: Vec<NodeIx> = joint_names.iter().map(|j| nodes[j]).collect();
    let ibm = joints.iter().map(|j| world[j.index()].inverse()).collect();
    let skin = scene.add_skin(Skin::new(name, joints, ibm));
    let mat = unlit(scene, name);
    let mut prim = Primitive::new(strip(seed, 4, u16::try_from(joint_names.len()).unwrap()));
    prim.material = Some(mat);
    let mut mesh = Mesh::new(name);
    mesh.primitives = vec![prim];
    let mesh = scene.add_mesh(mesh);
    let mut node = Node::new(name);
    node.mesh = Some(mesh);
    node.skin = Some(skin);
    let ix = scene.add_node(node);
    scene.attach(root, ix);
    ix
}

/// A humanoid, a Body with 52 morph targets that 52 expressions bind, a Hair and an Outfit skinned to the same bones.
fn new_rig() -> Rig {
    let mut scene = IoScene::new();
    let root = scene.add_node(Node::new("Armature"));
    scene.roots = vec![root];
    let mut nodes: BTreeMap<&'static str, NodeIx> = BTreeMap::new();
    let mut order: Vec<NodeIx> = Vec::new();
    let mut avatar = Avatar::new();
    for (name, parent, t) in BONES {
        let mut node = Node::new(name).at(Vec3::from_array(t));
        node.transform.rotation = Quat::from_rotation_z(0.03 * order.len() as f32);
        let ix = scene.add_node(node);
        scene.attach(parent.map_or(root, |p| order[p]), ix);
        order.push(ix);
        nodes.insert(name, ix);
        if !matches!(name, "hairA" | "skirt") {
            avatar.humanoid.assign(name, ix).unwrap();
        }
    }
    let body = add_skinned(&mut scene, &nodes, root, "Body", &["hips", "spine", "chest", "neck", "head"], 1);
    let hair = add_skinned(&mut scene, &nodes, root, "Hair", &["head", "neck", "hairA"], 2);
    let outfit = add_skinned(&mut scene, &nodes, root, "Outfit", &["hips", "spine", "chest", "skirt", "neck"], 3);

    let body_mesh = scene.nodes[body.index()].mesh.unwrap();
    let vertices = scene.meshes[body_mesh.index()].primitives[0].mesh.positions.len();
    scene.meshes[body_mesh.index()].primitives[0].morph_targets =
        (0..52).map(|k| MorphTarget::new(vec![Vec3::new(0.001 * k as f32, 0.0, 0.0); vertices])).collect();
    scene.meshes[body_mesh.index()].morph_target_names = (0..52).map(|k| format!("shape{k}")).collect();
    for k in 0..52u32 {
        avatar.expressions.insert(ExpressionName::parse(&format!("shape{k}")).unwrap(), Expression::morph(body, k));
    }
    scene.avatar = Some(avatar);
    scene.meta = VrmMeta::new("Fixture", vec!["Fixture Author".into()], VRM_LICENSE_URLS[0]);
    scene.provenance = Some(Provenance {
        source: Source::Authored,
        author: Some("Fixture Author".into()),
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    });
    Rig { scene, nodes, root, body, hair, outfit }
}

fn annotate(scene: &mut IoScene, flags: &[(NodeIx, FirstPersonFlag)]) {
    let annotations = flags.iter().map(|&(node, flag)| MeshAnnotation { node, flag }).collect();
    scene.avatar.as_mut().unwrap().first_person = Some(FirstPerson { annotations });
}

fn mesh_of(scene: &IoScene, node: NodeIx) -> &Mesh {
    &scene.meshes[scene.nodes[node.index()].mesh.unwrap().index()]
}

fn totals(scene: &IoScene) -> (usize, usize) {
    let prims = || scene.meshes.iter().flat_map(|m| &m.primitives);
    (prims().map(|p| p.mesh.positions.len()).sum(), prims().map(|p| p.mesh.triangle_count()).sum())
}

fn skinned_nodes(scene: &IoScene) -> usize {
    scene.nodes.iter().filter(|n| n.mesh.is_some() && n.skin.is_some()).count()
}

/// Rotates a few nodes, a different set per `k`, so the joints move relative to each other.
fn pose(scene: &mut IoScene, k: usize) {
    for (i, node) in scene.nodes.iter_mut().enumerate().skip(1) {
        if (i + k).is_multiple_of(3) {
            node.transform.rotation *= Quat::from_rotation_x(0.2 * k as f32) * Quat::from_rotation_y(0.1);
        }
    }
}

/// Every vertex of the meshes of `nodes`, skinned by the pose the scene is in, in node and primitive order.
fn skinned_world(scene: &IoScene, nodes: &[NodeIx]) -> Vec<Vec3> {
    let world = world_matrices(scene);
    let mut out = Vec::new();
    for &node in nodes {
        let n = &scene.nodes[node.index()];
        let skin = &scene.skins[n.skin.unwrap().index()];
        for prim in &scene.meshes[n.mesh.unwrap().index()].primitives {
            for ((p, js), ws) in prim.mesh.positions.iter().zip(&prim.mesh.joints).zip(&prim.mesh.weights) {
                let mut acc = Vec3::ZERO;
                for (&j, &w) in js.iter().zip(ws) {
                    let k = usize::from(j);
                    acc += w * (world[skin.joints[k].index()] * skin.inverse_bind[k]).transform_point3(*p);
                }
                out.push(acc);
            }
        }
    }
    out
}

fn assert_clean(scene: &IoScene) {
    let report = scene.check();
    assert!(!report.has_errors(), "{:?}", report.findings);
    assert!(
        !report.findings.iter().any(|f| f.rule == "scene.skin_without_mesh"),
        "emptied nodes must drop their skin too"
    );
}

#[test]
fn body_stays_and_hair_joins_outfit() {
    let mut rig = new_rig();
    assert_clean(&rig.scene);
    let before = rig.scene.clone();
    let body_before = mesh_of(&before, rig.body).clone();
    let report = merge_meshes(&mut rig.scene).unwrap();

    assert_eq!(report.skinned, (3, 2));
    assert_eq!(report.merged, [rig.outfit]);
    assert_eq!(report.apart, Vec::new());
    assert_eq!(skinned_nodes(&rig.scene), 2);
    assert_eq!(rig.scene.nodes.len(), before.nodes.len(), "nodes are never added or removed");
    assert_eq!(totals(&rig.scene), totals(&before), "vertices and triangles are unchanged");
    assert_eq!(mesh_of(&rig.scene, rig.body), &body_before, "the bound Body mesh is untouched");
    assert_eq!(rig.scene.nodes[rig.body.index()].skin.map(|s| rig.scene.skins[s.index()].clone()), {
        let b = &before.skins[before.nodes[rig.body.index()].skin.unwrap().index()];
        Some(b.clone())
    });
    assert!(rig.scene.nodes[rig.outfit.index()].mesh.is_none() && rig.scene.nodes[rig.outfit.index()].skin.is_none());
    assert_eq!((rig.scene.meshes.len(), rig.scene.skins.len()), (2, 2), "the emptied mesh and skin are gone");
    // Hair keeps its primitives first, then the Outfit's, each with its own material.
    let hair = mesh_of(&rig.scene, rig.hair);
    assert_eq!(hair.primitives.len(), 2);
    assert_eq!(hair.primitives[0].material, mesh_of(&before, rig.hair).primitives[0].material);
    assert_eq!(hair.primitives[1].material, mesh_of(&before, rig.outfit).primitives[0].material);
    assert_clean(&rig.scene);
}

#[test]
fn skinned_world_positions_are_unchanged_in_three_poses() {
    let rig = new_rig();
    for k in 0..3 {
        let mut before = rig.scene.clone();
        let mut after = rig.scene.clone();
        merge_meshes(&mut after).unwrap();
        pose(&mut before, k);
        pose(&mut after, k);
        let expected = skinned_world(&before, &[rig.hair, rig.outfit]);
        let got = skinned_world(&after, &[rig.hair]);
        assert_eq!(expected.len(), got.len());
        for (a, b) in expected.iter().zip(&got) {
            assert!((*a - *b).abs().max_element() < 1e-5, "pose {k}: {a} vs {b}");
        }
    }
}

#[test]
fn joined_skin_is_the_union_with_one_matrix_per_joint() {
    let mut rig = new_rig();
    let before = rig.scene.clone();
    merge_meshes(&mut rig.scene).unwrap();
    let skin = &rig.scene.skins[rig.scene.nodes[rig.hair.index()].skin.unwrap().index()];
    let n = &rig.nodes;
    // Hair's joints first, then the Outfit's joints Hair lacks, in the Outfit's order.
    let expected: Vec<NodeIx> =
        ["head", "neck", "hairA", "hips", "spine", "chest", "skirt"].iter().map(|j| n[j]).collect();
    assert_eq!(skin.joints, expected);
    assert_eq!(skin.inverse_bind.len(), skin.joints.len());
    let world = world_matrices(&before);
    for (j, m) in skin.joints.iter().zip(&skin.inverse_bind) {
        assert!(m.abs_diff_eq(world[j.index()].inverse(), 1e-6));
    }
}

#[test]
fn a_shared_joint_with_a_different_inverse_bind_keeps_the_pair_apart() {
    let mut rig = new_rig();
    // Outfit's skin: chest and neck differ. Hair shares only the neck, so the neck is the joint to name.
    let outfit_skin = rig.scene.nodes[rig.outfit.index()].skin.unwrap().index();
    for (k, joint) in rig.scene.skins[outfit_skin].joints.clone().iter().enumerate() {
        if *joint == rig.nodes["chest"] || *joint == rig.nodes["neck"] {
            rig.scene.skins[outfit_skin].inverse_bind[k] *= Mat4::from_translation(Vec3::new(0.0, 1e-3, 0.0));
        }
    }
    let before = rig.scene.clone();
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert_eq!(rig.scene, before, "nothing changes");
    assert_eq!(report.skinned, (3, 3));
    assert_eq!(report.merged, Vec::new());
    let pair = report.apart.iter().find(|p| p.target == rig.hair && p.other == rig.outfit).expect("the pair is named");
    assert_eq!(pair.reason, ApartReason::InverseBind { joint: rig.nodes["neck"] });
    assert!(pair.reason.to_string().contains("inverse bind"));
}

#[test]
fn a_difference_inside_the_tolerance_still_joins() {
    let mut rig = new_rig();
    let outfit_skin = rig.scene.nodes[rig.outfit.index()].skin.unwrap().index();
    rig.scene.skins[outfit_skin].inverse_bind[0].x_axis.x += 5e-7;
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert_eq!(report.merged, [rig.outfit]);
}

#[test]
fn third_person_hair_and_both_outfit_stay_apart() {
    let mut rig = new_rig();
    annotate(&mut rig.scene, &[(rig.hair, FirstPersonFlag::ThirdPersonOnly), (rig.outfit, FirstPersonFlag::Both)]);
    let before = rig.scene.clone();
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert_eq!(rig.scene, before);
    assert_eq!(report.apart.len(), 1);
    assert_eq!(report.apart[0].reason, ApartReason::FirstPerson);
}

#[test]
fn an_unannotated_mesh_counts_as_auto() {
    let mut rig = new_rig();
    annotate(&mut rig.scene, &[(rig.hair, FirstPersonFlag::Auto)]);
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert_eq!(report.merged, [rig.outfit], "explicit Auto equals no annotation");

    let mut rig = new_rig();
    annotate(&mut rig.scene, &[(rig.hair, FirstPersonFlag::Both)]);
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert!(report.merged.is_empty(), "Both differs from the unannotated Auto");
}

#[test]
fn two_both_meshes_join_with_the_annotation_on_the_target() {
    let mut rig = new_rig();
    annotate(&mut rig.scene, &[(rig.hair, FirstPersonFlag::Both), (rig.outfit, FirstPersonFlag::Both)]);
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert_eq!(report.merged, [rig.outfit]);
    let fp = rig.scene.avatar.as_ref().unwrap().first_person.as_ref().unwrap();
    assert_eq!(fp.annotations, [MeshAnnotation { node: rig.hair, flag: FirstPersonFlag::Both }]);
    assert_eq!(mesh_of(&rig.scene, rig.hair).primitives.len(), 2);
    assert_clean(&rig.scene);
}

#[test]
fn the_emptied_nodes_annotation_is_dropped_the_targets_is_kept() {
    let mut rig = new_rig();
    annotate(
        &mut rig.scene,
        &[(rig.outfit, FirstPersonFlag::ThirdPersonOnly), (rig.hair, FirstPersonFlag::ThirdPersonOnly)],
    );
    merge_meshes(&mut rig.scene).unwrap();
    let fp = rig.scene.avatar.as_ref().unwrap().first_person.as_ref().unwrap();
    assert_eq!(fp.annotations, [MeshAnnotation { node: rig.hair, flag: FirstPersonFlag::ThirdPersonOnly }]);
}

#[test]
fn a_morph_bound_or_morphing_or_shared_mesh_is_never_joined() {
    // The Body is bound: put the Hair on the same skin and give the Outfit a morph target no expression binds.
    let mut rig = new_rig();
    let outfit_mesh = rig.scene.nodes[rig.outfit.index()].mesh.unwrap();
    let n = rig.scene.meshes[outfit_mesh.index()].primitives[0].mesh.positions.len();
    rig.scene.meshes[outfit_mesh.index()].primitives[0].morph_targets = vec![MorphTarget::new(vec![Vec3::Y; n])];
    rig.scene.meshes[outfit_mesh.index()].morph_target_names = vec!["pose".into()];
    let before = rig.scene.clone();
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert_eq!(rig.scene, before, "Body is bound, Outfit morphs: only Hair remains and it has nobody to join");
    assert_eq!(report.merged, Vec::new());

    // A mesh drawn by two nodes can't be emptied.
    let mut rig = new_rig();
    let hair_mesh = rig.scene.nodes[rig.hair.index()].mesh;
    let skin = rig.scene.nodes[rig.hair.index()].skin;
    let mut twin = Node::new("HairTwin");
    twin.mesh = hair_mesh;
    twin.skin = skin;
    let twin = rig.scene.add_node(twin);
    rig.scene.attach(rig.root, twin);
    let before = rig.scene.clone();
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert!(!report.merged.contains(&rig.hair) && !report.merged.contains(&twin));
    assert_eq!(mesh_of(&rig.scene, rig.hair), mesh_of(&before, rig.hair));
}

#[test]
fn a_skin_shared_with_a_node_that_stays_is_not_widened() {
    let mut rig = new_rig();
    // A second Hair node shares the first one's skin but has its own mesh, and is bound (so it stays).
    let skin = rig.scene.nodes[rig.hair.index()].skin;
    let extra = rig.scene.nodes[rig.hair.index()].mesh.map(|m| rig.scene.meshes[m.index()].clone()).unwrap();
    let mesh = rig.scene.add_mesh(extra);
    let mut node = Node::new("HairBound");
    node.mesh = Some(mesh);
    node.skin = skin;
    let bound = rig.scene.add_node(node);
    rig.scene.attach(rig.root, bound);
    rig.scene.avatar.as_mut().unwrap().expressions.insert(ExpressionName::parse("hairShape").unwrap(), {
        // The mesh has no morph target to hit; give it one so the bind is valid.
        Expression::morph(bound, 0)
    });
    let n = rig.scene.meshes[mesh.index()].primitives[0].mesh.positions.len();
    rig.scene.meshes[mesh.index()].primitives[0].morph_targets = vec![MorphTarget::new(vec![Vec3::Y; n])];
    rig.scene.meshes[mesh.index()].morph_target_names = vec!["lift".into()];
    assert_clean(&rig.scene);
    let shared_before = rig.scene.skins[skin.unwrap().index()].clone();

    let report = merge_meshes(&mut rig.scene).unwrap();
    assert_eq!(report.merged, [rig.outfit]);
    let kept = rig.scene.nodes[bound.index()].skin.unwrap();
    assert_eq!(rig.scene.skins[kept.index()], shared_before, "the bound node's skin is as it was");
    assert_ne!(rig.scene.nodes[rig.hair.index()].skin, Some(kept), "the joined mesh got a skin of its own");
    assert_clean(&rig.scene);
}

fn props() -> (IoScene, NodeIx, NodeIx, NodeIx) {
    let mut scene = IoScene::new();
    let root = scene.add_node(Node::new("Root"));
    scene.roots = vec![root];
    let parent = scene.add_node(Node::new("Prop").at(Vec3::new(0.0, 1.0, 0.0)));
    scene.attach(root, parent);
    let mut a = Node::new("A").at(Vec3::new(0.1, 0.2, 0.0));
    a.transform.rotation = Quat::from_rotation_y(0.4);
    let mut b = Node::new("B").at(Vec3::new(-0.3, 0.0, 0.2));
    b.transform.rotation = Quat::from_rotation_x(-0.7);
    b.transform.scale = Vec3::new(1.5, 1.0, 0.5);
    let mut ids = Vec::new();
    for (k, mut node) in [a, b].into_iter().enumerate() {
        let mat = unlit(&mut scene, if k == 0 { "a" } else { "b" });
        let mut prim = Primitive::new(quad(10 + k as u64));
        prim.material = Some(mat);
        let mut mesh = Mesh::new("prop");
        mesh.primitives = vec![prim];
        node.mesh = Some(scene.add_mesh(mesh));
        let ix = scene.add_node(node);
        scene.attach(parent, ix);
        ids.push(ix);
    }
    (scene, parent, ids[0], ids[1])
}

fn basic_world(scene: &IoScene, nodes: &[NodeIx]) -> Vec<Vec3> {
    let world = world_matrices(scene);
    let mut out = Vec::new();
    for &node in nodes {
        for prim in &mesh_of(scene, node).primitives {
            out.extend(prim.mesh.positions.iter().map(|p| world[node.index()].transform_point3(*p)));
        }
    }
    out
}

#[test]
fn basic_meshes_under_one_parent_join_exactly_in_every_pose() {
    let (scene, parent, a, b) = props();
    let mut joined = scene.clone();
    let report = merge_meshes(&mut joined).unwrap();
    assert_eq!((report.basic, report.skinned), ((2, 1), (0, 0)));
    assert_eq!(report.merged, [b]);
    assert_eq!(totals(&joined), totals(&scene));
    assert_eq!(joined.meshes.len(), 1);
    assert_clean(&joined);
    // Materials follow their primitive, in node order.
    let materials: Vec<_> = mesh_of(&joined, a).primitives.iter().map(|p| p.material).collect();
    assert_eq!(materials, [mesh_of(&scene, a).primitives[0].material, mesh_of(&scene, b).primitives[0].material]);

    for turn in [0.0, std::f32::consts::FRAC_PI_2] {
        let mut before = scene.clone();
        let mut after = joined.clone();
        for s in [&mut before, &mut after] {
            s.nodes[parent.index()].transform.rotation = Quat::from_rotation_y(turn);
        }
        let expected = basic_world(&before, &[a, b]);
        let got = basic_world(&after, &[a]);
        assert_eq!(expected.len(), got.len());
        for (x, y) in expected.iter().zip(&got) {
            assert!((*x - *y).abs().max_element() < 1e-6, "turn {turn}: {x} vs {y}");
        }
    }
}

#[test]
fn baked_normals_follow_and_a_mirroring_bake_keeps_triangles_facing_out() {
    let (mut scene, _, a, b) = props();
    scene.nodes[b.index()].transform.scale = Vec3::new(-1.0, 1.0, 1.0);
    let facing = |s: &IoScene, node: NodeIx| {
        let world = world_matrices(s);
        let p = &mesh_of(s, node).primitives[0].mesh;
        let at = |i: u32| world[node.index()].transform_point3(p.positions[i as usize]);
        let (i0, i1, i2) = (p.indices[0], p.indices[1], p.indices[2]);
        (at(i1) - at(i0)).cross(at(i2) - at(i0)).normalize()
    };
    let expected = facing(&scene, b);
    let mut joined = scene.clone();
    merge_meshes(&mut joined).unwrap();
    // The moved primitive is the second one on A; its first triangle must face the same way in the world.
    let world = world_matrices(&joined);
    let p = &mesh_of(&joined, a).primitives[1].mesh;
    let at = |i: u32| world[a.index()].transform_point3(p.positions[i as usize]);
    let got = (at(p.indices[1]) - at(p.indices[0])).cross(at(p.indices[2]) - at(p.indices[0])).normalize();
    // A node with a negative determinant draws its triangles clockwise-front, so the facing it had is the reverse of
    // the plain cross product, and the baked (positive-determinant) copy must show the same side.
    assert!((-expected - got).length() < 1e-5, "{expected} vs {got}");
    let normal_world = world[a.index()].transform_vector3(p.normals[0]).normalize();
    assert!(normal_world.dot(got) > 0.95, "the baked normal still points out of the front");
}

#[test]
fn basic_meshes_under_the_head_and_under_the_hips_stay_apart() {
    let mut rig = new_rig();
    let mut ids = Vec::new();
    for (k, bone) in ["head", "hips"].into_iter().enumerate() {
        let mat = unlit(&mut rig.scene, bone);
        let mut prim = Primitive::new(quad(20 + k as u64));
        prim.material = Some(mat);
        let mut mesh = Mesh::new(bone);
        mesh.primitives = vec![prim];
        let mut node = Node::new(format!("prop-{bone}"));
        node.mesh = Some(rig.scene.add_mesh(mesh));
        let ix = rig.scene.add_node(node);
        rig.scene.attach(rig.nodes[bone], ix);
        ids.push(ix);
    }
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert!(!report.merged.contains(&ids[0]) && !report.merged.contains(&ids[1]));
    assert_eq!(report.basic, (2, 2));
    assert_eq!(report.apart.len(), 1);
    assert_eq!(
        (report.apart[0].target, report.apart[0].other, report.apart[0].reason),
        (ids[0], ids[1], ApartReason::Parent)
    );
}

#[test]
fn basic_meshes_on_nodes_that_can_move_stay_apart() {
    for reason_node in ["humanoid", "spring", "skin joint"] {
        let (mut scene, _, a, b) = props();
        match reason_node {
            "humanoid" => {
                let mut av = Avatar::new();
                av.humanoid.assign("hips", b).unwrap();
                scene.avatar = Some(av);
            }
            "spring" => {
                let mut av = Avatar::new();
                av.spring_bones.chains = vec![SpringChain {
                    name: "chain".into(),
                    joints: vec![SpringJoint::new(b)],
                    collider_groups: Vec::new(),
                    center: None,
                }];
                scene.avatar = Some(av);
            }
            _ => {
                let ibm = vec![Mat4::IDENTITY];
                scene.add_skin(Skin::new("unused", vec![b], ibm));
            }
        }
        let before = scene.clone();
        let report = merge_meshes(&mut scene).unwrap();
        assert_eq!(scene, before, "{reason_node}");
        assert_eq!(report.apart[0].reason, ApartReason::Moves { node: b }, "{reason_node}");
        assert_eq!((report.apart[0].target, report.apart[0].other), (a, b));
    }
}

#[test]
fn a_singular_target_transform_is_not_baked_into() {
    let (mut scene, _, a, _) = props();
    scene.nodes[a.index()].transform.scale = Vec3::new(1.0, 0.0, 1.0);
    let before = scene.clone();
    let report = merge_meshes(&mut scene).unwrap();
    assert_eq!(scene, before);
    assert_eq!(report.apart[0].reason, ApartReason::SingularTransform);
}

#[test]
fn two_runs_are_identical_and_a_second_pass_changes_nothing() {
    let rig = new_rig();
    let (mut one, mut two) = (rig.scene.clone(), rig.scene.clone());
    let (r1, r2) = (merge_meshes(&mut one).unwrap(), merge_meshes(&mut two).unwrap());
    assert_eq!((&one, &r1), (&two, &r2));
    let again = merge_meshes(&mut one).unwrap();
    assert_eq!(one, two);
    assert_eq!(again.merged, Vec::new());
}

#[test]
fn a_scene_that_breaks_its_invariants_is_rejected_and_left_alone() {
    let mut rig = new_rig();
    rig.scene.skins[0].inverse_bind.pop();
    let before = rig.scene.clone();
    assert!(matches!(merge_meshes(&mut rig.scene), Err(IoError::Rejected(_))));
    assert_eq!(rig.scene, before);
}

#[test]
fn many_pieces_collapse_to_one_mesh_per_compatible_group() {
    let mut rig = new_rig();
    for k in 0..4 {
        add_skinned(
            &mut rig.scene,
            &rig.nodes,
            rig.root,
            &format!("Piece{k}"),
            &["spine", "chest", "leftUpperArm"],
            40 + k,
        );
    }
    let report = merge_meshes(&mut rig.scene).unwrap();
    assert_eq!(report.skinned, (7, 2));
    assert_eq!(report.merged.len(), 5);
    assert_clean(&rig.scene);
}
