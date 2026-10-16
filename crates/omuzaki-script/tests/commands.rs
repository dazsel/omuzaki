//! The functions behind `omuzaki script run` and `omuzaki script check`: text in, text out, and the error that picks the
//! exit code.

#![allow(clippy::unwrap_used)] // a test file: a failed unwrap is the failure

mod common;

use std::sync::Arc;

use common::{Entry, FakeRunner};
use omuzaki_ops_api::{Actor, ErrorCode, ErrorDetail, Grants, Mode, Runner};
use omuzaki_script::{RunOptions, check_script, run_script};
use serde_json::json;

fn person() -> Actor {
    Actor::Person { name: "alice".into() }
}

fn run(source: &str, options: &RunOptions) -> (Arc<FakeRunner>, Result<String, omuzaki_ops_api::OpError>) {
    let runner = Arc::new(FakeRunner::default());
    let result = run_script(runner.clone(), "hello.rhai", source, &person(), &Grants::person(), options);
    (runner, result)
}

#[test]
fn run_prints_the_output_then_the_result() {
    let source = "print(\"hello\"); log(\"a note\"); 6 * 7";
    let (_, text) = run(source, &RunOptions::default());
    assert_eq!(text.unwrap(), "hello\nnote: a note\n42\n");
}

#[test]
fn a_script_with_no_result_prints_only_its_output() {
    let (_, text) = run("print(\"only this\");", &RunOptions::default());
    assert_eq!(text.unwrap(), "only this\n");
}

#[test]
fn run_is_one_transaction_with_the_script_cause_named_after_the_file() {
    let source = "ops::face::set_profile(#{ character: \"a\" }); ops::face::set_profile(#{ character: \"b\" });";
    let (runner, text) = run(source, &RunOptions::default());
    text.unwrap();
    assert_eq!((runner.begins(), runner.commits()), (1, 1));
    for request in runner.requests() {
        let cause = request.cause.unwrap();
        assert_eq!(cause.text.as_deref(), Some("hello.rhai"));
        assert_eq!(request.actor, person());
    }
}

#[test]
fn params_reach_the_script() {
    let options = RunOptions { params: json!({ "n": 4 }).as_object().unwrap().clone(), ..RunOptions::default() };
    let (_, text) = run("params.n * 2", &options);
    assert_eq!(text.unwrap(), "8\n");
}

#[test]
fn a_dry_run_lists_what_would_run_and_changes_nothing() {
    let source = "ops::face::set_profile(#{ character: \"a\" })";
    let (runner, text) = run(source, &RunOptions { dry_run: true, ..RunOptions::default() });
    let text = text.unwrap();
    assert!(text.contains("would run face.set_profile with {\"character\":\"a\"}: 0 change(s)"), "{text}");
    assert!(text.contains("dry run: 1 call(s) planned, nothing changed"), "{text}");
    assert!(runner.requests().iter().all(|r| r.mode == Mode::DryRun));
    assert_eq!(runner.begins(), 0);
}

#[test]
fn json_prints_the_outcome() {
    let (_, text) = run("print(\"x\"); 1 + 1", &RunOptions { json: true, ..RunOptions::default() });
    let outcome: serde_json::Value = serde_json::from_str(&text.unwrap()).unwrap();
    assert_eq!(outcome["value"], 2);
    assert_eq!(outcome["output"][0]["text"], "x");
    assert_eq!(outcome["calls"], 0);
}

#[test]
fn a_failed_op_exits_with_the_ops_own_error() {
    let (runner, text) = run("ops::face::fail(#{})", &RunOptions::default());
    let error = text.unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some("blend"));
    assert_eq!(error.hint.as_deref(), Some("use 0.5"));
    runner.assert_rolled_back();
}

#[test]
fn a_parse_error_is_invalid_with_the_position_in_the_message_and_the_detail() {
    let (runner, text) = run("let a = 1;\nlet b = ;", &RunOptions::default());
    let error = text.unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid, "exit code 3");
    assert!(error.message.ends_with("(line 2, column 9)"), "{}", error.message);
    assert_eq!(error.detail.as_deref(), Some(&ErrorDetail::Position { line: 2, column: 9 }));
    assert_eq!(runner.entries(), Vec::<Entry>::new());
}

#[test]
fn a_limit_is_invalid_and_keeps_what_was_printed_as_the_hint() {
    let (_, text) = run("print(\"before\"); loop { }", &RunOptions::default());
    let error = text.unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.hint.as_deref(), Some("it printed before failing: before"));
}

#[test]
fn check_prints_the_ops_the_problems_and_the_scopes() {
    let runner = FakeRunner::default();
    let output =
        check_script(&runner, "ops::face::set_profile(#{ character: \"a\" });\nops::measure::likeness(#{});", false);
    assert_eq!(output.failure, None);
    assert_eq!(
        output.text,
        "1:12: calls face.set_profile\n2:15: calls measure.likeness\nneeds: read, edit:character\n"
    );
}

#[test]
fn check_prints_the_report_as_json() {
    let runner = FakeRunner::default();
    let output = check_script(&runner, "ops::face::set_profile(#{ character: \"a\" });", true);
    assert_eq!(output.failure, None);
    let report: serde_json::Value = serde_json::from_str(&output.text).unwrap();
    assert_eq!(report["ops"][0]["name"], "face.set_profile");
    assert_eq!(report["scopes"], json!(["edit:character"]));
}

#[test]
fn check_fails_with_the_first_problem_when_the_script_has_one() {
    let runner = FakeRunner::default();
    let source = "let a = 1;\nops::face::set_profil(#{});\nops::face::set_profile(#{ nope: 1 });";
    let output = check_script(&runner, source, false);
    let error = output.failure.expect("a problem fails the check");
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(error.detail.as_deref(), Some(&ErrorDetail::Position { line: 2, column: 12 }));
    assert!(error.message.contains("face.set_profil"), "{}", error.message);
    assert!(error.hint.as_deref().unwrap().starts_with("3 problem(s)"), "{:?}", error.hint);
    // The report is still there to print: every problem, not only the first.
    assert!(output.text.contains("2:12: unknown op `face.set_profil`"), "{}", output.text);
    assert!(output.text.contains("3:27: `face.set_profile` has no field `nope`"), "{}", output.text);
}

#[test]
fn a_failing_check_still_returns_its_report_as_json() {
    let runner = FakeRunner::default();
    let output = check_script(&runner, "let a = 1;\nops::face::set_profil(#{});", true);
    let error = output.failure.expect("fails");
    assert_eq!(error.code, ErrorCode::Invalid);
    let report: serde_json::Value = serde_json::from_str(&output.text).expect("the text is the JSON report");
    assert_eq!(report["unknown_ops"][0]["name"], "face.set_profil");
}

#[test]
fn check_shows_the_ceiling() {
    let runner = FakeRunner::default();
    let text = check_script(&runner, "// omuzaki: grants = [\"edit:character\"]\n1", false).text;
    assert!(text.ends_with("needs: read\nceiling: edit:character\n"), "{text}");
}

#[test]
fn nothing_here_touches_the_runner_but_through_its_trait() {
    // The commands take the runner as a trait object, as the CLI holds it.
    let runner: Arc<dyn Runner> = Arc::new(FakeRunner::default());
    assert!(run_script(runner, "x", "1", &person(), &Grants::person(), &RunOptions::default()).is_ok());
}
