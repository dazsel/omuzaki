//! `render.turnaround`, `render.contact_sheet` and `render.compare` (spec v3 §11, #336): the loop an agent runs to see
//! what an edit did. Render, edit, render again, compare the two URIs, and lay a set out on one sheet. A kit character
//! and the same character imported from its own VRM file both go through it. No third-party model.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "avatar/kit.rs"]
mod kit;
#[path = "render/support.rs"]
mod support;

use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, Op, OpError};
use omuzaki_stage::ops::render::compare::{Compare, CompareResult};
use omuzaki_stage::ops::render::contact_sheet::{ContactSheet, ContactSheetResult};
use omuzaki_stage::ops::render::preview::{Preview, PreviewResult};
use omuzaki_stage::ops::render::turnaround::{Turnaround, TurnaroundResult};
use omuzaki_store_api::{ContentHash, Store};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use support::{Harness, TestDoc, mock_doc};

/// The mock character at `characters/mock` and its VRM export at `characters/mock_vrm`, original in the store.
fn project() -> TestDoc {
    let bytes = kit::vrm_bytes(&kit::kit());
    let def = AvatarDef::new(AvatarSource::new(ContentHash::of(&bytes), SourceFormat::Vrm1, "mock.vrm"));
    let TestDoc(doc) = mock_doc();
    let mut root = doc.root().clone();
    root["characters"]["mock_vrm"] = json!({ "avatar": serde_json::to_value(&def).unwrap() });
    TestDoc(MemDoc::from_value(root))
}

fn harness_on(renderer: Box<dyn omuzaki_render_api::Renderer>) -> Harness {
    let harness = Harness::on(project(), renderer);
    harness.store.put(&kit::vrm_bytes(&kit::kit()), kit::provenance()).expect("stored");
    harness
}

fn harness() -> Harness {
    harness_on(Box::new(omuzaki_render_mock::MockRenderer::new()))
}

fn run<O: Op + DeserializeOwned, R: DeserializeOwned>(harness: &Harness, args: Value) -> Result<R, OpError> {
    let op: O = serde_json::from_value(args).expect("the arguments read");
    let (plan, value) = harness.run(&op)?;
    assert!(plan.patch.is_empty(), "a render op changes nothing in the document");
    Ok(serde_json::from_value(value).expect("the result reads"))
}

fn picture(harness: &Harness, hash: &str) -> image::RgbaImage {
    let png = harness.store.get(&hash.parse().expect("a hash")).expect("stored");
    image::load_from_memory(&png).expect("a PNG").to_rgba8()
}

fn preview(harness: &Harness, character: &str, yaw: f32) -> PreviewResult {
    let args = json!({ "character": character, "yaw": yaw, "size": [64, 96], "quality": "offline" });
    run::<Preview, _>(harness, args).expect("previews")
}

#[test]
fn a_render_compared_with_itself_is_identical_and_a_turned_one_is_not() {
    let harness = harness();
    let front = preview(&harness, "mock", 0.0);
    let again = preview(&harness, "mock", 0.0);
    let same: CompareResult = run::<Compare, _>(&harness, json!({ "a": front.uri, "b": again.hash })).unwrap();
    assert!(same.identical && same.changed_pixels == 0 && same.bounds.is_none(), "{same:?}");
    assert_eq!(same.size, [64, 96]);

    let turned = preview(&harness, "mock", 60.0);
    let moved: CompareResult = run::<Compare, _>(&harness, json!({ "a": front.uri, "b": turned.uri })).unwrap();
    assert!(!moved.identical && moved.changed > 0.0 && moved.changed_pixels > 0, "{moved:?}");
    let [x, y, w, h] = moved.bounds.expect("a box");
    assert!(x + w <= 64 && y + h <= 96 && w > 0 && h > 0, "{moved:?}");
    for mode in ["diff", "side_by_side"] {
        let drawn: CompareResult =
            run::<Compare, _>(&harness, json!({ "a": front.uri, "b": turned.uri, "mode": mode })).unwrap();
        let image = picture(&harness, &drawn.hash);
        assert_eq!([image.width(), image.height()], drawn.size, "{mode}");
        assert_eq!(drawn.changed_pixels, moved.changed_pixels, "{mode}: the numbers don't depend on the drawing");
    }
}

