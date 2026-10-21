//! Parity between the executor and `omuzaki-ops-mock`'s `MockRunner`: each test sends the same calls to both, over
//! the same document, and every answer must be equal, error text included. Both make their checks through
//! `omuzaki_ops_api::checks`; these tests pin that they do, one check per test.

// Test helpers may panic: a panic is a clear failure (clippy.toml exempts only `#[test]` functions).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Debug;
use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, ErrorCode, Grants, Limits, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope,
    SessionSpec, Sessions, Transport, Undo, register_op,
};
use omuzaki_ops_mock::MockRunner;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};

fn counter() -> DocPath {
    DocPath::parse("counter").unwrap()
}

fn effect() -> DocPath {
    DocPath::parse("effect").unwrap()
}

/// A patch that sets `counter` to `value`.
fn set_counter(ctx: &OpCtx<'_>, value: i64) -> Patch {
    Patch::new().with(counter(), ctx.doc.get(&counter()).cloned(), Some(json!(value)))
}

/// An op's facts, its summary the name. `parity.set` has a summary of its own, which pins that both runners label a
/// one-op undo step by the summary (#1662).
const fn meta(name: &'static str, kind: OpKind, scope: Scope, undo: Undo) -> OpMeta {
    OpMeta { name, summary: name, kind, scope, undo }
}

/// The summary `parity.set`'s one-op steps are labelled with.
const SET_SUMMARY: &str = "Set the counter.";

/// Sets `counter`. Callers may spell `value` as `val`, and `note` has a default, so its arguments as sent are not
/// canonical. It coalesces.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Set {
    #[serde(alias = "val")]
    value: i64,
    #[serde(default)]
    note: String,
}

impl Op for Set {
    fn meta() -> OpMeta {
        OpMeta { summary: SET_SUMMARY, ..meta("parity.set", OpKind::Edit, Scope::EditScene, Undo::Auto) }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(set_counter(ctx, self.value)))
    }
}
register_op!(Set);

/// Sets `counter`, under `publish`: a scope the default agent grants lack.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Publish {
    value: i64,
}

impl Op for Publish {
    fn meta() -> OpMeta {
        meta("parity.publish", OpKind::Edit, Scope::Publish, Undo::Auto)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(set_counter(ctx, self.value)))
    }
}
register_op!(Publish);

/// Answers with `counter`, under `publish`: its dry-run is gated like the real thing.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Peek;

impl Op for Peek {
    fn meta() -> OpMeta {
        meta("parity.peek", OpKind::Query, Scope::Publish, Undo::None)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(ctx.doc.get(&counter()).cloned().unwrap_or(Value::Null)))
    }
}
register_op!(Peek);

/// An edit that plans a patch with no changes at all.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Nothing;

impl Op for Nothing {
    fn meta() -> OpMeta {
        meta("parity.nothing", OpKind::Edit, Scope::EditScene, Undo::Auto)
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new()))
    }
}
register_op!(Nothing);

/// A query that wrongly plans a change.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SneakyQuery;

impl Op for SneakyQuery {
    fn meta() -> OpMeta {
        meta("parity.sneaky_query", OpKind::Query, Scope::Read, Undo::None)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(set_counter(ctx, 99)))
    }
}
register_op!(SneakyQuery);

/// Marks `effect` and is `Undo::Explicit`, but records no effect outside the document: undone by its patch.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Explicit;

impl Op for Explicit {
    fn meta() -> OpMeta {
        meta("parity.explicit", OpKind::Edit, Scope::EditScene, Undo::Explicit)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new().with(effect(), ctx.doc.get(&effect()).cloned(), Some(json!(true)))))
    }
}
register_op!(Explicit);

/// An argument whose `Serialize` always fails, so its op's arguments can't be recorded.
#[derive(Deserialize, JsonSchema)]
struct Unrecordable(i64);

impl Serialize for Unrecordable {
    fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("this argument can't be recorded"))
    }
}

/// An edit under `publish` whose arguments can't be serialised.
#[derive(Serialize, Deserialize, JsonSchema)]
struct UnrecordableEdit {
    value: Unrecordable,
}

impl Op for UnrecordableEdit {
    fn meta() -> OpMeta {
        meta("parity.unrecordable_edit", OpKind::Edit, Scope::Publish, Undo::Auto)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(set_counter(ctx, self.value.0)))
    }
}
register_op!(UnrecordableEdit);

/// A query whose arguments can't be serialised.
#[derive(Serialize, Deserialize, JsonSchema)]
struct UnrecordableQuery {
    value: Unrecordable,
}

impl Op for UnrecordableQuery {
    fn meta() -> OpMeta {
        meta("parity.unrecordable_query", OpKind::Query, Scope::Read, Undo::None)
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!(self.value.0)))
    }
}
register_op!(UnrecordableQuery);

fn person() -> Actor {
    Actor::Person { name: "alice".into() }
}

