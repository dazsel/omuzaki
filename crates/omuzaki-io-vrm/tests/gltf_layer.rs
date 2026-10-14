//! The glTF layer under both VRM generations, tested through hand-made VRM 1.0 files: primitive modes, skin weights,
//! external data, strict mode and the scene roots.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    clippy::assert_is_empty
)]

mod common;

use std::collections::BTreeMap;

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{ImportCtx, ImportInput, ImportOptions, IoError, NodeIx, Resolver, Severity, run_import};
use omuzaki_io_vrm::Vrm1Importer;
use serde_json::json;

use common::{add_accessor, add_view, floats, glb, import, minimal1, skinned, try_import};

#[test]
fn strips_become_triangle_lists_and_lines_are_dropped_with_a_finding() {
    let (mut doc, mut bin) = minimal1();
    let positions = floats(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0]);
    let view = add_view(&mut doc, &mut bin, &positions);
    let acc = add_accessor(&mut doc, &json!({"bufferView": view, "componentType": 5126, "count": 4, "type": "VEC3"}));
    let normals = add_view(&mut doc, &mut bin, &floats(&[0.0, 0.0, 1.0].repeat(4)));
    let n = add_accessor(&mut doc, &json!({"bufferView": normals, "componentType": 5126, "count": 4, "type": "VEC3"}));
    let attributes = json!({"POSITION": acc, "NORMAL": n}); // with normals the vertices stay shared
    doc["meshes"][0]["primitives"] = json!([{"attributes": attributes, "mode": 5}, {"attributes": attributes, "mode": 1}, {"attributes": attributes, "mode": 6}]);
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    let prims = &out.scene.meshes[0].primitives;
    assert_eq!(prims.len(), 2, "the line primitive is gone");
    assert_eq!(prims[0].mesh.indices, [0, 1, 2, 2, 1, 3], "a strip alternates winding");
    assert_eq!(prims[1].mesh.indices, [0, 1, 2, 0, 2, 3], "a fan shares its first vertex");
    assert!(out.report.has_rule("gltf.primitive_dropped"));
}

#[test]
fn eight_influences_are_folded_to_the_four_strongest() {
    let u16s = |v: &[u16]| v.iter().flat_map(|j| j.to_le_bytes()).collect::<Vec<u8>>();
    let joints0 = u16s(&[0, 1, 2, 3].repeat(3));
    let joints1 = u16s(&[4, 5, 6, 7].repeat(3));
    let weights0 = floats(&[0.1, 0.1, 0.1, 0.1].repeat(3));
    let weights1 = floats(&[0.3, 0.2, 0.0, 0.0].repeat(3));
    let (doc, bin) = skinned(
        &json!({}),
        &[
            ("JOINTS_0", 3, joints0, "5123", "VEC4"),
            ("WEIGHTS_0", 3, weights0, "5126", "VEC4"),
            ("JOINTS_1", 3, joints1, "5123", "VEC4"),
            ("WEIGHTS_1", 3, weights1, "5126", "VEC4"),
        ],
    );
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    let m = &out.scene.meshes[0].primitives[0].mesh;
    assert_eq!(m.joints[0], [4, 5, 0, 1], "the 0.3 and 0.2 influences win; ties keep their order");
    let sum = 0.3 + 0.2 + 0.1 + 0.1;
    for (w, expected) in m.weights[0].iter().zip([0.3, 0.2, 0.1, 0.1]) {
        assert!((w - expected / sum).abs() < 1e-6, "weights are renormalised: {:?}", m.weights[0]);
    }
    assert!(out.report.has_rule("gltf.joints_folded"));
}

#[test]
fn eight_bit_weights_that_do_not_sum_to_one_are_renormalised() {
    let joints = [0u8, 1, 0, 0].repeat(3);
    let weights = [128u8, 128, 0, 0].repeat(3); // 2 x 128/255 = 1.0039
    let (doc, bin) =
        skinned(&json!({}), &[("JOINTS_0", 3, joints, "5121", "VEC4"), ("WEIGHTS_0", 3, weights, "5121", "VEC4")]);
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    let w = out.scene.meshes[0].primitives[0].mesh.weights[0];
    assert!((w[0] - 0.5).abs() < 1e-6 && (w[1] - 0.5).abs() < 1e-6, "{w:?}");
    assert!(!out.report.has_rule("scene.weights_not_normalised"));
}

