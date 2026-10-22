//! VRM 1.0 end to end: generated avatars round trip exactly, the file has the spec's shape, files written by other
//! tools (assembled here by hand, independently of the exporter) import correctly, and messy files import with
//! findings instead of failing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::too_many_lines,
    clippy::assert_is_empty,
    clippy::many_single_char_names
)]

mod common;

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    Confidence, ExpressionName, ImportInput, ImportOptions, Importer, IoError, IoScene, Severity, Shading,
};
use omuzaki_io_vrm::{Vrm1Exporter, Vrm1Importer};
use serde_json::{Value, json};

use common::{avatar, export, glb, import, minimal1, parts, try_import};

/// Compares everything the file carries, one field at a time so a failure names the part that differs. Provenance is
/// excluded: it isn't in the file, the importer derives it.
fn assert_same(a: &IoScene, b: &IoScene) {
    assert_eq!(a.nodes, b.nodes, "nodes");
    assert_eq!(a.roots, b.roots, "roots");
    assert_eq!(a.meshes, b.meshes, "meshes");
    assert_eq!(a.skins, b.skins, "skins");
    assert_eq!(a.images, b.images, "images");
    assert_eq!(a.textures, b.textures, "textures");
    assert_eq!(a.materials, b.materials, "materials");
    assert_eq!(a.meta, b.meta, "meta");
    assert_eq!(a.avatar, b.avatar, "avatar");
    assert_eq!(a.passthrough, b.passthrough, "root passthrough");
}

#[test]
fn a_rich_avatar_round_trips_exactly() {
    let (scene, _) = avatar();
    let file = export(&Vrm1Exporter, &scene);
    let out = import(&Vrm1Importer, &file);
    assert_same(&out.scene, &scene);
    assert_eq!(out.report.count(Severity::Error), 0);
    assert_eq!(out.report.count(Severity::Warning), 0, "{:#?}", out.report);
    assert!(out.report.has_rule("gltf.passthrough"), "carried data is announced");
}

#[test]
fn exports_are_deterministic_and_canonical() {
    let (scene, _) = avatar();
    let a = export(&Vrm1Exporter, &scene);
    assert_eq!(a, export(&Vrm1Exporter, &scene), "same scene, same bytes");
    let again = import(&Vrm1Importer, &a).scene;
    assert_eq!(a, export(&Vrm1Exporter, &again), "export(import(export(x))) == export(x)");
}

