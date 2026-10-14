//! `OpCtx::plan_step`: plan ops one after another against a scratch copy, as a composite op does.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch, Scratch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, ErrorDetail, Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

fn path(s: &str) -> DocPath {
    DocPath::parse(s).unwrap()
}

/// Sets `counter/value` to `value`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetValue {
    value: i64,
}

impl Op for SetValue {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stepcheck.set_value",
            summary: "Set the counter's value.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = path("counter/value");
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.value)))))
    }
}

/// Adds `by` to `counter/value`, so it only gives the right patch when planned against the result of earlier steps.
#[derive(Serialize, Deserialize, JsonSchema)]
struct AddValue {
    by: i64,
}

impl Op for AddValue {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stepcheck.add_value",
            summary: "Add to the counter's value.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = path("counter/value");
        let now = ctx.doc.get(&at).and_then(serde_json::Value::as_i64).unwrap_or(0);
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(now + self.by)))))
    }
}

/// A query step, to check that a dry-run still gates it.
#[derive(Serialize, Deserialize, JsonSchema)]
struct ReadValue {}

impl Op for ReadValue {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stepcheck.read_value",
            summary: "Read the counter's value.",
            kind: OpKind::Query,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(ctx.doc.get(&path("counter/value")).cloned().unwrap_or_default()))
    }
}

/// An edit that needs a person, to check that a step keeps the confirmation of the request it is planned in.
#[derive(Serialize, Deserialize, JsonSchema)]
struct PersonOnlyValue {}

impl Op for PersonOnlyValue {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stepcheck.person_value",
            summary: "Set the counter to 1, for a person only.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("only a person may set this")?;
        let at = path("counter/value");
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(1)))))
    }
}

omuzaki_ops_api::register_op!(PersonOnlyValue);
omuzaki_ops_api::register_op!(SetValue);
omuzaki_ops_api::register_op!(AddValue);
omuzaki_ops_api::register_op!(ReadValue);

fn actor() -> Actor {
    Actor::Agent { id: "test".into(), model: None }
}

#[test]
fn steps_planned_on_a_scratch_equal_running_them_in_turn() {
    let original = MemDoc::from_value(json!({"counter": {"value": 1}}));
    let (actor, clock, grants) =
        (actor(), FakeClock::new(Seconds(0.0)), Grants::of([Scope::Read, Scope::EditScene, Scope::EditCharacter]));
    let ctx = OpCtx::new(&original, &actor, &clock).with_grants(&grants);

    let mut scratch = Scratch::of(&original);
    let (meta_a, a) = ctx.plan_step("stepcheck.set_value", json!({"value": 10}), &scratch).unwrap();
    assert_eq!(meta_a.name, "stepcheck.set_value");
    scratch.apply(&a.patch).unwrap();
    // B only comes out right when planned after A: 10 + 5, not 1 + 5.
    let (_, b) = ctx.plan_step("stepcheck.add_value", json!({"by": 5}), &scratch).unwrap();
    scratch.apply(&b.patch).unwrap();

    let mut combined = a.patch.clone();
    combined.extend(b.patch.clone());

    let mut direct = original.clone();
    direct.apply(&a.patch).unwrap();
    direct.apply(&b.patch).unwrap();
    let mut via_combined = original.clone();
    via_combined.apply(&combined).unwrap();
    assert_eq!(via_combined.root(), direct.root());
    assert_eq!(via_combined.root(), scratch.root());
    assert_eq!(via_combined.get(&path("counter/value")), Some(&json!(15)));

    via_combined.apply(&combined.inverse()).unwrap();
    assert_eq!(via_combined.root(), original.root());
    // Planning never touched the original.
    assert_eq!(original.get(&path("counter/value")), Some(&json!(1)));
}

