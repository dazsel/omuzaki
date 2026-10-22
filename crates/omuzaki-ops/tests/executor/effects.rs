//! Effects outside the document (#1091, ADR 0036 decision 8): an Explicit op records a pair of inverse calls, and
//! undo, redo and rollback run them (with compensation) around the patch. The stand-in effect writes a marker file in
//! a temporary folder; its inverse removes it and its redo writes it again. A file named `.fail-undo-<name>` or
//! `.fail-redo-<name>` in the folder makes that call fail, so each test scripts its own failures.
#![allow(clippy::disallowed_methods, reason = "the stand-in effect is a file, which the tests write and check")]

use std::path::Path;
use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_history_api::{HistoryEntry, Stack};
use omuzaki_history_mock::{FakeWallClock, RecordingSink};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Effect, ErrorCode, Grants, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Sessions,
    Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{SET, open_agents, person, request, rig};

/// Sets a path, and claims an effect outside the document with no way back recorded (`effect: None`): undone by its
/// patch alone.
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

/// Writes marker file `name` in `dir` (when applied, while it plans) and records `files/<name>: true`. `stale` plans a
/// patch whose `before` is wrong, so the apply fails after the effect happened. `undo` overrides the inverse call.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Touch {
    pub dir: String,
    pub name: String,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub undo: Option<OpCall>,
}

impl Op for Touch {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.touch",
            summary: "Write a marker file.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !ctx.dry_run {
            write(&self.dir, &self.name)?;
        }
        let path =
            DocPath::parse(&format!("files/{}", self.name)).map_err(|e| OpError::invalid("name", e.to_string()))?;
        let before = if self.stale { Some(json!("stale")) } else { ctx.doc.get(&path).cloned() };
        let call = |op: &str| OpCall { op: op.to_owned(), args: json!({"dir": self.dir, "name": self.name}) };
        let effect = Effect {
            summary: format!("wrote {}", self.name),
            undo: self.undo.clone().unwrap_or_else(|| call("test.untouch")),
            redo: call("test.retouch"),
        };
        Ok(Plan { effect: Some(effect), ..Plan::edit(Patch::new().with(path, before, Some(json!(true)))) })
    }
}
register_op!(Touch);

/// Removes marker file `name`: the inverse call. Idempotent. A live action, as inverse calls are.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Untouch {
    pub dir: String,
    pub name: String,
}

impl Op for Untouch {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.untouch",
            summary: "Remove a marker file.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !ctx.dry_run {
            refuse_if_scripted(&self.dir, "undo", &self.name)?;
            let file = Path::new(&self.dir).join(&self.name);
            if file.exists() {
                std::fs::remove_file(file).map_err(|e| OpError::internal(e.to_string()))?;
            }
        }
        Ok(Plan::answer(json!({ "removed": self.name })))
    }
}
register_op!(Untouch);

/// Writes marker file `name` again: the redo call.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Retouch {
    pub dir: String,
    pub name: String,
}

impl Op for Retouch {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.retouch",
            summary: "Write a marker file again.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !ctx.dry_run {
            refuse_if_scripted(&self.dir, "redo", &self.name)?;
            write(&self.dir, &self.name)?;
        }
        Ok(Plan::answer(json!({ "written": self.name })))
    }
}
register_op!(Retouch);

/// An inverse call behind the `publish` scope, which agents lack.
#[derive(Serialize, Deserialize, JsonSchema)]
pub struct GuardedUntouch {
    pub dir: String,
    pub name: String,
}

impl Op for GuardedUntouch {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.guarded_untouch",
            summary: "Remove a published marker.",
            kind: OpKind::Edit,
            scope: Scope::Publish,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({})))
    }
}
register_op!(GuardedUntouch);

fn write(dir: &str, name: &str) -> Result<(), OpError> {
    std::fs::write(Path::new(dir).join(name), b"x").map_err(|e| OpError::internal(e.to_string()))
}

fn refuse_if_scripted(dir: &str, which: &str, name: &str) -> Result<(), OpError> {
    if Path::new(dir).join(format!(".fail-{which}-{name}")).exists() {
        Err(OpError::internal(format!("the {which} of {name} was told to fail")))
    } else {
        Ok(())
    }
}

/// A temporary folder and the args of a touch of `name` in it.
pub(crate) struct Folder(tempfile::TempDir);

impl Folder {
    pub(crate) fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn dir(&self) -> String {
        self.0.path().to_string_lossy().into_owned()
    }