#[test]
fn the_file_has_the_spec_shape() {
    let (scene, ids) = avatar();
    let file = export(&Vrm1Exporter, &scene);
    // The GLB header, checked by hand rather than through the crate's own reader.
    assert_eq!(&file[0..4], b"glTF");
    assert_eq!(u32::from_le_bytes(file[4..8].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(file[8..12].try_into().unwrap()) as usize, file.len());
    assert_eq!(file.len() % 4, 0);
    let (doc, bin) = parts(&file);
    assert!(!bin.is_empty());
    assert_eq!(doc["asset"]["version"], "2.0");
    assert_eq!(
        doc["extensionsUsed"],
        json!([
            "EXT_fixture_material",
            "EXT_fixture_node",
            "EXT_fixture_root",
            "KHR_materials_emissive_strength",
            "KHR_materials_unlit",
            "KHR_texture_transform",
            "VRMC_materials_mtoon",
            "VRMC_node_constraint",
            "VRMC_springBone",
            "VRMC_vrm"
        ]),
        "sorted, and listing every extension that appears"
    );
    assert!(doc.get("extensionsRequired").is_none());

    let vrm = &doc["extensions"]["VRMC_vrm"];
    assert_eq!(vrm["specVersion"], "1.0");
    assert_eq!(vrm["meta"]["licenseUrl"], "https://vrm.dev/licenses/1.0/");
    assert_eq!(vrm["meta"]["commercialUsage"], "corporation");
    assert_eq!(vrm["meta"]["allowRedistribution"], true);
    assert_eq!(vrm["meta"]["thumbnailImage"], 2);
    assert_eq!(vrm["humanoid"]["humanBones"].as_object().unwrap().len(), 28);
    assert_eq!(vrm["humanoid"]["humanBones"]["head"]["node"], ids.node("head").0);
    let happy = &vrm["expressions"]["preset"]["happy"];
    assert_eq!(happy["overrideBlink"], "block");
    assert_eq!(happy["overrideLookAt"], "blend");
    assert_eq!(happy["morphTargetBinds"][0], json!({"node": ids.body.0, "index": 3, "weight": 0.5}));
    assert_eq!(happy["materialColorBinds"][0]["type"], "color");
    assert_eq!(happy["textureTransformBinds"][0]["scale"], json!([0.5, 0.5]));
    assert!(vrm["expressions"]["preset"]["neutral"].get("morphTargetBinds").is_none(), "empty bind arrays are omitted");
    assert_eq!(vrm["expressions"]["custom"]["jawOpen"]["morphTargetBinds"][0]["index"], 0);
    assert_eq!(vrm["lookAt"]["type"], "expression");
    assert_eq!(vrm["lookAt"]["rangeMapHorizontalInner"], json!({"inputMaxValue": 80.0, "outputScale": 0.75}));
    assert_eq!(vrm["firstPerson"]["meshAnnotations"][0]["type"], "thirdPersonOnly");

    let spring = &doc["extensions"]["VRMC_springBone"];
    assert_eq!(spring["colliders"][1]["shape"]["capsule"]["radius"], 0.05);
    assert_eq!(spring["colliderGroups"][0]["colliders"], json!([0, 1]));
    assert_eq!(spring["springs"][0]["joints"].as_array().unwrap().len(), 3);
    assert_eq!(spring["springs"][0]["joints"][0]["gravityDir"], json!([0.0, -1.0, 0.0]), "defaults are written");
    assert_eq!(spring["springs"][0]["center"], ids.node("head").0);
    assert!(spring["springs"][1].get("center").is_none());

    let node = |name: &str| &doc["nodes"][ids.node(name).index()];
    assert_eq!(node("leftTwist")["extensions"]["VRMC_node_constraint"]["constraint"]["roll"]["rollAxis"], "X");
    assert_eq!(node("leftSleeve")["extensions"]["VRMC_node_constraint"]["constraint"]["aim"]["aimAxis"], "PositiveY");
    assert_eq!(node("rightSub")["extensions"]["VRMC_node_constraint"]["constraint"]["rotation"]["weight"], 0.25);

    let mtoon = &doc["materials"][0]["extensions"]["VRMC_materials_mtoon"];
    assert_eq!(mtoon["specVersion"], "1.0");
    assert_eq!(mtoon["outlineWidthMode"], "worldCoordinates");
    assert_eq!(mtoon["shadingShiftTexture"]["scale"], 0.5);
    assert_eq!(mtoon["shadeMultiplyTexture"]["extensions"]["KHR_texture_transform"]["scale"], json!([2.0, 0.5]));
    assert!(doc["materials"][0]["extensions"].get("KHR_materials_unlit").is_some(), "the fallback for plain viewers");
    assert_eq!(doc["materials"][1]["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"], 2.0);
    assert_eq!(doc["materials"][1]["emissiveFactor"], json!([1.0, 0.5, 0.0]));
    assert_eq!(doc["materials"][1]["alphaMode"], "MASK");
    assert_eq!(doc["materials"][2]["extensions"]["VRMC_materials_mtoon"]["renderQueueOffsetNumber"], 3);

    // Morph targets: names on the mesh; POSITION accessors carry min and max, sparse ones included; the zero target
    // needs no data; the dense one is stored densely.
    assert_eq!(doc["meshes"][0]["extras"]["targetNames"], json!(["jawOpen", "blink", "empty", "smile"]));
    let prim = &doc["meshes"][0]["primitives"][0];
    let target = |i: usize| &doc["accessors"][prim["targets"][i]["POSITION"].as_u64().unwrap() as usize];
    assert!(target(0).get("sparse").is_none() && target(0).get("bufferView").is_some());
    assert!(target(1).get("sparse").is_some() && target(1).get("bufferView").is_none());
    assert!(target(2).get("sparse").is_none() && target(2).get("bufferView").is_none());
    for i in 0..4 {
        assert!(target(i).get("min").is_some() && target(i).get("max").is_some(), "target {i} needs bounds");
    }
    assert_eq!(doc["accessors"][prim["attributes"]["POSITION"].as_u64().unwrap() as usize]["type"], "VEC3");
    // 16 vertices fit 16-bit indices.
    assert_eq!(doc["accessors"][prim["indices"].as_u64().unwrap() as usize]["componentType"], 5123);
}

#[test]
fn toon_and_unlit_materials_keep_every_field() {
    let (scene, _) = avatar();
    let back = import(&Vrm1Importer, &export(&Vrm1Exporter, &scene)).scene;
    for (i, original) in scene.materials.iter().enumerate() {
        assert_eq!(back.materials[i].shading, original.shading, "material {i} ({})", original.name);
    }
    let Shading::Toon(t) = &back.materials[0].shading else { panic!("skin is a toon material") };
    assert_eq!(t.outline_width, 0.0025);
    assert!(t.matcap_texture.is_some() && t.shading_shift_texture.is_some());
    assert!(matches!(back.materials[4].shading, Shading::Unlit));
}

#[test]
fn big_meshes_use_32_bit_indices_and_sparse_morphs_round_trip() {
    let (mut scene, ids) = avatar();
    // 301 x 301 vertices = 90 601: past 16-bit indices, and past the 16-bit sparse index range for the morph target.
    let mut m = common::grid(300, 300, 6);
    m.colours.clear();
    let n = m.positions.len();
    let mut target = vec![Vec3::ZERO; n];
    target[70_000] = Vec3::new(0.0, 0.5, 0.0);
    target[3] = Vec3::new(0.25, 0.0, 0.0);
    let body = scene.nodes[ids.body.index()].mesh.unwrap();
    let mesh = &mut scene.meshes[body.index()];
    mesh.primitives[0].mesh = m;
    mesh.primitives[0].morph_targets = vec![
        omuzaki_io_api::MorphTarget::new(target),
        omuzaki_io_api::MorphTarget::new(vec![Vec3::ZERO; n]),
        omuzaki_io_api::MorphTarget::new(vec![Vec3::ZERO; n]),
        omuzaki_io_api::MorphTarget::new(vec![Vec3::ZERO; n]),
    ];
    let file = export(&Vrm1Exporter, &scene);
    let (doc, _) = parts(&file);
    let prim = &doc["meshes"][0]["primitives"][0];
    assert_eq!(doc["accessors"][prim["indices"].as_u64().unwrap() as usize]["componentType"], 5125);
    let target0 = &doc["accessors"][prim["targets"][0]["POSITION"].as_u64().unwrap() as usize];
    assert_eq!(target0["sparse"]["indices"]["componentType"], 5125, "indices past 65 536 need 32 bits");
    let back = import(&Vrm1Importer, &file).scene;
    assert_eq!(back.meshes, scene.meshes);
}

#[test]
fn sniffing_tells_vrm1_from_other_glb_files() {
    let (scene, _) = avatar();
    let vrm = export(&Vrm1Exporter, &scene);
    assert_eq!(Vrm1Importer.sniff(&ImportInput::new(&vrm)), Confidence::Certain);
    let (mut doc, bin) = parts(&vrm);
    doc["extensions"].as_object_mut().unwrap().remove("VRMC_vrm");
    assert_eq!(Vrm1Importer.sniff(&ImportInput::new(&glb(&doc, &bin))), Confidence::No, "a plain glTF isn't a VRM");
    assert_eq!(Vrm1Importer.sniff(&ImportInput::new(b"not a glb")), Confidence::No);
}

// ---------------------------------------------------------------------------------------------------------------
// Files assembled by hand, the way another tool would write them.

#[test]
fn a_minimal_file_imports_with_the_spec_defaults() {
    let (doc, bin) = minimal1();
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    let s = &out.scene;
    assert_eq!((s.nodes.len(), s.roots.len()), (16, 2));
    let av = s.avatar.as_ref().unwrap();
    assert_eq!(av.humanoid.bones.len(), 15);
    assert!(av.expressions.is_empty() && av.look_at.is_none() && av.first_person.is_none());
    // The meta takes the spec defaults for everything the file omits: the most restrictive value.
    assert_eq!(s.meta.name, "Minimal");
    assert_eq!(s.meta.terms, omuzaki_io_api::UseTerms::default());
    // The file has no normals, so flat ones were generated (and the unindexed triangle got indices).
    let p = &s.meshes[0].primitives[0];
    assert_eq!((p.mesh.indices.as_slice(), p.mesh.normals.as_slice()), (&[0, 1, 2][..], &[Vec3::Z; 3][..]));
    assert!(out.report.has_rule("gltf.normals_generated"));
    assert_eq!(out.report.count(Severity::Warning), 0, "{:#?}", out.report);
    // The record is derived from the file's terms: personalNonProfit is not cleared for commercial use.
    assert_eq!(s.provenance.as_ref().unwrap().commercial, omuzaki_store_api::Commercial::No);
}

#[test]
fn matrices_quantised_data_and_interleaved_accessors_are_read() {
    let (mut doc, _) = minimal1();
    // Positions as normalised 16-bit integers (KHR_mesh_quantization), interleaved with a stray u16, under a matrix node.
    let mut bin = Vec::new();
    for corner in [[0i16, 0, 0], [i16::MAX, 0, 0], [0, i16::MAX, 0]] {
        for c in corner {
            bin.extend(c.to_le_bytes());
        }
        bin.extend(0xBEEFu16.to_le_bytes());
    }
    doc["accessors"][0] =
        json!({"bufferView": 0, "componentType": 5122, "normalized": true, "count": 3, "type": "VEC3"});
    doc["bufferViews"][0] = json!({"buffer": 0, "byteLength": 24, "byteStride": 8});
    doc["buffers"][0]["byteLength"] = json!(24);
    doc["extensionsUsed"] = json!(["VRMC_vrm", "KHR_mesh_quantization"]);
    doc["extensionsRequired"] = json!(["KHR_mesh_quantization"]);
    doc["nodes"][15] = json!({"name": "tri", "mesh": 0, "matrix": [2,0,0,0, 0,2,0,0, 0,0,2,0, 1,2,3,1]});
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    let p = &out.scene.meshes[0].primitives[0];
    assert_eq!(p.mesh.positions[1], Vec3::X, "i16::MAX normalised is exactly 1");
    let t = out.scene.nodes[15].transform;
    assert_eq!((t.translation, t.scale), (Vec3::new(1.0, 2.0, 3.0), Vec3::splat(2.0)));
}

#[test]
fn files_that_cannot_be_read_correctly_are_refused_with_a_reason() {
    let (doc, bin) = minimal1();
    let with = |edit: &dyn Fn(&mut Value)| {
        let mut d = doc.clone();
        edit(&mut d);
        try_import(&Vrm1Importer, &glb(&d, &bin), &ImportOptions::default())
    };
    let unsupported = with(&|d| d["extensionsRequired"] = json!(["KHR_draco_mesh_compression"]));
    assert!(matches!(unsupported, Err(IoError::Unsupported { ref what, .. }) if what.contains("KHR_draco")));
    assert!(matches!(
        with(&|d| d["extensions"]["VRMC_vrm"]["specVersion"] = json!("0.0")),
        Err(IoError::Unsupported { .. })
    ));
    assert!(matches!(with(&|d| d["asset"]["version"] = json!("1.0")), Err(IoError::Unsupported { .. })));
    assert!(matches!(
        with(&|d| {
            d["extensions"].as_object_mut().unwrap().remove("VRMC_vrm");
        }),
        Err(IoError::Malformed { .. })
    ));
    assert!(matches!(with(&|d| d["nodes"][0]["children"] = json!([99])), Err(IoError::Rejected(_))));
    let cyclic = with(&|d| d["nodes"][1]["children"] = json!([0]));
    assert!(
        matches!(cyclic, Err(IoError::Rejected(ref r)) if r.has_rule("scene.node_cycle") || r.has_rule("scene.node_multiple_parents"))
    );
}

#[test]
fn messy_optional_data_is_repaired_with_findings_not_rejected() {
    let (scene, ids) = avatar();
    let file = export(&Vrm1Exporter, &scene);
    let (mut doc, bin) = parts(&file);
    let vrm = &mut doc["extensions"]["VRMC_vrm"];
    vrm["expressions"]["preset"]["happy"]["morphTargetBinds"][0]["index"] = json!(99); // no such morph target
    vrm["expressions"]["preset"]["blink"]["morphTargetBinds"][0]["weight"] = json!(1.5); // out of range
    vrm["humanoid"]["humanBones"]["eyes"] = json!({"node": 1}); // not a bone
    vrm["expressions"]["custom"]["happy"] = json!({}); // reuses a preset name
    doc["extensions"]["VRMC_springBone"]["springs"][1]["joints"] =
        json!([{"node": ids.node("hairB2").0}, {"node": ids.node("hairB1").0}]);
    doc["extensions"]["VRMC_springBone"]["springs"][0]["center"] = json!(ids.node("hairA3").0); // not the first joint or an ancestor
    doc["extensions"]["VRMC_springBone"]["colliders"][2]["node"] = json!(9_999);
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    for rule in [
        "vrm.bind_dropped",
        "vrm.bind_weight_clamped",
        "vrm.humanoid_unknown_bone",
        "vrm.expression_name_invalid",
        "vrm.spring_chain_dropped",
        "vrm.spring_center_dropped",
        "vrm.collider_dropped",
    ] {
        assert!(out.report.has_rule(rule), "{rule}\n{:#?}", out.report);
    }
    let av = out.scene.avatar.as_ref().unwrap();
    assert_eq!(av.spring_bones.chains.len(), 1, "the out-of-order chain is gone, the other kept");
    assert_eq!(av.spring_bones.chains[0].center, None);
    assert_eq!(av.spring_bones.colliders.len(), 2);
    assert_eq!(av.spring_bones.collider_groups[1].colliders, Vec::<u32>::new(), "the group lost its only collider");
    let blink = &av.expressions[&ExpressionName::parse("blink").unwrap()];
    assert_eq!(blink.morph_binds[0].weight, 1.0);
    assert!(av.expressions[&ExpressionName::parse("happy").unwrap()].morph_binds.is_empty());
    assert!(!out.scene.check().has_errors());
}

#[test]
fn unknown_extensions_pass_through_and_animations_are_reported() {
    let (scene, ids) = avatar();
    let file = export(&Vrm1Exporter, &scene);
    let (mut doc, bin) = parts(&file);
    doc["extensionsUsed"].as_array_mut().unwrap().push(json!("EXT_texture_webp"));
    doc["animations"] = json!([{"channels": [], "samplers": []}]);
    let out = import(&Vrm1Importer, &glb(&doc, &bin));
    assert!(out.report.has_rule("gltf.animations_dropped"));
    assert_eq!(out.scene.nodes[ids.body.index()].passthrough.extensions["EXT_fixture_node"], json!({"flag": true}));
    assert_eq!(out.scene.materials[1].passthrough.extras, Some(json!({"layer": "cloth"})));
}

/// Scenes with ordinary spheres and capsules and no limits export the same spring JSON as before the extended shapes
/// existed. The baseline is the output of the exporter on `main`, serialised compactly.
#[test]
fn existing_spring_json_is_byte_identical_to_the_pre_extension_baseline() {
    let (scene, _) = avatar();
    let (doc, _) = parts(&export(&Vrm1Exporter, &scene));
    let baseline = include_str!("baseline/spring_vrm1.json").trim_end();
    assert_eq!(doc["extensions"]["VRMC_springBone"].to_string(), baseline);
}
