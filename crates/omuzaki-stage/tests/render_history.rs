//! `render.preview {state}` and `render.compare {a, b, shot}` (#400, `docs/design/branches.md` 6.3): any state of the
//! project renders from a copy of its tree, through the same shot as now, so a past state, a point or another branch is
//! seen exactly as the person and an agent see the live character.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use std::sync::Arc;

use omuzaki_doc_api::Value;
use omuzaki_history_api::branch::BranchError;
use omuzaki_history_api::state::{HistoryError, HistoryView, StateRef};
use omuzaki_ops_api::{ErrorCode, OpError, TxId};
use omuzaki_stage::ops::render::compare::{Compare, CompareResult};
use omuzaki_stage::ops::render::preview::{Preview, PreviewResult};
use serde_json::{Value as Json, json};

use support::{Harness, TestDoc, mock_doc};

/// The history of the mock project: transaction 1 is the document as it is now, transaction 2 has the outfit
/// recoloured, and the branch `empty` holds no character.
struct FakeHistory {
    now: Value,
}

impl HistoryView for FakeHistory {
    fn tree(&self, at: &StateRef) -> Result<Arc<Value>, HistoryError> {
        match at {
            StateRef::Tx { tx, .. } if *tx == TxId(1) => Ok(Arc::new(self.now.clone())),
            StateRef::Tx { tx, .. } if *tx == TxId(2) => {
                let mut tree = self.now.clone();
                tree["characters"]["mock"]["palette"]["outfit"] = json!("#d02020");
                Ok(Arc::new(tree))
            }
            StateRef::Branch { name, .. } if name.as_str() == "empty" => Ok(Arc::new(json!({ "characters": {} }))),
            StateRef::Branch { name, .. } => Err(HistoryError::Branch(BranchError::NotFound(name.to_string()))),
            StateRef::Tx { tx, .. } => Err(HistoryError::UnknownTx(*tx)),
            _ => Err(HistoryError::Io("not in this fake".to_owned())),
        }
    }
}

fn harness() -> Harness {
    let doc = mock_doc();
    let now = doc.0.root().clone();
    Harness::with_history(TestDoc(doc.0), Arc::new(FakeHistory { now }))
}

fn shot() -> Json {
    json!({ "character": "mock", "framing": "full", "yaw": 20.0, "size": [48, 64] })
}

fn preview(harness: &Harness, state: Json) -> Result<PreviewResult, OpError> {
    let mut args = shot();
    args["state"] = state;
    let op: Preview = serde_json::from_value(args).expect("the arguments read");
    let (plan, value) = harness.run(&op)?;
    assert!(plan.patch.is_empty(), "a preview of any state changes nothing");
    Ok(serde_json::from_value(value).expect("the result reads"))
}

fn compare(harness: &Harness, args: Json) -> Result<CompareResult, OpError> {
    let op: Compare = serde_json::from_value(args).expect("the arguments read");
    let (_, value) = harness.run(&op)?;
    Ok(serde_json::from_value(value).expect("the result reads"))
}

#[test]
fn a_past_state_renders_like_now_when_it_is_now_and_differently_when_it_differs() {
    let harness = harness();
    let now = preview(&harness, json!("now")).unwrap_or_else(|e| panic!("{e}"));
    let same = preview(&harness, json!({ "tx": 1 })).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(same.hash, now.hash, "the state that equals now renders to now's exact bytes");
    let recoloured = preview(&harness, json!({ "tx": 2 })).unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(recoloured.hash, now.hash, "a state with another outfit colour renders another picture");
    assert_eq!(recoloured.size, now.size);
    assert_eq!(recoloured.camera, now.camera, "the same shot");
}

#[test]
fn a_state_without_the_character_or_unknown_to_the_history_is_refused_by_name() {
    let harness = harness();
    let gone = preview(&harness, json!({ "branch": "empty" })).expect_err("no character there");
    assert_eq!(gone.code, ErrorCode::NotFound);
    assert!(gone.to_string().contains("in that state"), "{gone}");
    let unknown = preview(&harness, json!({ "tx": 99 })).expect_err("no such transaction");
    assert_eq!(unknown.code, ErrorCode::NotFound);
    let nowhere = preview(&harness, json!({ "branch": "nowhere" })).expect_err("no such branch");
    assert_eq!(nowhere.code, ErrorCode::NotFound);
}

#[test]
fn two_states_compare_at_one_shot_and_a_state_with_itself_is_identical() {
    let harness = harness();
    let itself =
        compare(&harness, json!({ "a": "now", "b": { "tx": 1 }, "shot": shot() })).unwrap_or_else(|e| panic!("{e}"));
    assert!(itself.identical, "{itself:?}");
    assert_eq!(itself.changed_pixels, 0);
    assert_eq!(itself.sides[0], itself.sides[1], "both sides rendered to the same picture");
    assert_eq!(itself.missing, Vec::<String>::new());

    let changed = compare(&harness, json!({ "a": { "tx": 1 }, "b": { "tx": 2 }, "shot": shot() }))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(!changed.identical);
    assert!(changed.changed_pixels > 0);
    let now = preview(&harness, json!("now")).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(changed.sides[0], now.uri, "side a is the render of tx 1, which is now's picture");

    // A side can stay a stored render: now's picture against a state.
    let mixed =
        compare(&harness, json!({ "a": now.uri, "b": { "tx": 1 }, "shot": shot() })).unwrap_or_else(|e| panic!("{e}"));
    assert!(mixed.identical);
}

#[test]
fn a_character_missing_on_one_side_is_an_empty_frame_and_named() {
    let harness = harness();
    let result = compare(&harness, json!({ "a": "now", "b": { "branch": "empty" }, "shot": shot() }))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(result.missing, vec!["b".to_owned()]);
    assert_eq!(result.sides[1], "");
    assert!(result.changed_pixels > 0, "the character shows on a and not on b");

    let both = compare(&harness, json!({ "a": { "branch": "empty" }, "b": { "branch": "empty" }, "shot": shot() }));
    assert_eq!(both.expect_err("nothing to compare").code, ErrorCode::NotFound);
}

#[test]
fn a_state_needs_a_shot_and_a_render_reference_still_reads_as_one() {
    let harness = harness();
    let refused = compare(&harness, json!({ "a": "now", "b": { "tx": 1 } })).expect_err("no shot");
    assert_eq!(refused.code, ErrorCode::Invalid);
    assert!(refused.to_string().contains("shot"), "{refused}");
    let op: Compare =
        serde_json::from_value(json!({ "a": "0".repeat(64), "b": "now", "shot": shot() })).expect("reads");
    assert!(matches!(op.a, omuzaki_stage::ops::render::compare::Side::Render(_)), "a hash stays a render");
    assert!(matches!(op.b, omuzaki_stage::ops::render::compare::Side::State(StateRef::Now)));
}

#[test]
fn a_tree_renders_outside_any_runner_to_the_same_picture_as_its_state() {
    let harness = harness();
    let by_state = preview(&harness, json!({ "tx": 2 })).unwrap_or_else(|e| panic!("{e}"));
    let mut tree = harness.doc().root().clone();
    tree["characters"]["mock"]["palette"]["outfit"] = json!("#d02020");
    let op: Preview = serde_json::from_value(shot()).expect("reads");
    let direct = harness
        .with_services(|services| omuzaki_stage::ops::render::history::picture_of(Arc::new(tree), &op, services))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(direct.hash, by_state.hash, "the studio's picture of a tree is the op's picture of that state");
}
