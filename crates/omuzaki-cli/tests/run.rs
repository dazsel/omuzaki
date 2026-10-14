//! `omuzaki run`, `undo`, `redo` and the error/exit-code path, against a fake runner that returns fixed outcomes.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::io::{self, Cursor, Write};
use std::sync::Arc;

use common::{FakeRunner, snapshot};
use omuzaki_cli::cli::try_parse_from;
use omuzaki_cli::commands::execute;
use omuzaki_cli::output::finish;
use omuzaki_cli::session::Session;
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocVersion, Patch};
use omuzaki_ops_api::{
    Actor, Cause, CauseKind, CostHint, ErrorCode, Grants, Mode, OpError, OpRecord, Outcome, Plan, TxId,
};
use serde_json::json;

fn path(p: &str) -> DocPath {
    DocPath::parse(p).unwrap()
}

/// A patch with one of each kind of change, plus a no-op that must not be shown.
fn mixed_patch() -> Patch {
    Patch::new()
        .with(path("characters/mira/face/blend"), Some(json!(0.2)), Some(json!(0.6)))
        .with(path("characters/mira/face/profile"), None, Some(json!({"weights": [1, 0], "name": "close-up"})))
        .with(path("characters/mira/hair/color"), Some(json!("#fff")), None)
        .with(path("characters/mira/face/mood"), Some(json!("calm")), Some(json!("calm")))
}

fn edit_outcome(applied: bool) -> Outcome {
    let patch = mixed_patch();
    let applied = applied.then(|| OpRecord {
        seq: 3,
        op: "face.set_profile".to_owned(),
        args: json!({"character": "mira", "blend": 0.6}),
        actor: Actor::Person { name: "tester".to_owned() },
        cause: None,
        tx: TxId(3),
        at: Seconds(1.5),
        patch: patch.clone(),
        version: DocVersion(7),
        on_behalf_of: None,
        approval: None,
    });
    let cost = Some(CostHint { seconds: Some(2.5), gpu_mib: Some(4096), usd: Some(0.1) });
    Outcome { plan: Plan { patch, output: None, cost }, applied }
}

fn omuzaki(runner: &Arc<FakeRunner>, args: &[&str]) -> Result<String, OpError> {
    omuzaki_with_stdin(runner, args, b"")
}

fn omuzaki_with_stdin(runner: &Arc<FakeRunner>, args: &[&str], stdin: &[u8]) -> Result<String, OpError> {
    let cli = try_parse_from(std::iter::once("omuzaki").chain(args.iter().copied())).expect("valid command line");
    execute(&cli, runner.clone(), &Session::person("tester"), &mut Cursor::new(stdin.to_vec()))
}

