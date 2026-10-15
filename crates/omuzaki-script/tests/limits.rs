//! The sandbox limits, cancel, output, errors and the transaction a run owns. No test sleeps: the wall clock is a fake.

#![allow(clippy::expect_used)] // the helper `fails` is outside a `#[test]` fn

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::{Entry, FakeRunner, Flag, Ticking, calls, host, request, run, run_with};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_ops_api::{ErrorCode, Mode, TxId};
use omuzaki_script::{Calls, LimitKind, Limits, OutputKind, ScriptError, ScriptErrorKind};

const EDIT: &str = "ops::face::set_profile(#{ character: \"a\" });";

/// Runs `source`, expects a failure, and checks it rolled back.
fn fails(source: &str) -> (Arc<FakeRunner>, ScriptError) {
    let (runner, result) = run(source);
    runner.assert_rolled_back();
    (runner, result.expect_err("the run should fail"))
}

fn assert_at(error: &ScriptError, kind: ScriptErrorKind, line: u32, column: u32) {
    assert_eq!((error.kind, error.line, error.column), (kind, line, column), "{error}");
}

fn limit(kind: LimitKind) -> ScriptErrorKind {
    ScriptErrorKind::Limit(kind)
}

#[test]
fn source_over_64_kib_is_refused_before_parsing() {
    let (runner, error) = fails(&" ".repeat(64 * 1024 + 1));
    assert_at(&error, limit(LimitKind::Source), 1, 1);
    assert_eq!(runner.entries(), []);
}

#[test]
fn endless_loop_stops_at_the_operation_limit() {
    let (_, error) = fails(&format!("{EDIT}\nloop {{ }}"));
    assert_at(&error, limit(LimitKind::Operations), 2, 6);
}

#[test]
fn the_10_001st_inner_call_stops_the_run() {
    let (runner, error) = fails(&format!("for i in 0..10001 {{\n  {EDIT}\n}}"));
    assert_at(&error, limit(LimitKind::Calls), 2, 14);
    assert_eq!(runner.requests().len(), 10_000);
    assert_eq!(runner.begins(), 1);
}

#[test]
fn call_depth_65_and_expression_depth_are_limits() {
    let (_, error) = fails("fn f(n) { f(n + 1) }\nf(0)");
    assert_at(&error, limit(LimitKind::Depth), 2, 1);
    let (_, error) = fails(&format!("{}1{}", "(".repeat(70), ")".repeat(70)));
    assert_eq!(error.kind, limit(LimitKind::Depth));
    assert_eq!(error.line, 1);
}

#[test]
fn string_array_and_map_sizes_are_limits() {
    let (_, error) = fails("let s = \"\".pad(1048577, 'x');");
    assert_at(&error, limit(LimitKind::StringSize), 1, 12);
    let (_, error) = fails("let a = [];\n a.pad(100001, 0);");
    assert_at(&error, limit(LimitKind::ArraySize), 2, 4);
    let (_, error) = fails("let m = #{};\nfor i in 0..10001 { m[`k${i}`] = i; }\nlet n = m;\nn.len()");
    assert_at(&error, limit(LimitKind::MapSize), 4, 3);
    // One short of each limit is fine.
    let (runner, result) = run("let a = []; a.pad(100000, 0); a.len()");
    assert_eq!(result.expect("within the limit").value, 100_000);
    assert_eq!(runner.commits(), 0, "a run that applied no edit opens no transaction");
}

#[test]
fn output_is_capped_at_1000_lines_then_one_marker() {
    let (_, result) = run("for i in 0..1001 { print(i); }");
    let output = result.expect("truncation is not an error").output;
    assert_eq!(output.len(), 1001);
    assert_eq!(output[999].text, "999");
    assert_eq!((output[1000].kind, output[1000].text.as_str()), (OutputKind::Note, "output truncated"));
}

#[test]
fn output_is_capped_at_64_kib_then_one_marker() {
    let (_, result) = run("let line = \"\"; line.pad(1000, 'x');\nfor i in 0..100 { print(line); }");
    let output = result.expect("truncation is not an error").output;
    assert_eq!(output.len(), 65 + 1);
    assert_eq!(output.iter().filter(|l| l.text == "output truncated").count(), 1);
}

