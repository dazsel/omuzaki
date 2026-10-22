//! `gather::avatar_import::gather_imported` (#1294): an imported avatar exports back to its file. The test VRM is
//! generated (T-pose, all 52 `ARKit` expressions, nothing to weld, merge or drop), written and read by the mock VRM
//! 1.0 writer and reader; no third-party model.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{add_skinned_mesh, def, humanoid, through_vrm};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::body::BodyView;
use omuzaki_avatar_api::def::{AvatarDef, MaterialEdit, NodeChoice};
use omuzaki_char_api::MaterialName;
use omuzaki_core::glam::Vec4;
use omuzaki_io_api::{ARKIT_52, Expression, ExpressionName, HumanBoneName, IoScene, Material, Pbr, Shading};
use omuzaki_io_ops::gather::avatar_import::gather_imported;
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_render_api::MtoonSupport;

/// A plain material with off-grid linear colours, the kind an 8-bit round trip would nudge.
fn off_grid() -> Material {
    let mut m =
        Material::new("Skin", Shading::Pbr(Pbr { metallic: 0.0, roughness: 1.0, metallic_roughness_texture: None }));
    m.base_color = Vec4::new(0.5, 0.123, 0.9999, 1.0);
    m.emissive = omuzaki_core::glam::Vec3::new(0.123, 0.5, 0.9999);
    m
}

/// The test VRM as the importer reads it: a T-posed humanoid whose face mesh carries one morph target per `ARKit`
/// channel and an expression of that name for each, so synthesis has nothing to add.
fn perfect_sync() -> IoScene {
    let mut f = humanoid(0.0);
    f.scene.add_material(off_grid());
    let joints: Vec<String> = f.nodes.keys().filter(|n| *n != "Root").cloned().collect();
    let joints: Vec<&str> = joints.iter().map(String::as_str).collect();
    let face = add_skinned_mesh(&mut f, "Face", &joints, ARKIT_52.len(), Some(0));
    let mesh = f.scene.nodes[face.index()].mesh.unwrap().index();
    f.scene.meshes[mesh].morph_target_names = ARKIT_52.iter().map(|c| (*c).to_owned()).collect();
    let avatar = f.scene.avatar.as_mut().unwrap();
    for (i, channel) in ARKIT_52.iter().enumerate() {
        avatar
            .expressions
            .insert(ExpressionName::parse(channel).unwrap(), Expression::morph(face, u32::try_from(i).unwrap()));
    }
    through_vrm(&f.scene)
}

/// `scene` imported with `def`, exported, and read back through the mock VRM writer and reader.
fn round_trip(scene: &IoScene, def: &AvatarDef) -> (ImportedAvatar, IoScene) {
    let (avatar, _) = to_avatar(scene, def, MtoonSupport::NONE).expect("imports");
    let mut out = gather_imported(&BodyView::imported(&avatar), def, &|_| None).expect("gathers");
    // The export op takes the record from the character's source row; here it is the file's own.
    out.provenance.clone_from(&scene.provenance);
    (avatar, through_vrm(&out))
}

#[test]
fn an_unedited_avatar_exports_back_to_its_file() {
    let original = perfect_sync();
    let (_, again) = round_trip(&original, &def());
    let mut expected = original.clone();
    expected.source_generator.clone_from(&again.source_generator);
    assert_eq!(again, expected, "every field, bit for bit");
}

#[test]
fn off_grid_colours_come_back_bit_exact() {
    let original = perfect_sync();
    let (_, again) = round_trip(&original, &def());
    let bits = |m: &Material| (m.base_color.to_array().map(f32::to_bits), m.emissive.to_array().map(f32::to_bits));
    assert_eq!(bits(&again.materials[0]), bits(&off_grid()));
}

#[test]
fn one_material_edit_changes_only_its_field() {
    let original = perfect_sync();
    let mut def = def();
    let mut edit = MaterialEdit::new();
    edit.base = Some([0.25, 0.5, 0.75, 1.0]);
    def.materials.insert(MaterialName::new("skin").unwrap(), edit);
    let (_, again) = round_trip(&original, &def);
    let mut expected = off_grid();
    expected.base_color = Vec4::new(0.25, 0.5, 0.75, 1.0);
    assert_eq!(again.materials[0], expected, "the edited field, and every other field as the file had it");
}