fn agent_session() -> SessionSpec {
    SessionSpec {
        transport: Transport::Stdio,
        client: "claude".into(),
        model: None,
        on_behalf_of: person(),
        grants: Grants::person(),
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: Vec::new(),
    }
}

fn agent() -> Actor {
    Actor::Agent { id: "claude#1".into(), model: None }
}

/// The executor and the mock over the same starting document. Every call goes to both and asserts that they answer
/// alike, then returns the (shared) answer.
struct Pair {
    exec: Executor,
    mock: MockRunner<MemDoc>,
}

impl Pair {
    fn new() -> Self {
        let clock = Arc::new(FakeClock::new(Seconds(0.0)));
        let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap();
        // The mock has no sessions: the executor's one is the agent `agent()` names.
        let session = exec.open(agent_session()).unwrap();
        assert_eq!(session.actor, agent());
        Self { exec, mock: MockRunner::new(MemDoc::from_value(json!({}))) }
    }

    fn both<T: PartialEq + Debug>(&self, call: impl Fn(&dyn Runner) -> T) -> T {
        let (exec, mock) = (call(&self.exec), call(&self.mock));
        assert_eq!(exec, mock, "the executor (left) and the mock (right) disagree");
        exec
    }

    fn run(&self, req: &Request) -> Result<Value, OpError> {
        // The whole outcome, as JSON: the plan and the record, with its arguments, transaction and version.
        self.both(|runner| runner.run(req.clone()).map(|outcome| serde_json::to_value(outcome).unwrap()))
    }

    fn undo(&self, grants: &Grants) -> Result<Option<String>, OpError> {
        self.both(|runner| runner.undo(&person(), grants))
    }

    fn redo(&self, grants: &Grants) -> Result<Option<String>, OpError> {
        self.both(|runner| runner.redo(&person(), grants))
    }

    fn counter(&self) -> Option<Value> {
        self.both(|runner| runner.get(&counter(), &Grants::person()).unwrap())
    }

    fn log(&self) -> Value {
        self.both(|runner| serde_json::to_value(runner.log(0, usize::MAX, &Grants::person()).unwrap()).unwrap())
    }
}

