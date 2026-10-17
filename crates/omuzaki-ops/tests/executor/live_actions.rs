//! Live actions (#139, `automation.md` D7): an edit with `Undo::None` and an empty patch changes live state, not the
//! document. It is logged and announced at the current version, and never enters an undo history or clears redo. One
//! that plans a patch is a bug in the op.

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{
    ErrorCode, Event, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{drain, request, rig};

/// Plays a sound: a live action.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Live {
    pub sound: String,
}

impl Op for Live {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.live",
            summary: "Play a sound.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "played": self.sound })))
    }
}
register_op!(Live);

/// Claims to be a live action but changes the document: a contract bug.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct BadLive {
    pub path: DocPath,
}

impl Op for BadLive {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.bad_live",
            summary: "Change the document without saying how to undo it.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = ctx.doc.get(&self.path).cloned();
        Ok(Plan::edit(Patch::new().with(self.path.clone(), before, Some(json!("changed")))))
    }
}
register_op!(BadLive);

fn live() -> Request {
    request("test.live", json!({"sound": "ding"}))
}

#[test]
fn a_live_action_is_logged_once_at_the_same_version_and_undo_passes_it_by() {
    let rig = rig(json!({"a": 0}));
    let edit = rig.set("a", json!(1)).applied.unwrap();
    let rx = rig.subscribe();
    let played = rig.exec.run(live()).unwrap().applied.unwrap();
    assert_eq!(played.version, edit.version, "no version bump");
    assert!(played.patch.is_empty());
    let applied = drain(&rx).iter().filter(|e| matches!(e, Event::OpApplied { .. })).count();
    assert_eq!(applied, 1);
    assert_eq!(rig.log().last().map(|r| r.op.clone()), Some("test.live".to_owned()), "logged");

    // Undo reaches the edit before it; redo then still has the edit after another live action.
    assert_eq!(rig.undo().unwrap().as_deref(), Some(super::SET));
    assert_eq!(rig.at("a"), Some(json!(0)));
    rig.exec.run(live()).unwrap();
    assert_eq!(rig.redo().unwrap().as_deref(), Some(super::SET), "a live action never clears redo");
    assert_eq!(rig.at("a"), Some(json!(1)));
}

#[test]
fn a_live_action_alone_leaves_nothing_to_undo() {
    let rig = rig(json!({}));
    rig.exec.run(live()).unwrap();
    assert_eq!(rig.undo().unwrap(), None);
}

#[test]
fn inside_a_transaction_the_step_holds_only_the_document_edits() {
    let rig = rig(json!({"a": 0, "b": 0}));
    let tx = rig.exec.begin("show", &super::person());
    let within = |req: Request| rig.exec.run(Request { tx: Some(tx), ..req }).unwrap();
    within(request("test.set", json!({"path": "a", "value": 1})));
    within(live());
    within(request("test.set", json!({"path": "b", "value": 1})));
    rig.exec.commit(tx, &super::person()).unwrap();
    let ops: Vec<String> = rig.log().iter().map(|r| r.op.clone()).collect();
    assert_eq!(ops, ["test.set", "test.live", "test.set"], "logged in order");
    assert_eq!(rig.undo().unwrap().as_deref(), Some("show"));
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(0)), Some(json!(0))));
}

#[test]
fn a_live_action_that_plans_a_patch_is_refused_and_changes_nothing() {
    let rig = rig(json!({"a": 0}));
    let e = rig.exec.run(request("test.bad_live", json!({"path": "a"}))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Internal);
    assert!(e.message.contains("test.bad_live"), "{e}");
    assert_eq!(rig.at("a"), Some(json!(0)));
    assert_eq!(rig.log().len(), 0);
}