#[test]
fn a_humanoid_override_and_a_synthesised_expression_show_in_the_export() {
    // The file leaves its left eye unmapped and has no jawOpen, only an `aa` preset synthesis copies.
    let mut f = humanoid(0.0);
    f.scene.add_material(off_grid());
    let joints: Vec<String> = f.nodes.keys().filter(|n| *n != "Root").cloned().collect();
    let joints: Vec<&str> = joints.iter().map(String::as_str).collect();
    let face = add_skinned_mesh(&mut f, "Face", &joints, 1, Some(0));
    let eye = f.node("leftEye");
    let avatar = f.scene.avatar.as_mut().unwrap();
    avatar.humanoid.bones.remove(&HumanBoneName::new("leftEye").unwrap());
    avatar.expressions.insert(ExpressionName::parse("aa").unwrap(), Expression::morph(face, 0));
    let scene = through_vrm(&f.scene);
    let mut def = def();
    def.humanoid.insert(omuzaki_anim_api::HumanoidBone::LeftEye, NodeChoice::new(eye.0, "leftEye"));
    let (_, again) = round_trip(&scene, &def);
    let exported = again.avatar.as_ref().unwrap();
    assert_eq!(exported.humanoid.node("leftEye"), Some(eye), "the person's choice");
    let jaw = exported.expressions.get(&ExpressionName::parse("jawOpen").unwrap()).expect("synthesised for export");
    assert_eq!(jaw.morph_binds.len(), 1);
}

#[test]
fn a_body_built_from_parts_is_not_gathered_here() {
    use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
    omuzaki_char_mock::link();
    let character = omuzaki_char_mock::mock_character().unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &character, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let body = omuzaki_avatar_api::body::Body::Built(Box::new(built));
    let error = gather_imported(&body.view(), &def(), &|_| None).unwrap_err();
    assert!(matches!(error, omuzaki_io_api::IoError::Unsupported { .. }));
}

#[test]
fn a_scaled_rigid_mesh_and_a_mesh_drawn_with_and_without_a_skin_export_where_the_file_had_them() {
    use omuzaki_core::glam::{Quat, Vec2, Vec3};
    use omuzaki_io_api::{MaterialIx, Mesh, MeshData, Node, Primitive};
    let mut f = humanoid(0.0);
    f.scene.add_material(off_grid());
    // A rigid ribbon on a scaled, turned node under the head.
    let data = MeshData {
        positions: vec![Vec3::ZERO, Vec3::X * 0.02, Vec3::Y * 0.02],
        normals: vec![Vec3::Z; 3],
        uvs: vec![Vec2::ZERO; 3],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    };
    let mut primitive = Primitive::new(data);
    primitive.material = Some(MaterialIx(0));
    let mut mesh = Mesh::new("Ribbon");
    mesh.primitives = vec![primitive];
    let mesh = f.scene.add_mesh(mesh);
    let mut ribbon = Node::new("Ribbon").at(Vec3::Y * 0.1);
    ribbon.mesh = Some(mesh);
    ribbon.transform.scale = Vec3::splat(2.0);
    ribbon.transform.rotation = Quat::from_rotation_y(0.3);
    let ribbon = f.scene.add_node(ribbon);
    f.scene.attach(f.node("head"), ribbon);
    // A skinned body mesh, also drawn rigidly by a second node under the hips.
    let body = add_skinned_mesh(&mut f, "Body", &["hips", "spine", "head"], 0, Some(0));
    let mut copy = Node::new("BodyCopy").at(Vec3::X * 0.5);
    copy.mesh = f.scene.nodes[body.index()].mesh;
    let copy = f.scene.add_node(copy);
    f.scene.attach(f.node("hips"), copy);
    let original = through_vrm(&f.scene);
    let (_, again) = round_trip(&original, &def());
    let error = common::max_error(&common::file_skinning(&again), &common::file_skinning(&original));
    assert!(error < 1e-5, "everything sits where the file had it: {error}");
    let skinned = again.nodes.iter().find(|n| n.name == "Body").and_then(|n| n.mesh).unwrap();
    assert!(!again.meshes[skinned.index()].primitives[0].mesh.joints.is_empty(), "the skinned use keeps its skin");
}

