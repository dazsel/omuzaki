//! Hostile and broken input: truncation and byte mutation of real files, every number in a document replaced by nasty
//! values, declared sizes that lie, and the limits that must fire before anything is allocated. The rule under test is
//! simple: the importer returns a value for any bytes, and never panics.

// Index arithmetic on small test buffers cannot truncate.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation)]

mod common;

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};

use common::{
    Builder, PNG, PlainExporter, PlainImporter, base64, export_files, export_glb, import_with, random_scene,
    sparse_morph_scene, try_import,
};
use omuzaki_core::rng::Rng;
use omuzaki_io_api::{
    ExportCtx, ImportCtx, ImportInput, ImportOptions, IoError, Limits, NoResolver, Progress, run_export, run_import,
};
use omuzaki_io_mock::fixtures::{glb_from_json, glb_parts, options_without_face_channels};
use serde_json::{Value, json};

/// Limits small enough that a lie about a size is refused instantly, so a mutation loop can't allocate gigabytes.
fn tight() -> ImportOptions {
    ImportOptions::default().with_limits(Limits {
        max_input_bytes: 1 << 22,
        max_nodes: 200,
        max_vertices: 2000,
        max_images: 20,
    })
}

thread_local! {
    /// How many inputs this test (each test has its own thread) has fed the importer.
    static TRIED: Cell<u64> = const { Cell::new(0) };
}

/// Prints how many hostile inputs the calling test tried, and fails if that is under `at_least`, so a change to the
/// corpus or the step sizes can't quietly shrink what the suite covers.
fn tried(label: &str, at_least: u64) {
    let n = TRIED.get();
    eprintln!("{label}: {n} hostile inputs, none panicked");
    assert!(n >= at_least, "{label} tried only {n} inputs; the suite promises {at_least}");
}

/// Runs the importer on `bytes`, failing the test with `label` if it panics. The result itself doesn't matter.
fn survives(label: &str, bytes: &[u8], options: &ImportOptions) {
    TRIED.set(TRIED.get() + 1);
    let ran = catch_unwind(AssertUnwindSafe(|| {
        let _ = import_with(bytes, options, &NoResolver);
    }));
    assert!(ran.is_ok(), "the importer panicked on {label}");
}

/// Documents that between them use every code path of the importer: exported random scenes (rich in everything the
/// scene can hold) and hand-built files with the shapes only a foreign exporter writes.
fn corpus() -> Vec<(Value, Vec<u8>)> {
    let mut docs: Vec<(Value, Vec<u8>)> =
        (0..5).map(|seed| glb_parts(&export_glb(&random_scene(seed * 7 + 1))).unwrap()).collect();
    docs.push(glb_parts(&export_glb(&sparse_morph_scene())).unwrap());
    docs.extend(foreign_documents());
    docs
}

