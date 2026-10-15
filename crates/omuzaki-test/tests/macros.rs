//! Macros over the real executor and the in-memory document: what the recorder keeps of a person's session, and that
//! playing it back reproduces the document as one undo step. The end-to-end half of `omuzaki-script`'s macro tests;
//! it lives here because an implementation crate may not depend on the executor.

// A test file: a failed unwrap is the failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Cause, CauseKind, ErrorCode, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope,
    Undo, register_op, registrations,
};
use omuzaki_script::{Macro, MacroRecorder, PlayError, RecordedCall, from_jsonl, play, to_jsonl};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Sets the value at a path (a plain edit: calls are kept as recorded).
#[derive(Serialize, Deserialize, JsonSchema)]
struct Set {
    path: DocPath,
    value: Value,
}

fn replace(ctx: &OpCtx<'_>, path: &DocPath, value: &Value) -> Result<Plan, OpError> {
    let (parent, _) = path.split_last().ok_or_else(|| OpError::invalid("path", "can't replace the root"))?;
    if ctx.doc.get(&parent).is_none() {
        return Err(OpError::not_found(format!("no part at {parent}")).hint("create the part first"));
    }
    let before = ctx.doc.get(path).cloned();
    Ok(Plan::edit(Patch::new().with(path.clone(), before, Some(value.clone()))))
}

impl Op for Set {
    fn meta() -> OpMeta {
        OpMeta {
            name: "macrotest.set",
            summary: "Set the value at a path.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        replace(ctx, &self.path, &self.value)
    }
}
register_op!(Set);

/// Sets the value at a path as an absolute setter, so a slider drag of it coalesces.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Setpoint {
    path: DocPath,
    value: Value,
}

impl Op for Setpoint {
    fn meta() -> OpMeta {
        OpMeta {
            name: "macrotest.setpoint",
            summary: "Set the value at a path; a drag of it coalesces.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        replace(ctx, &self.path, &self.value)
    }

    fn coalesces() -> bool {
        true
    }
}
register_op!(Setpoint);

/// Answers a value without changing anything.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Read {
    path: DocPath,
}

