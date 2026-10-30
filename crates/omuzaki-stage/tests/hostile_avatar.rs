//! Hostile models through the whole bring-your-own-model path: a file is read by the real importer, prepared as an
//! avatar (`to_avatar`, as `io.import {as: avatar}` prepares it), staged on the mock renderer and run for a few frames.
//! The importers' own suites mutate their files against the reader alone; this one checks that what a reader lets
//! through can't break anything after it. The rule: any bytes give a value or an error at every step, never a panic,
//! and whatever stages is given back whole.
//!
//! The files are the kit's own character, written as VRM 1.0 and as FBX; nothing third-party.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_possible_truncation, reason = "offsets into test buffers of a few megabytes")]

#[path = "avatar/fbx.rs"]
mod fbx;
#[path = "avatar/kit.rs"]
mod kit;

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_io_api::{ImportCtx, ImportInput, ImportOptions, Importer, Limits, NoResolver, Resolver, run_import};
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_render_api::{AlphaEncoding, Camera, MtoonSupport, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::avatar::AvatarStage;
use omuzaki_stage::frame::{FrameInput, StageClock};
use serde_json::{Value, json};

/// Limits small enough that a lie about a size is refused at once, and large enough for the kit's character.
fn options() -> ImportOptions {
    ImportOptions::default().with_limits(Limits {
        max_input_bytes: 1 << 24,
        max_nodes: 2_000,
        max_vertices: 200_000,
        max_images: 64,
    })
}

thread_local! {
    /// How many inputs this test fed the path, and how many of them staged.
    static TRIED: Cell<u64> = const { Cell::new(0) };
    static STAGED: Cell<u64> = const { Cell::new(0) };
}

/// Fails unless at least `at_least` inputs were tried, so a change to the corpus can't quietly shrink the suite, and
/// unless some of them got as far as the stage (a suite whose every input the reader refuses tests only the reader).
fn tried(label: &str, at_least: u64) {
    let (n, staged) = (TRIED.get(), STAGED.get());
    eprintln!("{label}: {n} hostile inputs, {staged} staged, none panicked");
    assert!(n >= at_least, "{label} tried only {n} inputs; the suite promises {at_least}");
    assert!(staged > 0, "{label}: no input reached the stage");
}

/// Reads `bytes` with `importer`, prepares an avatar, stages it and runs three frames, failing the test with `label`
/// if any step panics or a staged avatar can't be given back.
fn survives(label: &str, importer: &dyn Importer, format: SourceFormat, bytes: &[u8], resolver: &dyn Resolver) {
    TRIED.set(TRIED.get() + 1);
    let ran = catch_unwind(AssertUnwindSafe(|| {
        let options = options();
        let ctx = ImportCtx::new(&options).with_resolver(resolver);
        let Ok(outcome) = run_import(importer, &ImportInput::new(bytes).named("hostile"), &ctx) else { return };
        let def = AvatarDef::new(AvatarSource::new(omuzaki_store_api::ContentHash([3; 32]), format, "hostile"));
        let Ok((avatar, _)) = to_avatar(&outcome.scene, &def, MtoonSupport::ALL) else { return };
        let mut renderer = MockRenderer::new();
        let target =
            renderer.create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight }).unwrap();
        let camera = Camera::look_at(
            Vec3::new(0.0, 0.9, 4.0),
            Vec3::new(0.0, 0.9, 0.0),
            Vec3::Y,
            Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
        );
        let staged = AvatarStage::new(
            &mut renderer,
            avatar,
            Scene::new(camera),
            target,
            Quality::offline(),
            StageClock::fixed(60.0),
        );
        let Ok(mut stage) = staged else { return };
        STAGED.set(STAGED.get() + 1);
        let input = FrameInput { dt: 1.0 / 60.0, ..FrameInput::default() };
        for _ in 0..3 {
            if stage.tick(&mut renderer, &input).is_err() {
                break;
            }
        }
        stage.release(&mut renderer).expect("a staged avatar is given back");
    }));
    assert!(ran.is_ok(), "the avatar path panicked on {label}");
}