/// A small rigid triangle mesh on material 0.
fn ribbon(scene: &mut IoScene) -> omuzaki_io_api::MeshIx {
    use omuzaki_core::glam::{Vec2, Vec3};
    use omuzaki_io_api::{MaterialIx, Mesh, MeshData, Primitive};
    let data = MeshData {
        positions: vec![Vec3::ZERO, Vec3::X * 0.02, Vec3::Y * 0.02],
        normals: vec![Vec3::Z; 3],
        uvs: vec![Vec2::ZERO; 3],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    };
    let mut primitive = Primitive::new(data);
    primitive.material = Some(MaterialIx(0));
    let mut mesh = Mesh::new("Ribbon");
    mesh.primitives = vec![primitive];
    scene.add_mesh(mesh)
}

#[test]
fn one_rigid_mesh_drawn_at_two_scales_exports_each_at_its_own_size() {
    use omuzaki_core::glam::Vec3;
    use omuzaki_io_api::Node;
    let mut f = humanoid(0.0);
    f.scene.add_material(off_grid());
    add_skinned_mesh(&mut f, "Body", &["hips", "spine", "head"], 0, Some(0));
    let mesh = ribbon(&mut f.scene);
    for (name, scale, at) in [("Twice", 2.0, -0.1), ("Thrice", 3.0, 0.1)] {
        let mut node = Node::new(name).at(Vec3::X * at);
        node.mesh = Some(mesh);
        node.transform.scale = Vec3::splat(scale);
        let node = f.scene.add_node(node);
        f.scene.attach(f.node("head"), node);
    }
    let original = through_vrm(&f.scene);
    let (_, again) = round_trip(&original, &def());
    let error = common::max_error(&common::file_skinning(&again), &common::file_skinning(&original));
    assert!(error < 1e-5, "each copy sits and sizes as the file had it: {error}");
}

#[test]
fn a_target_bound_twice_keeps_both_binds_and_their_sum() {
    use omuzaki_io_api::{Mesh, MorphBind};
    let mut f = humanoid(0.0);
    f.scene.add_material(off_grid());
    let face = add_skinned_mesh(&mut f, "Face", &["head", "neck"], 2, Some(0));
    // A second primitive, so the runtime holds every file bind once per primitive.
    let mesh = f.scene.nodes[face.index()].mesh.unwrap().index();
    let copy = f.scene.meshes[mesh].primitives[0].clone();
    f.scene.meshes[mesh].primitives.push(copy);
    let _: &Mesh = &f.scene.meshes[mesh];
    let mut expression = Expression::new();
    expression.morph_binds = vec![
        MorphBind { node: face, index: 0, weight: 0.3 },
        MorphBind { node: face, index: 0, weight: 0.45 },
        MorphBind { node: face, index: 1, weight: 1.0 },
    ];
    f.scene.avatar.as_mut().unwrap().expressions.insert(ExpressionName::parse("Pout").unwrap(), expression);
    let original = through_vrm(&f.scene);
    let (_, again) = round_trip(&original, &def());
    let binds = |scene: &IoScene| {
        let e = &scene.avatar.as_ref().unwrap().expressions[&ExpressionName::parse("Pout").unwrap()];
        e.morph_binds.iter().map(|b| (b.index, b.weight)).collect::<Vec<_>>()
    };
    assert_eq!(binds(&again), vec![(0, 0.3), (0, 0.45), (1, 1.0)], "both binds of target 0, at their own weights");
    assert_eq!(binds(&again), binds(&original));
}