    pub(crate) fn touch(&self, name: &str) -> Request {
        request("test.touch", json!({"dir": self.dir(), "name": name}))
    }

    pub(crate) fn has(&self, name: &str) -> bool {
        self.0.path().join(name).exists()
    }

    pub(crate) fn fail(&self, which: &str, name: &str) {
        std::fs::write(self.0.path().join(format!(".fail-{which}-{name}")), b"").unwrap();
    }

    fn heal(&self, which: &str, name: &str) {
        std::fs::remove_file(self.0.path().join(format!(".fail-{which}-{name}"))).unwrap();
    }
}

/// A transaction of two touches, A then B, by the person.
fn two_effects(rig: &super::Rig, folder: &Folder) {
    let tx = rig.exec.begin("a and b", &person());
    for name in ["a", "b"] {
        rig.exec.run(Request { tx: Some(tx), ..folder.touch(name) }).unwrap();
    }
    rig.exec.commit(tx, &person()).unwrap();
}

#[test]
fn undo_reverses_the_effect_then_the_patch_and_redo_puts_both_back() {
    let (rig, folder) = (rig(json!({"files": {}})), Folder::new());
    let applied = rig.exec.run(folder.touch("a")).unwrap().applied.unwrap();
    assert!(applied.effect.is_some(), "the record carries the effect");
    assert!(folder.has("a"));
    assert_eq!(rig.undo().unwrap().as_deref(), Some("Write a marker file."));
    assert_eq!((folder.has("a"), rig.at("files/a")), (false, None));
    assert!(rig.redo().unwrap().is_some());
    assert_eq!((folder.has("a"), rig.at("files/a")), (true, Some(json!(true))));
}

#[test]
fn a_failed_call_is_compensated_and_the_step_stays() {
    let (rig, folder) = (rig(json!({"files": {}})), Folder::new());
    two_effects(&rig, &folder);
    // Undo runs B's undo, then A's, which fails: B is written back, nothing changed.
    folder.fail("undo", "a");
    let e = rig.undo().unwrap_err();
    assert!(e.message.contains("undo of a"), "{e}");
    assert!(folder.has("a") && folder.has("b"), "B's undo was compensated");
    assert_eq!((rig.at("files/a"), rig.at("files/b")), (Some(json!(true)), Some(json!(true))));
    folder.heal("undo", "a");
    assert_eq!(rig.undo().unwrap().as_deref(), Some("a and b"), "the step stayed");
    assert!(!folder.has("a") && !folder.has("b"));

    // Redo runs A's redo, then B's, which fails: A is removed again, and the step stays on the redo stack.
    folder.fail("redo", "b");
    assert!(rig.redo().is_err());
    assert!(!folder.has("a") && !folder.has("b"));
    assert_eq!(rig.at("files/a"), None);
    folder.heal("redo", "b");
    assert_eq!(rig.redo().unwrap().as_deref(), Some("a and b"));
    assert!(folder.has("a") && folder.has("b"));
}

#[test]
fn when_compensation_fails_too_the_step_is_dropped_and_the_hint_says_what_is_where() {
    let sink = Arc::new(RecordingSink::new());
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec =
        Executor::new(Box::new(MemDoc::from_value(json!({"files": {}}))), clock).unwrap().with_journal(sink.clone());
    open_agents(&exec);
    let rig = super::Rig { exec, clock: Arc::new(FakeClock::new(Seconds(0.0))) };
    let folder = Folder::new();
    two_effects(&rig, &folder);
    folder.fail("undo", "a");
    folder.fail("redo", "b");
    let e = rig.undo().unwrap_err();
    let hint = e.hint.clone().unwrap_or_default();
    assert!(hint.contains("mixed") && hint.contains("still undone") && hint.contains("wrote b"), "{hint}");
    assert_eq!(
        (rig.at("files/a"), rig.at("files/b")),
        (Some(json!(true)), Some(json!(true))),
        "the document is untouched"
    );
    assert!(!folder.has("b") && folder.has("a"), "B stayed undone, A was never undone");
    assert_eq!(rig.undo().unwrap(), None, "the step was dropped");
    let dropped = sink.entries().into_iter().any(|e| matches!(e, HistoryEntry::Dropped { stack: Stack::Undo, .. }));
    assert!(dropped, "and the journal says so");
}