#[test]
fn dry_run_prints_a_readable_diff_and_sends_a_dry_run_request() {
    let runner = FakeRunner::answering(edit_outcome(false));
    let text =
        omuzaki(&runner, &["run", "face.set_profile", "--args", r#"{"character":"mira"}"#, "--dry-run"]).unwrap();
    snapshot("run_dry.txt", &text);

    let req = runner.only_request();
    assert_eq!(req.op, "face.set_profile");
    assert_eq!(req.args, json!({"character": "mira"}));
    assert_eq!(req.mode, Mode::DryRun);
    assert_eq!(req.actor, Actor::Person { name: "tester".to_owned() });
    assert_eq!(req.grants, Grants::person());
    assert_eq!(req.cause, Some(Cause { kind: CauseKind::Direct, text: None }));
    assert_eq!(req.tx, None);
}

#[test]
fn apply_reports_the_log_record_and_sends_an_apply_request() {
    let runner = FakeRunner::answering(edit_outcome(true));
    let text = omuzaki(&runner, &["run", "face.set_profile", "--args", "{}"]).unwrap();
    snapshot("run_applied.txt", &text);
    assert_eq!(runner.only_request().mode, Mode::Apply);
}

#[test]
fn a_query_prints_its_output_and_no_diff() {
    let plan = Plan::answer(json!({"name": "my-show", "characters": 2}));
    let runner = FakeRunner::answering(Outcome { plan, applied: None });
    snapshot("run_query.txt", &omuzaki(&runner, &["run", "project.describe"]).unwrap());
}

#[test]
fn a_plan_that_changes_nothing_says_so() {
    let noop = Patch::new().with(path("a"), Some(json!(1)), Some(json!(1)));
    let runner = FakeRunner::answering(Outcome { plan: Plan::edit(noop), applied: None });
    let text = omuzaki(&runner, &["run", "x.y", "--dry-run"]).unwrap();
    assert_eq!(text, "dry run of x.y: nothing was changed\nno changes\n");
}

#[test]
fn json_output_is_the_outcome_with_keys_sorted_at_every_depth() {
    // The fixtures list their keys out of alphabetical order (`weights` before `name`, `plan`'s siblings, ...), so
    // this catches unsorted output whenever serde_json keeps insertion order (`preserve_order`).
    let runner = FakeRunner::answering(edit_outcome(true));
    let text = omuzaki(&runner, &["--json", "run", "face.set_profile"]).unwrap();
    snapshot("run_applied.json", &text);
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["applied"]["version"], 7);
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle} missing in {text}"));
    assert!(at("\"applied\"") < at("\"plan\""), "top-level keys sorted");
    assert!(at("\"name\": \"close-up\"") < at("\"weights\""), "keys inside patch values sorted");
    let plan = &text[at("\"plan\"")..];
    let in_plan = |needle: &str| plan.find(needle).unwrap();
    assert!(in_plan("\"cost\"") < in_plan("\"output\"") && in_plan("\"output\"") < in_plan("\"patch\""));
}

#[test]
fn text_output_shows_values_with_sorted_keys_too() {
    let runner = FakeRunner::answering(edit_outcome(false));
    let text = omuzaki(&runner, &["run", "face.set_profile", "--dry-run"]).unwrap();
    assert!(text.find("\"name\": \"close-up\"") < text.find("\"weights\""), "{text}");
    let answer =
        FakeRunner::answering(Outcome { plan: Plan::answer(json!({"name": "x", "characters": 2})), applied: None });
    let text = omuzaki(&answer, &["run", "project.describe"]).unwrap();
    assert!(text.find("\"characters\"") < text.find("\"name\""), "{text}");
}

#[test]
fn omitted_args_default_to_an_empty_object() {
    let runner = FakeRunner::answering(Outcome { plan: Plan::default(), applied: None });
    omuzaki(&runner, &["run", "project.describe"]).unwrap();
    assert_eq!(runner.only_request().args, json!({}));
}

#[test]
fn args_can_come_from_standard_input() {
    let runner = FakeRunner::answering(Outcome { plan: Plan::default(), applied: None });
    omuzaki_with_stdin(&runner, &["run", "face.set_profile", "--args", "@-"], br#"{"blend": 0.5}"#).unwrap();
    assert_eq!(runner.only_request().args, json!({"blend": 0.5}));
}

#[test]
fn bad_args_never_reach_the_runner() {
    let runner = FakeRunner::answering(Outcome { plan: Plan::default(), applied: None });
    for (bad, code) in [("{not json", ErrorCode::Invalid), ("@/definitely/not/here.json", ErrorCode::NotFound)] {
        let e = omuzaki(&runner, &["run", "face.set_profile", "--args", bad]).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (code, Some("args")), "{bad}: {e:?}");
    }
    assert!(runner.requests.lock().unwrap().is_empty());
}

