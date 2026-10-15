//! The mock runner stays a valid oracle for the executor on the points the real runner is specific about.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, ErrorCode, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Request, Runner, Scope, Undo,
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
