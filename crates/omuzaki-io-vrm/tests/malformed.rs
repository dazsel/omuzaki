//! Never panics on malformed input. A valid VRM 1.0 and a valid VRM 0.x are mutated systematically (every prefix of the
//! header and a spread of prefixes beyond it, flipped bytes, and every JSON value replaced by a hostile one in turn),
//! and every mutant must come back as `Ok` with a scene that passes the invariants, or as an `Err`. Debug builds run
//! these tests, so an integer overflow counts as a panic too.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use omuzaki_io_api::{ImportOptions, Importer, IoError, Limits, NoProgress, Progress, run_export};
use omuzaki_io_vrm::{Vrm0Exporter, Vrm0Importer, Vrm1Exporter, Vrm1Importer};
use serde_json::{Value, json};

use common::{avatar, export, floats, glb, parts, skinned, try_import, vrm0_friendly};

/// `OMUZAKI_FUZZ_FULL=1` tries every mutation instead of a spread (minutes; run it in release mode before a release).
fn full() -> bool {
    std::env::var_os("OMUZAKI_FUZZ_FULL").is_some()
}

/// A way to break a scene.
type Corruption = Box<dyn Fn(&mut omuzaki_io_api::IoScene)>;

/// A file, its importer's name and the importer.
struct Base {
    name: &'static str,
    bytes: Vec<u8>,
    importer: &'static dyn Importer,
}

fn bases() -> Vec<Base> {
    vec![
        Base { name: "vrm1", bytes: export(&Vrm1Exporter, &avatar().0), importer: &Vrm1Importer },
        Base { name: "vrm0", bytes: export(&Vrm0Exporter, &vrm0_friendly(avatar()).0), importer: &Vrm0Importer },
        // Files the exporters never write: eight skin influences, so the folding code is mutated too.
        Base { name: "vrm1-eight-influences", bytes: eight_influences(), importer: &Vrm1Importer },
    ]
}

/// A VRM 1.0 whose triangle has eight skin influences per vertex (`JOINTS_0` and `JOINTS_1`).
fn eight_influences() -> Vec<u8> {
    let u16s = |v: &[u16]| v.iter().flat_map(|j| j.to_le_bytes()).collect::<Vec<u8>>();
    let (doc, bin) = skinned(
        &json!({}),
        &[
            ("JOINTS_0", 3, u16s(&[0, 1, 2, 3].repeat(3)), "5123", "VEC4"),
            ("WEIGHTS_0", 3, floats(&[0.1, 0.1, 0.1, 0.1].repeat(3)), "5126", "VEC4"),
            ("JOINTS_1", 3, u16s(&[4, 5, 6, 7].repeat(3)), "5123", "VEC4"),
            ("WEIGHTS_1", 3, floats(&[0.3, 0.2, 0.0, 0.0].repeat(3)), "5126", "VEC4"),
        ],
    );
    glb(&doc, &bin)
}

/// Imports `bytes` and checks the outcome is sane; panics are caught so the failure names the mutation.
fn survives(base: &Base, bytes: &[u8], what: &str) {
    let result = catch_unwind(AssertUnwindSafe(|| try_import(base.importer, bytes, &ImportOptions::default())));
    match result {
        Err(panic) => {
            eprintln!("{}: {what} made the importer panic", base.name);
            std::panic::resume_unwind(panic);
        }
        Ok(Ok(out)) => {
            assert!(!out.scene.check().has_errors(), "{}: {what} gave a scene that breaks the invariants", base.name);
        }
        Ok(Err(_)) => {}
    }
}