/// Applies `op` with `args` as a person who may do anything.
fn request(op: &str, args: Value) -> Request {
    Request {
        op: op.into(),
        args,
        mode: Mode::Apply,
        actor: person(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    }
}

fn dry_run(req: Request) -> Request {
    Request { mode: Mode::DryRun, ..req }
}

fn as_agent(req: Request) -> Request {
    Request { actor: agent(), grants: Grants::agent_default(), ..req }
}

fn code<T: Debug>(result: Result<T, OpError>) -> ErrorCode {
    result.unwrap_err().code
}

#[test]
fn scopes_gate_an_apply_and_a_query_but_not_the_dry_run_of_an_edit() {
    let pair = Pair::new();
    let publish = request("parity.publish", json!({"value": 1}));
    assert_eq!(code(pair.run(&as_agent(publish.clone()))), ErrorCode::Denied);
    assert!(pair.run(&dry_run(as_agent(publish.clone()))).is_ok(), "a dry-run of an edit needs only `read`");
    assert_eq!(code(pair.run(&dry_run(as_agent(request("parity.peek", json!(null)))))), ErrorCode::Denied);
    let no_read = Request { grants: Grants::of([Scope::Publish]), ..dry_run(publish) };
    assert_eq!(code(pair.run(&no_read)), ErrorCode::Denied, "nothing runs without `read`, not even a dry-run");
    assert_eq!(pair.counter(), None);
}

#[test]
fn undo_and_redo_need_every_scope_their_step_recorded() {
    let pair = Pair::new();
    pair.run(&request("parity.publish", json!({"value": 1}))).unwrap();
    let read_only = Grants::of([Scope::Read]);
    assert_eq!(code(pair.undo(&read_only)), ErrorCode::Denied);
    assert_eq!(pair.counter(), Some(json!(1)), "a refused undo leaves the document and the step alone");
    assert_eq!(pair.undo(&Grants::person()).unwrap().as_deref(), Some("parity.publish"));
    assert_eq!(code(pair.redo(&read_only)), ErrorCode::Denied);
    assert_eq!(pair.redo(&Grants::person()).unwrap().as_deref(), Some("parity.publish"));
    assert_eq!(pair.counter(), Some(json!(1)));
}

#[test]
fn a_transaction_records_the_scopes_of_the_ops_that_changed_something() {
    let pair = Pair::new();
    let tx = pair.both(|runner| runner.begin("both", &person()));
    pair.run(&Request { tx: Some(tx), ..request("parity.set", json!({"value": 1})) }).unwrap();
    pair.run(&Request { tx: Some(tx), ..request("parity.publish", json!({"value": 2})) }).unwrap();
    pair.both(|runner| runner.commit(tx, &person())).unwrap();
    assert_eq!(code(pair.undo(&Grants::of([Scope::Read, Scope::EditScene]))), ErrorCode::Denied);
    assert_eq!(pair.undo(&Grants::person()).unwrap().as_deref(), Some("both"));
    assert_eq!(pair.counter(), None);
}

#[test]
fn a_query_that_plans_a_patch_is_internal() {
    let pair = Pair::new();
    let sneaky = request("parity.sneaky_query", json!(null));
    assert_eq!(code(pair.run(&sneaky)), ErrorCode::Internal);
    assert_eq!(code(pair.run(&dry_run(sneaky))), ErrorCode::Internal);
    assert_eq!(pair.counter(), None);
}

#[test]
fn no_op_edits_stay_out_of_history_and_keep_redo() {
    let pair = Pair::new();
    pair.run(&request("parity.set", json!({"value": 1}))).unwrap();
    pair.run(&request("parity.set", json!({"value": 2}))).unwrap();
    pair.undo(&Grants::person()).unwrap();
    // Setting the value it already has changes nothing: no step, and the redo of `2` survives.
    pair.run(&request("parity.set", json!({"value": 1}))).unwrap();
    assert_eq!(pair.redo(&Grants::person()).unwrap().as_deref(), Some(SET_SUMMARY));
    assert_eq!(pair.counter(), Some(json!(2)));
    pair.undo(&Grants::person()).unwrap();
    pair.undo(&Grants::person()).unwrap();
    assert_eq!(pair.counter(), None, "the second undo reached the first real edit, not the no-op");
    assert_eq!(pair.undo(&Grants::person()).unwrap(), None);
}

#[test]
fn an_empty_patch_keeps_the_version_and_redo() {
    let pair = Pair::new();
    pair.run(&request("parity.set", json!({"value": 1}))).unwrap();
    pair.run(&request("parity.set", json!({"value": 2}))).unwrap();
    pair.undo(&Grants::person()).unwrap();
    let logged = pair.log().as_array().unwrap().len();
    // `pair.run` compares the whole outcome, the record's version included, and `pair.log` the whole log.
    let once = pair.run(&request("parity.nothing", json!(null))).unwrap();
    let twice = pair.run(&request("parity.nothing", json!(null))).unwrap();
    assert_eq!(once["applied"]["version"], twice["applied"]["version"], "an empty patch keeps the version");
    assert_eq!(pair.log().as_array().unwrap().len(), logged + 2, "each run is still logged");
    assert_eq!(pair.redo(&Grants::person()).unwrap().as_deref(), Some(SET_SUMMARY));
    assert_eq!(pair.counter(), Some(json!(2)));
    assert_eq!(pair.undo(&Grants::person()).unwrap().as_deref(), Some(SET_SUMMARY));
    assert_eq!(pair.counter(), Some(json!(1)), "undo skipped the empty step");
}

#[test]
fn an_explicit_record_without_an_effect_is_undone_by_its_patch() {
    let pair = Pair::new();
    pair.run(&request("parity.set", json!({"value": 1}))).unwrap();
    pair.run(&request("parity.explicit", json!(null))).unwrap();
    assert_eq!(pair.undo(&Grants::person()).unwrap().as_deref(), Some("parity.explicit"));
    let effect = pair.both(|runner| runner.get(&effect(), &Grants::person()).unwrap());
    assert_eq!(effect, None, "its patch was reverted");
    assert_eq!(pair.undo(&Grants::person()).unwrap().as_deref(), Some(SET_SUMMARY), "labelled by its summary");
    assert_eq!(pair.counter(), None);
}

#[test]
fn the_log_records_the_canonical_arguments() {
    let pair = Pair::new();
    let applied = pair.run(&request("parity.set", json!({"val": 3}))).unwrap();
    assert_eq!(applied["applied"]["args"], json!({"value": 3, "note": ""}));
    assert_eq!(pair.log()[0]["args"], json!({"value": 3, "note": ""}));
}

#[test]
fn the_op_list_takes_coalesces_from_the_registration() {
    let pair = Pair::new();
    let ops = pair.both(|runner| runner.ops());
    let set = ops.iter().find(|op| op.name == "parity.set").unwrap();
    assert!(set.coalesces);
    assert!(!ops.iter().find(|op| op.name == "parity.publish").unwrap().coalesces);
}

#[test]
fn arguments_are_serialised_only_to_apply_after_authorization() {
    let pair = Pair::new();
    let edit = request("parity.unrecordable_edit", json!({"value": 1}));
    assert!(pair.run(&dry_run(edit.clone())).is_ok(), "a dry-run records nothing");
    assert!(pair.run(&request("parity.unrecordable_query", json!({"value": 1}))).is_ok(), "a query records nothing");
    assert_eq!(code(pair.run(&as_agent(edit.clone()))), ErrorCode::Denied, "authorization comes first");
    assert_eq!(code(pair.run(&edit)), ErrorCode::Internal, "an apply must record its arguments");
    assert_eq!(pair.counter(), None, "nothing applied without a record");
}
