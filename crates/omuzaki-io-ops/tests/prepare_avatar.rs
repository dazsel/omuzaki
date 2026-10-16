//! `prepare::avatar::to_avatar` (#1215): a generated VRM becomes a valid avatar whose meshes skin as the file does,
//! whose morphs are the file's deltas exactly, and whose report carries every step's findings. Every file is generated
//! here (the mock VRM 1.0 writer); no third-party model.

mod common;

use std::collections::BTreeMap;

use common::{
    add_skinned_mesh, def, file_skinning, humanoid, humanoid_without, max_error, runtime_skinning, through_vrm,
};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_avatar_api::def::NodeChoice;
use omuzaki_core::glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use omuzaki_io_api::{
    Expression, ExpressionName, IoError, IoScene, Material, MaterialIx, Mesh, MeshData, MorphBind, Node, Pbr,
    Primitive, Shading,
};
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_render_api::MtoonSupport;

/// A plain material the mock writer keeps.
fn plain(name: &str) -> Material {
    let mut m =
        Material::new(name, Shading::Pbr(Pbr { metallic: 0.0, roughness: 1.0, metallic_roughness_texture: None }));
    m.base_color = Vec4::new(0.8, 0.7, 0.6, 1.0);
    m
}

/// The bones the body mesh is skinned to.
const BODY: [&str; 12] = [
    "hips",
    "spine",
    "chest",
    "neck",
    "head",
    "leftUpperArm",
    "leftLowerArm",
    "leftHand",
    "rightUpperArm",
    "rightLowerArm",
    "leftUpperLeg",
    "rightUpperLeg",
];

/// A T-posed humanoid with a skinned body (two morph targets) and a hat whose skin shares the head joint with a
/// different inverse bind (bound 10 cm higher), both on material 0.
fn two_skins() -> common::Fixture {
    let mut f = humanoid(0.0);
    f.scene.add_material(plain("Skin"));
    add_skinned_mesh(&mut f, "Body", &BODY, 2, Some(0));
    let hat = add_skinned_mesh(&mut f, "Hat", &["head", "neck"], 0, Some(0));
    let skin = f.scene.nodes[hat.index()].skin.expect("a skin").index();
    let head = &mut f.scene.skins[skin].inverse_bind[0];
    *head = Mat4::from_translation(Vec3::Y * 0.1) * *head;
    f
}

#[test]
fn a_generated_vrm_skins_as_the_file_and_keeps_each_skins_matrices() {
    let scene = through_vrm(&two_skins().scene);
    let (avatar, report) = to_avatar(&scene, &def(), MtoonSupport::NONE).expect("the generated VRM imports");
    assert_eq!(report.count(omuzaki_io_api::Severity::Error), 0, "{report:?}");
    assert_eq!(avatar.validate(), Ok(()));
    assert_eq!(avatar.meshes.len(), 2);
    let error = max_error(&runtime_skinning(&avatar, &avatar.skeleton.rest_pose()), &file_skinning(&scene));
    assert!(error < 1e-5, "rest skinning matches the file: {error}");
    // The head is joint 4 of the body's skin and joint 0 of the hat's: each mesh keeps its own matrix for it.
    let (body, hat) = (&avatar.meshes[0], &avatar.meshes[1]);
    assert_eq!(body.mesh.joint_bones[4], hat.mesh.joint_bones[0], "the same bone");
    let skins = &scene.skins;
    assert_eq!((body.inverse_bind[4], hat.inverse_bind[0]), (skins[0].inverse_bind[4], skins[1].inverse_bind[0]));
    assert_ne!(body.inverse_bind[4], hat.inverse_bind[0]);
}

#[test]
fn sparse_morphs_are_the_files_dense_deltas_exactly() {
    let scene = through_vrm(&two_skins().scene);
    let (avatar, _) = to_avatar(&scene, &def(), MtoonSupport::NONE).expect("imports");
    let dense = &scene.meshes[0].primitives[0].morph_targets;
    let morphs = avatar.meshes[0].morphs.as_ref().expect("the body's targets");
    assert_eq!(morphs.len(), dense.len());
    for (sparse, target) in morphs.targets().iter().zip(dense) {
        let mut rebuilt = vec![Vec3::ZERO; target.positions.len()];
        for (&i, &d) in sparse.indices.iter().zip(&sparse.positions) {
            rebuilt[i as usize] = d;
        }
        assert_eq!(rebuilt, target.positions, "bit for bit");
        assert!(sparse.indices.len() < target.positions.len(), "only the moved vertices are kept");
    }
    let names = &avatar.retained.meshes[0].morph_target_names;
    assert_eq!(names, &["Target_0", "Target_1"], "the file's names are kept for export");
}

