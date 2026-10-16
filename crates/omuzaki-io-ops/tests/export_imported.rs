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