#[test]
fn wall_clock_stops_at_ten_seconds_of_fake_time() {
    let runner = Arc::new(FakeRunner::default());
    let clock = Arc::new(FakeClock::new(Seconds(100.0)));
    let host = host(&runner, Limits::console(), &clock);
    let error =
        host.run(request(&format!("{EDIT}\nloop {{ }}")), &calls(&runner), &Ticking(clock)).expect_err("times out");
    assert_at(&error, limit(LimitKind::Time), 2, 6);
    runner.assert_rolled_back();
}

#[test]
fn a_result_over_64_kib_is_a_limit() {
    let (_, error) = fails("let s = \"\"; s.pad(70000, 'x'); s");
    assert_at(&error, limit(LimitKind::Result), 1, 1);
}

#[test]
fn eval_fn_call_curry_and_import_are_refused_at_parse() {
    for (source, line, column) in [
        ("let x = 1;\n  eval(\"1\")", 2, 3),
        ("Fn(\"f\")", 1, 1),
        ("let f = |x| x;\ncall(f, 1)", 2, 1),
        ("let f = |x| x;\n f.curry(1)", 2, 4),
        ("import \"x\" as y;", 1, 1),
    ] {
        let (runner, error) = fails(source);
        assert_at(&error, ScriptErrorKind::Parse, line, column);
        assert_eq!(runner.entries(), []);
    }
}

#[test]
fn timestamp_is_an_unknown_function() {
    let (_, error) = fails("timestamp()");
    assert_eq!(error.kind, ScriptErrorKind::Runtime);
    assert!(error.message.contains("timestamp"), "{error}");
}

#[test]
fn cancel_before_the_run_ends_it_at_once() {
    let flag = Flag::default();
    flag.set();
    let (runner, result) = run_with(request("loop { }"), &flag, Limits::console(), FakeRunner::default());
    assert_eq!(result.expect_err("cancelled").kind, ScriptErrorKind::Cancelled);
    assert_eq!(runner.entries(), []);
}

#[test]
fn cancel_during_a_loop_stops_within_one_operation() {
    let runner = FakeRunner::default();
    let flag = Arc::new(Flag::default());
    let setter = flag.clone();
    *runner.on_request.lock().expect("lock") = Some(Box::new(move |_| setter.set()));
    let source = format!("{EDIT}\nloop {{ }}");
    let (runner, result) = run_with(request(&source), flag.as_ref(), Limits::console(), runner);
    let error = result.expect_err("cancelled");
    assert_eq!(error.kind, ScriptErrorKind::Cancelled);
    runner.assert_rolled_back();
}

#[test]
fn cancel_between_two_inner_calls_does_not_send_the_next() {
    let runner = FakeRunner::default();
    let flag = Arc::new(Flag::default());
    let setter = flag.clone();
    *runner.on_request.lock().expect("lock") = Some(Box::new(move |_| setter.set()));
    // `let x = 1` gives the loop no operation between the calls to stop on; the call itself must check.
    let (runner, result) = run_with(request(&format!("{EDIT} {EDIT}")), flag.as_ref(), Limits::console(), runner);
    let error = result.expect_err("cancelled");
    assert_eq!(error.kind, ScriptErrorKind::Cancelled);
    assert_eq!(runner.requests().len(), 1);
    runner.assert_rolled_back();
}

#[test]
fn success_commits_one_transaction_that_every_request_carries() {
    let (runner, result) = run(&format!("{EDIT} {EDIT} ops::measure::likeness(#{{}})"));
    let outcome = result.expect("runs");
    assert_eq!((outcome.calls, outcome.value.as_f64()), (3, Some(0.5)));
    let tx = TxId(1);
    assert_eq!(outcome.tx, Some(tx));
    let log = runner.entries();
    assert!(matches!(log.first(), Some(Entry::Begin(t)) if *t == tx));
    assert!(matches!(log.last(), Some(Entry::Commit(t)) if *t == tx));
    assert!(runner.requests().iter().all(|r| r.tx == Some(tx) && r.base.is_none() && r.mode == Mode::Apply));
    let cause = runner.requests()[0].cause.clone().expect("cause");
    assert_eq!((cause.kind, cause.text.as_deref()), (omuzaki_ops_api::CauseKind::Script, Some("test script")));
}

