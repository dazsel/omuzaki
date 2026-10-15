//! `render.turnaround` on the mock document and the mock renderer: the four views in order, their differences, the
//! determinism, the front view against `render.preview`, the refusals, and the dry run.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_char_api::CharId;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_ops_api::{Actor, ErrorCode, OpCtx, OpError, Plan};
use omuzaki_stage::ops::render::job::RenderQuality;
use omuzaki_stage::ops::render::preview::RenderPreview;
use omuzaki_stage::ops::render::turnaround::{RenderTurnaround, VIEWS};
use omuzaki_stage_api::{CameraDef, Framing};
use serde_json::Value;
use support::{Harness, mock_doc};

const SIDE: u32 = 64;

fn mock() -> CharId {
    CharId::new("mock").expect("a valid id")
}

/// A turnaround of the mock character at the test size, offline, full framing.
fn turnaround() -> RenderTurnaround {
    RenderTurnaround {
        character: mock(),
        variant: None,
        framing: Framing::Full,
        pitch: 0.0,
        light_rig: "viewer-studio".to_owned(),
        expression: None,
        size: [SIDE, SIDE],
        quality: RenderQuality::Offline,
    }
}

/// Plans `op` against the mock document without running its work: no build, no render, no store.
fn plan_only(op: &RenderTurnaround) -> Result<Plan, OpError> {
    let doc = mock_doc();
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let ctx = OpCtx::new(&doc.0, &actor, &clock).with_dry_run(true);
    omuzaki_ops_api::Op::plan(op, &ctx)
}

/// The hash each view was stored under, in order.
fn hashes(result: &Value) -> Vec<Value> {
    result["views"].as_array().expect("the result lists the views").iter().map(|v| v["hash"].clone()).collect()
}

#[test]
fn four_views_in_order_with_their_yaw() {
    let result = Harness::new(mock_doc()).run(&turnaround()).expect("renders").1;
    let views = result["views"].as_array().expect("the result lists the views");
    assert_eq!(views.len(), VIEWS.len(), "one view per name");
    for (view, (name, yaw)) in views.iter().zip(VIEWS) {
        assert_eq!(view["view"], name);
        assert_eq!(view["yaw"].as_f64(), Some(f64::from(yaw)), "{name} is at {yaw} degrees");
    }
    assert_eq!(result["size"], serde_json::json!([SIDE, SIDE]));
}

#[test]
fn the_views_differ() {
    let result = Harness::new(mock_doc()).run(&turnaround()).expect("renders").1;
    let hashes = hashes(&result);
    let (front, side, back) = (&hashes[0], &hashes[2], &hashes[3]);
    assert!(front != side && side != back && front != back, "the front, side and back are different images");
}

#[test]
fn the_same_arguments_give_the_same_pngs() {
    let first = Harness::new(mock_doc()).run(&turnaround()).expect("renders").1;
    let second = Harness::new(mock_doc()).run(&turnaround()).expect("renders").1;
    assert_eq!(hashes(&first), hashes(&second), "two offline runs store the same four PNGs");
}

#[test]
fn the_front_view_is_the_preview_front() {
    let harness = Harness::new(mock_doc());
    let turned = harness.run(&turnaround()).expect("renders").1;
    let preview = RenderPreview {
        character: mock(),
        variant: None,
        camera: CameraDef::preset(Framing::Full),
        light_rig: "viewer-studio".to_owned(),
        expression: None,
        size: [SIDE, SIDE],
        quality: RenderQuality::Offline,
    };
    let single = harness.run(&preview).expect("renders").1;
    assert_eq!(hashes(&turned)[0], single["hash"], "the front view is the preview of the same framing");
}

#[test]
fn free_framing_and_a_pitch_out_of_range_are_refused() {
    let free = RenderTurnaround { framing: Framing::Free, ..turnaround() };
    let error = plan_only(&free).expect_err("a turnaround takes a preset");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some("framing"));

    for pitch in [-31.0, 61.0, f32::NAN] {
        let steep = RenderTurnaround { pitch, ..turnaround() };
        let error = plan_only(&steep).expect_err("the pitch is outside -30 to 60");
        assert_eq!(error.field.as_deref(), Some("pitch"), "pitch {pitch}");
    }
}

#[test]
fn a_dry_run_costs_four_images() {
    let plan = plan_only(&turnaround()).expect("the arguments are fine");
    let cost = plan.cost.expect("a turnaround has a cost");
    assert_eq!(cost.seconds, Some(8.0), "four offline stills at two seconds each");
    assert_eq!(cost.gpu_mib, Some(65), "a 64 by 64 target is 1 MiB, plus 64 MiB of meshes");
}