/// A GLB's JSON and binary chunks.
fn split(glb: &[u8]) -> (Value, Vec<u8>) {
    let word = |at: usize| u32::from_le_bytes(glb[at..at + 4].try_into().unwrap()) as usize;
    let json_len = word(12);
    let json = serde_json::from_slice(&glb[20..20 + json_len]).unwrap();
    let bin_at = 20 + json_len;
    let bin = if glb.len() > bin_at + 8 { glb[bin_at + 8..bin_at + 8 + word(bin_at)].to_vec() } else { Vec::new() };
    (json, bin)
}

/// A GLB of `json` and `bin`, each chunk padded to four bytes as the format asks.
fn join(json: &Value, bin: &[u8]) -> Vec<u8> {
    let mut text = json.to_string().into_bytes();
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let mut bin = bin.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + text.len() + if bin.is_empty() { 0 } else { 8 + bin.len() };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2_u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&text);
    if !bin.is_empty() {
        out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        out.extend_from_slice(b"BIN\0");
        out.extend_from_slice(&bin);
    }
    out
}

/// The path of every leaf of `value` (numbers, strings, booleans, nulls, empty containers), as keys and indices.
fn leaves(value: &Value, at: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, child) in map {
                at.push(key.clone());
                leaves(child, at, out);
                at.pop();
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for (ix, child) in items.iter().enumerate() {
                at.push(ix.to_string());
                leaves(child, at, out);
                at.pop();
            }
        }
        _ => out.push(at.clone()),
    }
}

/// The value at `path` in `value`, to change.
fn at_path<'a>(value: &'a mut Value, path: &[String]) -> Option<&'a mut Value> {
    path.iter().try_fold(value, |v, key| match v {
        Value::Object(map) => map.get_mut(key),
        Value::Array(items) => items.get_mut(key.parse::<usize>().ok()?),
        _ => None,
    })
}

/// The kit's character as a VRM 1.0.
fn vrm() -> Vec<u8> {
    kit::vrm_bytes(&kit::kit())
}

#[test]
fn the_unmutated_files_stage() {
    let bytes = vrm();
    survives("the kit's VRM", &omuzaki_io_vrm::Vrm1Importer, SourceFormat::Vrm1, &bytes, &NoResolver);
    let (fbx, folder) = fbx::fbx(&kit::as_vrm(&kit::kit()));
    survives("the kit's FBX", &omuzaki_io_fbx::FbxImporter, SourceFormat::Fbx, &fbx, &folder);
    assert_eq!(STAGED.get(), 2, "both files stage as they are");
}

/// Every leaf of the VRM's JSON the importer reads for the avatar (the humanoid, expressions, look-at, springs,
/// constraints, skins, nodes, accessors and meshes), deleted and then replaced by each of a set of nasty values.
#[test]
fn every_avatar_field_of_a_vrm_replaced_by_nasty_values() {
    let nasty = [
        json!(-1),
        json!(0),
        json!(1),
        json!(7),
        json!(65_535),
        json!(4_294_967_296_u64),
        json!(-1e30),
        json!(1e30),
        json!(0.5),
        json!(null),
        json!(true),
        json!("leftHand"),
        json!([]),
        json!({}),
        json!({ "node": 0 }),
    ];
    let (doc, bin) = split(&vrm());
    let mut paths = Vec::new();
    leaves(&doc, &mut Vec::new(), &mut paths);
    let wanted = ["extensions", "skins", "nodes", "accessors", "meshes", "bufferViews"];
    paths.retain(|p| p.first().is_some_and(|top| wanted.contains(&top.as_str())));
    // A sample spread over every part: the kit's file has tens of thousands of leaves.
    let step = 1 + paths.len() / 600;
    for path in paths.iter().step_by(step) {
        let mut variants = vec![None];
        variants.extend(nasty.iter().cloned().map(Some));
        for replacement in variants {
            let mut edited = doc.clone();
            let (last, parent) = path.split_last().unwrap();
            let Some(holder) = at_path(&mut edited, parent) else { continue };
            match (holder, replacement) {
                (Value::Object(map), Some(v)) => {
                    map.insert(last.clone(), v);
                }
                (Value::Object(map), None) => {
                    map.remove(last);
                }
                (Value::Array(items), Some(v)) => {
                    if let Some(slot) = last.parse::<usize>().ok().and_then(|i| items.get_mut(i)) {
                        *slot = v;
                    }
                }
                (Value::Array(items), None) => {
                    if let Some(i) = last.parse::<usize>().ok().filter(|&i| i < items.len()) {
                        items.remove(i);
                    }
                }
                _ => continue,
            }
            let label = format!("{} edited", path.join("/"));
            survives(&label, &omuzaki_io_vrm::Vrm1Importer, SourceFormat::Vrm1, &join(&edited, &bin), &NoResolver);
        }
    }
    tried("VRM fields", 4_500);
}

