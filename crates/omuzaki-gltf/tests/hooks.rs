//! What a format built on glTF (VRM) needs from the library: its extensions kept out of the way, each node, mesh and
//! material handed over with the file's raw JSON and index, arenas that never move, and a writer it can add to.
//! A small fake VRM stands in: it interprets `VRMC_vrm` on the root, `VRMC_node_constraint` on nodes and
//! `VRMC_materials_mtoon` on materials.

// Test data is tiny: the casts cannot truncate.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::float_cmp
)]

mod common;

use common::{Builder, PNG, random_scene};
use omuzaki_core::glam::Vec3;
use omuzaki_gltf::schema::TextureInfo;
use omuzaki_gltf::{
    Carrier, Disposition, ExportHooks, ImportHooks, Imported, MaterialCx, MeshCx, NodeCx, RootCx, TargetNames,
    Unusable, parse, read_scene, to_json, write_scene,
};
use omuzaki_io_api::{
    ExportCtx, ExportOptions, ImportCtx, ImportOptions, IoError, IoScene, Limits, Material, Mesh, Node, Shading,
    TexRef, TextureIx, UvTransform,
};
use serde_json::{Value, json};

/// The parts of VRM this test cares about, as hooks.
#[derive(Default)]
struct FakeVrm {
    unusable: Unusable,
    constraints: Vec<(usize, Value)>,
    meshes_seen: Vec<(usize, String)>,
    root_seen: Option<Value>,
}

impl ImportHooks for FakeVrm {
    fn supported_extensions(&self) -> &[&str] {
        &["VRMC_vrm", "VRMC_materials_mtoon"]
    }

    fn extension(&mut self, carrier: Carrier, name: &str, _: &Value) -> Disposition {
        match (carrier, name) {
            (Carrier::Root, "VRMC_vrm")
            | (Carrier::Node, "VRMC_node_constraint")
            | (Carrier::Material, "VRMC_materials_mtoon") => Disposition::Interpreted,
            _ => Disposition::Carry,
        }
    }

    fn unusable(&self) -> Unusable {
        self.unusable
    }

    fn node(&mut self, cx: &mut NodeCx<'_>) -> Result<(), IoError> {
        if let Some(body) = cx.def.extensions.get("VRMC_node_constraint") {
            self.constraints.push((cx.index, body.clone()));
        }
        Ok(())
    }

    fn mesh(&mut self, cx: &mut MeshCx<'_>) -> Result<(), IoError> {
        self.meshes_seen.push((cx.index, cx.mesh.name.clone()));
        Ok(())
    }

    fn material(&mut self, cx: &mut MaterialCx<'_>) -> Result<(), IoError> {
        let Some(body) = cx.def.extensions.others.get("VRMC_materials_mtoon") else { return Ok(()) };
        // MToon's texture references are glTF texture infos, with their own `KHR_texture_transform`.
        let mut toon = omuzaki_io_api::Toon::default();
        if let Some(shade) = body.get("shadeMultiplyTexture") {
            let info: TextureInfo = serde_json::from_value(shade.clone()).unwrap();
            toon.shade_multiply_texture = cx.textures.tex_ref(&info, "mtoon/shadeMultiplyTexture", cx.tally);
        }
        toon.shading_shift = body["shadingShiftFactor"].as_f64().unwrap_or(0.0) as f32;
        cx.material.shading = Shading::Toon(Box::new(toon)); // takes precedence over `KHR_materials_unlit`
        Ok(())
    }

    fn root(&mut self, cx: &mut RootCx<'_>) -> Result<(), IoError> {
        self.root_seen = cx.document.extensions.get("VRMC_vrm").cloned();
        cx.tally.info("vrm.test", "the root hook ran", "/extensions/VRMC_vrm");
        Ok(())
    }
}

fn read(bytes: &[u8], hooks: &mut dyn ImportHooks) -> Result<Imported, IoError> {
    let options = ImportOptions::default();
    let parsed = parse(bytes, &Limits::default())?;
    read_scene(&parsed, &ImportCtx::new(&options), hooks)
}

