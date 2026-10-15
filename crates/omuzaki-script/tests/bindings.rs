//! The generated bindings: `try_op`, `get`, `params`, the error map, the dry run and the JSON round trip.

mod common;

use common::{FakeRunner, Flag, request, run, run_with};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::ErrorCode;
use omuzaki_script::{Limits, ScriptErrorKind};
use proptest::prelude::*;
use serde_json::json;

fn value_of(source: &str) -> Value {
    let (_, result) = run(source);
    result.unwrap_or_else(|e| panic!("{source}: {e}")).value
}

#[test]
fn try_op_returns_ok_value_and_error_for_a_call_that_works() {
    let value = value_of("try_op(|| ops::face::set_profile(#{ character: \"a\", blend: 0.5 }))");
    assert_eq!(value, json!({ "ok": true, "value": { "character": "a", "blend": 0.5 }, "error": null }));
}

#[test]
fn try_op_returns_the_three_keys_for_a_failed_op_and_the_error_carries_its_fields() {
    let value = value_of("try_op(|| ops::face::fail(#{}))");
    assert_eq!(value["ok"], json!(false));
    assert_eq!(value["value"], Value::Null);
    let error = &value["error"];
    assert_eq!(error["code"], "invalid");
    assert_eq!(error["message"], "blend must be between 0 and 1");
    assert_eq!(error["field"], "blend");
    assert_eq!(error["hint"], "use 0.5");
    assert_eq!(error["op"], "face.fail");
    assert_eq!((error["line"].as_i64(), error["column"].as_i64()), (Some(1), Some(22)));
}

#[test]
fn an_unknown_op_is_a_script_error_that_try_op_does_not_catch() {
    let (_, result) = run("try_op(|| ops::nothing::here(#{}))");
    assert_eq!(result.expect_err("not catchable").kind, ScriptErrorKind::Runtime);
}