/// The binary chunk's numbers (positions, normals, weights, joints, inverse binds and morph deltas) overwritten with
/// bit patterns that make NaN, infinities, huge and negative values and out-of-range joint indices.
#[test]
fn the_vrm_binary_chunk_overwritten_with_hostile_numbers() {
    let (doc, bin) = split(&vrm());
    let patterns: [[u8; 4]; 6] = [
        f32::NAN.to_le_bytes(),
        f32::INFINITY.to_le_bytes(),
        (-1e30_f32).to_le_bytes(),
        [0xFF; 4],
        [0x00; 4],
        1e-42_f32.to_le_bytes(),
    ];
    let mut rng = Rng::new(0x0bad_f00d);
    for round in 0..600 {
        let mut edited = bin.clone();
        let pattern = patterns[round % patterns.len()];
        // A run of words somewhere in the chunk, as a corrupted or crafted region would be.
        let words = 1 + (rng.next_u64() % 64) as usize;
        let start = (rng.next_u64() as usize % (edited.len() / 4)) * 4;
        for word in 0..words {
            let at = start + word * 4;
            if let Some(slot) = edited.get_mut(at..at + 4) {
                slot.copy_from_slice(&pattern);
            }
        }
        let label = format!("{words} words of {pattern:02x?} at byte {start}");
        survives(&label, &omuzaki_io_vrm::Vrm1Importer, SourceFormat::Vrm1, &join(&doc, &edited), &NoResolver);
    }
    tried("VRM binary", 600);
}

/// The FBX's text cut short and its numbers changed: ufbx reads what it can, and what it lets through must stage.
#[test]
fn the_fbx_cut_short_and_its_numbers_changed() {
    let (bytes, folder) = fbx::fbx(&kit::as_vrm(&kit::kit()));
    let text = String::from_utf8(bytes).expect("the fixture writes ASCII FBX");
    let mut rng = Rng::new(0xfb_0001);
    // Numbers: each edit swaps one number in the file for a nasty one.
    let numbers: Vec<(usize, usize)> = {
        let b = text.as_bytes();
        let mut spans = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
                let start = if i > 0 && b[i - 1] == b'-' { i - 1 } else { i };
                let mut end = i;
                while end < b.len() && (b[end].is_ascii_digit() || matches!(b[end], b'.' | b'e' | b'E' | b'-' | b'+')) {
                    end += 1;
                }
                spans.push((start, end));
                i = end;
            } else {
                i += 1;
            }
        }
        spans
    };
    let nasty = ["-1", "0", "1e30", "-1e30", "nan", "inf", "4294967296", "99999"];
    for round in 0..500 {
        let (start, end) = numbers[(rng.next_u64() as usize) % numbers.len()];
        let mut edited = String::with_capacity(text.len());
        edited.push_str(&text[..start]);
        edited.push_str(nasty[round % nasty.len()]);
        edited.push_str(&text[end..]);
        let label = format!("number at {start} as {}", nasty[round % nasty.len()]);
        survives(&label, &omuzaki_io_fbx::FbxImporter, SourceFormat::Fbx, edited.as_bytes(), &folder);
    }
    // Cuts: the file truncated at points spread over its length.
    for cut in (1..text.len()).step_by(1 + text.len() / 150) {
        let label = format!("cut at {cut}");
        survives(&label, &omuzaki_io_fbx::FbxImporter, SourceFormat::Fbx, &text.as_bytes()[..cut], &folder);
    }
    tried("FBX", 600);
}