/// A VRM-shaped file: two nodes (one with a constraint), a mesh, a material with an `MToon` block whose texture
/// reference carries `KHR_texture_transform`, a PNG image with a texture on it, and `VRMC_vrm` on the root.
fn avatar_file() -> Vec<u8> {
    let mut b = Builder::new();
    b.triangle();
    let png = b.view(&PNG, None);
    b.push("images", json!({ "bufferView": png, "mimeType": "image/png" }));
    b.push("textures", json!({ "source": 0 }));
    b.push(
        "materials",
        json!({
            "pbrMetallicRoughness": { "baseColorFactor": [1, 1, 1, 1] },
            "extensions": {
                "KHR_materials_unlit": {},
                "VRMC_materials_mtoon": {
                    "specVersion": "1.0", "shadingShiftFactor": -0.25,
                    "shadeMultiplyTexture": { "index": 0, "extensions": {
                        "KHR_texture_transform": { "offset": [0.5, 0.25], "scale": [2.0, 2.0] } } }
                }
            }
        }),
    );
    b.doc["meshes"][0]["name"] = json!("Body");
    b.doc["meshes"][0]["primitives"][0]["material"] = json!(0);
    b.push(
        "nodes",
        json!({ "name": "hips", "extensions": { "VRMC_node_constraint": { "constraint": { "roll": {} } } } }),
    );
    b.doc["nodes"][0]["children"] = json!([1]);
    b.doc["extensionsUsed"] =
        json!(["VRMC_vrm", "VRMC_materials_mtoon", "VRMC_node_constraint", "KHR_materials_unlit"]);
    b.doc["extensionsRequired"] = json!(["VRMC_vrm"]);
    b.doc["extensions"] =
        json!({ "VRMC_vrm": { "specVersion": "1.0", "humanoid": {} }, "EXT_studio_tag": { "shot": 1 } });
    b.glb()
}

#[test]
fn a_required_extension_the_format_reads_is_accepted_and_otherwise_refused() {
    let file = avatar_file();
    assert!(read(&file, &mut FakeVrm::default()).is_ok());
    let Err(IoError::Unsupported { what, .. }) = read(&file, &mut omuzaki_gltf::NoHooks) else { panic!("refused") };
    assert!(what.contains("VRMC_vrm"));
}

#[test]
fn the_format_interprets_its_extensions_and_the_rest_ride_through() {
    let mut vrm = FakeVrm::default();
    let Imported { scene, tally } = read(&avatar_file(), &mut vrm).unwrap();
    assert_eq!(scene.passthrough.extensions.keys().collect::<Vec<_>>(), ["EXT_studio_tag"], "VRMC_vrm is the format's");
    assert!(scene.nodes[1].passthrough.extensions.is_empty());
    assert!(scene.materials[0].passthrough.extensions.is_empty());
    let mut report = omuzaki_io_api::Report::new();
    tally.finish(&mut report);
    assert!(!report.has_rule("gltf.extension_dropped"), "interpreted extensions are not reported: {report:?}");
    assert!(report.has_rule("vrm.test"), "findings added by a hook are in the report");
}

#[test]
fn hooks_get_each_object_with_its_file_index_and_raw_json() {
    let mut vrm = FakeVrm::default();
    let Imported { scene, .. } = read(&avatar_file(), &mut vrm).unwrap();
    assert_eq!(vrm.constraints, [(1, json!({ "constraint": { "roll": {} } }))]);
    assert_eq!(vrm.meshes_seen, [(0, "Body".to_owned())]);
    assert_eq!(vrm.root_seen.as_ref().and_then(|v| v["specVersion"].as_str()), Some("1.0"));
    assert_eq!(scene.nodes[1].name, "hips", "node 1 in the scene is node 1 in the file");
}

#[test]
fn a_hook_can_turn_a_material_toon_and_map_the_textures_inside_its_extension_body() {
    let Imported { scene, .. } = read(&avatar_file(), &mut FakeVrm::default()).unwrap();
    let Shading::Toon(toon) = &scene.materials[0].shading else { panic!("MToon beats KHR_materials_unlit") };
    assert_eq!(toon.shading_shift, -0.25);
    let shade = toon.shade_multiply_texture.expect("the texture reference maps");
    assert_eq!(shade.texture.index(), 0);
    assert_eq!(shade.transform, Some(UvTransform { offset: [0.5, 0.25].into(), scale: [2.0, 2.0].into() }));
    // Without the hook the same file is a plain unlit material, MToon carried as raw JSON.
    let plain = read(&avatar_file(), &mut Accepting).unwrap().scene;
    assert!(matches!(plain.materials[0].shading, Shading::Unlit));
    assert!(plain.materials[0].passthrough.extensions.contains_key("VRMC_materials_mtoon"));
}

/// Drops every root extension with a message built from its body.
struct QuoteBodies;
impl ImportHooks for QuoteBodies {
    fn extension(&mut self, carrier: Carrier, name: &str, body: &Value) -> Disposition {
        match carrier {
            Carrier::Root => Disposition::Discard {
                severity: omuzaki_io_api::Severity::Warning,
                message: format!("{name} dropped; its body was {body}"),
                hint: None,
            },
            _ => Disposition::Carry,
        }
    }
}