#[test]
fn a_run_with_no_edit_opens_no_transaction() {
    let (runner, result) = run("ops::measure::likeness(#{})");
    assert_eq!(result.expect("runs").tx, None);
    assert_eq!((runner.begins(), runner.commits()), (0, 0));
    assert_eq!(runner.requests()[0].tx, None);
}

#[test]
fn a_failed_op_rolls_back_with_its_error_position_and_output() {
    let (runner, error) = fails(&format!("print(1);\n{EDIT}\nlog(\"two\");\nops::face::fail(#{{}});"));
    assert_at(&error, ScriptErrorKind::Op, 4, 12);
    assert_eq!(error.op_error.as_ref().map(|e| e.code), Some(ErrorCode::Invalid));
    let kept: Vec<_> = error.output.iter().map(|l| (l.kind, l.text.as_str())).collect();
    assert_eq!(kept, [(OutputKind::Print, "1"), (OutputKind::Note, "two")]);
    assert_eq!(runner.begins(), 1);
}

#[test]
fn print_and_log_keep_order_and_kind() {
    let (_, result) = run("log(1.5); print(2); log(\"x\"); 3");
    let output = result.expect("runs").output;
    let kept: Vec<_> = output.iter().map(|l| (l.kind, l.text.as_str())).collect();
    assert_eq!(kept, [(OutputKind::Note, "1.5"), (OutputKind::Print, "2"), (OutputKind::Note, "x")]);
}

#[test]
fn a_request_with_a_transaction_is_invalid() {
    let mut joined = request(EDIT);
    joined.tx = Some(TxId(9));
    let (runner, result) = run_with(joined, &Flag::default(), Limits::console(), FakeRunner::default());
    let error = result.expect_err("refused");
    assert_eq!(error.op_error.map(|e| e.code), Some(ErrorCode::Invalid));
    assert!(runner.requests().is_empty() && runner.begins() == 0);
}

#[test]
fn the_same_source_sends_the_same_requests_in_the_same_order() {
    let source = "for c in [\"b\", \"a\"] { ops::face::set_profile(#{ z: 1, a: c, m: params.n }); }";
    let sent = || {
        let mut req = request(source);
        req.params.insert("n".into(), 7.into());
        let (runner, result) = run_with(req, &Flag::default(), Limits::console(), FakeRunner::default());
        result.expect("runs");
        runner.requests()
    };
    let first = sent();
    assert_eq!(first.len(), 2);
    assert_eq!(first, sent());
}

#[test]
fn a_dry_run_plans_every_call_and_opens_nothing() {
    let mut dry = request(EDIT);
    dry.dry_run = true;
    let (runner, result) = run_with(dry, &Flag::default(), Limits::console(), FakeRunner::default());
    let outcome = result.expect("runs");
    assert_eq!((outcome.plans.len(), outcome.plans[0].op.as_str()), (1, "face.set_profile"));
    assert_eq!((runner.begins(), runner.requests()[0].mode), (0, Mode::DryRun));
}

#[test]
fn a_cancel_during_the_last_call_stops_the_commit() {
    let runner = FakeRunner::default();
    let flag = Arc::new(Flag::default());
    let setter = flag.clone();
    *runner.on_request.lock().expect("lock") = Some(Box::new(move |_| setter.set()));
    // The edit is the final expression: no progress tick or later call follows it.
    let source = EDIT.trim_end_matches(';');
    let (runner, result) = run_with(request(source), flag.as_ref(), Limits::console(), runner);
    assert_eq!(result.expect_err("cancelled").kind, ScriptErrorKind::Cancelled);
    runner.assert_rolled_back();
}

#[test]
fn the_wall_clock_running_out_during_the_last_call_stops_the_commit() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let runner = FakeRunner::default();
    let late = clock.clone();
    *runner.on_request.lock().expect("lock") = Some(Box::new(move |_| late.advance(Seconds(10.0))));
    let runner = Arc::new(runner);
    let host = host(&runner, Limits::console(), &clock);
    let result = host.run(request(EDIT.trim_end_matches(';')), &calls(&runner), &Flag::default());
    assert_eq!(result.expect_err("times out").kind, limit(LimitKind::Time));
    runner.assert_rolled_back();
}