impl Op for Read {
    fn meta() -> OpMeta {
        OpMeta {
            name: "macrotest.read",
            summary: "Answer the value at a path.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(ctx.doc.get(&self.path).cloned().unwrap_or(Value::Null)))
    }
}
register_op!(Read);

fn alice() -> Actor {
    Actor::Person { name: "alice".into() }
}

fn bob() -> Actor {
    Actor::Person { name: "bob".into() }
}

fn start_document() -> Value {
    json!({ "face": { "profile": "none", "blend": 0.0 }, "hair": { "colour": "black" }, "eye": { "size": 1.0 }, "brow": { "tilt": 0 } })
}

fn executor(doc: Value) -> Executor {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let ours = registrations().filter(|r| (r.meta)().name.starts_with("macrotest."));
    Executor::with_registrations(Box::new(MemDoc::from_value(doc)), clock, ours).unwrap()
}

fn document(runner: &dyn Runner) -> Value {
    runner.get(&DocPath::root(), &Grants::person()).unwrap().unwrap()
}

/// The person (or `actor`) applies `op` on their own, as the UI does: cause `Direct`, no transaction.
fn apply_as(runner: &dyn Runner, actor: &Actor, op: &str, args: Value) -> Result<(), OpError> {
    runner
        .run(Request {
            op: op.into(),
            args,
            mode: Mode::Apply,
            actor: actor.clone(),
            grants: Grants::person(),
            cause: Some(Cause { kind: CauseKind::Direct, text: None }),
            tx: None,
            base: None,
        })
        .map(|_| ())
}

fn set(runner: &dyn Runner, op: &str, at: &str, value: Value) {
    apply_as(runner, &alice(), op, json!({ "path": at, "value": value })).unwrap();
}

/// Three distinct edits, one that is undone, and a 20-record slider drag of one numeric field.
fn record_session(runner: &Executor) -> Macro {
    let mut recorder = MacroRecorder::start(runner, &alice(), &Grants::person()).unwrap();
    set(runner, "macrotest.set", "face/profile", json!("egg-oval"));
    set(runner, "macrotest.set", "hair/colour", json!("amber"));
    set(runner, "macrotest.set", "brow/tilt", json!(3));
    set(runner, "macrotest.set", "eye/size", json!(2.5));
    assert_eq!(runner.undo(&alice(), &Grants::person()).unwrap().as_deref(), Some("macrotest.set"));
    recorder.poll();
    for i in 1..=20 {
        set(runner, "macrotest.setpoint", "face/blend", json!(f64::from(i) / 20.0));
    }
    recorder.finish("oval and amber")
}

#[test]
fn records_three_edits_an_undone_one_and_a_slider_drag() {
    let runner = executor(start_document());
    let m = record_session(&runner);
    assert_eq!(m.name, "oval and amber");
    assert_eq!(m.steps.len(), 4, "{m:?}");
    assert_eq!(
        m.steps[..3],
        [
            RecordedCall { op: "macrotest.set".into(), args: json!({ "path": "face/profile", "value": "egg-oval" }) },
            RecordedCall { op: "macrotest.set".into(), args: json!({ "path": "hair/colour", "value": "amber" }) },
            RecordedCall { op: "macrotest.set".into(), args: json!({ "path": "brow/tilt", "value": 3 }) },
        ]
    );
    assert_eq!(m.steps[3].op, "macrotest.setpoint");
    assert_eq!(m.steps[3].args, json!({ "path": "face/blend", "value": 1.0 }), "the last step carries the final value");
}

#[test]
fn queries_and_another_persons_edits_are_never_recorded() {
    let runner = executor(start_document());
    let mut recorder = MacroRecorder::start(&runner, &alice(), &Grants::person()).unwrap();
    apply_as(&runner, &alice(), "macrotest.read", json!({ "path": "face" })).unwrap();
    apply_as(&runner, &bob(), "macrotest.set", json!({ "path": "hair/colour", "value": "teal" })).unwrap();
    // Alice undoing does not undo Bob's edit, and his edit is not hers to keep or drop.
    assert_eq!(runner.undo(&alice(), &Grants::person()).unwrap(), None);
    set(&runner, "macrotest.set", "eye/size", json!(4.0));
    recorder.poll();
    let m = recorder.finish("mine");
    assert_eq!(
        m.steps,
        vec![RecordedCall { op: "macrotest.set".into(), args: json!({ "path": "eye/size", "value": 4.0 }) }]
    );
}

#[test]
fn play_reproduces_the_document() {
    let recorded = executor(start_document());
    let m = record_session(&recorded);

    let fresh = executor(start_document());
    let report = play(&fresh, &m, &alice(), &Grants::person()).unwrap();
    assert_eq!(report.steps, 4);
    assert_eq!(document(&fresh), document(&recorded), "the replay reproduces the recorded document");

    // One transaction: a single undo restores the empty (starting) document.
    assert_eq!(fresh.undo(&alice(), &Grants::person()).unwrap().as_deref(), Some("oval and amber"));
    assert_eq!(document(&fresh), start_document());
    assert_eq!(fresh.undo(&alice(), &Grants::person()).unwrap(), None, "the macro was one undo step");
}

#[test]
fn a_replay_is_not_recorded_by_a_recorder_that_is_running() {
    let runner = executor(start_document());
    let m = Macro {
        name: "replayed".into(),
        steps: vec![RecordedCall {
            op: "macrotest.set".into(),
            args: json!({ "path": "hair/colour", "value": "red" }),
        }],
    };
    let mut recorder = MacroRecorder::start(&runner, &alice(), &Grants::person()).unwrap();
    play(&runner, &m, &alice(), &Grants::person()).unwrap();
    recorder.poll();
    assert_eq!(recorder.finish("x").steps, vec![], "a replay carries the Script cause, which is never recorded");
}

#[test]
fn failure_rolls_back() {
    let runner = executor(start_document());
    let m = Macro {
        name: "broken".into(),
        steps: vec![
            RecordedCall { op: "macrotest.set".into(), args: json!({ "path": "hair/colour", "value": "teal" }) },
            RecordedCall { op: "macrotest.set".into(), args: json!({ "path": "nowhere/colour", "value": "x" }) },
            RecordedCall { op: "macrotest.set".into(), args: json!({ "path": "eye/size", "value": 9 }) },
        ],
    };
    let error: PlayError = play(&runner, &m, &alice(), &Grants::person()).expect_err("step 2 names a missing part");
    assert_eq!((error.step, error.op.as_str()), (2, "macrotest.set"));
    assert_eq!(error.error.code, ErrorCode::NotFound, "the op's own error: {}", error.error);
    assert_eq!(document(&runner), start_document(), "the first step was rolled back");
    assert_eq!(runner.undo(&alice(), &Grants::person()).unwrap(), None, "nothing was committed");
}

#[test]
fn missing_scope_is_named() {
    let runner = executor(start_document());
    let m = Macro {
        name: "needs a scope".into(),
        steps: vec![
            RecordedCall { op: "macrotest.read".into(), args: json!({ "path": "face" }) },
            RecordedCall { op: "macrotest.set".into(), args: json!({ "path": "hair/colour", "value": "teal" }) },
        ],
    };
    let grants = Grants::person().without(Scope::EditCharacter);
    let error = play(&runner, &m, &alice(), &grants).expect_err("denied");
    assert_eq!(error.step, 2);
    assert_eq!(error.error.code, ErrorCode::Denied);
    assert!(error.error.message.contains("`edit:character`"), "the denial names the scope to grant: {}", error.error);
    assert_eq!(document(&runner), start_document());
}

#[test]
fn jsonl_round_trip_is_byte_stable() {
    let runner = executor(start_document());
    let m = record_session(&runner);
    let text = to_jsonl(&m);
    assert_eq!(text.lines().count(), 5, "a header and four calls");
    assert!(text.starts_with("{\"macro\":1,\"name\":\"oval and amber\"}\n"));
    let back = from_jsonl(&text).unwrap();
    assert_eq!(back, m);
    assert_eq!(to_jsonl(&back), text);
}

#[test]
fn from_jsonl_refuses_with_line_numbers() {
    let header = "{\"macro\":1,\"name\":\"m\"}\n";
    let cases = [
        ("{\"macro\":9,\"name\":\"m\"}\n".to_owned(), 1, "unknown macro version"),
        (format!("{header}{{\"args\":{{}}}}\n"), 2, "`op`"),
        (format!("{header}{{\"op\":\"a.b\",\"args\":[]}}\n"), 2, "must be an object"),
        (format!("{header}{{\"op\":\"a.b\",\"args\":{{}}}}\nnope\n"), 3, "not JSON"),
    ];
    for (text, line, reason) in cases {
        let error = from_jsonl(&text).unwrap_err();
        assert_eq!(error.line, line, "{text}: {error}");
        assert!(error.reason.contains(reason), "{error}");
    }
}