#[test]
fn the_format_sees_the_body_of_an_extension_when_it_chooses_what_to_do_with_it() {
    let mut b = Builder::new();
    b.triangle();
    b.doc["extensions"] = json!({ "X_licence": { "terms": "no commercial use" } });
    let out = read(&b.glb(), &mut QuoteBodies).unwrap();
    let mut report = omuzaki_io_api::Report::new();
    out.tally.finish(&mut report);
    let dropped = report.findings.iter().find(|f| f.rule == "gltf.extension_dropped").expect("dropped");
    assert_eq!(dropped.message, r#"X_licence dropped; its body was {"terms":"no commercial use"}"#);
    assert!(out.scene.passthrough.extensions.is_empty());
}

/// Accepts the required extensions and interprets nothing.
struct Accepting;
impl ImportHooks for Accepting {
    fn supported_extensions(&self) -> &[&str] {
        &["VRMC_vrm"]
    }
}

#[test]
fn unusable_images_and_textures_drop_by_default_and_refuse_when_the_format_indexes_into_them() {
    let mut b = Builder::new();
    b.triangle();
    let webp = b.view(b"RIFF\0\0\0\0WEBP", None);
    let png = b.view(&PNG, None);
    b.push("images", json!({ "bufferView": webp }));
    b.push("images", json!({ "bufferView": png }));
    b.push("textures", json!({ "source": 0 }));
    b.push("textures", json!({ "source": 1 }));
    let file = b.glb();

    let dropped = read(&file, &mut omuzaki_gltf::NoHooks).unwrap().scene;
    assert_eq!((dropped.images.len(), dropped.textures.len()), (1, 1), "the arenas shrink and indices move");

    let Err(IoError::Unsupported { what, hint }) =
        read(&file, &mut FakeVrm { unusable: Unusable::Refuse, ..FakeVrm::default() })
    else {
        panic!("a format that indexes into images refuses instead of renumbering")
    };
    assert!(what.contains("image 0") && hint.is_some_and(|h| h.contains("PNG or JPEG")));

    // A texture with no source (an extension-only texture) is refused the same way.
    let mut b = Builder::new();
    b.triangle();
    b.push("textures", json!({ "extensions": { "EXT_texture_webp": { "source": 3 } } }));
    let sourceless = b.glb();
    let Err(IoError::Unsupported { what, .. }) =
        read(&sourceless, &mut FakeVrm { unusable: Unusable::Refuse, ..FakeVrm::default() })
    else {
        panic!("refused")
    };
    assert!(what.contains("texture 0"));
    assert!(
        !read(&sourceless, &mut omuzaki_gltf::NoHooks).unwrap().tally.is_empty(),
        "plain reading drops it with a warning"
    );

    // Nothing unusable: the arenas are exactly the file's under either policy.
    let ok = read(&avatar_file(), &mut FakeVrm { unusable: Unusable::Refuse, ..FakeVrm::default() }).unwrap().scene;
    assert_eq!((ok.images.len(), ok.textures.len(), ok.nodes.len(), ok.meshes.len()), (1, 1, 2, 1));
}

#[test]
fn eight_bit_weights_are_scaled_to_the_scene_tolerance_and_exact_ones_are_left_alone() {
    let mut b = Builder::new();
    b.triangle();
    // 255-step weights (0.5, 0.25, 0.125, 0.12): they sum to 0.995, further from 1 than the scene check allows.
    let w = b.view(&[128, 64, 32, 31, 128, 64, 32, 31, 128, 64, 32, 31], None);
    let weights = b.accessor(w, 5121, 3, "VEC4");
    b.doc["accessors"][weights]["normalized"] = json!(true);
    let j = b.u16s(&[0, 1, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0], "VEC4");
    let attrs = &mut b.doc["meshes"][0]["primitives"][0]["attributes"];
    attrs["JOINTS_0"] = json!(j);
    attrs["WEIGHTS_0"] = json!(weights);
    let scene = read(&b.glb(), &mut omuzaki_gltf::NoHooks).unwrap().scene;
    for w in &scene.meshes[0].primitives[0].mesh.weights {
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6, "{w:?}");
    }
}