#[test]
fn a_loop_can_skip_what_fails() {
    let source = "
        let done = [];
        for c in [\"a\", \"b\"] {
            let r = try_op(|| ops::face::fail(#{ character: c }));
            if r.ok { done.push(c); } else { log(`skipped ${c}: ${r.error.hint}`); }
        }
        done.len()
    ";
    let (runner, result) = run(source);
    let outcome = result.expect("the run succeeds");
    assert_eq!(outcome.value, json!(0));
    assert_eq!(
        outcome.output.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
        ["skipped a: use 0.5", "skipped b: use 0.5"]
    );
    assert_eq!(runner.rollbacks(), 0, "a failure the script handled rolls nothing back");
}

#[test]
fn rhai_try_catch_sees_the_same_error_map() {
    let source = "let r = (); try { ops::face::fail(#{}); } catch (e) { r = [e.code, e.op, e.field, e.hint]; } r";
    assert_eq!(value_of(source), json!(["invalid", "face.fail", "blend", "use 0.5"]));
}

#[test]
fn an_uncaught_op_error_ends_the_run_with_the_op_error() {
    let (runner, result) = run("ops::face::set_profile(#{ character: \"a\" });\nops::face::fail(#{});");
    let error = result.expect_err("the run fails");
    assert_eq!(error.kind, ScriptErrorKind::Op);
    assert_eq!((error.line, error.column), (2, 12));
    let op_error = error.op_error.expect("the OpError");
    assert_eq!(op_error.code, ErrorCode::Invalid);
    assert_eq!(op_error.field.as_deref(), Some("blend"));
    runner.assert_rolled_back();
}

#[test]
fn a_limit_or_a_cancel_inside_try_op_is_not_caught() {
    let flag = Flag::default();
    flag.set();
    let (_, result) = run_with(request("try_op(|| 1)"), &flag, Limits::console(), FakeRunner::default());
    assert_eq!(result.expect_err("cancelled").kind, ScriptErrorKind::Cancelled);
    let mut limits = Limits::console();
    limits.calls = 1;
    let source =
        "ops::face::set_profile(#{ character: \"a\" }); try_op(|| ops::face::set_profile(#{ character: \"b\" }))";
    let (_, result) = run_with(request(source), &Flag::default(), limits, FakeRunner::default());
    assert!(matches!(result.expect_err("the call limit").kind, ScriptErrorKind::Limit(_)));
}

#[test]
fn a_script_error_inside_try_op_is_not_an_op_failure() {
    let (_, result) = run("try_op(|| throw \"boom\")");
    assert_eq!(result.expect_err("thrown").kind, ScriptErrorKind::Runtime);
}

#[test]
fn get_reads_the_document_through_the_runner() {
    let runner = FakeRunner::default();
    *runner.document.lock().unwrap() =
        json!({ "characters": { "aki": { "face": { "blend": 0.25, "marks": [1, 2] } } } });
    let (_, result) = run_with(
        request("let f = get(\"characters/aki/face\"); [f.blend, f.marks.len(), get(\"characters/nobody\")]"),
        &Flag::default(),
        Limits::console(),
        runner,
    );
    assert_eq!(result.expect("runs").value, json!([0.25, 2, null]));
}

#[test]
fn get_with_a_bad_path_is_an_op_error_naming_get() {
    let value = value_of("try_op(|| get(\"a//b\"))");
    assert_eq!(value["ok"], json!(false));
    assert_eq!(value["error"]["op"], "get");
    assert_eq!(value["error"]["field"], "path");
}

#[test]
fn params_is_the_requests_map() {
    let mut request = request("params.count + params.names.len()");
    request.params = json!({ "count": 2, "names": ["aki", "ren"] }).as_object().unwrap().clone();
    let (_, result) = run_with(request, &Flag::default(), Limits::console(), FakeRunner::default());
    assert_eq!(result.expect("runs").value, json!(4));
}

#[test]
fn a_dry_run_sends_every_call_as_a_dry_run_and_lists_the_plans() {
    let mut request =
        request("ops::face::set_profile(#{ character: \"a\" }); ops::measure::likeness(#{ character: \"a\" })");
    request.dry_run = true;
    let (runner, result) = run_with(request, &Flag::default(), Limits::console(), FakeRunner::default());
    let outcome = result.expect("runs");
    assert!(runner.requests().iter().all(|r| r.mode == omuzaki_ops_api::Mode::DryRun));
    assert_eq!(
        outcome.plans.iter().map(|p| p.op.as_str()).collect::<Vec<_>>(),
        ["face.set_profile", "measure.likeness"]
    );
    assert_eq!(outcome.plans[0].args, json!({ "character": "a" }));
    assert_eq!(runner.begins(), 0, "a dry run opens no transaction");
}

#[test]
fn a_keyword_area_is_written_with_a_trailing_underscore() {
    // The word Rhai uses to publish a module's items is reserved, so `ops::<it>::publish_clip` can't be written.
    let (runner, result) = run("ops::export_::publish_clip(#{ clip: \"c\" })");
    result.expect("runs");
    assert_eq!(runner.requests()[0].op, "export.publish_clip");
}

#[test]
fn the_same_source_sends_the_same_requests_twice() {
    let source = "for c in [\"a\", \"b\", \"c\"] { try_op(|| ops::face::set_profile(#{ character: c })); }";
    let (first, _) = run(source);
    let (second, _) = run(source);
    assert_eq!(first.requests(), second.requests());
}

// -- JSON <-> script values ---------------------------------------------------------------------------------------

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        (-1.0e9f64..1.0e9).prop_map(|f| json!(f)),
        "[a-zA-Z0-9 _-]{0,12}".prop_map(Value::from),
    ];
    leaf.prop_recursive(4, 48, 5, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(Value::Array),
            prop::collection::btree_map("[a-z]{1,6}", inner, 0..5).prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// A value goes into a script as `params` and comes back out as the result unchanged: maps keep their keys,
    /// integers stay integers and floats stay floats.
    #[test]
    fn json_round_trips_through_the_script_values(value in json_value()) {
        let mut request = request("params.v");
        request.params = json!({ "v": value.clone() }).as_object().unwrap().clone();
        let (_, result) = run_with(request, &Flag::default(), Limits::console(), FakeRunner::default());
        let back = result.expect("runs").value;
        prop_assert_eq!(&back, &value);
        // `i64` stays `i64`: a float 1.0 and an integer 1 are different JSON numbers.
        prop_assert_eq!(back.to_string(), value.to_string());
    }
}

#[test]
fn map_keys_come_back_sorted() {
    let value = value_of("#{ b: 1, a: 2, c: #{ z: 1, y: 2 } }");
    assert_eq!(value.to_string(), r#"{"a":2,"b":1,"c":{"y":2,"z":1}}"#);
}
