//! Import and export end to end through the contract's entry points: the mock's fixtures, generated scenes, byte
//! determinism, the file's spec shape and both layouts.

// Test data is tiny: the casts cannot truncate or lose precision that matters.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_precision_loss)]

mod common;

use common::{PlainImporter, assert_same_scene, export_files, export_glb, import, random_scene, sparse_morph_scene};
use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{Confidence, ImportInput, Layout, NodeIx, Severity};
use omuzaki_io_mock::MockVrm1Exporter;
use omuzaki_io_mock::fixtures::{glb_parts, options_without_face_channels, tiny_avatar, tiny_glb};
use omuzaki_store_api::Commercial;
use serde_json::json;

#[test]
fn imports_the_mocks_hand_built_glb() {
    let out = import(&tiny_glb());
    let s = &out.scene;
    assert_eq!((s.nodes.len(), s.roots.as_slice()), (1, &[NodeIx(0)][..]));
    assert_eq!(s.nodes[0].transform.translation, Vec3::Y);
    let prim = &s.meshes[0].primitives[0];
    assert_eq!(prim.mesh.positions[1], Vec3::X);
    assert_eq!(prim.mesh.indices, [0, 1, 2], "u16 indices widen to u32");
    assert_eq!(prim.mesh.normals, [Vec3::Z; 3], "the file has no normals: flat ones are generated");
    assert!(out.report.has_rule("gltf.normals_generated"));
    assert_eq!(s.materials[0].base_color.to_array(), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(s.meta.copyright_information.as_deref(), Some("Test Author"));
    assert_eq!(s.source_generator.as_deref(), Some("omuzaki-io-mock fixture"));
    // A plain glTF says nothing about rights, so the record is "unknown", which the gate treats as no.
    assert_eq!(s.provenance.as_ref().expect("run_import fills the provenance").commercial, Commercial::Unknown);
    assert_eq!(out.report.count(Severity::Error), 0);
}

#[test]
fn what_the_mock_exports_the_real_importer_reads_and_the_reverse() {
    let original = tiny_avatar();
    let options = options_without_face_channels();
    let mock =
        omuzaki_io_api::run_export(&MockVrm1Exporter, &original, &omuzaki_io_api::ExportCtx::new(&options)).unwrap();
    let from_mock = import(&mock.files[0].bytes).scene;
    assert_eq!(from_mock.nodes, original.nodes);
    assert_eq!(from_mock.meshes, original.meshes);
    assert_eq!(from_mock.skins, original.skins);
    assert_eq!(from_mock.materials, original.materials);

    // The mock's reader accepts our output.
    let ours = export_glb(&from_mock);
    let plain = omuzaki_io_api::run_import(
        &omuzaki_io_mock::MockGlbImporter,
        &ImportInput::new(&ours),
        &omuzaki_io_api::ImportCtx::new(&omuzaki_io_api::ImportOptions::default()),
    )
    .expect("the mock's glTF reader reads our GLB");
    assert_eq!(plain.scene.nodes, from_mock.nodes);
    assert_eq!(plain.scene.meshes, from_mock.meshes);
}

#[test]
fn generated_scenes_round_trip_exactly() {
    for seed in 0..200 {
        let scene = random_scene(seed);
        assert!(!scene.check().has_errors(), "seed {seed}: {:?}", scene.check());
        let back = import(&export_glb(&scene)).scene;
        assert_same_scene(&back, &scene);
    }
}

#[test]
fn sparse_morph_targets_are_stored_when_smaller_and_read_back_exactly() {
    let scene = sparse_morph_scene();
    let glb = export_glb(&scene);
    let (doc, _) = glb_parts(&glb).unwrap();
    let targets = &doc["meshes"][0]["primitives"][0]["targets"];
    let accessor = |t: usize| &doc["accessors"][targets[t]["POSITION"].as_u64().unwrap() as usize];
    assert_eq!(accessor(0)["sparse"]["count"], 3);
    assert_eq!(accessor(0)["sparse"]["indices"]["componentType"], 5123, "300 vertices: unsigned short indices");
    assert_eq!(accessor(1)["sparse"]["count"], 1);
    assert!(accessor(2).get("sparse").is_none() && accessor(2).get("bufferView").is_none(), "nothing moves");
    // Every position accessor, sparse or not, carries the bounds glTF requires.
    for t in 0..3 {
        assert_eq!(accessor(t)["min"].as_array().map(Vec::len), Some(3));
    }
    assert_same_scene(&import(&glb).scene, &scene);
    assert_eq!(export_glb(&import(&glb).scene), glb, "canonical: the choice depends only on the data");
}

#[test]
fn exports_are_deterministic_and_canonical() {
    for seed in 0..50 {
        let scene = random_scene(seed);
        let first = export_glb(&scene);
        assert_eq!(first, export_glb(&scene), "seed {seed}: same scene, same bytes");
        let again = export_glb(&import(&first).scene);
        assert_eq!(first, again, "seed {seed}: export(import(export(x))) == export(x)");
    }
}

#[test]
fn the_separate_layout_reads_back_to_the_same_scene() {
    let options = options_without_face_channels().with_layout(Layout::Separate);
    for seed in 0..40 {
        let scene = random_scene(seed);
        let files = export_files(&scene, &options).expect("export");
        assert_eq!(files[0].path, "model.gltf");
        let text: serde_json::Value = serde_json::from_slice(&files[0].bytes).expect("the .gltf is JSON");
        assert_eq!(text["buffers"].get(0).and_then(|b| b["uri"].as_str()), files.get(1).map(|_| "model.bin"));
        let sidecars = files.iter().map(|f| (f.path.clone(), f.bytes.clone())).collect();
        let out = common::import_with(
            &files[0].bytes,
            &omuzaki_io_api::ImportOptions::default(),
            &common::MapResolver(sidecars),
        )
        .expect("import");
        assert_same_scene(&out.scene, &scene);
        // The two layouts are the same model: re-exporting as binary gives the binary export.
        assert_eq!(export_glb(&out.scene), export_glb(&scene), "seed {seed}");
    }
}

#[test]
fn the_file_has_the_spec_shape() {
    let scene = random_scene(7);
    let file = export_glb(&scene);
    // The GLB header, checked by hand rather than through our own reader.
    assert_eq!(&file[0..4], b"glTF");
    assert_eq!(u32::from_le_bytes(file[4..8].try_into().unwrap()), 2);
    assert_eq!(u32::from_le_bytes(file[8..12].try_into().unwrap()) as usize, file.len());
    assert_eq!(file.len() % 4, 0);
    let (doc, bin) = glb_parts(&file).expect("parts");
    assert_eq!(bin.len() % 4, 0, "the BIN chunk is padded, and is not empty");
    assert_ne!(bin.len(), 0);
    assert_eq!(doc["asset"]["version"], "2.0");
    assert_eq!(doc["scene"], 0);
    assert_eq!(doc["buffers"][0]["byteLength"].as_u64().unwrap() as usize, bin.len());
    assert!(doc["buffers"][0].get("uri").is_none(), "the GLB's buffer has no uri");
    // Every view lies inside the buffer, aligned; every accessor lies inside its view; POSITION carries min and max.
    for view in doc["bufferViews"].as_array().unwrap() {
        let (offset, length) = (view["byteOffset"].as_u64().unwrap_or(0), view["byteLength"].as_u64().unwrap());
        assert_eq!(offset % 4, 0);
        assert!(length > 0 && (offset + length) as usize <= bin.len());
    }
    for mesh in doc["meshes"].as_array().unwrap() {
        for prim in mesh["primitives"].as_array().unwrap() {
            assert_eq!(prim["mode"], 4, "defaults are written explicitly");
            let mut positions = vec![prim["attributes"]["POSITION"].as_u64().unwrap()];
            positions.extend(prim["targets"].as_array().into_iter().flatten().map(|t| t["POSITION"].as_u64().unwrap()));
            for ix in positions {
                let acc = &doc["accessors"][usize::try_from(ix).unwrap()];
                assert!(acc["min"].is_array() && acc["max"].is_array(), "accessor {ix} needs min and max");
            }
        }
    }
    for material in doc["materials"].as_array().unwrap() {
        let pbr = &material["pbrMetallicRoughness"];
        assert!(pbr["baseColorFactor"].is_array() && pbr["metallicFactor"].is_number());
        assert!(material["alphaMode"].is_string() && material["doubleSided"].is_boolean());
    }
    // Every extension a material or texture slot uses is listed as used, and the list is sorted.
    let used: Vec<&str> =
        doc["extensionsUsed"].as_array().map_or(vec![], |u| u.iter().map(|e| e.as_str().unwrap()).collect());
    let mut sorted = used.clone();
    sorted.sort_unstable();
    assert_eq!(used, sorted);
    assert!(doc.get("extensionsRequired").is_none());
}

#[test]
fn bright_emissive_and_texture_transforms_use_their_extensions() {
    let mut scene = random_scene(1);
    let mut m = omuzaki_io_api::Material::new("glow", omuzaki_io_api::Shading::Unlit);
    m.emissive = Vec3::new(3.0, 1.0, 0.0);
    scene.add_material(m);
    let (doc, _) = glb_parts(&export_glb(&scene)).unwrap();
    let last = doc["materials"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["extensions"]["KHR_materials_unlit"], json!({}));
    assert_eq!(last["extensions"]["KHR_materials_emissive_strength"]["emissiveStrength"], 3.0, "the peak channel");
    assert_eq!(last["emissiveFactor"], json!([1.0, 0.333_333_34, 0.0]));
    let used = doc["extensionsUsed"].as_array().unwrap();
    assert!(used.contains(&json!("KHR_materials_unlit")) && used.contains(&json!("KHR_materials_emissive_strength")));
}

#[test]
fn the_probe_recognises_glb_and_text_gltf_and_nothing_else() {
    let text = br#"{"asset":{"version":"2.0"}}"#;
    assert_eq!(omuzaki_io_api::Importer::sniff(&PlainImporter, &ImportInput::new(text)), Confidence::Possible);
    assert_eq!(omuzaki_io_api::Importer::sniff(&PlainImporter, &ImportInput::new(&tiny_glb())), Confidence::Possible);
    for junk in [&b"not a model"[..], b"{}", b"", b"glT"] {
        assert_eq!(omuzaki_io_api::Importer::sniff(&PlainImporter, &ImportInput::new(junk)), Confidence::No);
    }
}

#[test]
fn strict_export_refuses_lossy_output_and_the_report_is_otherwise_clean() {
    let scene = random_scene(11);
    let strict = options_without_face_channels().strict();
    let outcome = omuzaki_io_api::run_export(&common::PlainExporter, &scene, &omuzaki_io_api::ExportCtx::new(&strict))
        .expect("a plain scene exports clean even in strict mode");
    assert_eq!(outcome.report.count(Severity::Error) + outcome.report.count(Severity::Warning), 0);
}

#[test]
fn unwritable_scenes_are_refused_with_every_problem() {
    let mut scene = random_scene(5);
    scene.add_mesh(omuzaki_io_api::Mesh::new("empty"));
    scene.add_skin(omuzaki_io_api::Skin::new("none", Vec::new(), Vec::new()));
    scene.materials[0].base_color.x = f32::NAN;
    let Err(omuzaki_io_api::IoError::Rejected(report)) = export_files(&scene, &options_without_face_channels()) else {
        panic!("expected a rejection")
    };
    for rule in ["gltf.mesh_empty", "gltf.skin_empty", "gltf.non_finite"] {
        assert!(report.has_rule(rule), "{rule} in {report:?}");
    }
}

#[test]
fn the_generator_exercises_every_part_of_the_mapping() {
    use omuzaki_io_api::Shading;
    let scenes: Vec<_> = (0..200).map(random_scene).collect();
    let any = |f: &dyn Fn(&omuzaki_io_api::IoScene) -> bool| scenes.iter().any(f);
    let prims = |s: &omuzaki_io_api::IoScene| -> Vec<omuzaki_io_api::Primitive> {
        s.meshes.iter().flat_map(|m| m.primitives.iter().cloned()).collect()
    };
    assert!(any(&|s| prims(s).iter().any(|p| p.mesh.is_skinned())), "skinning");
    assert!(any(&|s| prims(s).iter().any(|p| !p.morph_targets.is_empty())), "morph targets");
    assert!(any(&|s| prims(s).iter().any(|p| p.morph_targets.iter().any(|t| !t.normals.is_empty()))), "morph normals");
    assert!(any(&|s| s.meshes.iter().any(|m| !m.morph_target_names.is_empty())), "target names");
    assert!(any(&|s| prims(s).iter().any(|p| !p.mesh.colours.is_empty())), "vertex colours");
    assert!(any(&|s| prims(s).iter().any(|p| !p.mesh.uvs.is_empty())), "uvs");
    assert!(any(&|s| prims(s).iter().any(|p| !p.mesh.extra.is_empty())), "custom attributes");
    assert!(any(&|s| s.materials.iter().any(|m| m.shading == Shading::Unlit)), "unlit");
    assert!(any(&|s| s.materials.iter().any(|m| m.emissive.max_element() > 1.0)), "bright emissive");
    assert!(
        any(&|s| s.materials.iter().any(|m| m.base_color_texture.is_some_and(|t| t.transform.is_some()))),
        "uv transform"
    );
    assert!(any(&|s| s.materials.iter().any(|m| m.normal_texture.is_some())), "normal map");
    assert!(any(&|s| s.materials.iter().any(|m| !m.passthrough.is_empty())), "material pass-through");
    assert!(any(&|s| s.nodes.iter().any(|n| !n.passthrough.is_empty())), "node pass-through");
    assert!(any(&|s| !s.passthrough.is_empty()), "root pass-through");
    assert!(any(&|s| s.images.iter().any(|i| i.mime == omuzaki_io_api::ImageMime::Jpeg)), "jpeg");
    assert!(any(&|s| s.skins.iter().any(|k| k.skeleton.is_some())), "skeleton root");
    assert!(any(&|s| s.roots.len() > 1), "several roots");
}

#[test]
fn big_meshes_use_32_bit_indices_and_round_trip() {
    let mut scene = random_scene(0);
    scene.meshes.clear();
    scene.skins.clear();
    for node in &mut scene.nodes {
        (node.mesh, node.skin) = (None, None);
    }
    let n = 70_000usize;
    let mesh_data = omuzaki_io_api::MeshData {
        positions: (0..n).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect(),
        normals: vec![Vec3::Z; n],
        indices: (0..n as u32 - 2).flat_map(|i| [i, i + 1, i + 2]).collect(),
        ..omuzaki_io_api::MeshData::default()
    };
    let mut mesh = omuzaki_io_api::Mesh::new("big");
    mesh.primitives = vec![omuzaki_io_api::Primitive::new(mesh_data)];
    let ix = scene.add_mesh(mesh);
    scene.nodes[0].mesh = Some(ix);
    let file = export_glb(&scene);
    let (doc, _) = glb_parts(&file).unwrap();
    let indices = doc["meshes"][0]["primitives"][0]["indices"].as_u64().unwrap() as usize;
    assert_eq!(doc["accessors"][indices]["componentType"], 5125, "unsigned int");
    assert_same_scene(&import(&file).scene, &scene);
}

#[test]
fn a_skin_on_a_node_without_a_mesh_is_dropped_with_a_warning_and_the_file_stays_valid() {
    let mut scene = random_scene(6);
    let skin = omuzaki_io_api::SkinIx(0);
    let bare = scene.add_node(omuzaki_io_api::Node::new("bare"));
    scene.nodes[bare.index()].skin = Some(skin);
    scene.roots.push(bare);
    let report = omuzaki_io_api::run_export(
        &common::PlainExporter,
        &scene,
        &omuzaki_io_api::ExportCtx::new(&options_without_face_channels()),
    )
    .expect("a warning, not an error");
    assert_eq!(common::finding(&report.report, "gltf.skin_dropped").severity, Severity::Warning);
    let (doc, _) = glb_parts(&report.files[0].bytes).unwrap();
    assert!(doc["nodes"][bare.index()].get("skin").is_none());
}

#[test]
fn non_finite_morph_deltas_are_refused() {
    let has_morphs =
        |s: &omuzaki_io_api::IoScene| s.meshes.iter().flat_map(|m| &m.primitives).any(|p| !p.morph_targets.is_empty());
    let mut scene = (0..50).map(random_scene).find(has_morphs).expect("some seed has morph targets");
    let prim = scene.meshes.iter_mut().flat_map(|m| m.primitives.iter_mut()).find(|p| !p.morph_targets.is_empty());
    prim.expect("found above").morph_targets[0].positions[0].x = f32::INFINITY;
    let Err(omuzaki_io_api::IoError::Rejected(report)) = export_files(&scene, &options_without_face_channels()) else {
        panic!("expected a rejection")
    };
    assert!(report.has_rule("gltf.non_finite"));
}

#[test]
fn hyphenated_custom_attributes_survive_export_import_and_re_export() {
    // The kit's hair writes `hair-uv` and `hair-info` (#667): `_HAIR-UV` in the file must read back as `hair-uv`.
    let mut scene = (0..200)
        .map(random_scene)
        .find(|s| s.meshes.iter().any(|m| m.primitives.iter().any(|p| !p.mesh.extra.is_empty())))
        .expect("a generated scene has custom attributes");
    let mut renamed = 0;
    for primitive in scene.meshes.iter_mut().flat_map(|m| m.primitives.iter_mut()) {
        for extra in &mut primitive.mesh.extra {
            extra.name = omuzaki_core::id::Slug::new(extra.name.as_str().replace('_', "-")).unwrap();
            renamed += 1;
        }
    }
    assert!(renamed > 0);
    let glb = export_glb(&scene);
    let (doc, _) = glb_parts(&glb).unwrap();
    assert!(doc.to_string().contains("\"_STRAND-"), "the file keeps the name, in upper case");
    let back = import(&glb);
    assert!(!back.report.has_rule("gltf.attribute_dropped"), "{:?}", back.report);
    assert_same_scene(&back.scene, &scene);
    assert_eq!(export_glb(&back.scene), glb, "the re-export is byte for byte the export");
}