#[test]
fn undo_and_redo_name_the_step_or_say_there_is_nothing() {
    let runner = FakeRunner::answering(Outcome { plan: Plan::default(), applied: None });
    runner.set_history(Ok(Some("make the face more oval".to_owned())));
    assert_eq!(omuzaki(&runner, &["undo"]).unwrap(), "undid \"make the face more oval\"\n");
    assert_eq!(omuzaki(&runner, &["redo"]).unwrap(), "redid \"make the face more oval\"\n");
    assert_eq!(omuzaki(&runner, &["undo", "--json"]).unwrap(), "{\n  \"undone\": \"make the face more oval\"\n}\n");
    assert_eq!(omuzaki(&runner, &["redo", "--json"]).unwrap(), "{\n  \"redone\": \"make the face more oval\"\n}\n");

    runner.set_history(Ok(None));
    assert_eq!(omuzaki(&runner, &["undo"]).unwrap(), "nothing to undo\n");
    assert_eq!(omuzaki(&runner, &["redo"]).unwrap(), "nothing to redo\n");
    assert_eq!(omuzaki(&runner, &["undo", "--json"]).unwrap(), "{\n  \"undone\": null\n}\n");
}

/// Runs `finish` and returns (exit code, stdout, stderr).
fn finished(result: Result<String, OpError>, json: bool) -> (u8, String, String) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = finish(result, json, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

#[test]
fn success_prints_to_stdout_and_exits_zero() {
    assert_eq!(finished(Ok("hello\n".to_owned()), false), (0, "hello\n".to_owned(), String::new()));
}

#[test]
fn errors_print_to_stderr_with_field_and_hint() {
    let e = OpError::invalid("blend", "2 is out of range").hint("use 0..1");
    let (code, out, err) = finished(Err(e), false);
    assert_eq!((code, out.as_str()), (3, ""));
    assert_eq!(err, "error: invalid: 2 is out of range\n  field: blend\n  hint: use 0..1\n");
}

#[test]
fn json_errors_are_one_object_with_stable_fields() {
    let e = OpError::denied("publish isn't granted").hint("grant the publish scope");
    let (code, out, err) = finished(Err(e), true);
    assert_eq!((code, out.as_str()), (5, ""));
    snapshot("error.json", &err);
    let parsed: serde_json::Value = serde_json::from_str(&err).unwrap();
    assert_eq!(parsed["error"]["code"], "denied");
}

#[test]
fn each_error_code_has_its_own_exit_code() {
    let errors = [
        (OpError::internal("bug"), 1),
        (OpError::invalid("f", "bad"), 3),
        (OpError::not_found("gone"), 4),
        (OpError::denied("no"), 5),
        (OpError::conflict("changed"), 6),
        (OpError::unsupported("not here"), 7),
        (OpError::unavailable("dyn omuzaki_render_api::RenderService"), 8),
    ];
    for (error, expected) in errors {
        let code = error.code;
        assert_eq!(finished(Err(error), false).0, expected, "{code}");
    }
}

/// A writer that fails the way a closed pipe or a full disk does.
struct Failing(io::ErrorKind);

impl Write for Failing {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(self.0.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_closed_pipe_is_not_an_error_but_other_write_failures_are() {
    let mut err = Vec::new();
    assert_eq!(finish(Ok("x".to_owned()), false, &mut Failing(io::ErrorKind::BrokenPipe), &mut err), 0);
    assert_eq!(err, Vec::<u8>::new());

    assert_eq!(finish(Ok("x".to_owned()), false, &mut Failing(io::ErrorKind::StorageFull), &mut err), 1);
    assert!(String::from_utf8(err).unwrap().starts_with("error: internal: writing the output failed"));
}

#[test]
fn an_unwritable_stderr_keeps_the_ops_own_exit_code() {
    let refusal = OpError::denied("no");
    assert_eq!(finish(Err(refusal), false, &mut Vec::new(), &mut Failing(io::ErrorKind::StorageFull)), 5);
}

#[test]
fn runner_errors_pass_through_unchanged() {
    let refusal = OpError::denied("publish isn't granted").hint("grant the publish scope");
    let runner = FakeRunner::refusing(refusal.clone());
    assert_eq!(omuzaki(&runner, &["run", "export.upload"]).unwrap_err(), refusal);
    assert_eq!(omuzaki(&runner, &["undo"]).unwrap_err(), refusal);
}