#[test]
fn extensions_on_objects_the_scene_has_no_counterpart_for_are_reported() {
    let mut b = Builder::new();
    b.triangle();
    b.doc["accessors"][0]["extensions"] = json!({ "EXT_accessor_thing": {} });
    b.doc["samplers"] = json!([{ "extensions": { "EXT_sampler_thing": {} } }]);
    b.doc["bufferViews"][0]["extensions"] = json!({ "EXT_meshopt_compression": {} });
    let out = read(&b.glb(), &mut omuzaki_gltf::NoHooks).unwrap();
    let mut report = omuzaki_io_api::Report::new();
    out.tally.finish(&mut report);
    let dropped: Vec<_> =
        report.findings.iter().filter(|f| f.rule == "gltf.extension_dropped").map(|f| f.message.as_str()).collect();
    assert_eq!(dropped.len(), 3, "{dropped:?}");
    assert!(dropped.iter().any(|m| m.contains("accessor extension EXT_accessor_thing")));
    assert!(dropped.iter().any(|m| m.contains("sampler extension EXT_sampler_thing")));
}

// ---------------------------------------------------------------- writing

/// Adds a fake VRM on top of the generic writer.
struct FakeVrmWriter {
    names: TargetNames,
}

impl ExportHooks for FakeVrmWriter {
    fn target_names(&self) -> TargetNames {
        self.names
    }

    fn node(&mut self, index: usize, _: &Node, def: &mut omuzaki_gltf::schema::Node) {
        if index == 0 {
            def.extensions.insert("VRMC_node_constraint".into(), json!({ "node": index }));
        }
    }

    fn mesh(&mut self, index: usize, mesh: &Mesh, def: &mut omuzaki_gltf::schema::Mesh) {
        assert_eq!(def.name, mesh.name, "mesh {index}: the generic object is finished before the hook sees it");
    }

    fn material(
        &mut self,
        index: usize,
        m: &Material,
        def: &mut omuzaki_gltf::schema::Material,
        _: &mut omuzaki_gltf::Tally,
    ) {
        // A toon material arrives as a matte PBR stand-in; the format adds its extension and the unlit fallback.
        if matches!(m.shading, Shading::Toon(_)) {
            let pbr = def.pbr_metallic_roughness.as_ref().unwrap();
            assert_eq!((pbr.metallic_factor, pbr.roughness_factor), (Some(0.0), Some(1.0)), "material {index}");
            def.extensions.unlit = Some(omuzaki_gltf::schema::Empty::default());
            // The texture reference inside the body carries its own extension, which `extensionsUsed` must list too.
            // `texture_info` is how a format writes a scene texture reference, transform included, inside its own body.
            let reference = TexRef {
                texture: TextureIx(0),
                uv_set: 0,
                transform: Some(UvTransform { offset: [0.5, 0.25].into(), scale: [2.0, 1.0].into() }),
            };
            let shade = omuzaki_gltf::texture_info(&reference, None);
            def.extensions.others.insert(
                "VRMC_materials_mtoon".into(),
                json!({ "specVersion": "1.0", "shadeMultiplyTexture": to_json(&shade) }),
            );
        }
    }

    fn root(&mut self, _: &IoScene, document: &mut omuzaki_gltf::Document, _: &mut omuzaki_gltf::Tally) {
        document.extensions.insert("VRMC_vrm".into(), json!({ "specVersion": "1.0" }));
    }
}

fn written(scene: &IoScene, hooks: &mut dyn ExportHooks) -> omuzaki_gltf::Written {
    let options = ExportOptions::default();
    write_scene(scene, &ExportCtx::new(&options), hooks).unwrap()
}

