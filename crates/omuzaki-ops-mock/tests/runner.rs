//! The mock runner stays a valid oracle for the executor on the points the real runner is specific about.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, ErrorCode, Event, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Undo,
};
use omuzaki_ops_mock::MockRunner;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

fn counter() -> DocPath {
    DocPath::parse("counter").unwrap()
}

/// Sets `counter` to `value`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetCounter {
    value: i64,
}

impl Op for SetCounter {
    fn meta() -> OpMeta {
        OpMeta {
            name: "mocktest.set_counter",
            summary: "Set the counter.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new().with(counter(), ctx.doc.get(&counter()).cloned(), Some(json!(self.value)))))
    }
}

/// A composite op: it plans `mocktest.set_counter` as a nested step.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Composite;

impl Op for Composite {
    fn meta() -> OpMeta {
        OpMeta {
            name: "mocktest.composite",
            summary: "Plan a nested step.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.plan_step("mocktest.set_counter", json!({"value": 7}), ctx.doc).map(|(_, plan)| plan)
    }
}

/// Answers whether it is planned as a dry-run.
#[derive(Serialize, Deserialize, JsonSchema)]
struct AskDryRun;

impl Op for AskDryRun {
    fn meta() -> OpMeta {
        OpMeta {
            name: "mocktest.ask_dry_run",
            summary: "Say whether this is a dry-run.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!(ctx.dry_run)))
    }
}

/// Sets `counter` to `value`, which a caller may spell `val`; `note` has a default. Its arguments are not canonical as
/// sent, and it coalesces.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetAliased {
    #[serde(alias = "val")]
    value: i64,
    #[serde(default)]
    note: String,
}

impl Op for SetAliased {
    fn meta() -> OpMeta {
        OpMeta {
            name: "mocktest.set_aliased",
            summary: "Set the counter, with an alias and a default.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new().with(counter(), ctx.doc.get(&counter()).cloned(), Some(json!(self.value)))))
    }
}

/// A query that wrongly plans a change.
#[derive(Serialize, Deserialize, JsonSchema)]
struct BadQuery;

impl Op for BadQuery {
    fn meta() -> OpMeta {
        OpMeta {
            name: "mocktest.bad_query",
            summary: "A query that plans a patch.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::edit(Patch::new().with(counter(), None, Some(json!(1)))))
    }
}

/// An edit whose plan always fails, so only a check made before planning can refuse it as `Denied`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct AlwaysFails;

impl Op for AlwaysFails {
    fn meta() -> OpMeta {
        OpMeta {
            name: "mocktest.always_fails",
            summary: "An edit that cannot be planned.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Err(OpError::invalid("value", "never plannable"))
    }
}

omuzaki_ops_api::register_op!(BadQuery);
omuzaki_ops_api::register_op!(AlwaysFails);
omuzaki_ops_api::register_op!(SetAliased);
omuzaki_ops_api::register_op!(SetCounter);
omuzaki_ops_api::register_op!(Composite);
omuzaki_ops_api::register_op!(AskDryRun);

fn runner() -> MockRunner<MemDoc> {
    MockRunner::new(MemDoc::from_value(json!({"counter": 0})))
}

fn person() -> Actor {
    Actor::Person { name: "tester".to_owned() }
}

fn request(op: &str, args: serde_json::Value, mode: Mode, grants: Grants) -> Request {
    Request { op: op.to_owned(), args, mode, actor: person(), grants, cause: None, tx: None, base: None }
}

#[test]
fn a_composite_op_plans_its_nested_step_under_the_requests_grants() {
    let runner = runner();
    let grants = Grants::of([Scope::Read, Scope::EditScene]);
    let outcome = runner.run(request("mocktest.composite", json!(null), Mode::Apply, grants)).unwrap();
    assert!(outcome.applied.is_some());
    assert_eq!(runner.get(&counter(), &Grants::person()).unwrap(), Some(json!(7)));

    // Without the step's scope the nested step is refused, as the executor refuses it.
    let read_only = Grants::of([Scope::Read]);
    let error = runner.run(request("mocktest.composite", json!(null), Mode::Apply, read_only)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);
}

#[test]
fn a_dry_run_reaches_the_op_and_needs_only_read_for_nested_steps() {
    let runner = runner();
    let read_only = Grants::of([Scope::Read]);
    let preview = runner.run(request("mocktest.composite", json!(null), Mode::DryRun, read_only.clone())).unwrap();
    assert!(preview.applied.is_none());
    assert_eq!(runner.get(&counter(), &read_only).unwrap(), Some(json!(0)), "a dry-run changes nothing");

    let dry = runner.run(request("mocktest.ask_dry_run", json!(null), Mode::DryRun, read_only.clone())).unwrap();
    assert_eq!(dry.plan.output, Some(json!(true)));
    let real = runner.run(request("mocktest.ask_dry_run", json!(null), Mode::Apply, read_only)).unwrap();
    assert_eq!(real.plan.output, Some(json!(false)));
}

#[test]
fn a_rollback_that_conflicts_leaves_the_transaction_open() {
    let runner = runner();
    let me = person();
    let tx = runner.begin("set", &me);
    let mut in_tx = request("mocktest.set_counter", json!({"value": 1}), Mode::Apply, Grants::person());
    in_tx.tx = Some(tx);
    runner.run(in_tx).unwrap();
    // Someone else changes the same value, so the inverse of the transaction no longer applies.
    runner.run(request("mocktest.set_counter", json!({"value": 2}), Mode::Apply, Grants::person())).unwrap();

    let error = runner.rollback(tx, &me).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(runner.has_open_tx(), "the conflicted transaction stays open");
    runner.commit(tx, &me).unwrap();
    assert!(!runner.has_open_tx());
}

#[test]
fn a_rollback_by_another_actor_is_refused_and_keeps_the_transaction() {
    let runner = runner();
    let tx = runner.begin("set", &person());
    let error = runner.rollback(tx, &Actor::System).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied);
    assert!(runner.has_open_tx());
    runner.rollback(tx, &person()).unwrap();
    assert!(!runner.has_open_tx());
}

#[test]
fn ops_report_the_registrations_coalescing() {
    let ops = runner().ops();
    let by_name = |name: &str| ops.iter().find(|o| o.name == name).unwrap().coalesces;
    assert!(by_name("mocktest.set_aliased"), "an op that overrides `coalesces()` says so");
    assert!(!by_name("mocktest.set_counter"), "the default stays false");
}

#[test]
fn history_and_events_carry_the_decoded_ops_canonical_arguments() {
    let runner = runner();
    let grants = Grants::person();
    let events = runner.subscribe(&grants).unwrap();
    let sent = json!({"val": 3});
    let outcome = runner.run(request("mocktest.set_aliased", sent, Mode::Apply, grants.clone())).unwrap();

    let canonical = json!({"value": 3, "note": ""});
    assert_eq!(outcome.applied.unwrap().args, canonical);
    assert_eq!(runner.log(0, 10, &grants).unwrap()[0].args, canonical);
    match events.try_recv().unwrap() {
        Event::OpApplied { record, .. } => assert_eq!(record.args, canonical),
        other => panic!("expected OpApplied, got {other:?}"),
    }
}

#[test]
fn a_query_that_plans_a_change_is_an_internal_error() {
    let runner = runner();
    let error = runner.run(request("mocktest.bad_query", json!(null), Mode::Apply, Grants::person())).unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    let dry = runner.run(request("mocktest.bad_query", json!(null), Mode::DryRun, Grants::person())).unwrap_err();
    assert_eq!(dry.code, ErrorCode::Internal, "a dry-run plans it too");
    assert_eq!(runner.get(&counter(), &Grants::person()).unwrap(), Some(json!(0)));
}

#[test]
fn the_scope_is_checked_before_the_op_is_planned() {
    let runner = runner();
    let read_only = Grants::of([Scope::Read]);
    let error = runner.run(request("mocktest.always_fails", json!(null), Mode::Apply, read_only.clone())).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "an apply without the op's scope never reaches its plan");

    // A dry-run of an edit needs only `read`, so it does reach the plan and reports what the plan says.
    let dry = runner.run(request("mocktest.always_fails", json!(null), Mode::DryRun, read_only)).unwrap_err();
    assert_eq!(dry.code, ErrorCode::Invalid);
}

#[test]
fn nothing_runs_without_read_not_even_an_unknown_op() {
    let runner = runner();
    let none = Grants::of([]);
    let error = runner.run(request("mocktest.no_such_op", json!(null), Mode::DryRun, none)).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "`read` is checked before the op is looked up");
}
