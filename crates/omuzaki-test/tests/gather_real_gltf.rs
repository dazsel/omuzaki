//! Gathering the mock character through the real glTF format: `omuzaki-io-ops` gathers, `omuzaki-io-gltf`
//! writes and reads the GLB. The gather tests in `omuzaki-io-ops` can only use `omuzaki-io-mock` (implementations may not depend on each other),
//! so this app-crate test is the permanent check that the real format keeps what the gather produced.

// Test helpers may panic: a panic is a clear test failure. The casts build values from small indices.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_possible_truncation)]

use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{
    ExportCtx, ExportOptions, ImportCtx, ImportInput, ImportOptions, IoScene, VRM_LICENSE_URLS, VrmMeta, run_export,
    run_import,
};
use omuzaki_io_gltf::{GltfExporter, GltfImporter};
use omuzaki_io_mock::fixtures::authored_provenance;
use omuzaki_io_ops::gather::{GatherOptions, GatherPurpose, gather};

fn mock_built() -> BuiltCharacter {
    let def = mock_character().unwrap();
    RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

/// The gathered scene with the records the exporter insists on (provenance and the meta block).
fn gathered_scene(built: &BuiltCharacter) -> IoScene {
    let options = GatherOptions { target_height: None, purpose: GatherPurpose::Gltf };
    let mut scene = gather(built, &options).unwrap().scene;
    scene.provenance = Some(authored_provenance());
    scene.meta = VrmMeta::new("Mock", vec!["Test Author".into()], VRM_LICENSE_URLS[0]);
    scene
}

fn export_glb(scene: &IoScene) -> Vec<u8> {
    let options = ExportOptions::default();
    let outcome = run_export(&GltfExporter, scene, &ExportCtx::new(&options)).unwrap();
    outcome.files.into_iter().next().unwrap().bytes
}

fn import_glb(bytes: &[u8]) -> IoScene {
    let options = ImportOptions::default();
    run_import(&GltfImporter, &ImportInput::new(bytes), &ImportCtx::new(&options)).unwrap().scene
}

fn names(scene: &IoScene) -> Vec<&str> {
    scene.nodes.iter().map(|n| n.name.as_str()).collect()
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

/// Where the vertices of the mesh on node `node` land with every bone at its rest transform.
fn skinned_at_rest(scene: &IoScene, node: usize) -> Vec<Vec3> {
    let node = &scene.nodes[node];
    let mesh = &scene.meshes[node.mesh.unwrap().index()].primitives[0].mesh;
    let skin = &scene.skins[node.skin.unwrap().index()];
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

#[test]
fn the_mock_character_survives_a_gather_and_glb_round_trip() {
    let built = mock_built();
    let scene = gathered_scene(&built);
    let back = import_glb(&export_glb(&scene));

    assert_eq!(names(&scene), names(&back), "the same node names in the same order");
    assert_eq!(
        (scene.meshes.len(), scene.skins.len(), scene.materials.len()),
        (back.meshes.len(), back.skins.len(), back.materials.len())
    );
    assert_eq!(scene.skins[0].joints, back.skins[0].joints, "skin joints");
    assert_eq!(scene.skins[0].inverse_bind, back.skins[0].inverse_bind, "inverse bind matrices");
    assert_eq!(scene.roots, back.roots);

    // Vertex positions and joint indices of every mesh survive the format.
    for (mesh, back_mesh) in scene.meshes.iter().zip(&back.meshes) {
        let (p, b) = (&mesh.primitives[0].mesh, &back_mesh.primitives[0].mesh);
        assert_eq!(p.positions.len(), b.positions.len(), "{}", mesh.name);
        let diff = p.positions.iter().zip(&b.positions).map(|(a, b)| (*a - *b).abs().max_element()).fold(0.0, f32::max);
        assert!(diff < 1e-5, "{}: vertex positions moved by {diff}", mesh.name);
        assert_eq!(p.joints, b.joints, "{}: joint indices", mesh.name);
    }

    // Skinned at rest, every exported mesh is the built mesh; a rigid mesh stays a rigid mesh.
    for (built_mesh, ix) in built.meshes.iter().zip(built.rig.skeleton.len()..) {
        assert_eq!(
            back.nodes[ix].skin.is_some(),
            built_mesh.mesh.geometry.is_skinned(),
            "{}: skinned",
            back.nodes[ix].name
        );
        let skinned = if back.nodes[ix].skin.is_some() {
            skinned_at_rest(&back, ix)
        } else {
            back.meshes[back.nodes[ix].mesh.unwrap().index()].primitives[0].mesh.positions.clone()
        };
        let diff = skinned
            .iter()
            .zip(&built_mesh.mesh.geometry.positions)
            .map(|(a, b)| (*a - *b).abs().max_element())
            .fold(0.0, f32::max);
        assert!(diff < 1e-5, "{}: skinned at rest is {diff} from the built mesh", built_mesh.mesh.name);
    }
}

#[test]
fn exporting_the_same_gathered_scene_twice_gives_identical_bytes() {
    let built = mock_built();
    let scene = gathered_scene(&built);
    assert_eq!(export_glb(&scene), export_glb(&scene), "same scene, same bytes");
}

/// Re-exporting what was imported is not byte-identical yet: the hair mesh's hyphenated attributes (`_HAIR-INFO`,
/// `_HAIR-UV`) are written, then dropped on import, so the re-export is 11 KB smaller.
#[test]
#[ignore = "known gap: hyphenated custom attributes are dropped on import"]
fn re_exporting_the_imported_scene_gives_identical_bytes() {
    let built = mock_built();
    let first = export_glb(&gathered_scene(&built));
    assert_eq!(export_glb(&import_glb(&first)), first, "re-export of the re-imported scene is byte-identical");
}