#[test]
fn every_steps_findings_reach_the_one_report() {
    // No humanoid map and a knee named oddly (the humanoid step infers it), arms down (the T rest corrects them), a
    // second primitive on the same material and an unused material (cleanup merges and drops), and an `aa` preset
    // (synthesis builds jawOpen from it and reports the channels it can't build).
    let mut f = humanoid(30.0);
    f.scene.avatar.as_mut().expect("an avatar").humanoid.bones.clear();
    let knee = f.node("leftLowerLeg").index();
    f.scene.nodes[knee].name = "Knee_Thing".into();
    f.scene.add_material(plain("Skin"));
    f.scene.add_material(plain("Unused"));
    // Skinned to every bone: without a map, the humanoid step looks for bones among the skin's joints.
    let every: Vec<String> = f.nodes.keys().filter(|n| *n != "Root").cloned().collect();
    let every: Vec<&str> = every.iter().map(String::as_str).collect();
    let body = add_skinned_mesh(&mut f, "Body", &every, 1, Some(0));
    let mesh = f.scene.nodes[body.index()].mesh.expect("a mesh").index();
    let copy = f.scene.meshes[mesh].primitives[0].clone();
    let mut moved = copy.clone();
    moved.mesh.positions.iter_mut().for_each(|p| *p += Vec3::Z);
    f.scene.meshes[mesh].primitives.push(moved);
    let aa = Expression::morph(body, 0);
    f.scene.avatar.as_mut().expect("an avatar").expressions.insert(ExpressionName::parse("aa").expect("a name"), aa);
    let (_, report) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    for rule in [
        "humanoid.inferred",
        "avatar.rest_normalised",
        "cleanup.merged",
        "cleanup.dropped_unused",
        "arkit.synthesised",
        "arkit.missing",
    ] {
        assert!(report.has_rule(rule), "{rule} reaches the report: {report:?}");
    }
}

#[test]
fn a_missing_left_lower_leg_is_rejected_naming_the_bone() {
    let mut f = humanoid_without(0.0, &["leftLowerLeg"]);
    f.scene.add_material(plain("Skin"));
    add_skinned_mesh(&mut f, "Body", &["hips", "spine", "head", "leftUpperLeg", "leftFoot"], 0, Some(0));
    let Err(IoError::Rejected(report)) = to_avatar(&f.scene, &def(), MtoonSupport::NONE) else {
        panic!("a humanoid without a left lower leg can't perform");
    };
    let missing: Vec<_> = report.findings.iter().filter(|x| x.rule == "humanoid.required_missing").collect();
    assert_eq!(missing.len(), 1, "{report:?}");
    assert_eq!(missing[0].location.as_deref(), Some("avatar/humanoid/leftLowerLeg"));
}

#[test]
fn a_stale_override_is_found_again_by_name() {
    let mut f = humanoid(0.0);
    f.scene.add_material(plain("Skin"));
    add_skinned_mesh(&mut f, "Body", &BODY, 0, Some(0));
    // The person mapped the left hand to a node by name; its stored index points somewhere else now.
    let hand = f.node("leftHand");
    let mut def = def();
    def.humanoid.insert(HumanoidBone::LeftHand, NodeChoice::new(hand.0 + 7, "leftHand"));
    let (avatar, report) = to_avatar(&f.scene, &def, MtoonSupport::NONE).expect("imports");
    assert!(report.has_rule("humanoid.override_refound"), "{report:?}");
    let bone = avatar.skeleton.humanoid_bone(HumanoidBone::LeftHand).expect("the left hand");
    assert_eq!(avatar.retained.nodes[hand.index()].bone, Some(bone), "the node of that name plays it");

    def.humanoid.insert(HumanoidBone::LeftHand, NodeChoice::new(hand.0 + 7, "Gone"));
    let (_, report) = to_avatar(&f.scene, &def, MtoonSupport::NONE).expect("the file's own map stands");
    assert!(report.has_rule("humanoid.override_invalid"), "{report:?}");
}

