//! `render.preview` on the mock document and the mock renderer: determinism, the dry run, the stored PNG, the framings,
//! free cameras, the refusals while planning, and the schema.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_char_api::CharId;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan, find};
use omuzaki_stage::ops::render::job::RenderQuality;
use omuzaki_stage::ops::render::preview::RenderPreview;
use omuzaki_stage_api::{CameraDef, Framing};
use omuzaki_store_api::{ContentHash, Store};
use serde_json::Value;
use support::{Harness, mock_doc};

const SIDE: u32 = 64;

fn mock() -> CharId {
    CharId::new("mock").expect("a valid id")
}

/// A preview of the mock character with `camera`, at the test size, offline.
fn preview(camera: CameraDef) -> RenderPreview {
    RenderPreview {
        character: mock(),
        variant: None,
        camera,
        light_rig: "viewer-studio".to_owned(),
        expression: None,
        size: [SIDE, SIDE],
        quality: RenderQuality::Offline,
    }
}

/// Plans `op` against the mock document without running its work: no build, no render, no store.
fn plan_only(op: &RenderPreview) -> Result<Plan, OpError> {
    let doc = mock_doc();
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let ctx = OpCtx::new(&doc.0, &actor, &clock).with_dry_run(true);
    omuzaki_ops_api::Op::plan(op, &ctx)
}

fn code(error: &OpError) -> ErrorCode {
    error.code
}

/// The content hash that a `hex` string spells, as the result names it.
fn hash_of(hex: &str) -> ContentHash {
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("two hex digits");
    }
    ContentHash(bytes)
}

#[test]
fn the_same_arguments_give_the_same_png() {
    let first = Harness::new(mock_doc()).run(&preview(CameraDef::preset(Framing::Full))).expect("renders").1;
    let second = Harness::new(mock_doc()).run(&preview(CameraDef::preset(Framing::Full))).expect("renders").1;
    assert_eq!(first["hash"], second["hash"], "two offline runs store one PNG");
}

#[test]
fn a_dry_run_gives_the_cost_and_renders_nothing() {
    let harness = Harness::new(mock_doc());
    let before = harness.renderer.lock().expect("the renderer is free").stats();
    let plan = plan_only(&preview(CameraDef::preset(Framing::Full))).expect("the arguments are fine");
    let cost = plan.cost.expect("a render has a cost");
    assert_eq!(cost.gpu_mib, Some(65), "a 64 by 64 target is 1 MiB, plus 64 MiB of meshes");
    assert_eq!(plan.output.expect("a plan says what it renders")["framing"], "full");
    assert_eq!(harness.renderer.lock().expect("the renderer is free").stats(), before, "nothing was created");
    assert!(harness.store.is_empty(), "nothing was stored");
}

#[test]
fn the_result_names_the_stored_png() {
    let harness = Harness::new(mock_doc());
    let result = harness.run(&preview(CameraDef::preset(Framing::Full))).expect("renders").1;
    let hash = result["hash"].as_str().expect("the result names the hash");
    assert_eq!(result["uri"], Value::String(format!("omuzaki://render/{hash}.png")));
    assert_eq!(result["size"], serde_json::json!([SIDE, SIDE]));

    let bytes = harness.store.get(&hash_of(hash)).expect("the store holds the PNG");
    let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).expect("it is a PNG").to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (SIDE, SIDE));
}

#[test]
fn each_framing_the_mock_has_renders() {
    // The mock has no eye joints, so a face shot has nothing to frame: it is refused, naming the camera. The bust and the
    // full shot render, and differ.
    let harness = Harness::new(mock_doc());
    let face = harness.run(&preview(CameraDef::preset(Framing::Face))).expect_err("the mock has no eyes");
    assert_eq!(face.field.as_deref(), Some("camera"));
    let bust = harness.run(&preview(CameraDef::preset(Framing::Bust))).expect("renders").1["hash"].clone();
    let full = harness.run(&preview(CameraDef::preset(Framing::Full))).expect("renders").1["hash"].clone();
    assert_ne!(bust, full, "a bust and a full shot are different images");
}

#[test]
fn a_free_camera_renders_and_one_without_a_target_is_refused() {
    let harness = Harness::new(mock_doc());
    let free = CameraDef {
        position: Some([0.0, 0.8, 2.5]),
        target: Some([0.0, 0.78, 0.0]),
        ..CameraDef::preset(Framing::Free)
    };
    assert!(harness.run(&preview(free)).is_ok(), "a free camera with a position and a target renders");

    let no_target = CameraDef { position: Some([0.0, 0.8, 2.5]), ..CameraDef::preset(Framing::Free) };
    let error = harness.run(&preview(no_target)).expect_err("a free camera needs a target");
    assert_eq!(code(&error), ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some("camera"));
}

#[test]
fn bad_arguments_are_refused_while_planning() {
    let small = RenderPreview { size: [8, 8], ..preview(CameraDef::preset(Framing::Full)) };
    let error = plan_only(&small).expect_err("8 pixels is below the minimum");
    assert_eq!(error.field.as_deref(), Some("size"));

    let rig = RenderPreview { light_rig: "no-such-rig".to_owned(), ..preview(CameraDef::preset(Framing::Full)) };
    let error = plan_only(&rig).expect_err("the rig does not exist");
    assert_eq!(error.field.as_deref(), Some("light_rig"));

    let nobody = RenderPreview {
        character: CharId::new("nobody").expect("a valid id"),
        ..preview(CameraDef::preset(Framing::Full))
    };
    let error = plan_only(&nobody).expect_err("the character does not exist");
    assert_eq!(code(&error), ErrorCode::NotFound);

    let zoomed = CameraDef { zoom: 0.0, ..CameraDef::preset(Framing::Full) };
    let error = plan_only(&preview(zoomed)).expect_err("a zoom of 0 frames nothing");
    assert_eq!(error.field.as_deref(), Some("zoom"));
}

#[test]
fn the_schema_describes_every_argument() {
    let registration = find("render.preview").expect("the op registers itself");
    let schema = serde_json::to_value((registration.schema)()).expect("the schema serialises");
    let properties = schema["properties"].as_object().expect("the arguments are properties");
    assert!(!properties.is_empty(), "the op has arguments");
    for (name, property) in properties {
        let described = property.get("description").and_then(Value::as_str).is_some_and(|d| !d.is_empty());
        assert!(described, "argument '{name}' has no description");
    }
}
