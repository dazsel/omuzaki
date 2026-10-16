//! `run`: lookup, decoding, planning, dry-run, permissions, queries, errors, the log and the registry.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::{Executor, ExecutorError};
use omuzaki_ops_api::{Cause, CauseKind, ErrorCode, Grants, Mode, OpRegistration, Request, Runner, Scope, find};
use serde_json::json;

use super::ops::Set;
use super::{agent, drain, path, request, rig};

#[test]
fn ops_are_listed_sorted_with_inlined_schemas() {
    let rig = rig(json!({}));
    let ops = rig.exec.ops();
    let names: Vec<&str> = ops.iter().map(|o| o.name).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    let set = ops.iter().find(|o| o.name == "test.set").unwrap();
    assert_eq!((set.scope, set.summary), (Scope::EditCharacter, "Set the value at a path."));
    let schema = serde_json::to_value(&set.schema).unwrap();
    assert!(schema["properties"]["path"].is_object(), "schema: {schema}");
}

#[test]
fn duplicate_names_fail_startup_and_name_both_registrations() {
    // A second op type claiming `test.set`, built from explicit registrations (not the global registry).
    #[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
    struct Impostor {}
    impl omuzaki_ops_api::Op for Impostor {
        fn meta() -> omuzaki_ops_api::OpMeta {
            omuzaki_ops_api::OpMeta {
                name: "test.set",
                summary: "The impostor.",
                kind: omuzaki_ops_api::OpKind::Edit,
                scope: Scope::EditScene,
                undo: omuzaki_ops_api::Undo::Auto,
            }
        }
        fn plan(&self, _: &omuzaki_ops_api::OpCtx<'_>) -> Result<omuzaki_ops_api::Plan, omuzaki_ops_api::OpError> {
            Ok(omuzaki_ops_api::Plan::default())
        }
    }
    static REAL: OpRegistration = OpRegistration::of::<Set>();
    static FAKE: OpRegistration = OpRegistration::of::<Impostor>();

    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let Err(err) = Executor::with_registrations(Box::new(MemDoc::new()), clock, [&REAL, &FAKE]) else {
        panic!("duplicate names must be refused");
    };
    assert_eq!(
        err,
        ExecutorError::DuplicateOp { name: "test.set", first: "Set the value at a path.", second: "The impostor." }
    );
    let message = err.to_string();
    assert!(message.contains("Set the value at a path.") && message.contains("The impostor."), "{message}");
}

#[test]
fn the_global_registry_is_used_by_new() {
    assert!(find("test.set").is_some());
    assert!(rig(json!({})).exec.ops().iter().any(|o| o.name == "test.get"));
}

#[test]
fn unknown_ops_and_bad_arguments_are_invalid_with_a_hint() {
    let rig = rig(json!({}));
    let e = rig.run("test.nope", json!({})).unwrap_err();
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("op")));
    let hint = e.hint.unwrap();
    assert!(
        hint.contains("`test` area") && hint.contains("test.broken"),
        "the hint lists ops in the same area: {hint}"
    );

    let e = rig.run("nope", json!({})).unwrap_err();
    assert!(e.hint.unwrap().contains("tools/list"));
}

#[test]
fn malformed_arguments_are_invalid() {
    let rig = rig(json!({}));
    let e = rig.run("test.set", json!({"path": "/leading/slash", "value": 1})).unwrap_err();
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("path")));
    let e = rig.run("test.set", json!({"value": 1})).unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

#[test]
fn dry_run_returns_the_plan_and_changes_nothing() {
    let rig = rig(json!({"a": 1}));
    let rx = rig.subscribe();
    let req = Request { mode: Mode::DryRun, ..request("test.set", json!({"path": "a", "value": 2})) };
    let out = rig.exec.run(req).unwrap();
    assert_eq!(out.plan.patch.changes.len(), 1);
    assert_eq!(out.plan.patch.changes[0].after, Some(json!(2)));
    assert!(out.applied.is_none());
    assert_eq!(rig.at("a"), Some(json!(1)));
    assert_eq!(rig.log(), vec![]);
    assert_eq!(drain(&rx), vec![]);
    assert!(rig.undo().unwrap().is_none(), "a dry-run is not an undo step");
}

#[test]
fn apply_changes_the_document_and_logs_provenance() {
    let rig = rig(json!({"a": 1}));
    rig.clock.set(Seconds(12.5));
    let cause = Cause { kind: CauseKind::Prompt, text: Some("make it two".into()) };
    let req =
        Request { cause: Some(cause.clone()), actor: agent(), ..request("test.set", json!({"path": "a", "value": 2})) };
    let out = rig.exec.run(req).unwrap();
    assert_eq!(rig.at("a"), Some(json!(2)));

    let record = out.applied.unwrap();
    assert_eq!((record.seq, record.op.as_str()), (1, "test.set"));
    assert_eq!(record.args, json!({"path": "a", "value": 2}));
    assert_eq!((&record.actor, &record.cause), (&agent(), &Some(cause)));
    assert_eq!(record.at, Seconds(12.5));
    assert_eq!(record.patch, out.plan.patch);
    assert_eq!(record.version.0, 1);
    assert_eq!(rig.log(), vec![record]);
}