#[test]
fn truncated_and_corrupted_files_never_panic() {
    for base in bases() {
        let total = base.bytes.len();
        let mut tried = 0;
        // Every prefix near the headers, then a spread of them.
        let step = if full() { 1 } else { (total / 700).max(1) };
        for len in (0..total).filter(|l| *l < 4096 || l % step == 0) {
            survives(&base, &base.bytes[..len], &format!("truncation to {len} bytes"));
            tried += 1;
        }
        // Flipped bytes: the container header and JSON start densely, the rest spread out.
        let step = if full() { 1 } else { (total / 900).max(1) };
        for at in (0..total).filter(|a| *a < 256 || a % step == 0) {
            for value in [0x00, 0xFF, 0x7F] {
                let mut bytes = base.bytes.clone();
                bytes[at] = value;
                survives(&base, &bytes, &format!("byte {at} set to {value:#x}"));
                tried += 1;
            }
        }
        eprintln!("{}: {total} bytes, {tried} truncations and byte flips", base.name);
    }
}

/// How to reach a value in a JSON document.
#[derive(Clone, Debug)]
enum Seg {
    Key(String),
    Index(usize),
}

fn collect(v: &Value, here: &mut Vec<Seg>, out: &mut Vec<Vec<Seg>>) {
    out.push(here.clone());
    match v {
        Value::Object(map) => {
            for (k, x) in map {
                here.push(Seg::Key(k.clone()));
                collect(x, here, out);
                here.pop();
            }
        }
        Value::Array(items) => {
            for (i, x) in items.iter().enumerate() {
                here.push(Seg::Index(i));
                collect(x, here, out);
                here.pop();
            }
        }
        _ => {}
    }
}

fn slot<'a>(v: &'a mut Value, path: &[Seg]) -> Option<&'a mut Value> {
    let mut cur = v;
    for seg in path {
        cur = match seg {
            Seg::Key(k) => cur.get_mut(k)?,
            Seg::Index(i) => cur.get_mut(*i)?,
        };
    }
    Some(cur)
}

/// Removes the value at `path` from its parent.
fn remove(v: &mut Value, path: &[Seg]) {
    let Some((last, parent)) = path.split_last() else { return };
    match (slot(v, parent), last) {
        (Some(Value::Object(m)), Seg::Key(k)) => {
            m.remove(k);
        }
        (Some(Value::Array(a)), Seg::Index(i)) if *i < a.len() => {
            a.remove(*i);
        }
        _ => {}
    }
}

#[test]
fn every_json_value_replaced_by_a_hostile_one_never_panics() {
    let hostile = [
        Value::Null,
        json!(-1),
        json!(0),
        json!(1),
        json!(4_294_967_296_u64),
        json!(1e30),
        json!(0.5),
        json!("x"),
        json!(true),
        json!([]),
        json!({}),
    ];
    for base in bases() {
        let (doc, bin) = parts(&base.bytes);
        let mut paths = Vec::new();
        collect(&doc, &mut Vec::new(), &mut paths);
        // Keep each file's run to a couple of thousand mutants.
        let stride = if full() { 1 } else { (paths.len() * (hostile.len() + 1) / 2500).max(1) };
        let mut tried = 0;
        for (i, path) in paths.iter().enumerate() {
            if i % stride != 0 {
                continue;
            }
            let mut mutants: Vec<(String, Value)> = Vec::new();
            for h in &hostile {
                let mut d = doc.clone();
                if let Some(s) = slot(&mut d, path) {
                    *s = h.clone();
                    mutants.push((format!("{path:?} replaced by {h}"), d));
                }
            }
            let mut d = doc.clone();
            remove(&mut d, path);
            mutants.push((format!("{path:?} removed"), d));
            for (what, d) in mutants {
                survives(&base, &glb(&d, &bin), &what);
                tried += 1;
            }
        }
        eprintln!("{}: {tried} JSON mutants", base.name);
        assert!(tried > 400, "{}: only {tried} mutants", base.name);
    }
}