#[test]
fn joints_without_weights_are_ignored_with_a_finding() {
    let joints = [0u8, 1, 0, 0].repeat(3);
    let (doc, bin) = skinned(&json!({}), &[("JOINTS_0", 3, joints, "5121", "VEC4")]);
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    assert!(out.scene.meshes[0].primitives[0].mesh.joints.is_empty());
    assert!(out.report.has_rule("gltf.attribute_dropped"));
}

/// Serves named byte strings, like a host reading beside the model.
struct Files(BTreeMap<String, Vec<u8>>);

impl Resolver for Files {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        self.0.get(uri).cloned().ok_or_else(|| IoError::Resource { uri: uri.to_owned(), reason: "not found".into() })
    }
}

#[test]
fn data_uris_and_sidecar_files_are_read_through_the_resolver() {
    // Move the geometry out of the GLB: into a base64 data URI, and a PNG image into a sidecar file.
    let (mut doc, bin) = minimal1();
    let positions = bin[..36].to_vec();
    let b64 = {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for chunk in positions.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |a, (i, b)| a | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..4 {
                s.push(if i <= chunk.len() { A[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
            }
        }
        s
    };
    doc["buffers"][0] = json!({"byteLength": 36, "uri": format!("data:application/octet-stream;base64,{b64}")});
    doc["images"] = json!([{"uri": "textures/albedo.png"}]);
    doc["textures"] = json!([{"source": 0}]);
    let file = glb(&doc, &[]);
    let options = ImportOptions::default();
    // Without a resolver the sidecar image can't be read.
    let denied = try_import(&Vrm1Importer, &file, &options);
    assert!(matches!(denied, Err(IoError::Resource { ref uri, .. }) if uri == "textures/albedo.png"));
    let files = Files([("textures/albedo.png".to_owned(), common::PNG.to_vec())].into());
    let ctx = ImportCtx::new(&options).with_resolver(&files);
    let out = run_import(&Vrm1Importer, &ImportInput::new(&file), &ctx).unwrap();
    assert_eq!(out.scene.meshes[0].primitives[0].mesh.positions[1], Vec3::X);
    assert_eq!(out.scene.images[0].bytes.as_slice(), common::PNG);
}

#[test]
fn strict_mode_turns_findings_into_refusals() {
    let (mut doc, bin) = minimal1();
    doc["extensions"]["VRMC_vrm"]["humanoid"]["humanBones"]["tail"] = json!({"node": 0});
    let file = glb(&doc, &bin);
    let lenient = import(&Vrm1Importer, &file);
    assert!(lenient.report.findings.iter().any(|f| f.severity == Severity::Warning));
    let strict = try_import(&Vrm1Importer, &file, &ImportOptions::default().strict());
    assert!(matches!(strict, Err(IoError::Rejected(ref r)) if r.has_rule("vrm.humanoid_unknown_bone")));
}

#[test]
fn without_a_scene_the_roots_are_the_nodes_nobody_owns() {
    let (mut doc, bin) = minimal1();
    doc.as_object_mut().unwrap().remove("scene");
    doc.as_object_mut().unwrap().remove("scenes");
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    assert_eq!(out.scene.roots, [NodeIx(0), NodeIx(15)]);
}

#[test]
fn a_matrix_with_shear_or_none_is_approximated_and_reported() {
    let (mut doc, bin) = minimal1();
    doc["nodes"][15] = json!({"name": "tri", "mesh": 0, "matrix": [1, 0, 0, 0, 1, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1]});
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    assert!(out.report.has_rule("gltf.matrix_approximated"));
    // A singular matrix has no TRS: omuzaki-gltf keeps its translation and says so, instead of refusing the file.
    doc["nodes"][15] = json!({"name": "tri", "mesh": 0, "matrix": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 1]});
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    assert!(out.report.has_rule("gltf.matrix_approximated"));
    assert_eq!(out.scene.nodes[15].transform.translation, Vec3::new(7.0, 0.0, 0.0));
}