fn foreign_documents() -> Vec<(Value, Vec<u8>)> {
    let mut docs = Vec::new();
    // Interleaved, normalised, quantised, strip, fan, sparse and multi-set skinning, a matrix node and a webp image.
    let mut b = Builder::new();
    let pos: Vec<u8> = [0i16, 0, 0, 0, 32767, 0, 0, 0, 0, 32767, 0, 0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let pv = b.view(&pos, Some(6));
    let p = b.accessor(pv, 5122, 4, "VEC3");
    b.doc["accessors"][p]["normalized"] = json!(true);
    let strip = b.u16s(&[0, 1, 2, 3], "SCALAR");
    let fan = b.u16s(&[0, 1, 2, 3], "SCALAR");
    let sv = b.view(&[1, 0, 0, 0], None);
    let vv = b.view(&[0.0f32, 0.0, 0.5].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>(), None);
    let sparse = b.push("accessors", json!({
        "componentType": 5126, "count": 4, "type": "VEC3",
        "sparse": { "count": 1, "indices": { "bufferView": sv, "componentType": 5123 }, "values": { "bufferView": vv } }
    }));
    let j = b.u16s(&[0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3], "VEC4");
    let w = b.f32s(&[0.25; 16], "VEC4");
    let png = b.view(&PNG, None);
    let webp = b.view(b"RIFF\0\0\0\0WEBP", None);
    b.push("images", json!({ "bufferView": png }));
    b.push("images", json!({ "bufferView": webp }));
    b.push("images", json!({ "uri": format!("data:image/png;base64,{}", base64(&PNG)) }));
    b.push("samplers", json!({ "magFilter": 9728, "minFilter": 9987, "wrapS": 33071 }));
    b.push("textures", json!({ "source": 0, "sampler": 0 }));
    b.push("textures", json!({ "source": 1 }));
    b.push("materials", json!({
        "pbrMetallicRoughness": { "baseColorTexture": { "index": 0, "extensions": { "KHR_texture_transform": { "offset": [1.0, 0.0], "rotation": 1.0 } } } },
        "normalTexture": { "index": 1, "scale": 2.0 },
        "extensions": { "KHR_materials_unlit": {}, "KHR_materials_emissive_strength": { "emissiveStrength": 3.0 }, "VRMC_materials_mtoon": {} },
        "emissiveFactor": [1.0, 0.5, 0.0]
    }));
    for (mode, indices) in [(5, strip), (6, fan), (4, strip)] {
        b.push(
            "meshes",
            json!({ "extras": { "targetNames": ["a"] }, "primitives": [{
            "attributes": { "POSITION": p, "JOINTS_0": j, "WEIGHTS_0": w, "JOINTS_1": j, "WEIGHTS_1": w, "_HAIR": w },
            "indices": indices, "mode": mode, "material": 0, "targets": [{ "POSITION": sparse }]
        }] }),
        );
    }
    b.push("nodes", json!({ "mesh": 0, "skin": 0, "matrix": [2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 1.0, 2.0, 3.0, 1.0], "children": [1] }));
    b.push(
        "nodes",
        json!({ "mesh": 1, "rotation": [0.0, 0.0, 0.0, 2.0], "extensions": { "EXT_x": {} }, "extras": {} }),
    );
    b.push("skins", json!({ "joints": [0, 1] }));
    b.doc["scenes"] = json!([{ "nodes": [0] }, { "nodes": [1] }]);
    b.doc["animations"] = json!([{}]);
    b.doc["extensionsUsed"] = json!(["KHR_materials_unlit", "KHR_texture_transform", "KHR_mesh_quantization"]);
    docs.push((b.finish().clone(), b.bin.clone()));
    // The same file as text with a sidecar buffer.
    docs
}

#[test]
fn truncating_a_file_anywhere_never_panics() {
    let options = tight();
    for (n, (doc, bin)) in corpus().into_iter().enumerate() {
        let file = glb_from_json(&doc, &bin);
        for cut in (0..file.len()).step_by(1 + file.len() / 3000) {
            survives(&format!("file {n} cut at {cut}"), &file[..cut], &options);
        }
        let text = doc.to_string();
        for cut in (0..text.len()).step_by(1 + text.len() / 1500) {
            survives(&format!("text {n} cut at {cut}"), &text.as_bytes()[..cut], &options);
        }
    }
    tried("truncations", 25_000);
}

#[test]
fn mutating_any_byte_never_panics() {
    let options = tight();
    for (n, (doc, bin)) in corpus().into_iter().enumerate() {
        let file = glb_from_json(&doc, &bin);
        let mut mutated = file.clone();
        for at in (0..file.len()).step_by(1 + file.len() / 1200) {
            for value in [0x00, 0xFF, 0x7F, file[at] ^ 0x01, file[at].wrapping_add(0x40)] {
                mutated[at] = value;
                survives(&format!("file {n} byte {at} = {value:#x}"), &mutated, &options);
            }
            mutated[at] = file[at];
        }
    }
    tried("single-byte mutations", 35_000);
}

#[test]
fn random_multi_byte_damage_never_panics() {
    let options = tight();
    let mut rng = Rng::new(0x5EED);
    for (n, (doc, bin)) in corpus().into_iter().enumerate() {
        let file = glb_from_json(&doc, &bin);
        for round in 0..300 {
            let mut mutated = file.clone();
            for _ in 0..=rng.next_u64() % 8 {
                let at = (rng.next_u64() % mutated.len() as u64) as usize;
                mutated[at] = (rng.next_u64() & 0xFF) as u8;
            }
            survives(&format!("file {n} round {round}"), &mutated, &options);
        }
    }
    tried("multi-byte damages", 2_000);
}

/// Every value in the document, as a path of keys and indices.
fn leaf_paths(value: &Value, here: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                here.push(key.clone());
                leaf_paths(child, here, out);
                here.pop();
            }
            out.push(here.clone());
        }
        Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                here.push(i.to_string());
                leaf_paths(child, here, out);
                here.pop();
            }
            out.push(here.clone());
        }
        _ => out.push(here.clone()),
    }
}

fn at_path<'a>(doc: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    path.iter().try_fold(doc, |v, key| match v {
        Value::Object(map) => map.get_mut(key),
        Value::Array(items) => items.get_mut(key.parse::<usize>().ok()?),
        _ => None,
    })
}