#[test]
fn compare_refuses_what_it_cannot_read_or_line_up() {
    let harness = harness();
    let code = |args: Value| run::<Compare, CompareResult>(&harness, args).expect_err("refused").code;
    let small = preview(&harness, "mock", 0.0);
    let missing = ContentHash::of(b"never stored").to_string();
    assert_eq!(code(json!({ "a": "not a render", "b": small.uri })), ErrorCode::Invalid);
    assert_eq!(code(json!({ "a": missing, "b": small.uri })), ErrorCode::NotFound);
    let not_png = harness.store.put(b"GIF89a", kit::provenance()).unwrap().to_string();
    assert_eq!(code(json!({ "a": not_png, "b": small.uri })), ErrorCode::Invalid);
    let args = json!({ "character": "mock", "size": [32, 32], "quality": "live" });
    let other: PreviewResult = run::<Preview, _>(&harness, args).unwrap();
    assert_eq!(code(json!({ "a": small.uri, "b": other.uri })), ErrorCode::Invalid, "different sizes");
}

#[test]
fn a_contact_sheet_lays_renders_out_in_a_grid() {
    let harness = harness();
    let renders: Vec<String> =
        [0.0, 30.0, 60.0, 90.0, 120.0].iter().map(|yaw| preview(&harness, "mock", *yaw).uri).collect();
    let sheet: ContactSheetResult =
        run::<ContactSheet, _>(&harness, json!({ "images": renders, "labels": false })).unwrap();
    assert_eq!((sheet.columns, sheet.rows, sheet.cell), (3, 2, [64, 96]));
    assert_eq!(sheet.size, [3 * (64 + 8) + 8, 2 * (96 + 8) + 8]);
    let image = picture(&harness, &sheet.hash);
    assert_eq!([image.width(), image.height()], sheet.size);
    // The second render sits in the second cell, byte for byte.
    let second = picture(&harness, &renders[1]["omuzaki://render/".len()..renders[1].len() - 4]);
    let (x0, y0) = (8 + 64 + 8, 8);
    for (x, y, p) in second.enumerate_pixels() {
        assert_eq!(image.get_pixel(x0 + x, y0 + y), p, "({x}, {y})");
    }
}

#[test]
fn a_turnaround_renders_four_views_of_either_body_in_a_row() {
    let harness = harness();
    for (character, body) in [("mock", "kit"), ("mock_vrm", "imported")] {
        let args = json!({ "character": character, "size": [48, 80], "quality": "live" });
        let row: TurnaroundResult = run::<Turnaround, _>(&harness, args).unwrap();
        assert_eq!(row.body, body);
        assert_eq!(row.views, ["front", "three_quarter", "side", "back"]);
        assert_eq!(row.size, [4 * 48 + 3 * 8, 80]);
        let image = picture(&harness, &row.hash);
        assert_eq!([image.width(), image.height()], row.size);
    }
    let code = |args: Value| run::<Turnaround, TurnaroundResult>(&harness, args).expect_err("refused").code;
    assert_eq!(code(json!({ "character": "nobody" })), ErrorCode::NotFound);
    assert_eq!(code(json!({ "character": "mock", "framing": "hands" })), ErrorCode::Invalid);
}

#[test]
fn the_ops_are_registered_render_jobs() {
    for (name, meta) in [
        ("render.compare", Compare::meta()),
        ("render.contact_sheet", ContactSheet::meta()),
        ("render.turnaround", Turnaround::meta()),
    ] {
        assert_eq!(meta.name, name);
        assert_eq!(meta.kind, omuzaki_ops_api::OpKind::Job);
        assert_eq!(meta.scope, omuzaki_ops_api::Scope::Render);
        assert!(omuzaki_ops_api::find(name).is_some(), "{name} is registered, so MCP and the CLI list it");
    }
}

/// On the GPU: the agent's loop on the imported character. A turnaround, a preview before and after a turn of the
/// camera, their heatmap, and a sheet of all of it. With `OMUZAKI_EVIDENCE_DIR` set the pictures are written as PNGs.
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence images are written beside the test, not by an op")]
fn on_the_gpu_an_agent_sees_a_turnaround_and_what_a_change_did() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};

    let renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let harness = harness_on(Box::new(renderer));
    let args = json!({ "character": "mock_vrm", "framing": "full", "size": [160, 240], "quality": "offline" });
    let row: TurnaroundResult = run::<Turnaround, _>(&harness, args).unwrap();
    let image = picture(&harness, &row.hash);
    // Every view has the character in it: pixels that differ from the view's corner.
    for view in 0..4 {
        let x0 = view * (160 + 8);
        let corner = *image.get_pixel(x0, 0);
        let covered = (0..240)
            .flat_map(|y| (0..160).map(move |x| (x, y)))
            .filter(|&(x, y)| image.get_pixel(x0 + x, y).0.iter().zip(corner.0).any(|(a, b)| a.abs_diff(b) > 8))
            .count();
        assert!(covered > 160 * 240 / 30, "view {view}: {covered} pixels");
    }
    let shot = |yaw: f32| {
        let args =
            json!({ "character": "mock_vrm", "framing": "bust", "yaw": yaw, "size": [160, 240], "quality": "offline" });
        run::<Preview, PreviewResult>(&harness, args).unwrap()
    };
    let (before, after) = (shot(0.0), shot(25.0));
    let heat: CompareResult = run::<Compare, _>(&harness, json!({ "a": before.uri, "b": after.uri })).unwrap();
    assert!(heat.changed > 0.02, "{heat:?}");
    let sheet: ContactSheetResult =
        run::<ContactSheet, _>(&harness, json!({ "images": [before.uri, after.uri, heat.uri], "columns": 3 })).unwrap();
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        for (name, hash) in [("render-turnaround", &row.hash), ("render-compare-sheet", &sheet.hash)] {
            let png = harness.store.get(&hash.parse().unwrap()).unwrap();
            std::fs::write(dir.join(format!("{name}.png")), png).unwrap();
        }
    }
}