/// A mesh no node draws, with `targets` morph targets (each with normal deltas), on material 0.
fn spare(name: &str, targets: usize) -> omuzaki_io_api::Mesh {
    use omuzaki_core::glam::{Vec2, Vec3};
    use omuzaki_io_api::{MaterialIx, Mesh, MeshData, MorphTarget, Primitive};
    let data = MeshData {
        positions: vec![Vec3::new(0.1, 1.2, 0.0), Vec3::new(0.123, 1.2, 0.0), Vec3::new(0.1, 1.2345, 0.01)],
        normals: vec![Vec3::Z; 3],
        uvs: vec![Vec2::ZERO, Vec2::X, Vec2::Y],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    };
    let mut primitive = Primitive::new(data);
    primitive.material = Some(MaterialIx(0));
    primitive.morph_targets = (0..targets)
        .map(|t| {
            let step = f32::from(u8::try_from(t).unwrap()) + 1.0;
            let mut target = MorphTarget::new(vec![Vec3::Y * (0.001 * step), Vec3::ZERO, Vec3::X * 0.0007]);
            target.normals = vec![Vec3::ZERO, Vec3::new(0.0, 0.1, -0.1), Vec3::ZERO];
            target
        })
        .collect();
    let mut mesh = Mesh::new(name);
    mesh.primitives = vec![primitive];
    mesh.morph_target_names = (0..targets).map(|t| format!("Spare_{t}")).collect();
    mesh
}

#[test]
fn meshes_no_node_draws_export_back_with_their_morphs() {
    let mut scene = perfect_sync();
    scene.add_mesh(spare("SpareWithMorphs", 2));
    scene.add_mesh(spare("SpareWithout", 0));
    let original = through_vrm(&scene);
    assert_eq!(original.meshes.len(), scene.meshes.len(), "the writer keeps meshes no node draws");
    let (avatar, again) = round_trip(&original, &def());
    let kept = avatar.retained.meshes.iter().filter(|m| m.instances.is_empty()).count();
    assert_eq!(kept, 2, "both are retained with their geometry");
    let mut expected = original.clone();
    expected.source_generator.clone_from(&again.source_generator);
    assert_eq!(again, expected, "every field, bit for bit, the undrawn meshes in their places");
}

/// A mesh drawn by a skinned node and by a rigid one, the rigid node first in the file when `rigid_first`.
fn shared_skinned_and_rigid(rigid_first: bool) -> IoScene {
    use omuzaki_core::glam::Vec3;
    use omuzaki_io_api::{MeshIx, Node};
    let mut f = humanoid(0.0);
    f.scene.add_material(off_grid());
    // `add_skinned_mesh` adds the mesh at the end of the list, so a node made first can name it.
    let next = MeshIx(u32::try_from(f.scene.meshes.len()).unwrap());
    let rigid = |f: &mut common::Fixture| {
        let mut node = Node::new("Copy").at(Vec3::X * 0.5);
        node.mesh = Some(next);
        let node = f.scene.add_node(node);
        f.scene.attach(f.node("hips"), node);
    };
    if rigid_first {
        rigid(&mut f);
    }
    add_skinned_mesh(&mut f, "Body", &["hips", "spine", "head"], 0, Some(0));
    if !rigid_first {
        rigid(&mut f);
    }
    f.scene
}

#[test]
fn a_mesh_drawn_skinned_and_rigidly_keeps_its_skin_whichever_node_comes_first() {
    for rigid_first in [true, false] {
        let original = through_vrm(&shared_skinned_and_rigid(rigid_first));
        let (_, again) = round_trip(&original, &def());
        let body = |scene: &IoScene| {
            let mesh = scene.nodes.iter().find(|n| n.name == "Body").and_then(|n| n.mesh).unwrap();
            scene.meshes[mesh.index()].primitives[0].mesh.clone()
        };
        let (was, now) = (body(&original), body(&again));
        assert_eq!((&now.joints, &now.weights), (&was.joints, &was.weights), "rigid first: {rigid_first}");
        let error = common::max_error(&common::file_skinning(&again), &common::file_skinning(&original));
        assert!(error < 1e-5, "rigid first: {rigid_first}: {error}");
    }
}