#[test]
fn a_deeply_nested_document_is_an_error_not_a_stack_overflow() {
    // serde_json refuses to build such a value tree, so the JSON text is written by hand.
    let depth = 100_000;
    let text = format!("{{\"asset\":{{\"version\":\"2.0\"}},\"extras\":{}0{}}}", "[".repeat(depth), "]".repeat(depth));
    let mut padded = text.into_bytes();
    padded.resize(padded.len().next_multiple_of(4), b' ');
    let total = 12 + 8 + padded.len();
    let mut file = b"glTF".to_vec();
    file.extend(2u32.to_le_bytes());
    file.extend(u32::try_from(total).unwrap().to_le_bytes());
    file.extend(u32::try_from(padded.len()).unwrap().to_le_bytes());
    file.extend(b"JSON");
    file.extend(padded);
    for importer in [&Vrm1Importer as &dyn Importer, &Vrm0Importer] {
        assert!(matches!(try_import(importer, &file, &ImportOptions::default()), Err(IoError::Malformed { .. })));
    }
}

#[test]
fn limits_are_enforced_before_the_work_is_done() {
    let file = export(&Vrm1Exporter, &avatar().0);
    let with = |limits: Limits| try_import(&Vrm1Importer, &file, &ImportOptions::default().with_limits(limits));
    let limit_of = |r: Result<_, IoError>| match r {
        Err(IoError::Limit { what, .. }) => what,
        other => panic!("expected a limit error, got {:?}", other.map(|_: omuzaki_io_api::ImportOutcome| ())),
    };
    assert_eq!(limit_of(with(Limits { max_input_bytes: 100, ..Limits::default() })), "input size in bytes");
    assert_eq!(limit_of(with(Limits { max_nodes: 5, ..Limits::default() })), "node count");
    assert_eq!(limit_of(with(Limits { max_images: 1, ..Limits::default() })), "image count");
    assert_eq!(limit_of(with(Limits { max_vertices: 10, ..Limits::default() })), "vertex count");
    // A file may not make the importer decode more than the vertex limit implies, however its accessors are arranged.
    let (mut doc, bin) = parts(&file);
    let target = doc["meshes"][0]["primitives"][0]["targets"][0]["POSITION"].as_u64().unwrap() as usize;
    doc["accessors"][target]["count"] = json!(900_000_000_u64);
    let result = try_import(&Vrm1Importer, &glb(&doc, &bin), &ImportOptions::default());
    assert!(result.is_err(), "a lying accessor count is refused");
}

/// Cancels at the first checkpoint.
struct CancelNow;

impl Progress for CancelNow {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        true
    }
}

#[test]
fn cancellation_is_honoured_by_importers_and_exporters() {
    let (scene, _) = avatar();
    let options = ImportOptions::default();
    for (importer, bytes) in [
        (&Vrm1Importer as &dyn Importer, export(&Vrm1Exporter, &scene)),
        (&Vrm0Importer, export(&Vrm0Exporter, &vrm0_friendly(avatar()).0)),
    ] {
        let ctx = omuzaki_io_api::ImportCtx::new(&options).with_progress(&CancelNow);
        let result = importer.import(&omuzaki_io_api::ImportInput::new(&bytes), &ctx);
        assert!(matches!(result, Err(IoError::Cancelled)));
    }
    let opts = common::options();
    let ctx = omuzaki_io_api::ExportCtx::new(&opts).with_progress(&CancelNow);
    assert!(matches!(run_export(&Vrm1Exporter, &scene, &ctx), Err(IoError::Cancelled)));
    let _ = NoProgress;
}

#[test]
fn images_the_scene_cannot_carry_are_refused() {
    let (doc, bin) = parts(&export(&Vrm1Exporter, &avatar().0));
    // Swap the first image for a WebP: not PNG or JPEG.
    let view = doc["images"][0]["bufferView"].as_u64().unwrap() as usize;
    let at = doc["bufferViews"][view]["byteOffset"].as_u64().unwrap() as usize;
    let mut bin = bin;
    bin[at..at + 12].copy_from_slice(b"RIFF\0\0\0\0WEBP");
    let result = try_import(&Vrm1Importer, &glb(&doc, &bin), &ImportOptions::default());
    assert!(matches!(result, Err(IoError::Unsupported { ref what, .. }) if what.contains("image 0")));
}