/// The acceptance case of #336: one parameter of the face's outline changes, and `render.compare` shows the change only
/// where the head is. More than 90% of the changed pixels fall inside the head's box (its mesh before and after the edit,
/// projected through the shot's camera, with a margin for its outline and shadow).
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence images are written beside the test, not by an op")]
#[allow(clippy::too_many_lines, reason = "one acceptance case, read top to bottom")]
fn on_the_gpu_a_face_outline_edit_changes_only_the_head() {
    use omuzaki_char::ops::character::create::CharacterCreate;
    use omuzaki_char::ops::part::set_param::PartSetParam;
    use omuzaki_char_api::{BuildServices, CharId, Quality as BuildQuality, RegistryBuilder};
    use omuzaki_core::glam::{UVec2, Vec2, Vec3, Vec4Swizzles};
    use omuzaki_core::time::{FakeClock, Seconds};
    use omuzaki_doc_api::DocWrite;
    use omuzaki_ops_api::{Actor, OpCtx};
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_render_api::{AlphaEncoding, Image, MtoonSupport};
    use omuzaki_stage::body::{BodyServices, load_body};
    use omuzaki_stage::ops::render::compare::Difference;
    use omuzaki_stage::scene::frame::frame;
    use omuzaki_stage_api::{CameraDef, Framing};

    const SIZE: [u32; 2] = [256, 320];
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Person { name: "test".to_owned() };
    let plan_into = |doc: &mut MemDoc, op: &dyn Fn(&MemDoc) -> omuzaki_ops_api::Plan| {
        let patch = op(doc).patch;
        doc.apply(&patch).expect("applies");
    };
    let mut before = MemDoc::from_value(json!({}));
    plan_into(&mut before, &|doc| {
        let create: CharacterCreate =
            serde_json::from_value(json!({ "id": "kit", "name": "Kit", "template": "standard", "seed": 7 })).unwrap();
        create.plan(&OpCtx::new(doc, &actor, &clock)).expect("creates")
    });
    let mut after = MemDoc::from_value(before.root().clone());
    plan_into(&mut after, &|doc| {
        let edit: PartSetParam = serde_json::from_value(
            json!({ "character": "kit", "part": "head", "path": "outline/exponent", "value": 1.4 }),
        )
        .unwrap();
        edit.plan(&OpCtx::new(doc, &actor, &clock)).expect("the outline edit plans")
    });

    // The head's box on screen: its mesh's bounds before and after, through the camera the preview frames.
    #[allow(clippy::cast_precision_loss, reason = "small sizes")]
    let aspect = SIZE[0] as f32 / SIZE[1] as f32;
    let store = omuzaki_store_mock::MemStore::new();
    let build = BuildServices::default();
    let services = BodyServices::new(&RegistryBuilder, &build, &store, MtoonSupport::NONE, &clock);
    let id = CharId::new("kit").unwrap();
    let (mut lo, mut hi) = (Vec2::MAX, Vec2::MIN);
    for doc in [&before, &after] {
        let landmarks = load_body(doc, &services, &id, None, BuildQuality::Offline).unwrap().landmarks().unwrap();
        let camera = frame(Framing::Bust, &landmarks, &CameraDef::preset(Framing::Bust), aspect).unwrap();
        let clip = camera.projection_matrix(aspect) * camera.view();
        let head = landmarks.head.expect("the kit has a head");
        for i in 0..8 {
            let corner = Vec3::new(
                if i & 1 == 0 { head.min.x } else { head.max.x },
                if i & 2 == 0 { head.min.y } else { head.max.y },
                if i & 4 == 0 { head.min.z } else { head.max.z },
            );
            let p = clip * corner.extend(1.0);
            let ndc = p.xy() / p.w;
            let pixel = (ndc * Vec2::new(0.5, -0.5) + 0.5) * UVec2::from(SIZE).as_vec2();
            (lo, hi) = (lo.min(pixel), hi.max(pixel));
        }
    }

    let shoot = |doc: MemDoc| {
        let renderer =
            WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
        let harness = Harness::on(TestDoc(doc), Box::new(renderer));
        let args = json!({ "character": "kit", "framing": "bust", "size": SIZE, "quality": "offline" });
        let shot: PreviewResult = run::<Preview, _>(&harness, args).unwrap();
        let png = harness.store.get(&shot.hash.parse().unwrap()).unwrap();
        (harness, shot, png)
    };
    let (_, _, png_before) = shoot(before);
    let (harness, shot_after, png_after) = shoot(after);
    let hash_before = harness.store.put(&png_before, kit::provenance()).unwrap().to_string();
    let compared: CompareResult =
        run::<Compare, _>(&harness, json!({ "a": hash_before, "b": shot_after.uri, "mode": "heatmap" })).unwrap();
    assert!(!compared.identical && compared.changed_pixels > 50, "the edit shows: {compared:?}");

    let image = |png: &[u8]| {
        let rgba = image::load_from_memory(png).unwrap().to_rgba8();
        Image::new(UVec2::from(SIZE), AlphaEncoding::Straight, rgba.into_raw()).unwrap()
    };
    let difference = Difference::of(&image(&png_before), &image(&png_after), 8);
    assert_eq!(difference.changed, compared.changed_pixels, "the op and the test count the same pixels");
    // The head's footprint is its mesh plus what the mesh does around it: its outline and the chin's shadow on the
    // neck, which fall just outside the mesh's box. A margin of 4% of the picture's height holds them (measured: the
    // farthest changed pixel is 9 pixels below the chin at 320 high).
    let margin = 0.04 * UVec2::from(SIZE).as_vec2().y;
    let inside = (0..SIZE[1])
        .flat_map(|y| (0..SIZE[0]).map(move |x| (x, y)))
        .zip(&difference.per_pixel)
        .filter(|(_, worst)| **worst > 8)
        .filter(|((x, y), _)| {
            #[allow(clippy::cast_precision_loss, reason = "small sizes")]
            let (x, y) = (*x as f32, *y as f32);
            x >= lo.x - margin && x <= hi.x + margin && y >= lo.y - margin && y <= hi.y + margin
        })
        .count();
    #[allow(clippy::cast_precision_loss, reason = "pixel counts")]
    let share = inside as f64 / difference.changed as f64;
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let sheet: ContactSheetResult = run::<ContactSheet, _>(
            &harness,
            json!({ "images": [hash_before, shot_after.uri, compared.uri], "columns": 3 }),
        )
        .unwrap();
        let png = harness.store.get(&sheet.hash.parse().unwrap()).unwrap();
        std::fs::write(dir.join("render-compare-face-outline.png"), png).unwrap();
        let args = json!({ "character": "kit", "framing": "full", "size": [192, 320], "quality": "offline" });
        let row: TurnaroundResult = run::<Turnaround, _>(&harness, args).unwrap();
        let png = harness.store.get(&row.hash.parse().unwrap()).unwrap();
        std::fs::write(dir.join("render-turnaround-kit.png"), png).unwrap();
        let args = json!({
            "character": "kit", "expressions": ["happy", "sad", "angry", "surprised"], "framing": "face",
            "cell": [192, 192], "quality": "offline"
        });
        let faces: ContactSheetResult = run::<ContactSheet, _>(&harness, args).unwrap();
        let png = harness.store.get(&faces.hash.parse().unwrap()).unwrap();
        std::fs::write(dir.join("render-contact-expressions.png"), png).unwrap();
        let args = json!({
            "character": "kit", "framing": "full", "cell": [192, 320], "quality": "offline",
            "poses": [
                { "id": "wave-right" }, { "id": "wave-right", "mirror": true },
                { "id": "wave-right", "blend": { "with": "wave-left", "weight": 0.5 } }
            ]
        });
        let poses: ContactSheetResult = run::<ContactSheet, _>(&harness, args).unwrap();
        let png = harness.store.get(&poses.hash.parse().unwrap()).unwrap();
        std::fs::write(dir.join("render-contact-poses.png"), png).unwrap();
    }
    println!("changed {} pixels, {:.1}% inside the head box {lo:?}..{hi:?}", difference.changed, share * 100.0);
    assert!(share > 0.9, "{:.1}% of the changed pixels are inside the head's box", share * 100.0);
}
