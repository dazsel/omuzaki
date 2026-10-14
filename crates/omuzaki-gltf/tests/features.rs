//! The importer on hand-built glTF documents: every accessor shape, primitive mode, image source and extension the
//! importer claims to handle, and every thing it drops, checked against the finding that says so.

// The floats compared are spec defaults and values written as literals, exact by construction.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]

mod common;

use common::{Builder, PNG, base64, fake_jpeg, finding, import, import_with, try_import};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_io_api::{
    AlphaMode, Filter, ImageMime, ImportOptions, IoError, NoResolver, Severity, Shading, UvTransform, Wrap,
};
use serde_json::json;

/// A builder holding one triangle mesh whose primitive can be edited through `b.doc`.
fn triangle() -> Builder {
    let mut b = Builder::new();
    b.triangle();
    b
}

fn primitive(b: &mut Builder) -> &mut serde_json::Value {
    &mut b.doc["meshes"][0]["primitives"][0]
}

#[test]
fn interleaved_vertex_data_is_read_by_stride() {
    // Three vertices of (position, normal, uv), 32 bytes apart.
    let vertices = [
        ([0.0f32, 0.0, 0.0], [0.0f32, 0.0, 1.0], [0.0f32, 0.0]),
        ([1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [1.0, 0.0]),
        ([0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0]),
    ];
    let data: Vec<f32> = vertices.iter().flat_map(|(p, n, uv)| p.iter().chain(n).chain(uv).copied()).collect();
    let bytes: Vec<u8> = data.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut b = Builder::new();
    let view = b.view(&bytes, Some(32));
    let pos = b.accessor(view, 5126, 3, "VEC3");
    let normal = b.accessor(view, 5126, 3, "VEC3");
    b.doc["accessors"][normal]["byteOffset"] = json!(12);
    let uv = b.accessor(view, 5126, 3, "VEC2");
    b.doc["accessors"][uv]["byteOffset"] = json!(24);
    let idx = b.u16s(&[0, 1, 2], "SCALAR");
    b.push("meshes", json!({ "primitives": [{ "attributes": { "POSITION": pos, "NORMAL": normal, "TEXCOORD_0": uv }, "indices": idx }] }));
    b.push("nodes", json!({ "mesh": 0 }));
    let out = import(&b.glb());
    let mesh = &out.scene.meshes[0].primitives[0].mesh;
    assert_eq!(mesh.positions, [Vec3::ZERO, Vec3::X, Vec3::Y]);
    assert_eq!(mesh.normals, [Vec3::Z; 3]);
    assert_eq!(mesh.uvs, [Vec2::ZERO, Vec2::X, Vec2::Y]);
    assert!(!out.report.has_rule("gltf.normals_generated"));
}

#[test]
fn quantised_integer_attributes_are_decoded() {
    // KHR_mesh_quantization: i16 normalised positions, u8 normalised UVs.
    let pos: Vec<u8> = [0i16, 0, 0, 32767, 0, 0, 0, -32767, 0, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut b = Builder::new();
    let pv = b.view(&pos[..18], None);
    let p = b.accessor(pv, 5122, 3, "VEC3");
    b.doc["accessors"][p]["normalized"] = json!(true);
    let uvv = b.view(&[0, 0, 255, 0, 0, 255], None);
    let uv = b.accessor(uvv, 5121, 3, "VEC2");
    b.doc["accessors"][uv]["normalized"] = json!(true);
    let idx = b.u16s(&[0, 1, 2], "SCALAR");
    b.push("meshes", json!({ "primitives": [{ "attributes": { "POSITION": p, "TEXCOORD_0": uv }, "indices": idx }] }));
    b.push("nodes", json!({ "mesh": 0, "scale": [2.0, 2.0, 2.0] }));
    b.doc["extensionsUsed"] = json!(["KHR_mesh_quantization"]);
    b.doc["extensionsRequired"] = json!(["KHR_mesh_quantization"]);
    let out = import(&b.glb());
    let mesh = &out.scene.meshes[0].primitives[0].mesh;
    assert_eq!(mesh.positions[1], Vec3::X);
    assert_eq!(mesh.positions[2], Vec3::NEG_Y);
    assert_eq!(mesh.uvs, [Vec2::ZERO, Vec2::X, Vec2::Y]);
}

#[test]
fn sparse_morph_targets_are_expanded() {
    let mut b = triangle();
    // A morph target moving only vertex 2 by +0.5 in Z, stored sparsely (Blender does this).
    let ix = b.view(&[2, 0, 0, 0], None);
    let vals = b.view(&[0.0f32, 0.0, 0.5].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(), None);
    let acc = b.push("accessors", json!({
        "componentType": 5126, "count": 3, "type": "VEC3",
        "sparse": { "count": 1, "indices": { "bufferView": ix, "componentType": 5123 }, "values": { "bufferView": vals } }
    }));
    primitive(&mut b)["targets"] = json!([{ "POSITION": acc }]);
    b.doc["meshes"][0]["extras"] = json!({ "targetNames": ["lift"] });
    let out = import(&b.glb());
    let mesh = &out.scene.meshes[0];
    assert_eq!(mesh.primitives[0].morph_targets[0].positions, [Vec3::ZERO, Vec3::ZERO, Vec3::new(0.0, 0.0, 0.5)]);
    assert_eq!(mesh.morph_target_names, ["lift"]);
}

#[test]
fn strips_and_fans_become_triangle_lists() {
    // The repeated vertices at the end make zero-area triangles, which are dropped; the first triangle stays.
    for (mode, indices, want) in [
        (5, vec![0u16, 1, 2, 1], vec![0u32, 1, 2]),
        (6, vec![0, 1, 2, 0], vec![0, 1, 2]),
        (5, vec![0, 1, 2, 2], vec![0, 1, 2]),
    ] {
        let mut b = triangle();
        let normals = b.f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "VEC3");
        let idx = b.u16s(&indices, "SCALAR");
        let prim = primitive(&mut b);
        prim["attributes"]["NORMAL"] = json!(normals);
        prim["mode"] = json!(mode);
        prim["indices"] = json!(idx);
        let out = import(&b.glb());
        assert_eq!(out.scene.meshes[0].primitives[0].mesh.indices, want, "mode {mode}");
        assert!(out.report.has_rule("gltf.primitive_converted"));
    }
}

#[test]
fn points_and_lines_are_dropped_with_a_warning_and_the_mesh_keeps_its_place() {
    let mut b = triangle();
    primitive(&mut b)["mode"] = json!(1);
    let out = import(&b.glb());
    assert_eq!(out.scene.meshes.len(), 1);
    assert_eq!(out.scene.meshes[0].primitives.len(), 0);
    assert_eq!(finding(&out.report, "gltf.primitive_dropped").severity, Severity::Warning);
    let mut b = triangle();
    primitive(&mut b)["mode"] = json!(9);
    assert!(matches!(try_import(&b.glb()), Err(IoError::Malformed { .. })));
}

#[test]
fn several_skinning_sets_fold_to_the_four_strongest_influences() {
    let mut b = triangle();
    let j0 = b.u16s(&[0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3], "VEC4");
    let w0 = b.f32s(&[0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1, 0.1], "VEC4");
    let j1 = b.u16s(&[4, 5, 6, 7, 4, 5, 6, 7, 4, 5, 6, 7], "VEC4");
    let w1 = b.f32s(&[0.3, 0.2, 0.0, 0.0, 0.3, 0.2, 0.0, 0.0, 0.3, 0.2, 0.0, 0.0], "VEC4");
    let attrs = &mut primitive(&mut b)["attributes"];
    attrs["JOINTS_0"] = json!(j0);
    attrs["WEIGHTS_0"] = json!(w0);
    attrs["JOINTS_1"] = json!(j1);
    attrs["WEIGHTS_1"] = json!(w1);
    b.doc["nodes"][0]["skin"] = json!(0);
    let joint_nodes: Vec<usize> = (0..8).map(|_| b.push("nodes", json!({}))).collect();
    let ibm = b.f32s(
        &(0..8)
            .flat_map(|_| [1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0])
            .collect::<Vec<_>>(),
        "MAT4",
    );
    b.push("skins", json!({ "joints": joint_nodes, "inverseBindMatrices": ibm }));
    let out = import(&b.glb());
    let mesh = &out.scene.meshes[0].primitives[0].mesh;
    assert_eq!(mesh.joints[0], [4, 5, 0, 1], "0.3, 0.2, then the first two 0.1s");
    let sum: f32 = mesh.weights[0].iter().sum();
    assert!((sum - 1.0).abs() < 1e-6);
    assert_eq!(finding(&out.report, "gltf.joints_folded").severity, Severity::Warning);
    assert_eq!(out.scene.skins[0].inverse_bind.len(), 8);
}

#[test]
fn half_a_skinning_pair_is_ignored_with_a_note_and_a_short_second_set_is_malformed() {
    let mut b = triangle();
    let joints = b.u16s(&[0, 1, 0, 0].repeat(3), "VEC4");
    primitive(&mut b)["attributes"]["JOINTS_0"] = json!(joints);
    let out = import(&b.glb());
    assert!(out.scene.meshes[0].primitives[0].mesh.joints.is_empty(), "no weights: no skinning");
    assert_eq!(finding(&out.report, "gltf.attribute_dropped").severity, Severity::Info);

    // A second set with a gap before it (JOINTS_2 without JOINTS_1) is not read either.
    let weights = b.f32s(&[0.5, 0.5, 0.0, 0.0].repeat(3), "VEC4");
    primitive(&mut b)["attributes"]["WEIGHTS_0"] = json!(weights);
    primitive(&mut b)["attributes"]["JOINTS_2"] = json!(joints);
    primitive(&mut b)["attributes"]["WEIGHTS_2"] = json!(weights);
    let out = import(&b.glb());
    assert_eq!(out.scene.meshes[0].primitives[0].mesh.joints.len(), 3);
    assert!(out.report.findings.iter().any(|f| f.rule == "gltf.attribute_dropped" && f.message.contains("JOINTS_2")));

    // Every set must have one entry per vertex, and the error names the attribute.
    let short_joints = b.u16s(&[0, 1, 0, 0].repeat(2), "VEC4");
    let short_weights = b.f32s(&[0.5, 0.5, 0.0, 0.0].repeat(2), "VEC4");
    let attributes = &mut primitive(&mut b)["attributes"];
    attributes.as_object_mut().unwrap().remove("JOINTS_2");
    attributes.as_object_mut().unwrap().remove("WEIGHTS_2");
    attributes["JOINTS_1"] = json!(short_joints);
    attributes["WEIGHTS_1"] = json!(short_weights);
    let Err(IoError::Malformed { message, .. }) = try_import(&b.glb()) else { panic!("expected a malformed file") };
    assert!(message.starts_with("JOINTS_1:"), "{message}");
}

#[test]
fn matrix_nodes_are_decomposed() {
    let mut b = triangle();
    // Translation (1, 2, 3), scale 2.
    b.doc["nodes"][0]["matrix"] =
        json!([2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 1.0, 2.0, 3.0, 1.0]);
    let out = import(&b.glb());
    let t = out.scene.nodes[0].transform;
    assert_eq!((t.translation, t.scale), (Vec3::new(1.0, 2.0, 3.0), Vec3::splat(2.0)));
    assert!(out.report.has_rule("gltf.matrix_decomposed"));
}

#[test]
fn text_gltf_reads_data_uris_sidecar_buffers_and_images() {
    let mut b = triangle();
    let png_uri = format!("data:image/png;base64,{}", base64(&PNG));
    b.push("images", json!({ "uri": png_uri, "name": "inline" }));
    b.push("images", json!({ "uri": "tex%20ture.jpg" }));
    b.push("textures", json!({ "source": 0 }));
    b.push("textures", json!({ "source": 1 }));
    let (text, mut files) = b.gltf_with_sidecar();
    files.0.insert("tex ture.jpg".into(), fake_jpeg(1));
    let out = import_with(&text, &ImportOptions::default(), &files).expect("import");
    let s = &out.scene;
    assert_eq!(s.images.len(), 2);
    assert_eq!(
        (s.images[0].name.as_str(), s.images[0].mime, s.images[0].bytes.as_slice()),
        ("inline", ImageMime::Png, &PNG[..])
    );
    assert_eq!((s.images[1].name.as_str(), s.images[1].mime), ("tex ture.jpg", ImageMime::Jpeg));
    // Without a resolver the sidecar is a clear error, not a panic.
    assert!(matches!(import_with(&text, &ImportOptions::default(), &NoResolver), Err(IoError::Resource { .. })));
    // A data: URI buffer works too.
    let mut inline = triangle();
    let uri = format!("data:application/octet-stream;base64,{}", base64(&inline.bin));
    inline.doc["buffers"] = json!([{ "uri": uri, "byteLength": inline.bin.len() }]);
    assert_eq!(import(inline.doc.to_string().as_bytes()).scene.meshes.len(), 1);
}

#[test]
fn network_and_absolute_uris_are_refused() {
    for uri in ["https://example.com/a.bin", "file:///etc/passwd", "//host/a.bin", "C:/a.bin"] {
        let mut b = triangle();
        b.doc["buffers"] = json!([{ "uri": uri, "byteLength": 42 }]);
        let err = import_with(b.doc.to_string().as_bytes(), &ImportOptions::default(), &NoResolver).unwrap_err();
        assert!(matches!(err, IoError::Resource { .. }), "{uri}: {err:?}");
    }
}

#[test]
fn images_are_identified_by_signature_and_unusable_ones_drop_their_textures() {
    let mut b = triangle();
    let png = b.view(&PNG, None);
    let webp = b.view(b"RIFF\0\0\0\0WEBPVP8 ", None);
    b.push("images", json!({ "bufferView": png, "mimeType": "image/jpeg" })); // the label lies
    b.push("images", json!({ "bufferView": webp, "mimeType": "image/webp" }));
    b.push("textures", json!({ "source": 0 }));
    b.push("textures", json!({ "source": 1 }));
    b.push("textures", json!({ "extensions": { "EXT_texture_webp": { "source": 1 } } })); // no core source
    b.push(
        "materials",
        json!({
            "pbrMetallicRoughness": { "baseColorTexture": { "index": 0 }, "metallicRoughnessTexture": { "index": 1 } },
            "emissiveTexture": { "index": 2 }
        }),
    );
    let out = import(&b.glb());
    let s = &out.scene;
    assert_eq!((s.images.len(), s.images[0].mime), (1, ImageMime::Png), "signature beats the label");
    assert_eq!(s.textures.len(), 1);
    let m = &s.materials[0];
    assert!(m.base_color_texture.is_some(), "the PNG texture survives");
    assert!(m.emissive_texture.is_none());
    assert!(matches!(&m.shading, Shading::Pbr(p) if p.metallic_roughness_texture.is_none()));
    assert_eq!(finding(&out.report, "gltf.image_dropped").severity, Severity::Warning);
    assert!(out.report.has_rule("gltf.texture_dropped"));
}

#[test]
fn samplers_materials_and_texture_transforms_map_to_the_scene() {
    let mut b = triangle();
    let png = b.view(&PNG, None);
    b.push("images", json!({ "bufferView": png }));
    b.push("samplers", json!({ "magFilter": 9728, "minFilter": 9984, "wrapS": 33071, "wrapT": 33648 }));
    b.push("textures", json!({ "source": 0, "sampler": 0 }));
    b.push("textures", json!({ "source": 0 }));
    b.push("materials", json!({
        "name": "m",
        "pbrMetallicRoughness": {
            "baseColorFactor": [0.5, 0.25, 0.125, 0.5],
            "baseColorTexture": { "index": 0, "texCoord": 1, "extensions": { "KHR_texture_transform": { "offset": [0.5, 0.25], "scale": [2.0, 3.0], "rotation": 0.5 } } },
            "metallicFactor": 0.25
        },
        "normalTexture": { "index": 1, "scale": 0.5 },
        "occlusionTexture": { "index": 1 },
        "emissiveFactor": [0.5, 0.5, 0.5],
        "extensions": { "KHR_materials_emissive_strength": { "emissiveStrength": 4.0 } },
        "alphaMode": "MASK",
        "doubleSided": true
    }));
    b.doc["extensionsUsed"] = json!(["KHR_texture_transform", "KHR_materials_emissive_strength"]);
    let out = import(&b.glb());
    let s = &out.scene;
    let sampler = s.textures[0].sampler;
    assert_eq!((sampler.mag, sampler.min, sampler.mipmaps), (Filter::Nearest, Filter::Nearest, true));
    assert_eq!((sampler.wrap_u, sampler.wrap_v), (Wrap::ClampToEdge, Wrap::MirroredRepeat));
    assert_eq!(s.textures[1].sampler, omuzaki_io_api::Sampler::default(), "no sampler means the default");
    let m = &s.materials[0];
    assert_eq!(m.base_color.to_array(), [0.5, 0.25, 0.125, 0.5]);
    assert_eq!(m.emissive, Vec3::splat(2.0), "factor times strength");
    assert_eq!(m.alpha, AlphaMode::Mask { cutoff: 0.5 }, "the spec's default cutoff");
    assert!(m.double_sided);
    assert!(matches!(&m.shading, Shading::Pbr(p) if p.metallic == 0.25 && p.roughness == 1.0));
    let base = m.base_color_texture.unwrap();
    assert_eq!(base.uv_set, 1);
    assert_eq!(base.transform, Some(UvTransform { offset: Vec2::new(0.5, 0.25), scale: Vec2::new(2.0, 3.0) }));
    assert_eq!(m.normal_texture.unwrap().scale, 0.5);
    assert!(out.report.has_rule("gltf.texture_transform_dropped"));
    assert!(out.report.has_rule("gltf.occlusion_dropped"));
    // TEXCOORD_1 is not kept, so the scene check says the material reads a set that isn't there.
    assert_eq!(finding(&out.report, "scene.uv_set_dropped").severity, Severity::Warning);
}

#[test]
fn unlit_materials_and_defaults() {
    let mut b = triangle();
    b.push("materials", json!({ "extensions": { "KHR_materials_unlit": {} } }));
    b.push("materials", json!({}));
    let out = import(&b.glb());
    assert_eq!(out.scene.materials[0].shading, Shading::Unlit);
    let plain = &out.scene.materials[1];
    assert!(matches!(&plain.shading, Shading::Pbr(p) if p.metallic == 1.0 && p.roughness == 1.0), "spec defaults");
    assert_eq!((plain.base_color.to_array(), plain.alpha, plain.double_sided), ([1.0; 4], AlphaMode::Opaque, false));
}

#[test]
fn vertex_attributes_beyond_the_scene_are_reported_and_custom_ones_kept() {
    let mut b = triangle();
    let col = b.view(&[255, 0, 0, 0, 255, 0, 0, 0, 255], None);
    let color = b.accessor(col, 5121, 3, "VEC3");
    b.doc["accessors"][color]["normalized"] = json!(true);
    let uv1 = b.f32s(&[0.0; 6], "VEC2");
    let tangent = b.f32s(&[1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0], "VEC4");
    let hair = b.f32s(&[0.1, 0.2, 0.3], "SCALAR");
    let lower = b.f32s(&[0.0, 0.0, 0.0], "SCALAR");
    let attrs = &mut primitive(&mut b)["attributes"];
    attrs["COLOR_0"] = json!(color);
    attrs["TEXCOORD_1"] = json!(uv1);
    attrs["TANGENT"] = json!(tangent);
    attrs["_STRAND"] = json!(hair);
    attrs["_lower"] = json!(lower);
    let out = import(&b.glb());
    let mesh = &out.scene.meshes[0].primitives[0].mesh;
    assert_eq!(mesh.colours, [[1.0, 0.0, 0.0, 1.0], [0.0, 1.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]]);
    assert_eq!(mesh.extra.len(), 1);
    assert_eq!(
        (mesh.extra[0].name.as_str(), mesh.extra[0].components, mesh.extra[0].data.as_slice()),
        ("strand", 1, &[0.1, 0.2, 0.3][..])
    );
    let dropped: Vec<_> =
        out.report.findings.iter().filter(|f| f.rule == "gltf.attribute_dropped").map(|f| f.message.as_str()).collect();
    assert_eq!(dropped.len(), 3, "{dropped:?}");
}

#[test]
fn damaged_geometry_is_repaired_or_dropped_with_a_finding() {
    let mut b = triangle();
    let idx = b.u16s(&[0, 1, 2, 0, 1], "SCALAR"); // two stray indices
    primitive(&mut b)["indices"] = json!(idx);
    let out = import(&b.glb());
    assert_eq!(out.scene.meshes[0].primitives[0].mesh.indices, [0, 1, 2]);
    assert_eq!(finding(&out.report, "gltf.indices_truncated").severity, Severity::Warning);
    // A primitive with no indices uses 0..n; with one stray index and too few for a triangle it is dropped.
    let mut b = triangle();
    let idx = b.u16s(&[0, 1], "SCALAR");
    primitive(&mut b)["indices"] = json!(idx);
    assert_eq!(import(&b.glb()).scene.meshes[0].primitives.len(), 0, "too few indices for a triangle");
    // An index past the last vertex is malformed, with or without normals to generate.
    let mut b = triangle();
    let idx = b.u16s(&[0, 1, 9], "SCALAR");
    primitive(&mut b)["indices"] = json!(idx);
    assert!(matches!(try_import(&b.glb()), Err(IoError::Malformed { .. })));
    let normals = b.f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0], "VEC3");
    primitive(&mut b)["attributes"]["NORMAL"] = json!(normals);
    let Err(IoError::Malformed { message, at }) = try_import(&b.glb()) else { panic!("expected a malformed file") };
    assert!(message.contains("index 9 is past the last vertex (2)"), "{message}");
    assert!(at.is_some_and(|at| at.ends_with("/indices")));
}