#[test]
fn log_sequence_numbers_run_from_one_and_versions_follow_the_document() {
    let rig = rig(json!({}));
    let records: Vec<_> = (0..3).map(|i| rig.set(&format!("k{i}"), json!(i)).applied.unwrap()).collect();
    assert_eq!(records.iter().map(|r| r.seq).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(records.iter().map(|r| r.version.0).collect::<Vec<_>>(), [1, 2, 3]);
    assert_eq!(rig.log(), records);
}

#[test]
fn ops_see_the_executors_clock() {
    let rig = rig(json!({"t": 0}));
    rig.clock.set(Seconds(3.25));
    rig.run("test.stamp", json!({"path": "t"})).unwrap();
    assert_eq!(rig.at("t"), Some(json!(3.25)));
}

#[test]
fn a_missing_scope_is_denied_with_a_hint_naming_it_and_nothing_changes() {
    let rig = rig(json!({"a": 1}));
    let rx = rig.subscribe();
    let req = Request {
        actor: agent(),
        grants: Grants::agent_default(),
        ..request("test.publish", json!({"path": "a", "value": 2}))
    };
    let e = rig.exec.run(req.clone()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(e.hint.as_deref().unwrap().contains("publish"), "{e}");
    assert!(e.message.contains("agent claude#1") && e.message.contains("test.publish"), "{e}");
    assert_eq!(rig.at("a"), Some(json!(1)));
    assert!(rig.log().is_empty() && drain(&rx).is_empty());

    // A per-session grant is enough.
    let granted = Request { grants: Grants::agent_default().with(Scope::Publish), ..req };
    rig.exec.run(granted).unwrap();
    assert_eq!(rig.at("a"), Some(json!(2)));
}

#[test]
fn every_run_needs_read_even_a_dry_run() {
    let rig = rig(json!({"a": 1}));
    let rx = rig.subscribe();
    for mode in [Mode::Apply, Mode::DryRun] {
        for grants in [Grants::default(), Grants::of([Scope::EditCharacter])] {
            let req = Request { mode, grants, ..request("test.set", json!({"path": "a", "value": 2})) };
            let e = rig.exec.run(req).unwrap_err();
            assert_eq!(e.code, ErrorCode::Denied, "{mode:?}");
            assert!(e.hint.as_deref().unwrap().contains("read"), "{e}");
        }
    }
    // Not even an unknown op or bad arguments get an answer without `read`.
    let bare = Request { grants: Grants::default(), ..request("test.nope", json!({})) };
    assert_eq!(rig.exec.run(bare).unwrap_err().code, ErrorCode::Denied);
    assert_eq!(rig.at("a"), Some(json!(1)));
    assert_eq!(rig.log(), vec![]);
    assert_eq!(drain(&rx), vec![]);
}

#[test]
fn a_dry_run_needs_only_read_but_applying_needs_the_ops_scope_too() {
    let rig = rig(json!({"a": 1}));
    let read_only = Grants::of([Scope::Read]);
    let dry = Request {
        mode: Mode::DryRun,
        grants: read_only.clone(),
        ..request("test.set", json!({"path": "a", "value": 2}))
    };
    let out = rig.exec.run(dry).unwrap();
    assert_eq!(out.plan.patch.changes[0].before, Some(json!(1)));

    let apply = Request { grants: read_only, ..request("test.set", json!({"path": "a", "value": 2})) };
    let e = rig.exec.run(apply).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(e.hint.as_deref().unwrap().contains("edit:character"), "{e}");
    assert_eq!(rig.at("a"), Some(json!(1)));
}

#[test]
fn a_dry_run_of_a_query_also_needs_the_queries_own_scope() {
    let rig = rig(json!({"a": 1}));
    let dry = |grants| Request { mode: Mode::DryRun, grants, ..request("test.scoped_query", json!({"path": "a"})) };
    // The plan of a query is its answer, so `read` alone is not enough when the op sits behind another scope.
    let e = rig.exec.run(dry(Grants::of([Scope::Read]))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(e.hint.as_deref().unwrap().contains("render"), "{e}");
    let out = rig.exec.run(dry(Grants::of([Scope::Read, Scope::Render]))).unwrap();
    assert_eq!(out.plan.output, Some(json!(1)));
}

#[test]
fn queries_answer_without_changing_or_logging_anything() {
    let rig = rig(json!({"a": {"b": 7}}));
    let rx = rig.subscribe();
    let out = rig.run("test.get", json!({"path": "a/b"})).unwrap();
    assert_eq!(out.plan.output, Some(json!(7)));
    assert!(out.plan.patch.changes.is_empty() && out.applied.is_none());
    assert_eq!(rig.exec.get(&path(""), &Grants::person()).unwrap().unwrap(), json!({"a": {"b": 7}}));
    assert!(rig.log().is_empty() && drain(&rx).is_empty());

    let no_read = Request { grants: Grants::of([Scope::EditCharacter]), ..request("test.get", json!({"path": "a/b"})) };
    assert_eq!(rig.exec.run(no_read).unwrap_err().code, ErrorCode::Denied);
}

#[test]
fn a_query_that_plans_a_change_is_an_internal_error_and_changes_nothing() {
    let rig = rig(json!({}));
    for mode in [Mode::Apply, Mode::DryRun] {
        let e = rig.exec.run(Request { mode, ..request("test.sneaky_query", json!({"path": "x"})) }).unwrap_err();
        assert_eq!(e.code, ErrorCode::Internal, "{mode:?}");
        assert!(e.message.contains("test.sneaky_query"));
    }
    assert_eq!(rig.at("x"), None);
    assert_eq!(rig.log(), vec![]);
}

#[test]
fn validation_errors_come_from_the_op_untouched_and_leave_no_trace() {
    let rig = rig(json!({}));
    let rx = rig.subscribe();
    let e = rig.run("test.reject", json!({})).unwrap_err();
    assert_eq!((e.code, e.field.as_deref(), e.hint.as_deref()), (ErrorCode::Invalid, Some("blend"), Some("use 0..1")));
    let e = rig.run("test.set", json!({"path": "no/parent", "value": 1})).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
    assert!(rig.log().is_empty() && drain(&rx).is_empty());
    assert_eq!(rig.exec.get(&path(""), &Grants::person()).unwrap(), Some(json!({})));
}

#[test]
fn a_stale_patch_is_a_conflict_that_says_to_replan_and_changes_nothing() {
    let rig = rig(json!({"a": 1}));
    let rx = rig.subscribe();
    let e = rig.run("test.set_if", json!({"path": "a", "expect": 99, "value": 2})).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(e.hint.as_deref().unwrap().contains("re-plan and retry"));
    assert!(e.message.contains("test.set_if") && e.message.contains("99"), "{e}");
    assert_eq!(rig.at("a"), Some(json!(1)));
    assert!(rig.log().is_empty() && drain(&rx).is_empty());
    // The same op succeeds once the expectation is right.
    rig.run("test.set_if", json!({"path": "a", "expect": 1, "value": 2})).unwrap();
}

#[test]
fn an_unappliable_patch_is_an_internal_error_not_a_conflict() {
    let rig = rig(json!({}));
    let e = rig.run("test.broken", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::Internal);
    assert!(e.message.contains("test.broken") && e.message.contains("no/such/parent"), "{e}");
    assert_eq!(rig.log(), vec![]);
}

#[test]
fn a_panicking_op_does_not_brick_the_executor() {
    let rig = rig(json!({"a": 1}));
    let crashed = catch_unwind(AssertUnwindSafe(|| rig.run("test.explode", json!({}))));
    assert!(crashed.is_err());
    // The lock was poisoned by the panic; nothing was half-applied, so work carries on.
    rig.set("a", json!(2));
    assert_eq!(rig.at("a"), Some(json!(2)));
    assert_eq!(rig.log().len(), 1);
}

#[test]
fn get_reads_the_document() {
    let rig = rig(json!({"a": {"b": [1, 2]}}));
    assert_eq!(rig.exec.get(&path("a/b/1"), &Grants::person()).unwrap(), Some(json!(2)));
    assert_eq!(rig.exec.get(&path("a/x"), &Grants::person()).unwrap(), None);
}

#[test]
fn an_executor_can_be_shared_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Executor>();
}

/// Document values, the event stream and the op log all reveal the document, so each needs `read`.
#[test]
fn get_subscribe_and_log_refuse_without_read() {
    let rig = rig(json!({"secret": "hunter2"}));
    rig.set("a", json!(1));
    for grants in [Grants::default(), Grants::of([Scope::EditCharacter])] {
        let denied = [
            rig.exec.get(&path("secret"), &grants).map(drop),
            rig.exec.subscribe(&grants).map(drop),
            rig.exec.log(0, 10, &grants).map(drop),
        ];
        for result in denied {
            let e = result.unwrap_err();
            assert_eq!(e.code, ErrorCode::Denied);
            assert!(e.hint.as_deref().is_some_and(|h| h.contains("read")), "the hint names the scope: {e:?}");
            assert!(!format!("{e:?}").contains("hunter2"), "LEAK: {e:?}");
        }
    }
}

#[test]
fn log_pages_by_sequence_number() {
    let rig = rig(json!({}));
    for i in 0..5 {
        rig.set(&format!("k{i}"), json!(i));
    }
    let person = Grants::person();
    let seqs =
        |after, limit| -> Vec<u64> { rig.exec.log(after, limit, &person).unwrap().iter().map(|r| r.seq).collect() };
    assert_eq!(seqs(0, 2), [1, 2]);
    assert_eq!(seqs(2, 2), [3, 4]);
    assert_eq!(seqs(4, 10), [5]);
    assert_eq!(seqs(5, 10), Vec::<u64>::new());
    assert_eq!(seqs(0, 0), Vec::<u64>::new());
}