#[test]
fn rollback_undoes_an_open_transactions_effects_newest_first() {
    let (rig, folder) = (rig(json!({"files": {}})), Folder::new());
    let tx = rig.exec.begin("open", &person());
    for name in ["a", "b"] {
        rig.exec.run(Request { tx: Some(tx), ..folder.touch(name) }).unwrap();
    }
    rig.exec.rollback(tx, &person()).unwrap();
    assert!(!folder.has("a") && !folder.has("b"));
    assert_eq!(rig.at("files/a"), None);
}

#[test]
fn an_inverse_call_that_changes_the_document_is_a_bug_and_a_missing_scope_keeps_the_step() {
    let (rig, folder) = (rig(json!({"files": {}, "x": 0})), Folder::new());
    let patching = json!({"op": "test.set", "args": {"path": "x", "value": 1}});
    rig.exec.run(request("test.touch", json!({"dir": folder.dir(), "name": "a", "undo": patching}))).unwrap();
    let e = rig.undo().unwrap_err();
    assert_eq!(e.code, ErrorCode::Internal, "{e:?}");
    assert_eq!(rig.at("x"), Some(json!(0)));

    let guarded = json!({"op": "test.guarded_untouch", "args": {"dir": folder.dir(), "name": "b"}});
    let agent = super::agent();
    let req = Request {
        actor: agent.clone(),
        grants: Grants::agent_default(),
        ..request("test.touch", json!({"dir": folder.dir(), "name": "b", "undo": guarded}))
    };
    rig.exec.run(req).unwrap();
    let e = rig.exec.undo(&agent, &Grants::agent_default()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied, "{e:?}");
    assert_eq!(rig.exec.approvals().len(), 0, "an inverse call never queues");
    assert!(rig.exec.undo(&agent, &Grants::agent_default()).is_err(), "the step stayed");
}

#[test]
fn an_explicit_apply_that_fails_after_its_effect_undoes_the_effect_at_once() {
    let (rig, folder) = (rig(json!({"files": {}})), Folder::new());
    let e = rig.exec.run(request("test.touch", json!({"dir": folder.dir(), "name": "a", "stale": true}))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(!folder.has("a"), "the file the plan wrote is gone again");
}

#[test]
fn an_explicit_record_without_an_effect_is_undone_by_its_patch() {
    let rig = rig(json!({"a": 1, "b": 1}));
    rig.set("a", json!(2));
    rig.run("test.explicit_set", json!({"path": "b", "value": 2})).unwrap();
    assert_eq!(rig.undo().unwrap().as_deref(), Some("Set a path and have an effect outside the document."));
    assert_eq!(rig.at("b"), Some(json!(1)));
    assert_eq!(rig.undo().unwrap().as_deref(), Some(SET));
}

#[test]
fn effects_survive_a_history_export_and_restore_and_a_journal_replay() {
    let folder = Folder::new();
    let sink = Arc::new(RecordingSink::new());
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let wall = Arc::new(FakeWallClock::new(omuzaki_history_api::WallTime(1_000)));
    let start = json!({"files": {}});
    let exec =
        Executor::new(Box::new(MemDoc::from_value(start.clone())), clock.clone()).unwrap().with_journal(sink.clone());
    exec.run(folder.touch("a")).unwrap();
    let tx = exec.begin("open", &person());
    exec.run(Request { tx: Some(tx), ..folder.touch("b") }).unwrap();
    let document = exec.get(&DocPath::root(), &Grants::person()).unwrap().unwrap();

    // From a snapshot: the document and the history export.
    let restored =
        Executor::restore(Box::new(MemDoc::from_value(document)), clock.clone(), wall.clone(), &exec.history_export())
            .unwrap();
    restored.rollback(tx, &person()).unwrap();
    assert!(!folder.has("b"), "the open transaction's effect is rolled back after a restore");
    assert!(restored.undo(&person(), &Grants::person()).unwrap().is_some());
    assert!(!folder.has("a"));
    assert!(restored.redo(&person(), &Grants::person()).unwrap().is_some());
    assert!(folder.has("a"));

    // From the start and the journal: replay rebuilds the step with its effect.
    let replayed = Executor::restore(
        Box::new(MemDoc::from_value(start)),
        clock,
        wall,
        &omuzaki_history_api::HistoryExport::empty(),
    )
    .unwrap();
    replayed.replay(&sink.numbered()).unwrap();
    replayed.rollback(tx, &person()).unwrap();
    assert!(replayed.undo(&person(), &Grants::person()).unwrap().is_some());
    assert!(!folder.has("a"), "undone with its effect after a replay");
}