/// A rigid mesh called `name` (one triangle, no skin) drawn by a new node under `parent` at `at`.
fn rigid(
    f: &mut common::Fixture,
    name: &str,
    parent: &str,
    at: Vec3,
    mesh: Option<omuzaki_io_api::MeshIx>,
) -> omuzaki_io_api::NodeIx {
    let mesh = mesh.unwrap_or_else(|| {
        let data = MeshData {
            positions: vec![Vec3::ZERO, Vec3::X * 0.02, Vec3::Y * 0.02],
            normals: vec![Vec3::Z; 3],
            uvs: vec![Vec2::ZERO; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        };
        let mut primitive = Primitive::new(data);
        primitive.material = Some(MaterialIx(0));
        primitive.morph_targets = vec![omuzaki_io_api::MorphTarget::new(vec![Vec3::Z * 0.01, Vec3::ZERO, Vec3::ZERO])];
        let mut mesh = Mesh::new(name);
        mesh.primitives = vec![primitive];
        f.scene.add_mesh(mesh)
    });
    let mut node = Node::new(name).at(at);
    node.mesh = Some(mesh);
    let node = f.scene.add_node(node);
    f.scene.attach(f.node(parent), node);
    f.nodes.insert(name.to_owned(), node);
    node
}

#[test]
fn one_mesh_drawn_by_two_nodes_is_one_runtime_mesh_per_node() {
    let mut f = humanoid(0.0);
    f.scene.add_material(plain("Skin"));
    let left = rigid(&mut f, "Glove.L", "leftHand", Vec3::X * 0.05, None);
    let mesh = f.scene.nodes[left.index()].mesh;
    let right = rigid(&mut f, "Glove.R", "rightHand", Vec3::X * -0.05, mesh);
    let mut squeeze = Expression::new();
    squeeze.morph_binds =
        vec![MorphBind { node: left, index: 0, weight: 0.25 }, MorphBind { node: right, index: 0, weight: 0.75 }];
    f.scene
        .avatar
        .as_mut()
        .expect("an avatar")
        .expressions
        .insert(ExpressionName::parse("Squeeze").expect("a name"), squeeze);
    let (avatar, _) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    assert_eq!(avatar.meshes.len(), 2, "one per drawing node");
    let source = &avatar.retained.meshes[mesh.expect("a mesh").index()];
    let instances: Vec<(u32, Vec<u16>)> = source.instances.iter().map(|i| (i.node, i.meshes.clone())).collect();
    assert_eq!(instances, [(left.0, vec![0]), (right.0, vec![1])]);
    let binds: Vec<(u16, f32, Option<u32>)> =
        avatar.expressions[0].morph.iter().map(|b| (b.mesh, b.weight, b.node)).collect();
    assert_eq!(binds, [(0, 0.25, Some(left.0)), (1, 0.75, Some(right.0))], "each bind moves its own copy");
    // Each copy follows its own node: a rigid mesh is a one-joint skin on its node's bone.
    let error = max_error(&runtime_skinning(&avatar, &avatar.skeleton.rest_pose()), &file_skinning(&f.scene));
    assert!(error < 1e-5, "{error}");
}

#[test]
fn a_scaled_node_is_baked_and_its_mesh_still_skins_as_the_file() {
    let mut f = humanoid(0.0);
    f.scene.add_material(plain("Skin"));
    let ribbon = rigid(&mut f, "Ribbon", "head", Vec3::new(0.0, 0.1, 0.0), None);
    let t = &mut f.scene.nodes[ribbon.index()].transform;
    t.scale = Vec3::splat(2.0);
    t.rotation = Quat::from_rotation_y(0.3);
    let (avatar, _) = to_avatar(&f.scene, &def(), MtoonSupport::NONE).expect("imports");
    let error = max_error(&runtime_skinning(&avatar, &avatar.skeleton.rest_pose()), &file_skinning(&f.scene));
    assert!(error < 1e-5, "the scale is folded into the mesh's matrix: {error}");
    let bone = avatar.retained.nodes[ribbon.index()].bone.expect("a bone");
    let rest = avatar.skeleton.bones()[bone.index()].rest;
    assert!((rest.translation - Vec3::new(0.0, 0.1, 0.0)).length() < 1e-6, "an unscaled parent keeps the offset");
}

#[test]
fn two_runs_give_equal_avatars_and_reports() {
    let scene = through_vrm(&two_skins().scene);
    let a = to_avatar(&scene, &def(), MtoonSupport::NONE).expect("imports");
    let b = to_avatar(&scene, &def(), MtoonSupport::NONE).expect("imports");
    assert_eq!(a, b);
}

#[test]
fn the_retained_scene_graph_is_the_files() {
    let scene = through_vrm(&two_skins().scene);
    let (avatar, _) = to_avatar(&scene, &def(), MtoonSupport::NONE).expect("imports");
    let r = &avatar.retained;
    assert_eq!(r.nodes.len(), scene.nodes.len());
    let names: BTreeMap<&str, usize> = r.nodes.iter().enumerate().map(|(i, n)| (n.name.as_str(), i)).collect();
    for (i, node) in scene.nodes.iter().enumerate() {
        assert_eq!(names[node.name.as_str()], i, "names and order");
        assert_eq!(r.nodes[i].transform.translation, node.transform.translation, "transforms bit for bit");
    }
    assert_eq!(r.roots, scene.roots.iter().map(|n| n.0).collect::<Vec<_>>());
    assert_eq!(r.skins.len(), 2);
    assert_eq!(r.skins[1].inverse_bind, scene.skins[1].inverse_bind);
    assert_eq!(r.meta.as_ref().map(|m| m.name.as_str()), Some("Generated Test Avatar"));
    assert!(r.nodes.iter().filter(|n| n.bone.is_some()).count() == avatar.skeleton.len(), "every bone has its node");
}

#[test]
fn a_scene_from_a_later_layout_is_refused() {
    let scene: IoScene = humanoid(0.0).scene;
    let mut def = def();
    def.schema = 2;
    let Err(IoError::Rejected(report)) = to_avatar(&scene, &def, MtoonSupport::NONE) else { panic!("refused") };
    assert!(report.has_rule("avatar.schema_unknown"));
}