#[test]
fn what_the_scene_cannot_hold_is_reported() {
    let mut b = triangle();
    b.doc["animations"] = json!([{ "channels": [], "samplers": [] }]);
    b.doc["cameras"] = json!([{ "type": "perspective" }]);
    b.doc["scenes"] = json!([{ "nodes": [0] }, { "nodes": [] }]);
    b.doc["meshes"][0]["weights"] = json!([0.5]);
    b.doc["meshes"][0]["extras"] = json!({ "other": 1 });
    b.doc["meshes"][0]["extensions"] = json!({ "EXT_on_a_mesh": {} });
    let out = import(&b.glb());
    assert_eq!(finding(&out.report, "gltf.animations_dropped").severity, Severity::Warning);
    assert_eq!(finding(&out.report, "gltf.cameras_dropped").severity, Severity::Info);
    assert_eq!(finding(&out.report, "gltf.scenes_dropped").severity, Severity::Info);
    assert_eq!(finding(&out.report, "gltf.morph_weights_dropped").severity, Severity::Warning);
    assert!(out.report.has_rule("gltf.extras_dropped") && out.report.has_rule("gltf.extension_dropped"));
}

#[test]
fn extensions_are_carried_dropped_or_refused() {
    // Unknown optional extensions and extras ride through at every level that has a bag.
    let mut b = triangle();
    b.doc["extensionsUsed"] = json!(["EXT_studio_tag", "KHR_materials_sheen", "VRMC_vrm", "EXT_mesh_gpu_instancing"]);
    b.doc["extensions"] = json!({ "EXT_studio_tag": { "shot": 7 }, "VRMC_vrm": { "specVersion": "1.0" } });
    b.doc["extras"] = json!({ "pipeline": "v2" });
    b.doc["nodes"][0]["extras"] = json!({ "rig": "hand" });
    b.doc["nodes"][0]["extensions"] = json!({ "EXT_mesh_gpu_instancing": { "attributes": { "TRANSLATION": 1 } } });
    b.push(
        "materials",
        json!({ "extensions": { "KHR_materials_sheen": { "sheenRoughnessFactor": 0.8 }, "VRMC_materials_mtoon": {} } }),
    );
    let out = import(&b.glb());
    let s = &out.scene;
    // Plain library, no hooks: a format's own extensions are carried like any other (omuzaki-io-gltf drops VRM's).
    assert_eq!(s.passthrough.extensions.keys().collect::<Vec<_>>(), ["EXT_studio_tag", "VRMC_vrm"]);
    assert_eq!(s.passthrough.extras, Some(json!({ "pipeline": "v2" })));
    assert!(s.nodes[0].passthrough.extensions.is_empty(), "a buffer-bound extension would dangle");
    assert_eq!(s.nodes[0].passthrough.extras, Some(json!({ "rig": "hand" })));
    assert_eq!(
        s.materials[0].passthrough.extensions.keys().collect::<Vec<_>>(),
        ["KHR_materials_sheen", "VRMC_materials_mtoon"]
    );
    let dropped: Vec<_> = out.report.findings.iter().filter(|f| f.rule == "gltf.extension_dropped").collect();
    assert_eq!(dropped.len(), 1, "{dropped:?}");
    assert_eq!(dropped[0].severity, Severity::Warning);
    assert!(dropped[0].message.contains("EXT_mesh_gpu_instancing"));
    assert_eq!(finding(&out.report, "gltf.passthrough").severity, Severity::Info);

    // A required extension the importer doesn't read is refused, with the way out for compression.
    for (ext, hint) in [("KHR_draco_mesh_compression", "gltf-transform"), ("EXT_fancy", "support")] {
        let mut b = triangle();
        b.doc["extensionsRequired"] = json!([ext]);
        let Err(IoError::Unsupported { what, hint: found }) = try_import(&b.glb()) else {
            panic!("expected a refusal")
        };
        assert!(what.contains(ext) && found.is_some_and(|h| h.contains(hint)), "{ext}");
    }
}