#[test]
fn replacing_any_value_in_the_document_never_panics() {
    let options = tight();
    // Negative, zero and small indices, sizes at the edges of u32 and of what JSON numbers hold exactly, wrong types.
    #[rustfmt::skip]
    let nasty = [
        json!(-1), json!(0), json!(1), json!(3), json!(65_535), json!(2_147_483_647), json!(4_294_967_296_u64), json!(1e30),
        json!(0.5), json!(null), json!(true), json!("x"), json!([]), json!({}), json!([1, 2]), json!({"index": 0}),
    ];
    for (n, (doc, bin)) in corpus().into_iter().enumerate() {
        let mut paths = Vec::new();
        leaf_paths(&doc, &mut Vec::new(), &mut paths);
        for path in paths.iter().filter(|p| !p.is_empty()).step_by(1 + paths.len() / 700) {
            // Delete the value, then replace it with each nasty one.
            let mut variants = vec![None];
            variants.extend(nasty.iter().cloned().map(Some));
            for replacement in variants {
                let mut edited = doc.clone();
                let (last, parent) = path.split_last().unwrap();
                let Some(holder) = at_path(&mut edited, parent) else { continue };
                match (holder, &replacement) {
                    (Value::Object(map), Some(v)) => {
                        map.insert(last.clone(), v.clone());
                    }
                    (Value::Object(map), None) => {
                        map.remove(last);
                    }
                    (Value::Array(items), Some(v)) => {
                        if let Some(slot) = last.parse::<usize>().ok().and_then(|i| items.get_mut(i)) {
                            *slot = v.clone();
                        }
                    }
                    (Value::Array(items), None) => {
                        if let Some(i) = last.parse::<usize>().ok().filter(|&i| i < items.len()) {
                            items.remove(i);
                        }
                    }
                    _ => {}
                }
                survives(&format!("doc {n}, {path:?} -> {replacement:?}"), &glb_from_json(&edited, &bin), &options);
            }
        }
    }
    tried("documents with one value replaced or deleted", 35_000);
}

#[test]
fn sizes_the_file_lies_about_are_refused_before_anything_is_allocated() {
    let default = ImportOptions::default();
    // Four billion vertices claimed by a 100-byte file.
    let mut b = Builder::new();
    b.triangle();
    b.doc["accessors"][0]["count"] = json!(4_000_000_000_u64);
    assert!(matches!(import_with(&b.glb(), &default, &NoResolver), Err(IoError::Limit { what: "vertex count", .. })));
    // A count under the limit but over the buffer: caught against the data, still without allocating.
    b.doc["accessors"][0]["count"] = json!(7_000_000);
    assert!(matches!(import_with(&b.glb(), &default, &NoResolver), Err(IoError::Malformed { .. })));
    // Indices.
    let mut b = Builder::new();
    b.triangle();
    b.doc["accessors"][1]["count"] = json!(4_000_000_000_u64);
    assert!(matches!(import_with(&b.glb(), &default, &NoResolver), Err(IoError::Limit { what: "index count", .. })));
    // A buffer that claims more than the file has.
    let mut b = Builder::new();
    b.triangle();
    let mut doc = b.finish().clone();
    doc["buffers"][0]["byteLength"] = json!(4_000_000_000_u64);
    assert!(matches!(import_with(&glb_from_json(&doc, &b.bin), &default, &NoResolver), Err(IoError::Malformed { .. })));
}

#[test]
fn a_sparse_count_cannot_exceed_its_accessor() {
    let mut b = Builder::new();
    b.triangle();
    let ix = b.view(&[0, 0, 0, 0], None);
    let vals = b.view(&[0u8; 12], None);
    b.doc["accessors"][0]["sparse"] = json!({ "count": 4_000_000_000_u64, "indices": { "bufferView": ix, "componentType": 5125 }, "values": { "bufferView": vals } });
    assert!(matches!(try_import(&b.glb()), Err(IoError::Malformed { .. })));
}

#[test]
fn a_morph_target_bomb_hits_the_delta_budget() {
    // 200 targets that all name the same accessor: a few kilobytes of JSON that would decode into 200 copies.
    let mut b = Builder::new();
    b.triangle();
    let targets: Vec<Value> = (0..200).map(|_| json!({ "POSITION": 0 })).collect();
    b.doc["meshes"][0]["primitives"][0]["targets"] = Value::Array(targets);
    let options = ImportOptions::default().with_limits(Limits { max_vertices: 100, ..Limits::default() });
    // 3 vertices x 200 targets = 600 deltas, against a budget of 4 x 100.
    assert!(matches!(
        import_with(&b.glb(), &options, &NoResolver),
        Err(IoError::Limit { what: "morph target delta count", .. })
    ));
}