#[test]
fn a_collider_on_a_scaled_bone_exports_where_and_as_large_as_the_file_had_it() {
    use omuzaki_core::glam::Vec3;
    use omuzaki_io_api::{Collider, ColliderGroup, ColliderShape};
    let mut f = humanoid(0.0);
    f.scene.add_material(off_grid());
    add_skinned_mesh(&mut f, "Body", &["hips", "spine", "head"], 0, Some(0));
    let head = f.node("head");
    f.scene.nodes[head.index()].transform.scale = Vec3::splat(2.0);
    let springs = &mut f.scene.avatar.as_mut().unwrap().spring_bones;
    springs.colliders = vec![
        Collider { node: head, shape: ColliderShape::Sphere { offset: Vec3::Y * 0.05, radius: 0.03 } },
        Collider {
            node: head,
            shape: ColliderShape::Capsule { offset: Vec3::X * 0.01, radius: 0.02, tail: Vec3::Y * 0.04 },
        },
    ];
    springs.collider_groups = vec![ColliderGroup { name: "Head".into(), colliders: vec![0, 1] }];
    let original = through_vrm(&f.scene);
    let (first, again) = round_trip(&original, &def());
    // The head exports without its scale, so its colliders carry it: twice the offsets and radii.
    let exported = &again.avatar.as_ref().unwrap().spring_bones.colliders;
    let ColliderShape::Sphere { offset, radius } = exported[0].shape else { panic!("a sphere: {exported:?}") };
    assert!(offset.distance(Vec3::Y * 0.1) < 1e-6 && (radius - 0.06).abs() < 1e-6, "{offset} {radius}");
    // And importing the export again gives the same colliders, in the same place, as the first import.
    let (second, _) = to_avatar(&again, &def(), MtoonSupport::NONE).expect("imports again");
    let shapes = |a: &ImportedAvatar| a.springs.groups[0].colliders.iter().map(|c| c.shape).collect::<Vec<_>>();
    for (again, first) in shapes(&second).iter().zip(shapes(&first)) {
        let close = |x: Vec3, y: Vec3| x.distance(y) < 1e-6;
        let near = match (again, first) {
            (
                omuzaki_anim_api::ColliderShape::Sphere { offset, radius },
                omuzaki_anim_api::ColliderShape::Sphere { offset: was, radius: was_radius },
            ) => close(*offset, was) && (radius - was_radius).abs() < 1e-6,
            (
                omuzaki_anim_api::ColliderShape::Capsule { offset, radius, tail },
                omuzaki_anim_api::ColliderShape::Capsule { offset: was, radius: was_radius, tail: was_tail },
            ) => close(*offset, was) && (radius - was_radius).abs() < 1e-6 && close(*tail, was_tail),
            _ => false,
        };
        assert!(near, "{again:?} against {first:?}");
    }
}

#[test]
fn a_rigid_morphed_mesh_on_a_scaled_node_keeps_its_morphed_normals() {
    use omuzaki_core::glam::Vec3;
    use omuzaki_io_api::{MorphTarget, Node};
    let mut f = humanoid(0.0);
    f.scene.add_material(off_grid());
    add_skinned_mesh(&mut f, "Body", &["hips", "spine", "head"], 0, Some(0));
    let mesh = ribbon(&mut f.scene);
    let mut target = MorphTarget::new(vec![Vec3::Y * 0.001; 3]);
    target.normals = vec![Vec3::new(0.0, 0.3, -0.2); 3];
    f.scene.meshes[mesh.index()].primitives[0].morph_targets = vec![target];
    f.scene.meshes[mesh.index()].morph_target_names = vec!["Bend".into()];
    let mut node = Node::new("Ribbon").at(Vec3::X * 0.1);
    node.mesh = Some(mesh);
    node.transform.scale = Vec3::splat(2.0);
    let node = f.scene.add_node(node);
    f.scene.attach(f.node("head"), node);
    let original = through_vrm(&f.scene);
    let (_, again) = round_trip(&original, &def());
    let ribbon = |scene: &IoScene| {
        let mesh = scene.nodes.iter().find(|n| n.name == "Ribbon").and_then(|n| n.mesh).unwrap();
        scene.meshes[mesh.index()].primitives[0].clone()
    };
    let (was, now) = (ribbon(&original), ribbon(&again));
    for v in 0..3 {
        let morphed = |p: &omuzaki_io_api::Primitive| (p.mesh.normals[v] + p.morph_targets[0].normals[v]).normalize();
        assert!(morphed(&now).distance(morphed(&was)) < 1e-6, "the morphed normal keeps its direction at 2×");
        assert!((now.mesh.normals[v].length() - 1.0).abs() < 1e-6, "the base normal stays a unit normal");
    }
}