#[test]
fn a_failed_commit_rolls_the_transaction_back() {
    let runner = FakeRunner::default();
    runner.fail_commit.store(true, Ordering::SeqCst);
    let (runner, result) = run_with(request(EDIT), &Flag::default(), Limits::console(), runner);
    assert_eq!(result.expect_err("commit fails").kind, ScriptErrorKind::Op);
    assert_eq!((runner.commits(), runner.rollbacks()), (0, 1));
}

#[test]
fn a_failed_rollback_keeps_the_transaction_for_a_retry() {
    let runner = Arc::new(FakeRunner::default());
    runner.fail_rollback.store(true, Ordering::SeqCst);
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let calls = calls(&runner);
    let source = format!("{EDIT} ops::face::fail(#{{}})");
    let error =
        host(&runner, Limits::console(), &clock).run(request(&source), &calls, &Flag::default()).expect_err("fails");
    assert!(error.message.contains("rollback failed"), "{}", error.message);
    assert_eq!(calls.open_tx(), Some(TxId(1)), "the open transaction is still reachable");
    calls.rollback().expect("the retry succeeds");
    assert_eq!(calls.open_tx(), None);
    runner.assert_rolled_back();
}

#[test]
fn params_over_the_collection_limits_are_refused_before_the_run() {
    let limits = Limits { string_bytes: 4, array_len: 2, map_len: 2, ..Limits::console() };
    for (params, kind) in [
        (serde_json::json!({ "s": "12345" }), LimitKind::StringSize),
        (serde_json::json!({ "deep": [[1, 2, 3]] }), LimitKind::ArraySize),
        (serde_json::json!({ "m": { "a": 1, "b": 2, "c": 3 } }), LimitKind::MapSize),
        (serde_json::json!({ "a": 1, "b": 2, "c": 3 }), LimitKind::MapSize),
        (serde_json::json!({ "long_key": 1 }), LimitKind::StringSize),
        (serde_json::json!({ "m": { "long_key": 1 } }), LimitKind::StringSize),
    ] {
        let mut req = request("params");
        req.params = params.as_object().expect("an object").clone();
        let (runner, result) = run_with(req, &Flag::default(), limits.clone(), FakeRunner::default());
        assert_at(&result.expect_err("refused"), limit(kind), 1, 1);
        assert_eq!(runner.requests(), Vec::new());
    }
    let mut fits = request("params.s");
    fits.params = serde_json::json!({ "s": "1234" }).as_object().expect("an object").clone();
    let (_, result) = run_with(fits, &Flag::default(), limits, FakeRunner::default());
    assert_eq!(result.expect("fits").value, serde_json::json!("1234"));
}

#[test]
fn an_op_output_over_a_collection_limit_stops_the_run() {
    // 9 bytes lets the edit's `character` key through; the report's 10-byte key is over. Rhai checks the size of a
    // native function's string, array and map values itself, but not map keys.
    let limits = Limits { string_bytes: 9, ..Limits::console() };
    let source = format!("{EDIT}\nlet r = ops::measure::report(#{{}}).len(); r");
    let (runner, result) = run_with(request(&source), &Flag::default(), limits, FakeRunner::default());
    assert_at(&result.expect_err("too long"), limit(LimitKind::StringSize), 2, 23);
    runner.assert_rolled_back();
}

#[test]
fn jobs_and_edits_with_outside_effects_are_refused_before_they_are_sent() {
    // The position is the function name's, after `ops::area::`.
    for (op, says, column) in [("render::video", "is a job", 22), ("file::write", "outside the document", 20)] {
        let source = format!("{EDIT}\n  try {{ ops::{op}(#{{}}) }} catch {{ 0 }}");
        let (runner, result) = run_with(request(&source), &Flag::default(), Limits::console(), FakeRunner::default());
        let error = result.expect_err("refused");
        assert_at(&error, ScriptErrorKind::Refused, 2, column);
        assert!(error.message.contains(says), "{}", error.message);
        assert_eq!(runner.requests().len(), 1, "only the first edit was sent");
        runner.assert_rolled_back();
    }
}