#[test]
fn a_file_of_endless_nodes_or_images_is_refused_by_count() {
    let nodes = format!(r#"{{"asset":{{"version":"2.0"}},"nodes":[{}]}}"#, vec!["{}"; 5000].join(","));
    let options = ImportOptions::default().with_limits(Limits { max_nodes: 100, ..Limits::default() });
    assert!(matches!(
        import_with(nodes.as_bytes(), &options, &NoResolver),
        Err(IoError::Limit { what: "node count", got: 5000, .. })
    ));
    let images = format!(r#"{{"asset":{{"version":"2.0"}},"images":[{}]}}"#, vec![r#"{"uri":"a.png"}"#; 50].join(","));
    let options = ImportOptions::default().with_limits(Limits { max_images: 10, ..Limits::default() });
    assert!(matches!(
        import_with(images.as_bytes(), &options, &NoResolver),
        Err(IoError::Limit { what: "image count", .. })
    ));
}

#[test]
fn input_and_sidecar_sizes_are_capped() {
    let mut b = Builder::new();
    b.triangle();
    let file = b.glb();
    let tiny = ImportOptions::default().with_limits(Limits { max_input_bytes: 16, ..Limits::default() });
    assert!(matches!(import_with(&file, &tiny, &NoResolver), Err(IoError::Limit { what: "input size in bytes", .. })));
    // A data: URI that decodes past the cap is refused before it is decoded.
    let mut b = Builder::new();
    b.triangle();
    b.doc["buffers"] = json!([{ "uri": format!("data:;base64,{}", "A".repeat(4000)), "byteLength": 100 }]);
    let small = ImportOptions::default().with_limits(Limits { max_input_bytes: 4096, ..Limits::default() });
    let text = b.doc.to_string();
    assert!(matches!(
        import_with(text.as_bytes(), &small, &NoResolver),
        Err(IoError::Malformed { .. } | IoError::Limit { .. })
    ));
    // Many sidecar reads add up against the same cap.
    let mut b = Builder::new();
    b.triangle();
    let (text, mut files) = b.gltf_with_sidecar();
    files.0.insert("scene.bin".into(), vec![0; 5000]);
    let doc: Value = serde_json::from_slice(&text).unwrap();
    let mut doc = doc;
    doc["buffers"] = json!([{ "uri": "scene.bin", "byteLength": 100 }, { "uri": "scene.bin", "byteLength": 100 }]);
    let capped = ImportOptions::default().with_limits(Limits { max_input_bytes: 8000, ..Limits::default() });
    let err = import_with(doc.to_string().as_bytes(), &capped, &files).unwrap_err();
    assert!(matches!(err, IoError::Limit { what: "total size of external data in bytes", .. }), "{err:?}");
}

#[test]
fn absurd_nesting_and_garbage_are_malformed_not_a_stack_overflow() {
    let deep = format!("{}{}", "[".repeat(200_000), "]".repeat(200_000));
    assert!(matches!(try_import(deep.as_bytes()), Err(IoError::Malformed { .. })));
    let nested = format!(r#"{{"asset":{{"version":"2.0"}},"extras":{}{}}}"#, "[".repeat(5000), "]".repeat(5000));
    assert!(matches!(try_import(nested.as_bytes()), Err(IoError::Malformed { .. })));
    for garbage in [&b"\x00\x01\x02"[..], b"glTF\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF", &[0xFF; 64], b"PK\x03\x04"] {
        assert!(try_import(garbage).is_err());
    }
}

struct Cancelled(AtomicBool);

impl Progress for Cancelled {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[test]
fn cancellation_stops_import_and_export() {
    let cancelled = Cancelled(AtomicBool::new(true));
    let scene = random_scene(2);
    let file = export_glb(&scene);
    let options = ImportOptions::default();
    let ctx = ImportCtx::new(&options).with_progress(&cancelled);
    assert_eq!(run_import(&PlainImporter, &ImportInput::new(&file), &ctx).unwrap_err(), IoError::Cancelled);
    let export_options = options_without_face_channels();
    let ctx = ExportCtx::new(&export_options).with_progress(&cancelled);
    assert_eq!(run_export(&PlainExporter, &scene, &ctx).unwrap_err(), IoError::Cancelled);
}

#[test]
fn reports_are_deterministic() {
    let (doc, bin) = foreign_documents().remove(0);
    let file = glb_from_json(&doc, &bin);
    let first = try_import(&file);
    let second = try_import(&file);
    assert_eq!(first.map(|o| o.report), second.map(|o| o.report));
    assert!(export_files(&random_scene(4), &options_without_face_channels()).is_ok());
}

#[test]
fn naming_one_buffer_view_as_many_images_hits_the_copy_budget() {
    // A 40 KB view named by 200 images would copy 8 MB; the budget is the input limit.
    let mut b = Builder::new();
    b.triangle();
    let view = b.view(&[PNG.to_vec(), vec![0; 40_000]].concat(), None);
    for _ in 0..200 {
        b.push("images", json!({ "bufferView": view }));
    }
    let options = ImportOptions::default().with_limits(Limits { max_input_bytes: 1 << 20, ..Limits::default() });
    let err = import_with(&b.glb(), &options, &NoResolver).unwrap_err();
    assert!(matches!(err, IoError::Limit { what: "total size of external data in bytes", .. }), "{err:?}");
}
