//! Gathering a built character into an `IoScene`, against the mock character and the mock GLB format.
//!
//! The round-trip tests print their tables; run them with `--nocapture` to see the evidence the PR quotes.

// Integration-test helpers may panic: a panic is a clear test failure. The casts build values from small indices.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::collections::BTreeSet;

use omuzaki_anim_api::{BoneDef, HumanoidBone, HumanoidPose, Skeleton, SpringSet, Transform};
use omuzaki_char_api::{
    BoneName, BuildOptions, BuildServices, BuiltCharacter, BuiltMesh, CharId, CharacterBuilder, CharacterRig, JointRef,
    MaterialName, MeshData, PartId, PartMesh, RegistryBuilder, TextureData, TextureName,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_io_api::{
    ExportCtx, HUMAN_BONES, ImportCtx, ImportInput, ImportOptions, IoScene, NodeIx, Severity, VRM_LICENSE_URLS,
    VrmMeta, VrmVersion, run_export, run_import, vrm_profile,
};
use omuzaki_io_mock::fixtures::{authored_provenance, options_without_face_channels};
use omuzaki_io_mock::{MockGlbExporter, MockGlbImporter};
use omuzaki_io_ops::convert::bones::to_file;
use omuzaki_io_ops::gather::skeleton::{bone_node, humanoid_nodes};
use omuzaki_io_ops::gather::{GatherError, GatherOptions, GatherPurpose, gather};
use omuzaki_render_api::TextureDesc;

// ---------------------------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------------------------

fn mock_built() -> BuiltCharacter {
    let def = mock_character().unwrap();
    RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

fn gltf(height: Option<f32>) -> GatherOptions {
    GatherOptions { target_height: height, purpose: GatherPurpose::Gltf }
}

fn joint(name: &str) -> JointRef {
    JointRef::Named(BoneName::new(name).unwrap())
}

/// A triangle skinned half to each of `joints[0]` and `joints[1]`, or rigid if there are none.
fn triangle(name: &str, part: &str, joints: &[JointRef], material: &str) -> BuiltMesh {
    let skinned = !joints.is_empty();
    let geometry = MeshData {
        positions: vec![Vec3::new(0.0, 0.1, 0.0), Vec3::new(0.2, 0.1, 0.0), Vec3::new(0.0, 0.9, 0.1)],
        normals: vec![Vec3::Z; 3],
        indices: vec![0, 1, 2],
        joints: if skinned { vec![[0, 1, 0, 0]; 3] } else { Vec::new() },
        weights: if skinned { vec![[0.25, 0.75, 0.0, 0.0]; 3] } else { Vec::new() },
        ..MeshData::default()
    };
    BuiltMesh {
        part: PartId::new(part).unwrap(),
        local: 0,
        mesh: PartMesh {
            name: name.to_owned(),
            geometry,
            joint_bones: joints.to_vec(),
            material: MaterialName::new(material).unwrap(),
            casts_shadow: true,
            render_order: 0,
            dynamic: false,
        },
    }
}

/// The mock character on a skeleton with all 55 humanoid bones (fingers and toes too), one metre of body per hip.
fn full_humanoid_built() -> BuiltCharacter {
    let mut built = mock_built();
    let skeleton = Skeleton::humanoid(&HumanoidBone::ALL, |bone| {
        Transform::from_translation(Vec3::new(0.01 * (bone.index() % 5) as f32, 0.05, 0.002))
    })
    .unwrap();
    built.rig = CharacterRig { skeleton, springs: SpringSet::default(), drivers: Vec::new(), bind: HumanoidPose::REST };
    built
}

/// A creature: a spine of three bones and a tail, none of them a humanoid role. One bone is called `head`, which is a
/// humanoid bone's name but means nothing here.
fn creature_built() -> BuiltCharacter {
    let bone = |name: &str, parent: Option<u16>, y: f32| BoneDef {
        name: name.to_owned(),
        parent: parent.map(omuzaki_anim_api::BoneId),
        rest: Transform::from_translation(Vec3::new(0.0, y, 0.0)),
        humanoid: None,
    };
    let skeleton = Skeleton::generic(vec![
        bone("body", None, 0.3),
        bone("neck", Some(0), 0.2),
        bone("head", Some(1), 0.2),
        bone("tail", Some(0), -0.1),
    ])
    .unwrap();
    let mut built = mock_built();
    built.rig = CharacterRig { skeleton, springs: SpringSet::default(), drivers: Vec::new(), bind: HumanoidPose::REST };
    built.meshes = vec![
        triangle("fur", "body", &[joint("body"), joint("head")], "mock-body-skin"),
        triangle("tail", "tail", &[joint("tail"), joint("body")], "mock-body-skin"),
    ];
    built
}

fn names(scene: &IoScene) -> Vec<&str> {
    scene.nodes.iter().map(|n| n.name.as_str()).collect()
}

fn max_diff(a: &[Vec3], b: &[Vec3]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(a, b)| (*a - *b).abs().max_element()).fold(0.0, f32::max)
}

/// The world matrix of node `i`: its own transform under its parent's.
fn world(scene: &IoScene, parent: &[Option<usize>], i: usize) -> Mat4 {
    let t = &scene.nodes[i].transform;
    let local = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
    parent[i].map_or(local, |p| world(scene, parent, p) * local)
}

/// The world matrix of every node, from the scene's own transforms.
fn world_matrices(scene: &IoScene) -> Vec<Mat4> {
    let mut parent = vec![None; scene.nodes.len()];
    for (i, node) in scene.nodes.iter().enumerate() {
        for child in &node.children {
            parent[child.index()] = Some(i);
        }
    }
    (0..scene.nodes.len()).map(|i| world(scene, &parent, i)).collect()
}

/// Where the vertices of the mesh on `node` end up with every bone at its rest transform: what a viewer shows before
/// any animation. Rigid meshes are not moved.
fn skinned_at_rest(scene: &IoScene, node: NodeIx) -> Vec<Vec3> {
    let node = &scene.nodes[node.index()];
    let mesh = &scene.meshes[node.mesh.unwrap().index()].primitives[0].mesh;
    let Some(skin) = node.skin.map(|s| &scene.skins[s.index()]) else { return mesh.positions.clone() };
    let world = world_matrices(scene);
    let matrices: Vec<Mat4> =
        skin.joints.iter().zip(&skin.inverse_bind).map(|(joint, ibm)| world[joint.index()] * *ibm).collect();
    (0..mesh.positions.len())
        .map(|v| {
            let mut sum = Vec3::ZERO;
            for (joint, weight) in mesh.joints[v].iter().zip(mesh.weights[v]) {
                sum += matrices[usize::from(*joint)].transform_point3(mesh.positions[v]) * weight;
            }
            sum
        })
        .collect()
}

/// The scene as an exporter needs it: the geometry half plus the records `run_export` insists on.
fn exportable(mut scene: IoScene) -> IoScene {
    scene.provenance = Some(authored_provenance());
    scene.meta = VrmMeta::new("Mock", vec!["Test Author".into()], VRM_LICENSE_URLS[0]);
    scene
}

fn export_glb(scene: &IoScene) -> Vec<u8> {
    let options = options_without_face_channels();
    let outcome = run_export(&MockGlbExporter, scene, &ExportCtx::new(&options)).unwrap();
    outcome.files.into_iter().next().unwrap().bytes
}

fn import_glb(bytes: &[u8]) -> IoScene {
    let options = ImportOptions::default();
    run_import(&MockGlbImporter, &ImportInput::new(bytes), &ImportCtx::new(&options)).unwrap().scene
}

fn errors(scene: &IoScene) -> Vec<String> {
    scene.check().findings.iter().filter(|f| f.severity == Severity::Error).map(|f| f.message.clone()).collect()
}

fn rules(report: &omuzaki_io_api::Report) -> Vec<&str> {
    report.findings.iter().map(|f| f.rule.as_str()).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// The mock character
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn the_mock_character_gathers_into_a_valid_scene() {
    let built = mock_built();
    let gathered = gather(&built, &gltf(None)).unwrap();
    let scene = &gathered.scene;
    assert_eq!(errors(scene), Vec::<String>::new());
    assert_eq!(gathered.scale, 1.0);

    // Bone i is node i, parents first, and the hierarchy is the skeleton's.
    let bones = built.rig.skeleton.bones();
    assert_eq!(scene.nodes.len(), bones.len() + built.meshes.len());
    for (i, bone) in bones.iter().enumerate() {
        assert_eq!(scene.nodes[i].name, bone.name);
        assert_eq!(
            scene.parent_of(bone_node(omuzaki_anim_api::BoneId(i as u16))).map(NodeIx::index),
            bone.parent.map(omuzaki_anim_api::BoneId::index)
        );
        assert_eq!(scene.nodes[i].transform.translation, bone.rest.translation);
    }
    // One mesh and one primitive per built mesh, each on its own root node, in build order.
    assert_eq!((scene.meshes.len(), scene.roots.len()), (5, 1 + 5));
    for (built_mesh, ix) in built.meshes.iter().zip(bones.len()..) {
        let node = &scene.nodes[ix];
        assert_eq!(node.name, format!("{}.{}", built_mesh.part, built_mesh.mesh.name));
        let mesh = &scene.meshes[node.mesh.unwrap().index()];
        assert_eq!((mesh.name.as_str(), mesh.primitives.len()), (node.name.as_str(), 1));
        assert_eq!(node.skin.is_some(), built_mesh.mesh.geometry.is_skinned());
        assert_eq!(mesh.primitives[0].mesh.indices, built_mesh.mesh.geometry.indices);
    }
    // One skin: every bone is a joint, in order, and the matrices are the skeleton's.
    assert_eq!(scene.skins.len(), 1);
    let skin = &scene.skins[0];
    assert_eq!(skin.joints, (0..bones.len() as u32).map(NodeIx).collect::<Vec<_>>());
    assert_eq!(skin.inverse_bind, built.rig.skeleton.inverse_bind());
    assert_eq!(built.rig.inverse_bind(), built.rig.skeleton.inverse_bind(), "an identity bind is the rest");
    assert_eq!(skin.skeleton, Some(NodeIx(0)));
    // Only used materials, one per name.
    assert_eq!(scene.materials.len(), built.materials.len());
    let unique: BTreeSet<_> = names(scene).into_iter().collect();
    assert_eq!(unique.len(), scene.nodes.len(), "node names are unique");
}

/// A humanoid resting in the T (the forearm and hand level along +X on the left) and bound with the left upper arm
/// swung 1 radian down about Z: a character sculpted in an A-pose.
fn a_bound_built() -> BuiltCharacter {
    let skeleton = Skeleton::humanoid(&HumanoidBone::ALL, |bone| {
        Transform::from_translation(match bone {
            HumanoidBone::LeftLowerArm | HumanoidBone::LeftHand => Vec3::new(0.25, 0.0, 0.0),
            HumanoidBone::RightLowerArm | HumanoidBone::RightHand => Vec3::new(-0.25, 0.0, 0.0),
            _ => Vec3::new(0.0, 0.1, 0.0),
        })
    })
    .unwrap();
    let mut bind = HumanoidPose::REST;
    bind.set(HumanoidBone::LeftUpperArm, omuzaki_core::glam::Quat::from_rotation_z(-1.0));
    let mut built = mock_built();
    built.rig = CharacterRig { skeleton, springs: SpringSet::default(), drivers: Vec::new(), bind };
    built
}

#[test]
fn the_skin_takes_its_inverse_bind_matrices_from_the_rigs_bind_pose_scaled_with_the_scene() {
    let built = a_bound_built();
    let rig = &built.rig;
    assert_ne!(rig.inverse_bind(), rig.skeleton.inverse_bind(), "the bind differs from the rest");
    for height in [None, Some(2.0_f32)] {
        let gathered = gather(&built, &gltf(height)).unwrap();
        let scale = gathered.scale;
        let expected: Vec<Mat4> = rig
            .inverse_bind()
            .into_iter()
            .map(|mut m| {
                m.w_axis = (m.w_axis.truncate() * scale).extend(1.0);
                m
            })
            .collect();
        assert_eq!(gathered.scene.skins[0].inverse_bind, expected, "height {height:?}");
    }

    // A vertex at the bound forearm joint, skinned at the exported nodes' rest, lands level with the T-pose upper arm.
    let scene = gather(&built, &gltf(None)).unwrap().scene;
    let id = |bone| rig.skeleton.humanoid_bone(bone).unwrap().index();
    let worlds = world_matrices(&scene);
    let (upper, lower) = (id(HumanoidBone::LeftUpperArm), id(HumanoidBone::LeftLowerArm));
    let skin = &scene.skins[0];
    let elbow_at_bind = skin.inverse_bind[lower].inverse().transform_point3(Vec3::ZERO);
    let at_rest = (worlds[lower] * skin.inverse_bind[lower]).transform_point3(elbow_at_bind);
    let shoulder = worlds[upper].transform_point3(Vec3::ZERO);
    assert!((at_rest.y - shoulder.y).abs() < 1e-5, "level: {shoulder} {at_rest}");
    assert!(elbow_at_bind.y < shoulder.y - 0.05, "and it was hanging at the bind: {elbow_at_bind}");
}

#[test]
fn gathering_is_deterministic() {
    let built = mock_built();
    let options = GatherOptions { target_height: Some(1.2), purpose: GatherPurpose::Vrm };
    let (a, b) = (gather(&built, &options).unwrap(), gather(&built, &options).unwrap());
    assert_eq!(a, b);
    assert_eq!(export_glb(&exportable(a.scene)), export_glb(&exportable(b.scene)), "same scene, same bytes");
}

#[test]
fn a_round_trip_through_the_mock_glb_keeps_the_structure_and_the_rest_pose() {
    let built = mock_built();
    let scene = exportable(gather(&built, &gltf(None)).unwrap().scene);
    let bytes = export_glb(&scene);
    let back = import_glb(&bytes);

    println!("\n| what | gathered | re-imported |\n|---|---|---|");
    println!("| nodes | {} | {} |", scene.nodes.len(), back.nodes.len());
    println!("| meshes | {} | {} |", scene.meshes.len(), back.meshes.len());
    println!("| skins | {} | {} |", scene.skins.len(), back.skins.len());
    println!("| skin joints | {} | {} |", scene.skins[0].joints.len(), back.skins[0].joints.len());
    println!("| materials | {} | {} |", scene.materials.len(), back.materials.len());
    println!("| GLB bytes | | {} |", bytes.len());

    assert_eq!(names(&scene), names(&back), "the same node names in the same order");
    assert_eq!(
        (scene.meshes.len(), scene.skins.len(), scene.materials.len()),
        (back.meshes.len(), 1, back.materials.len())
    );
    assert_eq!(scene.skins[0].joints, back.skins[0].joints);
    assert_eq!(scene.skins[0].inverse_bind, back.skins[0].inverse_bind);
    assert_eq!(scene.roots, back.roots);

    // Skinning the exported mesh at rest gives back the built mesh.
    println!("\n| mesh | vertices | max distance to the built mesh |\n|---|---|---|");
    for (built_mesh, ix) in built.meshes.iter().zip(built.rig.skeleton.len()..) {
        let skinned = skinned_at_rest(&back, NodeIx(ix as u32));
        let diff = max_diff(&skinned, &built_mesh.mesh.geometry.positions);
        println!("| {} | {} | {diff:e} |", back.nodes[ix].name, skinned.len());
        assert!(diff < 1e-5, "{}: {diff}", back.nodes[ix].name);
    }
}

#[test]
fn the_scale_scales_every_vertex_and_joint_by_the_same_factor() {
    let built = mock_built();
    let base = gather(&built, &gltf(None)).unwrap();
    let mut rng = Rng::new(0x5ca1e);
    println!(
        "\n| target height | scale | max vertex error | max joint error | skinned at rest error |\n|---|---|---|---|---|"
    );
    for _ in 0..24 {
        let height = rng.range_f32(0.2, 4.0);
        let scaled = gather(&built, &gltf(Some(height))).unwrap();
        let s = scaled.scale;

        // The target is met: the meshes span `height` along Y.
        let ys: Vec<f32> =
            scaled.scene.meshes.iter().flat_map(|m| m.primitives[0].mesh.positions.iter().map(|p| p.y)).collect();
        let span = ys.iter().copied().fold(f32::MIN, f32::max) - ys.iter().copied().fold(f32::MAX, f32::min);
        assert!((span - height).abs() < 1e-4 * height, "height {height}: spans {span}");

        let (mut vertex_error, mut joint_error, mut rest_error) = (0.0f32, 0.0f32, 0.0f32);
        for (a, b) in base.scene.meshes.iter().zip(&scaled.scene.meshes) {
            let (a, b) = (&a.primitives[0].mesh, &b.primitives[0].mesh);
            vertex_error =
                vertex_error.max(max_diff(&a.positions.iter().map(|p| *p * s).collect::<Vec<_>>(), &b.positions));
            assert_eq!(
                (&a.joints, &a.weights, &a.normals, &a.indices),
                (&b.joints, &b.weights, &b.normals, &b.indices)
            );
        }
        for (a, b) in base.scene.nodes.iter().zip(&scaled.scene.nodes) {
            joint_error = joint_error.max((a.transform.translation * s - b.transform.translation).abs().max_element());
            assert_eq!((a.transform.rotation, b.transform.scale), (b.transform.rotation, Vec3::ONE));
        }
        for (a, b) in base.scene.skins[0].inverse_bind.iter().zip(&scaled.scene.skins[0].inverse_bind) {
            let mut expected = *a;
            expected.w_axis = (a.w_axis.truncate() * s).extend(1.0);
            joint_error = joint_error.max((expected - *b).abs().to_cols_array().into_iter().fold(0.0, f32::max));
        }
        for (built_mesh, ix) in built.meshes.iter().zip(built.rig.skeleton.len()..) {
            let skinned = skinned_at_rest(&scaled.scene, NodeIx(ix as u32));
            let expected: Vec<Vec3> = built_mesh.mesh.geometry.positions.iter().map(|p| *p * s).collect();
            rest_error = rest_error.max(max_diff(&skinned, &expected));
        }
        println!("| {height:.3} | {s:.4} | {vertex_error:e} | {joint_error:e} | {rest_error:e} |");
        assert!(vertex_error < 1e-6 && joint_error < 1e-5 && rest_error < 1e-5);
    }
}

#[test]
fn a_bad_target_height_is_refused() {
    let built = mock_built();
    for bad in [0.0, -1.7, f32::NAN, f32::INFINITY] {
        assert!(matches!(gather(&built, &gltf(Some(bad))), Err(GatherError::BadHeight(_))), "{bad}");
    }
    let mut empty = mock_built();
    empty.meshes.clear();
    assert!(matches!(gather(&empty, &gltf(Some(1.6))), Err(GatherError::NoHeight(_))));
    assert_eq!(gather(&empty, &gltf(None)).unwrap().scene.meshes.len(), 0, "nothing to scale is fine when not scaling");
}

#[test]
fn the_report_says_what_was_scaled_and_what_stays_at_rest() {
    let built = mock_built();
    let gathered = gather(&built, &gltf(Some(1.5))).unwrap();
    let rules = rules(&gathered.report);
    assert!(rules.contains(&"gather.scaled"), "{rules:?}");
    // The mock's hair is simulated at runtime and has no bake meshes: it goes out at its rest shape.
    assert!(rules.contains(&"gather.dynamic_at_rest"), "{rules:?}");
    // The head and hair composite face layers; the face baker paints those into the base texture later.
    assert!(rules.contains(&"material.not_exported"), "{rules:?}");
    assert!(gather(&built, &gltf(None)).unwrap().report.findings.iter().all(|f| f.rule != "gather.scaled"));
}

// ---------------------------------------------------------------------------------------------------------------
// Humanoid and generic skeletons
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn all_55_humanoid_bones_including_the_30_fingers_map_to_nodes() {
    let built = full_humanoid_built();
    let gathered = gather(&built, &GatherOptions { target_height: None, purpose: GatherPurpose::Vrm }).unwrap();
    let scene = gathered.scene;
    let table = humanoid_nodes(&built.rig.skeleton).unwrap();
    assert_eq!(table.len(), 55);
    let fingers = table
        .iter()
        .filter(|(bone, _)| ["Thumb", "Index", "Middle", "Ring", "Little"].iter().any(|f| bone.as_str().contains(f)))
        .count();
    assert_eq!(fingers, 30, "RG-09: five fingers of three bones on both hands");

    for (row, (bone, node)) in HUMAN_BONES.iter().zip(&table) {
        assert_eq!(bone.as_str(), row.name, "table order");
        assert_eq!(scene.nodes[node.index()].name, row.name, "the node is named after its bone");
        let parent = scene.parent_of(*node).map(|p| scene.nodes[p.index()].name.as_str());
        assert_eq!(parent, row.parent, "{} hangs under the bone VRM prescribes", row.name);
    }
    // The independent check: a scene whose humanoid map is this table passes the VRM 1.0 profile.
    let mut scene = exportable(scene);
    let mut avatar = omuzaki_io_api::Avatar::new();
    avatar.humanoid.bones.extend(table);
    scene.avatar = Some(avatar);
    let profile = vrm_profile(&scene, VrmVersion::V1);
    assert!(!profile.has_errors(), "{profile:?}");
    assert_eq!(errors(&scene), Vec::<String>::new());
}

#[test]
fn every_humanoid_bone_converts_to_a_file_name_that_names_its_node() {
    let built = full_humanoid_built();
    let scene = gather(&built, &gltf(None)).unwrap().scene;
    for bone in HumanoidBone::ALL {
        let id = built.rig.skeleton.humanoid_bone(bone).unwrap();
        assert_eq!(scene.nodes[bone_node(id).index()].name, to_file(bone).unwrap().as_str());
    }
}

#[test]
fn a_creature_gathers_for_gltf_but_is_not_a_vrm() {
    let built = creature_built();
    let gathered = gather(&built, &gltf(None)).unwrap();
    let scene = &gathered.scene;
    assert_eq!(errors(scene), Vec::<String>::new());
    assert_eq!(&names(scene)[..4], ["body", "neck", "head", "tail"], "a creature's `head` is just a name");
    assert_eq!(scene.skins[0].joints.len(), 4);

    // The mesh was skinned to [body, head] in its own list; the skin indexes the skeleton.
    let fur = &scene.meshes[0].primitives[0].mesh;
    assert!(fur.joints.iter().all(|j| *j == [0, 2, 0, 0]), "{:?}", fur.joints[0]);
    let tail = &scene.meshes[1].primitives[0].mesh;
    assert!(tail.joints.iter().all(|j| *j == [3, 0, 0, 0]), "tail then body");

    // It survives a round trip, and skins to itself at rest.
    let back = import_glb(&export_glb(&exportable(scene.clone())));
    for (ix, mesh) in [(4, &built.meshes[0]), (5, &built.meshes[1])] {
        assert!(max_diff(&skinned_at_rest(&back, NodeIx(ix)), &mesh.mesh.geometry.positions) < 1e-5);
    }

    let vrm = GatherOptions { target_height: None, purpose: GatherPurpose::Vrm };
    let err = gather(&built, &vrm).unwrap_err();
    assert!(matches!(err, GatherError::NotHumanoid(_)), "{err}");
    assert!(err.to_string().contains("generic"), "{err}");
    assert!(matches!(humanoid_nodes(&built.rig.skeleton), Err(GatherError::NotHumanoid(_))));
}

#[test]
fn an_extra_bone_cannot_be_called_like_a_humanoid_one() {
    let mut built = mock_built();
    let mut bones = built.rig.skeleton.bones().to_vec();
    // `jaw` is optional and absent from the mock's skeleton, so nothing stops a part from adding an extra bone by that name.
    bones.push(BoneDef {
        name: "jaw".to_owned(),
        parent: Some(omuzaki_anim_api::BoneId(4)),
        rest: Transform::IDENTITY,
        humanoid: None,
    });
    built.rig.skeleton = Skeleton::new(bones).unwrap();
    assert!(matches!(gather(&built, &gltf(None)), Err(GatherError::ReservedBoneName(name)) if name == "jaw"));
}

// ---------------------------------------------------------------------------------------------------------------
// Meshes, materials and textures
// ---------------------------------------------------------------------------------------------------------------

#[test]
fn mesh_names_are_unique_and_never_read_as_bones() {
    let mut built = creature_built();
    // Two meshes both called `fur` in one part, and one called like a bone.
    built.meshes = vec![
        triangle("fur", "body", &[joint("body"), joint("head")], "mock-body-skin"),
        triangle("fur", "body", &[joint("body"), joint("head")], "mock-body-skin"),
        triangle("head", "head", &[], "mock-body-skin"),
    ];
    let scene = gather(&built, &gltf(None)).unwrap().scene;
    assert_eq!(&names(&scene)[4..], ["body.fur", "body.fur.2", "head.head"]);
    let unique: BTreeSet<_> = scene.meshes.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(unique.len(), 3);
    assert_eq!(scene.nodes.iter().filter(|n| n.name == "head").count(), 1);
    // The rigid mesh has no skin and stays where it was built.
    assert_eq!(scene.nodes[6].skin, None);
    assert_eq!(skinned_at_rest(&scene, NodeIx(6)), built.meshes[2].mesh.geometry.positions);
}

#[test]
fn only_the_materials_and_textures_a_mesh_uses_are_exported() {
    let mut built = creature_built();
    let weave = TextureName::new("weave").unwrap();
    let pixels = |seed: u8| TextureData {
        desc: TextureDesc::colour(UVec2::new(4, 2)),
        rgba8: (0..32).map(|i| seed.wrapping_mul(31).wrapping_add(i * 7)).collect(),
    };
    built.textures.clear();
    built.textures.insert(weave.clone(), pixels(1));
    built.textures.insert(TextureName::new("unused").unwrap(), pixels(2));
    built.materials.get_mut(&MaterialName::new("mock-body-skin").unwrap()).unwrap().base_texture = Some(weave.clone());
    let scene = gather(&built, &gltf(None)).unwrap().scene;

    assert_eq!(scene.materials.len(), 1, "the other four materials have no mesh");
    assert_eq!(scene.materials[0].name, "mock-body-skin");
    assert_eq!((scene.textures.len(), scene.images.len()), (1, 1));
    assert_eq!(scene.images[0].name, "weave");
    assert!(scene.materials[0].base_color_texture.is_some());
    assert_eq!(errors(&scene), Vec::<String>::new());
    let decoded = image::load_from_memory(scene.images[0].bytes.as_slice()).unwrap().to_rgba8();
    assert_eq!(decoded.into_raw(), built.textures[&weave].rgba8, "the pixels survive byte for byte");
}

#[test]
fn a_texture_the_character_lacks_names_the_material_that_asked() {
    let mut built = creature_built();
    built.materials.get_mut(&MaterialName::new("mock-body-skin").unwrap()).unwrap().base_texture =
        Some(TextureName::new("ghost").unwrap());
    let err = gather(&built, &gltf(None)).unwrap_err();
    assert!(matches!(err, GatherError::MissingTexture(_)), "{err}");
    assert!(err.to_string().contains("mock-body-skin") && err.to_string().contains("ghost"), "{err}");
}

#[test]
fn meshes_that_cannot_be_gathered_are_refused_with_their_name() {
    let mut built = creature_built();
    built.meshes[0].mesh.joint_bones[1] = joint("wing");
    let err = gather(&built, &gltf(None)).unwrap_err();
    assert_eq!(err, GatherError::MissingBone { mesh: "fur".into(), joint: "wing".into() });

    let mut built = creature_built();
    built.meshes[0].mesh.geometry.joints[1] = [0, 7, 0, 0];
    assert_eq!(
        gather(&built, &gltf(None)).unwrap_err(),
        GatherError::JointOutOfRange { mesh: "fur".into(), index: 7, len: 2 }
    );

    let mut built = creature_built();
    built.meshes[1].mesh.material = MaterialName::new("nothing").unwrap();
    assert!(matches!(gather(&built, &gltf(None)), Err(GatherError::UnknownMaterial { mesh, .. }) if mesh == "tail"));
}

#[test]
fn unused_joint_slots_point_at_joint_zero() {
    let mut built = creature_built();
    // A slot with no weight may name anything in the mesh's list, even a joint outside the list.
    built.meshes[0].mesh.geometry.joints = vec![[1, 9, 9, 9]; 3];
    built.meshes[0].mesh.geometry.weights = vec![[1.0, 0.0, 0.0, 0.0]; 3];
    let scene = gather(&built, &gltf(None)).unwrap().scene;
    assert!(scene.meshes[0].primitives[0].mesh.joints.iter().all(|j| *j == [2, 0, 0, 0]));
    assert_eq!(errors(&scene), Vec::<String>::new());
}

#[test]
fn an_empty_mesh_is_left_out_and_said_so() {
    let mut built = creature_built();
    built.meshes.push(triangle("whiskers", "body", &[], "mock-body-skin"));
    built.meshes[2].mesh.geometry.indices.clear();
    let gathered = gather(&built, &gltf(None)).unwrap();
    assert_eq!(gathered.scene.meshes.len(), 2);
    let gather_rules: Vec<_> = rules(&gathered.report).into_iter().filter(|r| r.starts_with("gather.")).collect();
    assert_eq!(gather_rules, ["gather.mesh_empty"]);
    assert_eq!(errors(&gathered.scene), Vec::<String>::new());
}

#[test]
fn a_character_with_only_rigid_meshes_has_no_skin() {
    let mut built = creature_built();
    built.meshes = vec![triangle("prop", "body", &[], "mock-body-skin")];
    let scene = gather(&built, &gltf(None)).unwrap().scene;
    assert!(scene.skins.is_empty() && scene.nodes[4].skin.is_none());
    assert_eq!(errors(&scene), Vec::<String>::new());
}

#[test]
fn a_skeleton_with_two_roots_leaves_the_skin_without_a_skeleton_node() {
    let mut built = creature_built();
    let bone = |name: &str, parent: Option<u16>| BoneDef {
        name: name.to_owned(),
        parent: parent.map(omuzaki_anim_api::BoneId),
        rest: Transform::from_translation(Vec3::new(0.1, 0.2, 0.0)),
        humanoid: None,
    };
    built.rig.skeleton =
        Skeleton::generic(vec![bone("body", None), bone("neck", Some(0)), bone("head", Some(1)), bone("tail", None)])
            .unwrap();
    let scene = gather(&built, &gltf(None)).unwrap().scene;
    // Two bone roots, then the two mesh nodes; the glTF `skeleton` hint names one root, which would be wrong here.
    assert_eq!(scene.roots, [NodeIx(0), NodeIx(3), NodeIx(4), NodeIx(5)]);
    assert_eq!(scene.skins[0].skeleton, None);
    assert_eq!(errors(&scene), Vec::<String>::new());
}

#[test]
fn a_character_built_at_real_size_is_gathered_as_it_is_without_a_second_scale() {
    // The stature is a record of the scale the build already applied: the gather must not apply it again.
    let plain = mock_built();
    let mut sized = mock_built();
    sized.stature = Some(omuzaki_char_api::Stature { scale: 0.75, height_m: 1.58 });
    let (a, b) = (gather(&plain, &gltf(None)).unwrap(), gather(&sized, &gltf(None)).unwrap());
    assert_eq!(b.scale, 1.0, "no target height, no scale");
    assert_eq!(a.scene, b.scene, "the same scene whether or not the build recorded a stature");
}
