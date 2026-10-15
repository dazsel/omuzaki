//! The check library grades an attempt from a fixture runner, a result runner and a transcript: each check kind passes
//! on a right result and fails on a wrong one, and the outcomes keep the model's mistakes apart from the harness's.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo};
use omuzaki_ops_mock::MockRunner;
use omuzaki_test::eval::check::Outcome::{Fail, Pass};
use omuzaki_test::eval::check::{CheckReport, Checker, Outcome};
use omuzaki_test::eval::task::Task;
use omuzaki_test::eval::transcript::{ToolCall, Transcript};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn path(s: &str) -> DocPath {
    DocPath::parse(s).unwrap()
}

macro_rules! op {
    ($name:ident, $text:literal, $kind:ident, $scope:ident, $undo:ident, $plan:expr) => {
        impl Op for $name {
            fn meta() -> OpMeta {
                OpMeta {
                    name: $text,
                    summary: "Test op.",
                    kind: OpKind::$kind,
                    scope: Scope::$scope,
                    undo: Undo::$undo,
                }
            }
            fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
                let run: fn(&Self, &OpCtx<'_>) -> Result<Plan, OpError> = $plan;
                run(self, ctx)
            }
        }
        omuzaki_ops_api::register_op!($name);
    };
}

fn counter(ctx: &OpCtx<'_>) -> i64 {
    ctx.doc.get(&path("counter")).and_then(Value::as_i64).unwrap_or_default()
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct ReadCounter {}
op!(ReadCounter, "evaltest.counter", Query, Read, None, |_, ctx| Ok(Plan::answer(json!({"counter": counter(ctx)}))));

#[derive(Serialize, Deserialize, JsonSchema)]
struct ReadDoc {}
op!(ReadDoc, "evaltest.doc", Query, Read, None, |_, ctx| Ok(Plan::answer(json!({
    "counter": ctx.doc.get(&path("counter")).cloned(),
    "chars": ctx.doc.get(&path("chars")).cloned()
}))));

#[derive(Serialize, Deserialize, JsonSchema)]
struct ListChars {}
op!(ListChars, "evaltest.chars", Query, Read, None, |_, ctx| {
    let ids: Vec<&String> = match ctx.doc.get(&path("chars")) {
        Some(Value::Object(map)) => map.keys().collect(),
        _ => Vec::new(),
    };
    Ok(Plan::answer(json!({ "ids": ids })))
});

#[derive(Serialize, Deserialize, JsonSchema)]
struct ReadChar {
    id: String,
}
op!(ReadChar, "evaltest.char", Query, Read, None, |op, ctx| {
    ctx.doc
        .get(&path(&format!("chars/{}", op.id)))
        .map(|v| Plan::answer(v.clone()))
        .ok_or_else(|| OpError::not_found(format!("no character {}", op.id)))
});

#[derive(Serialize, Deserialize, JsonSchema)]
struct Boom {}
op!(Boom, "evaltest.boom", Query, Read, None, |_, _| Err(OpError::internal("a bug")));

#[derive(Serialize, Deserialize, JsonSchema)]
struct CounterGate {}
op!(CounterGate, "evaltest.gate", Query, Read, None, |_, ctx| Ok(Plan::answer(json!({"passed": counter(ctx) <= 10}))));

#[derive(Serialize, Deserialize, JsonSchema)]
struct SetCounter {
    value: i64,
}
op!(SetCounter, "evaltest.set_counter", Edit, EditScene, Auto, |op, ctx| Ok(Plan::edit(Patch::new().with(
    path("counter"),
    ctx.doc.get(&path("counter")).cloned(),
    Some(json!(op.value))
))));

fn runner(doc: &Value) -> Arc<dyn Runner> {
    Arc::new(MockRunner::new(MemDoc::from_value(doc.clone())))
}

/// An edit's call as the server logs it: an applied, accepted call carries an application record.
fn call(op: &str, args: &Value, mode: Mode, ok: bool) -> ToolCall {
    let call = ToolCall::new(op, args.clone(), mode, ok);
    if mode == Mode::Apply && ok { call.with_application() } else { call }
}

fn task(toml: &str) -> Task {
    let head = "id = 't'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    toml::from_str(&format!("{head}{toml}")).unwrap()
}

/// Grades with a fixture whose counter is 1 and a result whose counter is 5.
fn grade(toml: &str, transcript: Vec<ToolCall>) -> CheckReport {
    let fixture = runner(&json!({"counter": 1, "chars": {"a": {"name": "A"}}}));
    let result = runner(&json!({"counter": 5, "chars": {"a": {"name": "A"}, "fox": {"name": "Fox"}}}));
    Checker::new(fixture, result, Transcript::new(transcript)).grade(&task(toml))
}

fn outcome(toml: &str) -> Outcome {
    grade(toml, Vec::new()).results[0].outcome
}

fn query(at: &str, expect: &str) -> String {
    format!("[[check]]\nid='c'\nkind='query'\nop='evaltest.counter'\nat='{at}'\nexpect={expect}\n")
}

/// The same check over `evaltest.doc`, which returns the raw `counter` and `chars` of the document.
fn doc_query(at: &str, expect: &str) -> String {
    query(at, expect).replace("evaltest.counter", "evaltest.doc")
}

#[test]
fn a_query_passes_on_the_right_value_and_fails_on_the_wrong_one() {
    assert_eq!(outcome(&query("/counter", "{ equals = 5 }")), Outcome::Pass);
    assert_eq!(outcome(&query("/counter", "{ equals = 4 }")), Outcome::Fail);
    assert_eq!(outcome(&query("/counter", "{ range = [4.0, 6.0] }")), Outcome::Pass);
    assert_eq!(outcome(&query("/counter", "{ range = [6.0, 9.0] }")), Outcome::Fail);
    assert_eq!(outcome(&query("/missing", "{ equals = 5 }")), Outcome::Fail);
}

#[test]
fn a_query_change_compares_the_result_with_the_fixture() {
    assert_eq!(outcome(&query("/counter", "{ change = 'up', at_least = 4.0 }")), Outcome::Pass);
    assert_eq!(outcome(&query("/counter", "{ change = 'up', at_least = 5.0 }")), Outcome::Fail);
    assert_eq!(outcome(&query("/counter", "{ change = 'up', at_least = 3.0, relative = true }")), Outcome::Pass);
    assert_eq!(outcome(&query("/counter", "{ change = 'down' }")), Outcome::Fail);
}

#[test]
fn a_query_that_expects_nothing_is_an_error_not_a_pass() {
    assert_eq!(outcome(&query("/counter", "{}")), Outcome::Error);
}

#[test]
fn a_query_naming_an_edit_op_is_refused_before_it_runs() {
    let toml = "[[check]]\nid='c'\nkind='query'\nop='evaltest.set_counter'\nargs={ value = 99 }\nat='/x'\nexpect={ equals = 1 }\n";
    let fixture = runner(&json!({"counter": 1}));
    let result = Arc::new(MockRunner::new(MemDoc::from_value(json!({"counter": 5}))));
    let report = Checker::new(fixture, result.clone(), Transcript::default()).grade(&task(toml));
    assert_eq!(report.results[0].outcome, Outcome::Error);
    assert!(report.results[0].detail.contains("not read-only"), "{}", report.results[0].detail);
    assert_eq!(
        result.get(&path("counter"), &omuzaki_ops_api::Grants::person()).unwrap(),
        Some(json!(5)),
        "nothing ran"
    );
}

#[test]
fn an_op_error_is_a_fail_when_the_agent_broke_it_and_an_error_when_the_harness_did() {
    let gone = "[[check]]\nid='c'\nkind='query'\nop='evaltest.char'\nargs={ id = 'nobody' }\nat='/name'\nexpect={ equals = 'x' }\n";
    assert_eq!(outcome(gone), Outcome::Fail);
    let boom = "[[check]]\nid='c'\nkind='query'\nop='evaltest.boom'\nat='/x'\nexpect={ equals = 1 }\n";
    assert_eq!(outcome(boom), Outcome::Error);
    let unknown = "[[check]]\nid='c'\nkind='query'\nop='evaltest.nope'\nat='/x'\nexpect={ equals = 1 }\n";
    assert_eq!(outcome(unknown), Outcome::Error);
}

const BIND: &str = "[bind]\nfox = { op='evaltest.chars', at='/ids/*', pick='new' }\n";

fn bound_check(name: &str) -> String {
    format!(
        "{BIND}[[check]]\nid='c'\nkind='query'\nop='evaltest.char'\nargs={{ id = '{{fox}}' }}\nat='/name'\nexpect={{ equals = '{name}' }}\n"
    )
}

#[test]
fn a_binding_names_the_one_new_id_and_checks_use_it() {
    assert_eq!(outcome(&bound_check("Fox")), Outcome::Pass);
    assert_eq!(outcome(&bound_check("Wolf")), Outcome::Fail);
}

#[test]
fn zero_or_several_new_ids_fail_the_check_that_uses_the_binding() {
    let toml = bound_check("Fox");
    // The fixture already has every character the result has.
    let same = runner(&json!({"chars": {"fox": {"name": "Fox"}}}));
    let report = Checker::new(same.clone(), same, Transcript::default()).grade(&task(&toml));
    assert_eq!(report.results[0].outcome, Outcome::Fail);
    assert!(report.results[0].detail.contains("found 0 new ids"), "{}", report.results[0].detail);
    // The agent made two where one was asked for.
    let two = runner(&json!({"chars": {"fox": {"name": "Fox"}, "owl": {"name": "Owl"}}}));
    let report = Checker::new(runner(&json!({})), two, Transcript::default()).grade(&task(&toml));
    assert_eq!(report.results[0].outcome, Outcome::Fail);
    assert!(report.results[0].detail.contains("found 2 new ids"), "{}", report.results[0].detail);
}

#[test]
fn a_gate_op_passes_when_it_says_so_and_declared_gates_need_project_describe() {
    assert_eq!(outcome("[[check]]\nid='c'\nkind='gate'\nops=['evaltest.gate']\n"), Outcome::Pass);
    let fixture = runner(&json!({"counter": 1}));
    let result = runner(&json!({"counter": 50}));
    let gate = task("[[check]]\nid='c'\nkind='gate'\nops=['evaltest.gate']\n");
    assert_eq!(
        Checker::new(fixture.clone(), result.clone(), Transcript::default()).grade(&gate).results[0].outcome,
        Outcome::Fail
    );
    let declared = task("[[check]]\nid='c'\nkind='gate'\ndeclared=true\n");
    let report = Checker::new(fixture, result, Transcript::default()).grade(&declared);
    assert_eq!(report.results[0].outcome, Outcome::Error);
    assert!(report.results[0].detail.contains("not available"), "{}", report.results[0].detail);
    assert_eq!(outcome("[[check]]\nid='c'\nkind='gate'\nops=['evaltest.set_counter']\n"), Outcome::Error);
}

fn log_outcomes(transcript: Vec<ToolCall>, checks: &str) -> Vec<Outcome> {
    grade(checks, transcript).results.iter().map(|r| r.outcome).collect()
}

#[test]
fn log_checks_read_what_the_server_returned() {
    let a = json!({"value": 5});
    let transcript = vec![
        call("evaltest.set_counter", &a, Mode::DryRun, true),
        call("evaltest.set_counter", &a, Mode::Apply, true),
        call("evaltest.boom", &json!({}), Mode::Apply, false),
    ];
    let checks = "[[check]]\nid='a'\nkind='log.applied'\nop='evaltest.set_*'\n\
                  [[check]]\nid='b'\nkind='log.not_applied'\nop='evaltest.set_counter'\n\
                  [[check]]\nid='c'\nkind='log.not_applied'\nop='evaltest.boom'\n\
                  [[check]]\nid='d'\nkind='log.within'\nops=['evaltest.set_*']\n\
                  [[check]]\nid='e'\nkind='log.within'\nops=['evaltest.char']\n\
                  [[check]]\nid='f'\nkind='log.previewed'\n\
                  [[check]]\nid='g'\nkind='log.max_errors'\nn=1\n\
                  [[check]]\nid='h'\nkind='log.max_errors'\nn=0\n";
    assert_eq!(log_outcomes(transcript, checks), [Pass, Fail, Pass, Pass, Fail, Pass, Pass, Fail]);
}

#[test]
fn an_edit_applied_without_a_matching_dry_run_is_not_previewed() {
    let checks = "[[check]]\nid='f'\nkind='log.previewed'\n";
    let dry = call("evaltest.set_counter", &json!({"value": 4}), Mode::DryRun, true);
    let applied = call("evaltest.set_counter", &json!({"value": 5}), Mode::Apply, true);
    assert_eq!(log_outcomes(vec![dry, applied], checks), [Outcome::Fail]);
    let applied_first = call("evaltest.set_counter", &json!({"value": 5}), Mode::Apply, true);
    let late_dry = call("evaltest.set_counter", &json!({"value": 5}), Mode::DryRun, true);
    assert_eq!(log_outcomes(vec![applied_first, late_dry], checks), [Outcome::Fail]);
}

#[test]
fn checks_not_graded_yet_are_errors_and_soft_checks_carry_a_score_but_never_fail_the_attempt() {
    assert_eq!(outcome("[[check]]\nid='c'\nkind='doc.exists'\npath='counter'\n"), Outcome::Error);
    let soft = "[[check]]\nid='hard'\nkind='gate'\nops=['evaltest.gate']\n\
                [[check]]\nid='soft'\nhard=false\nkind='query'\nop='evaltest.counter'\nat='/counter'\nexpect={ equals = 1 }\n";
    let report = grade(soft, Vec::new());
    assert_eq!(report.results[0].score, None);
    assert_eq!(report.results[1].score, Some(0.0));
    assert!(report.passed());
}

#[test]
fn grading_the_same_inputs_twice_gives_byte_identical_reports() {
    let toml = format!("{}{}", bound_check("Fox"), query("/counter", "{ change = 'up' }"));
    let a = grade(&toml, Vec::new()).to_json().unwrap();
    let b = grade(&toml, Vec::new()).to_json().unwrap();
    assert_eq!(a, b);
    assert!(a.contains("\"before\"") && a.contains("\"after\""));
}

/// Grades one query against a result whose counter is `counter`, on the standard fixture.
fn counter_outcome(counter: &Value, expect: &str) -> Outcome {
    let fixture = runner(&json!({"counter": 1}));
    let result = runner(&json!({ "counter": counter }));
    let report = Checker::new(fixture, result, Transcript::default()).grade(&task(&doc_query("/counter", expect)));
    report.results[0].outcome
}

#[test]
fn a_fixture_that_lacks_what_the_query_reads_is_an_error_not_the_agents_fail() {
    // The result has `/chars/fox/name`; the pristine fixture does not, so the baseline is broken, not the agent.
    let change = "{ change = 'any' }";
    assert_eq!(outcome(&doc_query("/chars/fox/name", change)), Outcome::Error);
    // The fixture has a string where a change needs a number.
    assert_eq!(outcome(&doc_query("/chars/a/name", change)), Outcome::Error);
    // A pointer that reaches two values on the fixture, and one on the result, is the fixture's trouble too.
    let two = runner(&json!({"chars": {"a": {"name": "A"}, "b": {"name": "B"}}}));
    let one = runner(&json!({"chars": {"a": {"name": "A"}}}));
    let report = Checker::new(two, one, Transcript::default()).grade(&task(&doc_query("/chars/*/name", change)));
    assert_eq!(report.results[0].outcome, Outcome::Error);
    // Whereas a result that lost or mangled the value is the agent's.
    assert_eq!(outcome(&doc_query("/missing", "{ equals = 1 }")), Outcome::Fail);
    let broken = runner(&json!({"counter": "five"}));
    let fixture = runner(&json!({"counter": 1}));
    let report = Checker::new(fixture, broken, Transcript::default()).grade(&task(&doc_query("/counter", change)));
    assert_eq!(report.results[0].outcome, Outcome::Fail);
}

#[test]
fn an_errored_check_carries_no_score_hard_or_soft() {
    let toml = "[[check]]\nid='a'\nhard=false\nkind='doc.exists'\npath='counter'\n\
                [[check]]\nid='b'\nhard=false\nkind='query'\nop='evaltest.doc'\nat='/chars/fox/name'\n\
                expect={ change = 'any' }\n\
                [[check]]\nid='c'\nhard=false\nkind='query'\nop='evaltest.counter'\nat='/counter'\n\
                expect={ equals = 5 }\n\
                [[check]]\nid='d'\nhard=false\nkind='query'\nop='evaltest.counter'\nat='/counter'\n\
                expect={ equals = 4 }\n";
    let report = grade(toml, Vec::new());
    let seen: Vec<_> = report.results.iter().map(|r| (r.outcome, r.score)).collect();
    assert_eq!(
        seen,
        [(Outcome::Error, None), (Outcome::Error, None), (Outcome::Pass, Some(1.0)), (Outcome::Fail, Some(0.0))]
    );
    let json = report.to_json().unwrap();
    assert!(json.contains("\"score\": null"), "an errored soft check serialises without a score");
}

#[test]
fn equals_compares_integers_exactly_and_still_reads_one_as_one_point_zero() {
    let big = json!(9_007_199_254_740_993_i64);
    assert_eq!(counter_outcome(&big, "{ equals = 9007199254740993 }"), Outcome::Pass);
    assert_eq!(counter_outcome(&big, "{ equals = 9007199254740992 }"), Outcome::Fail);
    assert_eq!(counter_outcome(&big, "{ equals = 9007199254740994 }"), Outcome::Fail);
    assert_eq!(counter_outcome(&json!(9_007_199_254_740_992_i64), "{ equals = 9007199254740993 }"), Outcome::Fail);
    assert_eq!(counter_outcome(&json!(9_007_199_254_740_991_i64), "{ equals = 9007199254740992 }"), Outcome::Fail);
    assert_eq!(counter_outcome(&json!(5), "{ equals = 5.0 }"), Outcome::Pass);
    assert_eq!(counter_outcome(&json!(5.0), "{ equals = 5 }"), Outcome::Pass);
    assert_eq!(counter_outcome(&json!(5.000_000_000_000_001), "{ equals = 5 }"), Outcome::Fail);
}

#[test]
fn a_pointer_without_a_leading_slash_is_a_broken_check_everywhere_it_is_used() {
    // The query would otherwise read the whole output like the empty pointer.
    assert_eq!(outcome(&query("counter", "{ equals = 5 }")), Outcome::Error);
    assert_eq!(outcome(&query("counter", "{ change = 'up' }")), Outcome::Error);
    // So would a binding.
    let binding = bound_check("Fox").replace("/ids/*", "ids/*");
    assert_eq!(outcome(&binding), Outcome::Error);
    assert_eq!(outcome(&bound_check("Fox")), Outcome::Pass);
}

/// What the MCP session logs for a successful query: it runs in `Mode::Apply` and carries no application record.
fn mcp_query(op: &str) -> ToolCall {
    ToolCall::new(op, json!({}), Mode::Apply, true)
}

#[test]
fn a_read_only_query_in_apply_mode_is_not_an_application() {
    let transcript = vec![mcp_query("evaltest.counter"), mcp_query("evaltest.chars")];
    let checks = "[[check]]\nid='a'\nkind='log.applied'\nop='evaltest.counter'\n\
                  [[check]]\nid='b'\nkind='log.not_applied'\nop='evaltest.counter'\n\
                  [[check]]\nid='c'\nkind='log.within'\nops=['evaltest.set_*']\n\
                  [[check]]\nid='d'\nkind='log.previewed'\n";
    assert_eq!(log_outcomes(transcript, checks), [Fail, Pass, Pass, Pass]);
    // A real application record still counts, and a refused edit never does.
    let edit = ToolCall::new("evaltest.set_counter", json!({"value": 5}), Mode::Apply, true).with_application();
    let refused = ToolCall::new("evaltest.set_counter", json!({"value": 6}), Mode::Apply, false);
    let checks = "[[check]]\nid='a'\nkind='log.applied'\nop='evaltest.set_counter'\n\
                  [[check]]\nid='b'\nkind='log.within'\nops=['evaltest.char']\n";
    assert_eq!(log_outcomes(vec![edit, refused], checks), [Pass, Fail]);
}

#[test]
fn a_preview_is_matched_to_its_apply_without_the_dry_run_transport_flag() {
    let checks = "[[check]]\nid='f'\nkind='log.previewed'\n";
    // As the MCP server logs them: the preview carries `dry_run: true`, the apply omits it.
    let preview = ToolCall::new("evaltest.set_counter", json!({"value": 5, "dry_run": true}), Mode::DryRun, true);
    let apply = ToolCall::new("evaltest.set_counter", json!({"value": 5}), Mode::Apply, true).with_application();
    assert_eq!(log_outcomes(vec![preview.clone(), apply], checks), [Outcome::Pass]);
    // Real arguments still have to match.
    let other = ToolCall::new("evaltest.set_counter", json!({"value": 6}), Mode::Apply, true).with_application();
    assert_eq!(log_outcomes(vec![preview, other], checks), [Outcome::Fail]);
}