#[test]
fn a_step_whose_scope_is_missing_is_refused_unless_dry_run() {
    let doc = MemDoc::from_value(json!({"counter": {"value": 1}}));
    let (actor, clock) = (actor(), FakeClock::new(Seconds(0.0)));
    let grants = Grants::of([Scope::Read, Scope::EditCharacter]);

    let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants);
    let err = ctx.plan_step("stepcheck.set_value", json!({"value": 2}), &doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::Denied);
    assert!(err.message.contains("edit:scene"), "{}", err.message);
    assert!(err.hint.as_deref().is_some_and(|h| h.contains("edit:scene")));
    // The granted scope is fine.
    ctx.plan_step("stepcheck.add_value", json!({"by": 1}), &doc).unwrap();

    let dry = OpCtx::new(&doc, &actor, &clock).with_grants(&grants).with_dry_run(true);
    dry.plan_step("stepcheck.set_value", json!({"value": 2}), &doc).unwrap();
}

#[test]
fn a_context_with_no_grants_refuses_every_applied_step() {
    let doc = MemDoc::new();
    let (actor, clock) = (actor(), FakeClock::new(Seconds(0.0)));
    let err = OpCtx::new(&doc, &actor, &clock).plan_step("stepcheck.add_value", json!({"by": 1}), &doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::Denied);
}

#[test]
fn a_query_step_is_gated_even_in_a_dry_run() {
    let doc = MemDoc::from_value(json!({"counter": {"value": 3}}));
    let (actor, clock) = (actor(), FakeClock::new(Seconds(0.0)));
    let none = Grants::of([Scope::Read]);
    let err = OpCtx::new(&doc, &actor, &clock)
        .with_grants(&none)
        .with_dry_run(true)
        .plan_step("stepcheck.read_value", json!({}), &doc)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Denied);
    assert!(err.message.contains("perform"));

    let some = Grants::of([Scope::Read, Scope::Perform]);
    let (_, plan) = OpCtx::new(&doc, &actor, &clock)
        .with_grants(&some)
        .with_dry_run(true)
        .plan_step("stepcheck.read_value", json!({}), &doc)
        .unwrap();
    assert_eq!(plan.output, Some(json!(3)));
}

#[test]
fn an_unknown_op_names_op_and_suggests_its_area() {
    let doc = MemDoc::new();
    let (actor, clock) = (actor(), FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, &actor, &clock);
    let err = ctx.plan_step("stepcheck.set_vlaue", json!({}), &doc).unwrap_err();
    assert_eq!((err.code, err.field.as_deref()), (ErrorCode::Invalid, Some("op")));
    let hint = err.hint.unwrap();
    assert!(hint.contains("stepcheck.set_value") && hint.contains("stepcheck.add_value"), "{hint}");

    let err = ctx.plan_step("nosuch", json!({}), &doc).unwrap_err();
    assert_eq!(err.field.as_deref(), Some("op"));
}

#[test]
fn bad_arguments_name_the_field() {
    let doc = MemDoc::new();
    let (actor, clock) = (actor(), FakeClock::new(Seconds(0.0)));
    let err = OpCtx::new(&doc, &actor, &clock).plan_step("stepcheck.add_value", json!({"by": "x"}), &doc).unwrap_err();
    assert_eq!((err.code, err.field.as_deref()), (ErrorCode::Invalid, Some("by")));
}

#[test]
fn a_step_keeps_the_persons_confirmation_of_its_request() {
    let doc = MemDoc::default();
    let (agent, person) = (actor(), Actor::Person { name: "owner".into() });
    let clock = FakeClock::new(Seconds(0.0));
    let grants = Grants::of([Scope::Read, Scope::EditScene]);
    let unconfirmed = OpCtx::new(&doc, &agent, &clock).with_grants(&grants);
    let err = unconfirmed.plan_step("stepcheck.person_value", json!({}), &doc).unwrap_err();
    assert_eq!(err.code, ErrorCode::Denied);
    assert!(matches!(err.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })), "{err:?}");
    let confirmed = OpCtx::new(&doc, &agent, &clock).with_grants(&grants).with_confirmation(&person);
    confirmed.plan_step("stepcheck.person_value", json!({}), &doc).unwrap();
}
