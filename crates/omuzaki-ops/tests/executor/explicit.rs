//! Undo of a step that holds an `Undo::Explicit` op: refused with `Unsupported`, the step dropped, the document left
//! alone (ADR 0036 decision 9). Redo is covered by a unit test in `state.rs`, since undo never leaves such a step on
//! the redo stack.

use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_ops_api::{
    ErrorCode, Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{person, request, rig};

/// Sets a path, and claims an effect outside the document (a file written, say) that undo can't revert.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ExplicitSet {
    pub path: DocPath,
    pub value: Value,
}

impl Op for ExplicitSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.explicit_set",
            summary: "Set a path and have an effect outside the document.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = ctx.doc.get(&self.path).cloned();
        Ok(Plan::edit(Patch::new().with(self.path.clone(), before, Some(self.value.clone()))))
    }
}
register_op!(ExplicitSet);

/// An op with an effect outside the document and no change to it.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ExplicitNoop {}

impl Op for ExplicitNoop {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.explicit_noop",
            summary: "Have an effect outside the document and change nothing in it.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new()))
    }
}
register_op!(ExplicitNoop);

/// Like [`ExplicitNoop`], but behind the `publish` scope, so a transaction can mix two scopes.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct ExplicitPublishNoop {}

impl Op for ExplicitPublishNoop {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.explicit_publish_noop",
            summary: "Publish outside the document and change nothing in it.",
            kind: OpKind::Edit,
            scope: Scope::Publish,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new()))
    }
}
register_op!(ExplicitPublishNoop);

#[test]
fn undoing_a_step_with_an_explicit_op_is_refused_and_the_step_is_dropped() {
    let rig = rig(json!({"a": 1, "b": 1}));
    rig.set("a", json!(2)); // an Auto step
    rig.run("test.explicit_set", json!({"path": "b", "value": 2})).unwrap(); // an Explicit step, the newest

    let e = rig.undo().unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert!(e.message.contains("test.explicit_set"), "{e}");
    assert!(e.hint.as_deref().unwrap().contains("by hand"), "{e}");
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(2)), Some(json!(2))), "a refused undo changes nothing");

    assert_eq!(rig.redo().unwrap(), None, "the refused step is not on the redo stack");
    assert_eq!(rig.undo().unwrap().as_deref(), Some("test.set"), "the next undo reaches the step before");
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(1)), Some(json!(2))));
}

#[test]
fn a_transaction_with_an_explicit_op_that_changed_nothing_is_still_refused() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("write and notify", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 2})) }).unwrap();
    rig.exec.run(Request { tx: Some(tx), ..request("test.explicit_noop", json!({})) }).unwrap();
    rig.exec.commit(tx, &person()).unwrap();

    let e = rig.undo().unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert!(e.message.contains("test.explicit_noop"), "{e}");
    assert_eq!(rig.at("a"), Some(json!(2)), "a refused undo changes nothing");
    assert_eq!(rig.undo().unwrap(), None, "the refused step was dropped");
}

#[test]
fn a_caller_without_the_scope_is_denied_and_the_explicit_step_stays() {
    let rig = rig(json!({"a": 1}));
    rig.run("test.explicit_set", json!({"path": "a", "value": 2})).unwrap();

    let e = rig.exec.undo(&person(), &Grants::of([Scope::Read])).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert_eq!(rig.at("a"), Some(json!(2)));

    // Still there: the caller with the scope is refused by the explicit op, not by a missing step.
    assert_eq!(rig.undo().unwrap_err().code, ErrorCode::Unsupported);
}

#[test]
fn a_standalone_explicit_op_with_an_empty_patch_needs_its_scope_to_be_refused() {
    let rig = rig(json!({"a": 1}));
    rig.run("test.explicit_noop", json!({})).unwrap();

    // Without `edit:character` the caller is denied, and the step stays on the stack.
    let e = rig.exec.undo(&person(), &Grants::of([Scope::Read])).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);

    // With the scope, the step is refused and dropped, as for any Explicit step.
    let e = rig.undo().unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert_eq!(rig.undo().unwrap(), None, "the refused step was dropped");
}

#[test]
fn a_transaction_mixing_scopes_needs_every_scope_to_undo_its_explicit_op() {
    let rig = rig(json!({"a": 1}));
    let tx = rig.exec.begin("edit and publish", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 2})) }).unwrap();
    rig.exec.run(Request { tx: Some(tx), ..request("test.explicit_publish_noop", json!({})) }).unwrap();
    rig.exec.commit(tx, &person()).unwrap();

    // `edit:character` alone covers the patch but not the Explicit op's `publish`: denied, and the step stays.
    let e = rig.exec.undo(&person(), &Grants::of([Scope::EditCharacter])).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert_eq!(rig.at("a"), Some(json!(2)), "a denied undo changes nothing");

    // With every scope the step is refused as Explicit, not denied.
    assert_eq!(rig.undo().unwrap_err().code, ErrorCode::Unsupported);
}