#[test]
fn an_unindexed_primitive_gets_sequential_indices_and_flat_normals() {
    let mut b = triangle();
    primitive(&mut b).as_object_mut().unwrap().remove("indices");
    let out = import(&b.glb());
    let mesh = &out.scene.meshes[0].primitives[0].mesh;
    assert_eq!((mesh.indices.as_slice(), mesh.normals.as_slice()), (&[0, 1, 2][..], &[Vec3::Z; 3][..]));
}

#[test]
fn json_quirks_are_tolerated() {
    // A byte-order mark, surrounding whitespace and trailing NULs around a text glTF.
    let (gltf, files) = triangle().gltf_with_sidecar();
    let padded = [b"\xEF\xBB\xBF  ".as_slice(), gltf.as_slice(), b"\n\0\0"].concat();
    assert_eq!(import_with(&padded, &ImportOptions::default(), &files).unwrap().scene.meshes.len(), 1);
}

#[test]
fn versions_and_structure_are_validated() {
    let mut b = triangle();
    b.doc["asset"]["version"] = json!("1.0");
    assert!(matches!(try_import(&b.glb()), Err(IoError::Unsupported { .. })));
    let mut b = triangle();
    b.doc["asset"] = json!({});
    assert!(matches!(try_import(&b.glb()), Err(IoError::Malformed { .. })));
    let mut b = triangle();
    b.doc["scene"] = json!(4);
    assert!(matches!(try_import(&b.glb()), Err(IoError::Malformed { .. })));
    let mut b = triangle();
    b.doc["nodes"][0]["rotation"] = json!([0.0, 0.0, 0.0, 0.0]);
    assert!(matches!(try_import(&b.glb()), Err(IoError::Malformed { .. })));
    assert!(matches!(try_import(b"{ not json"), Err(IoError::Malformed { .. })));
    assert!(matches!(try_import(b""), Err(IoError::Malformed { .. })));
}

#[test]
fn a_primitive_without_a_position_attribute_is_malformed() {
    let mut b = triangle();
    primitive(&mut b)["attributes"] = json!({});
    let Err(IoError::Malformed { at, .. }) = try_import(&b.glb()) else { panic!("expected malformed") };
    assert_eq!(at.as_deref(), Some("/meshes/0/primitives/0/attributes"));
}
