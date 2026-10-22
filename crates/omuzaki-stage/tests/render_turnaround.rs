//! `render.turnaround` (#1714): the front, three-quarter, side and back of a character in one row, from one staging,
//! at a pitch and with an expression on every view, the same bytes every run. The mock renderer draws them; the views are
//! cut out of the row to compare.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError, Plan};
use omuzaki_stage::ops::render::turnaround::{GUTTER, Turnaround, TurnaroundResult};
use omuzaki_store_api::Store;
use serde_json::{Value, json};

use support::{Harness, TestDoc};

/// One view's size in these tests.
const VIEW: [u32; 2] = [48, 80];

/// A project with the kit's standard character at `characters/kit`.
fn harness() -> Harness {
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Person { name: "test".to_owned() };
    let mut doc = MemDoc::from_value(json!({}));
    let op: CharacterCreate =
        serde_json::from_value(json!({ "id": "kit", "name": "Kit", "template": "standard", "seed": 7 })).unwrap();
    doc.apply(&op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap().patch).unwrap();
    Harness::new(TestDoc(doc))
}

fn run(harness: &Harness, mut args: Value) -> Result<(Plan, TurnaroundResult), OpError> {
    args["character"] = json!("kit");
    args["size"] = json!(VIEW);
    let op: Turnaround = serde_json::from_value(args).expect("the arguments read");
    let (plan, value) = harness.run(&op)?;
    Ok((plan, serde_json::from_value(value).expect("the result reads")))
}

/// The four views cut out of a stored row, left to right.
fn views(harness: &Harness, row: &TurnaroundResult) -> Vec<Vec<u8>> {
    let png = harness.store.get(&row.hash.parse().unwrap()).unwrap();
    let image = image::load_from_memory(&png).unwrap().to_rgba8();
    (0..4)
        .map(|i| image::imageops::crop_imm(&image, i * (VIEW[0] + GUTTER), 0, VIEW[0], VIEW[1]).to_image().into_raw())
        .collect()
}

#[test]
fn four_different_views_in_order_and_the_same_bytes_every_run() {
    let harness = harness();
    let (plan, row) = run(&harness, json!({})).unwrap();
    assert_eq!(row.views, ["front", "three_quarter", "side", "back"]);
    assert_eq!(row.size, [4 * VIEW[0] + 3 * GUTTER, VIEW[1]]);
    let cut = views(&harness, &row);
    for (i, a) in cut.iter().enumerate() {
        for (j, b) in cut.iter().enumerate().skip(i + 1) {
            assert_ne!(a, b, "{} and {} differ", row.views[i], row.views[j]);
        }
    }
    let (_, again) = run(&harness, json!({})).unwrap();
    assert_eq!(row, again, "the same arguments give the same row");
    let cost = plan.cost.expect("a turnaround is costed");
    assert!(cost.seconds.is_some_and(|s| s > 0.0) && cost.gpu_mib.is_some_and(|m| m > 0), "{cost:?}");
}

#[test]
fn a_pitch_and_an_expression_reach_every_view() {
    let harness = harness();
    let (_, level) = run(&harness, json!({})).unwrap();
    let (_, above) = run(&harness, json!({ "pitch": 30.0 })).unwrap();
    let (_, happy) = run(&harness, json!({ "expression": "happy" })).unwrap();
    let (level, above, happy) = (views(&harness, &level), views(&harness, &above), views(&harness, &happy));
    for view in 0..4 {
        assert_ne!(level[view], above[view], "view {view}: from above it looks different");
    }
    // The face shows from the front and the three-quarter view; the back shows none of it.
    assert_ne!(level[0], happy[0], "the front view shows the expression");
    assert_ne!(level[1], happy[1], "and so does the three-quarter one");
}

#[test]
fn free_framing_a_pitch_out_of_range_and_an_unknown_expression_are_refused() {
    let harness = harness();
    let refused = |args: Value| run(&harness, args).expect_err("refused");
    for (args, field) in [
        (json!({ "framing": "free" }), "framing"),
        (json!({ "framing": "hands" }), "framing"),
        (json!({ "pitch": 61.0 }), "pitch"),
        (json!({ "pitch": -31.0 }), "pitch"),
        (json!({ "light_rig": "nope" }), "light_rig"),
        (json!({ "expression": "no-such-face" }), "expression"),
    ] {
        let error = refused(args.clone());
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some(field)), "{args}: {error:?}");
    }
    let unknown = refused(json!({ "expression": "no-such-face" }));
    assert!(unknown.hint.as_deref().is_some_and(|h| h.contains("happy")), "it lists the expressions: {unknown:?}");
}