#[test]
fn the_writer_takes_the_formats_extensions_and_lists_every_extension_used() {
    let mut scene = random_scene(8);
    let toon = scene.add_material(Material::new("toon", Shading::Toon(Box::default())));
    let mut w = written(&scene, &mut FakeVrmWriter { names: TargetNames::OnMesh });
    // A thumbnail is a texture index in VRM 0.x: add one for an existing image if the scene has any.
    if !scene.images.is_empty() {
        let t = w.texture_for_image(0);
        assert_eq!(w.texture_for_image(0), t, "reused");
    }
    let doc = &w.document;
    assert!(doc.extensions.contains_key("VRMC_vrm") && doc.nodes[0].extensions.contains_key("VRMC_node_constraint"));
    let outcome = w.into_outcome().unwrap();
    let (json, _) = omuzaki_io_mock::fixtures::glb_parts(&outcome.files[0].bytes).unwrap();
    let used: Vec<&str> = json["extensionsUsed"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert!(used.windows(2).all(|p| p[0] < p[1]), "sorted and unique: {used:?}");
    for name in
        ["VRMC_vrm", "VRMC_node_constraint", "VRMC_materials_mtoon", "KHR_materials_unlit", "KHR_texture_transform"]
    {
        assert!(used.contains(&name), "{name} in {used:?}");
    }
    assert_eq!(json["materials"][toon.index()]["extensions"]["KHR_materials_unlit"], json!({}));
    let shade = &json["materials"][toon.index()]["extensions"]["VRMC_materials_mtoon"]["shadeMultiplyTexture"];
    assert_eq!(shade["extensions"]["KHR_texture_transform"], json!({ "offset": [0.5, 0.25], "scale": [2.0, 1.0] }));
}

#[test]
fn morph_target_names_go_on_the_mesh_or_on_every_primitive() {
    let mut scene = (0..60)
        .map(random_scene)
        .find(|s| s.meshes.iter().any(|m| !m.morph_target_names.is_empty() && m.primitives.len() > 1))
        .expect("some seed has named targets on a multi-primitive mesh");
    scene.provenance = None;
    let m = scene.meshes.iter().position(|m| !m.morph_target_names.is_empty() && m.primitives.len() > 1).unwrap();
    let names = json!({ "targetNames": scene.meshes[m].morph_target_names });
    for (placement, on_mesh) in [(TargetNames::OnMesh, true), (TargetNames::OnPrimitives, false)] {
        let doc = written(&scene, &mut FakeVrmWriter { names: placement }).document;
        assert_eq!(doc.meshes[m].extras.as_ref() == Some(&names), on_mesh, "{placement:?}");
        assert!(doc.meshes[m].primitives.iter().all(|p| (p.extras.as_ref() == Some(&names)) != on_mesh));
        assert_eq!(
            doc.meshes[m].target_names(),
            Some(scene.meshes[m].morph_target_names.clone()),
            "readable either way"
        );
    }
}

#[test]
fn big_meshes_use_32_bit_indices_and_sparse_morph_targets_round_trip() {
    let n = 70_000usize;
    let mut positions = vec![Vec3::ZERO; n];
    for (i, p) in positions.iter_mut().enumerate() {
        *p = Vec3::new(i as f32 * 0.001, 0.0, 1.0);
    }
    let mut data =
        omuzaki_io_api::MeshData { positions, normals: vec![Vec3::Z; n], ..omuzaki_io_api::MeshData::default() };
    data.indices = (0..n as u32 - 2).flat_map(|i| [i, i + 1, i + 2]).collect();
    let mut prim = omuzaki_io_api::Primitive::new(data);
    let mut smile = vec![Vec3::ZERO; n];
    smile[3] = Vec3::new(0.0, 0.5, 0.0);
    smile[69_999] = Vec3::new(0.25, -0.5, 0.0);
    prim.morph_targets.push(omuzaki_io_api::MorphTarget::new(smile));
    prim.morph_targets.push(omuzaki_io_api::MorphTarget::new(vec![Vec3::ZERO; n]));
    let mut scene = IoScene::new();
    let mut mesh = Mesh::new("face");
    mesh.primitives.push(prim);
    scene.add_mesh(mesh);

    let out = written(&scene, &mut omuzaki_gltf::NoHooks).into_outcome().unwrap();
    let (doc, bin) = omuzaki_io_mock::fixtures::glb_parts(&out.files[0].bytes).unwrap();
    let prim = &doc["meshes"][0]["primitives"][0];
    assert_eq!(doc["accessors"][prim["indices"].as_u64().unwrap() as usize]["componentType"], 5125, "unsigned int");
    let smile = &doc["accessors"][prim["targets"][0]["POSITION"].as_u64().unwrap() as usize];
    assert_eq!(smile["sparse"]["count"], 2);
    assert_eq!(smile["sparse"]["indices"]["componentType"], 5125, "70,000 elements need 32-bit indices");
    assert!(smile.get("bufferView").is_none(), "a sparse accessor over zeros has no base data");
    assert_eq!(smile["max"][1], 0.5);
    assert_eq!(smile["min"][1], -0.5);
    let none = &doc["accessors"][prim["targets"][1]["POSITION"].as_u64().unwrap() as usize];
    assert!(none.get("bufferView").is_none() && none.get("sparse").is_none(), "an all-zero target stores nothing");
    // Positions, normals and (32-bit) indices are 3 x 840,000 bytes; two dense targets would add as much again.
    let geometry = n * 12 * 3;
    assert!(bin.len() < geometry + 1024, "the buffer is {} bytes for {geometry} of geometry", bin.len());

    // And it reads back to the same deltas.
    let back = read(&out.files[0].bytes, &mut omuzaki_gltf::NoHooks).unwrap().scene;
    assert_eq!(back.meshes[0].primitives[0].morph_targets, scene.meshes[0].primitives[0].morph_targets);
    assert_eq!(back.meshes[0].primitives[0].mesh.indices, scene.meshes[0].primitives[0].mesh.indices);
}