#[test]
fn exporters_reject_corrupt_scenes_instead_of_panicking() {
    let (clean, ids) = avatar();
    let corruptions: Vec<(&str, Corruption)> = vec![
        ("child out of range", Box::new(|s| s.nodes[0].children.push(omuzaki_io_api::NodeIx(9_999)))),
        ("mesh out of range", Box::new(|s| s.nodes[1].mesh = Some(omuzaki_io_api::MeshIx(99)))),
        ("skin joint out of range", Box::new(|s| s.skins[0].joints[0] = omuzaki_io_api::NodeIx(9_999))),
        ("index out of range", Box::new(|s| s.meshes[0].primitives[0].mesh.indices[0] = 99_999)),
        (
            "material out of range",
            Box::new(|s| s.meshes[0].primitives[0].material = Some(omuzaki_io_api::MaterialIx(77))),
        ),
        (
            "texture out of range",
            Box::new(|s| {
                s.materials[0].base_color_texture = Some(omuzaki_io_api::TexRef::of(omuzaki_io_api::TextureIx(50)));
            }),
        ),
        (
            "humanoid node out of range",
            Box::new(|s| {
                s.avatar.as_mut().unwrap().humanoid.assign("hips", omuzaki_io_api::NodeIx(9_999)).unwrap();
            }),
        ),
        (
            "spring joint out of range",
            Box::new(|s| {
                s.avatar.as_mut().unwrap().spring_bones.chains[0].joints[0].node = omuzaki_io_api::NodeIx(9_999);
            }),
        ),
        (
            "collider group out of range",
            Box::new(|s| {
                s.avatar.as_mut().unwrap().spring_bones.collider_groups[0].colliders.push(99);
            }),
        ),
        ("thumbnail out of range", Box::new(|s| s.meta.thumbnail_image = Some(omuzaki_io_api::ImageIx(40)))),
        ("empty avatar", Box::new(|s| s.avatar = Some(omuzaki_io_api::Avatar::new()))),
        ("no avatar", Box::new(|s| s.avatar = None)),
        ("no nodes", Box::new(|s| s.nodes.clear())),
    ];
    let _ = ids;
    for (what, corrupt) in corruptions {
        let mut s = clean.clone();
        corrupt(&mut s);
        for vrm0 in [false, true] {
            let options = common::options();
            let ctx = omuzaki_io_api::ExportCtx::new(&options);
            let result = catch_unwind(AssertUnwindSafe(|| {
                if vrm0 { run_export(&Vrm0Exporter, &s, &ctx) } else { run_export(&Vrm1Exporter, &s, &ctx) }
            }));
            match result {
                Err(panic) => {
                    eprintln!("{what} (vrm0={vrm0}) made the exporter panic");
                    std::panic::resume_unwind(panic);
                }
                Ok(result) => assert!(
                    matches!(result, Err(IoError::Rejected(_))),
                    "{what} (vrm0={vrm0}) should be rejected: {:?}",
                    result.map(|o| o.files.len())
                ),
            }
        }
    }
}

#[test]
fn skin_sets_of_different_lengths_are_refused() {
    let u16s = |v: &[u16]| v.iter().flat_map(|j| j.to_le_bytes()).collect::<Vec<u8>>();
    let (doc, bin) = skinned(
        &json!({}),
        &[
            ("JOINTS_0", 3, u16s(&[0, 1, 2, 3].repeat(3)), "5123", "VEC4"),
            ("WEIGHTS_0", 3, floats(&[0.25; 12]), "5126", "VEC4"),
            ("JOINTS_1", 2, u16s(&[4, 5, 6, 7].repeat(2)), "5123", "VEC4"),
            ("WEIGHTS_1", 2, floats(&[0.0; 8]), "5126", "VEC4"),
        ],
    );
    let result = try_import(&Vrm1Importer, &glb(&doc, &bin), &ImportOptions::default());
    assert!(matches!(result, Err(IoError::Malformed { ref message, .. }) if message.contains("JOINTS_1")));
}
