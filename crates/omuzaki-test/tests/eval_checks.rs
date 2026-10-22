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

/// An edit that is registered with the `read` scope: the scope alone must not make it safe to run while grading.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SneakyEdit {}
op!(SneakyEdit, "evaltest.sneaky_edit", Edit, Read, Auto, |_, ctx| Ok(Plan::edit(Patch::new().with(
    path("counter"),
    ctx.doc.get(&path("counter")).cloned(),
    Some(json!(999))
))));

/// A job that is registered with the `render` scope, as a render job is.
#[derive(Serialize, Deserialize, JsonSchema)]
struct RenderJob {}
op!(RenderJob, "evaltest.render_job", Job, Render, None, |_, _| Ok(Plan::answer(json!({"passed": true}))));

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
    assert!(report.results[0].detail.contains("not a query"), "{}", report.results[0].detail);
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
fn judge_is_not_graded_yet_and_soft_checks_carry_a_score_but_never_fail_the_attempt() {
    assert_eq!(outcome("[[check]]\nid='c'\nkind='judge'\nrubric=['Is it round?']\n"), Outcome::Error);
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
    let toml = "[[check]]\nid='a'\nhard=false\nkind='judge'\nrubric=['Is it round?']\n\
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

#[test]
fn a_change_query_whose_pointer_is_absent_from_the_fixture_too_is_a_broken_check() {
    // An obsolete pointer: neither the pristine fixture nor the result has it, so no attempt could ever pass.
    assert_eq!(outcome(&doc_query("/chars/gone/hp", "{ change = 'up' }")), Outcome::Error);
    assert_eq!(outcome(&doc_query("/chars/gone/hp", "{ change = 'none' }")), Outcome::Error);
    // Whereas an `equals` on a value the agent was to create is still the agent's to get right.
    assert_eq!(outcome(&doc_query("/chars/gone/hp", "{ equals = 1 }")), Outcome::Fail);
    // And a fixture value that is present but moved or lost in the result stays the agent's.
    let lost = runner(&json!({"chars": {}}));
    let fixture = runner(&json!({"counter": 1}));
    let report =
        Checker::new(fixture, lost, Transcript::default()).grade(&task(&doc_query("/counter", "{ change = 'up' }")));
    assert_eq!(report.results[0].outcome, Outcome::Fail);
}

#[test]
fn checks_can_name_the_fixtures_exports_and_a_name_cannot_be_both_an_export_and_a_bind() {
    let by_export = "[[check]]\nid='c'\nkind='query'\nop='evaltest.char'\nargs={ id = '{main}' }\nat='/name'\nexpect={ equals = 'A' }\n";
    let exports = std::collections::BTreeMap::from([("main".to_owned(), "a".to_owned())]);
    let fixture = runner(&json!({"counter": 1, "chars": {"a": {"name": "A"}}}));
    let result = runner(&json!({"counter": 5, "chars": {"a": {"name": "A"}, "fox": {"name": "Fox"}}}));
    let checker = Checker::new(fixture.clone(), result.clone(), Transcript::default()).with_exports(exports.clone());
    assert_eq!(checker.grade(&task(by_export)).results[0].outcome, Outcome::Pass);
    // Exports and bindings work together in one task.
    let both = format!("{BIND}{}", by_export.replace("'c'", "'e'") + &bound_check("Fox").replace(BIND, ""));
    let report = Checker::new(fixture.clone(), result.clone(), Transcript::default())
        .with_exports(exports.clone())
        .grade(&task(&both));
    assert_eq!(report.results.iter().map(|r| r.outcome).collect::<Vec<_>>(), [Outcome::Pass, Outcome::Pass]);
    // A name both exported and bound cannot be resolved either way.
    let clash = std::collections::BTreeMap::from([("fox".to_owned(), "a".to_owned())]);
    let report =
        Checker::new(fixture, result, Transcript::default()).with_exports(clash).grade(&task(&bound_check("Fox")));
    assert_eq!(report.results[0].outcome, Outcome::Error);
    assert!(
        report.results[0].detail.contains("both a fixture export and a [bind] name"),
        "{}",
        report.results[0].detail
    );
}

/// Grades one query on a fixture and a result whose counters are given.
fn counters(before: &Value, after: &Value, expect: &str) -> Outcome {
    let report = Checker::new(
        runner(&json!({ "counter": before })),
        runner(&json!({ "counter": after })),
        Transcript::default(),
    )
    .grade(&task(&doc_query("/counter", expect)));
    report.results[0].outcome
}

#[test]
fn change_checks_measure_integers_exactly_and_relative_change_from_any_nonzero_baseline() {
    let (low, high) = (json!(9_007_199_254_740_992_i64), json!(9_007_199_254_740_993_i64));
    assert_eq!(counters(&low, &high, "{ change = 'up' }"), Outcome::Pass);
    assert_eq!(counters(&low, &high, "{ change = 'none' }"), Outcome::Fail);
    assert_eq!(counters(&low, &low, "{ change = 'none' }"), Outcome::Pass);
    assert_eq!(counters(&high, &low, "{ change = 'down', at_least = 1.0 }"), Outcome::Pass);
    assert_eq!(counters(&low, &high, "{ change = 'up', at_least = 2.0 }"), Outcome::Fail);
    // 1e-20 to 1.1e-20 is a 10% move, not an infinite one.
    let (tiny, more) = (json!(1.0e-20), json!(1.1e-20));
    assert_eq!(counters(&tiny, &more, "{ change = 'up', relative = true, at_least = 1.0 }"), Outcome::Fail);
    assert_eq!(counters(&tiny, &more, "{ change = 'up', relative = true, at_least = 0.09 }"), Outcome::Pass);
    // Only an exact zero baseline is infinite.
    assert_eq!(
        counters(&json!(0), &json!(0.1), "{ change = 'up', relative = true, at_least = 1000.0 }"),
        Outcome::Pass
    );
}

#[test]
fn a_placeholder_nothing_defines_is_a_broken_check_for_every_kind_that_can_carry_one() {
    let exports = std::collections::BTreeMap::from([("main".to_owned(), "a".to_owned())]);
    let fixture = runner(&json!({"counter": 1, "chars": {"a": {"name": "A"}}}));
    let result = runner(&json!({"counter": 5, "chars": {"a": {"name": "A"}}}));
    let grade_with = |toml: &str| {
        Checker::new(fixture.clone(), result.clone(), Transcript::default())
            .with_exports(exports.clone())
            .grade(&task(toml))
            .results[0]
            .clone()
    };
    // A misspelt export in a query's arguments, in its pointer and in its expected value.
    for misspelt in [
        "[[check]]\nid='c'\nkind='query'\nop='evaltest.char'\nargs={ id = '{mian}' }\nat='/name'\nexpect={ equals = 'A' }\n",
        "[[check]]\nid='c'\nkind='query'\nop='evaltest.doc'\nat='/chars/{mian}/name'\nexpect={ equals = 'A' }\n",
        "[[check]]\nid='c'\nkind='query'\nop='evaltest.doc'\nat='/chars/a/name'\nexpect={ equals = '{mian}' }\n",
        // The log checks and gates carry names too, and a placeholder left there is no more the agent's fault.
        "[[check]]\nid='c'\nkind='log.applied'\nop='{mian}'\n",
        "[[check]]\nid='c'\nkind='log.within'\nops=['{mian}']\n",
        "[[check]]\nid='c'\nkind='gate'\nops=['{mian}']\n",
        "[[check]]\nid='c'\nkind='doc.exists'\npath='chars/{mian}'\n",
    ] {
        let result = grade_with(misspelt);
        assert_eq!(result.outcome, Outcome::Error, "{misspelt}");
        assert!(result.detail.contains("{mian}") && result.detail.contains("main"), "{}", result.detail);
    }
    // The right name passes, and braces that are not a plain name are left alone.
    let right = "[[check]]\nid='c'\nkind='query'\nop='evaltest.char'\nargs={ id = '{main}' }\nat='/name'\nexpect={ equals = 'A' }\n";
    assert_eq!(grade_with(right).outcome, Outcome::Pass);
    let json_text =
        "[[check]]\nid='c'\nkind='query'\nop='evaltest.doc'\nat='/chars/a/name'\nexpect={ equals = '{\"a\": 1}' }\n";
    assert_eq!(grade_with(json_text).outcome, Outcome::Fail);
}

/// The check kinds that carry a name, each with a way to put `name` where the kind reads one.
/// A check kind's name, and how it puts a name where the kind reads one.
type NameUser = (&'static str, fn(&str) -> String);

/// A source of names: what it is, the task text it needs, the name to use, and the exports it gets.
type NameSource = (&'static str, &'static str, &'static str, Vec<(&'static str, &'static str)>);

fn name_users() -> Vec<NameUser> {
    vec![
        ("query", |n| {
            format!("kind='query'\nop='evaltest.char'\nargs={{ id = '{n}' }}\nat='/name'\nexpect={{ equals = 'A' }}\n")
        }),
        ("gate", |n| format!("kind='gate'\nops=['{n}']\n")),
        ("log.applied", |n| format!("kind='log.applied'\nop='{n}'\n")),
        ("log.not_applied", |n| format!("kind='log.not_applied'\nop='{n}'\n")),
        ("log.within", |n| format!("kind='log.within'\nops=['{n}']\n")),
        ("doc.exists", |n| format!("kind='doc.exists'\npath='chars/{n}'\n")),
    ]
}

/// Where a name can come from, and one malformed input for each: the task text and the exports it gets.
fn broken_name_sources() -> Vec<NameSource> {
    let bind =
        |body: &str| -> &'static str { Box::leak(format!("[bind]\nx = {{ {body}, pick='new' }}\n").into_boxed_str()) };
    vec![
        // (source, task head, the name to use, exports)
        ("an undefined name", "", "{nope}", vec![]),
        ("a binding whose pointer reaches numbers", bind("op='evaltest.doc', at='/counter'"), "{x}", vec![]),
        ("a binding with a malformed pointer", bind("op='evaltest.chars', at='ids/*'"), "{x}", vec![]),
        ("a binding whose op fails on the fixture", bind("op='evaltest.boom', at='/ids/*'"), "{x}", vec![]),
        ("a binding naming an op nothing registers", bind("op='evaltest.nope', at='/ids/*'"), "{x}", vec![]),
        (
            "a binding naming an edit op",
            bind("op='evaltest.set_counter', args={ value = 1 }, at='/ids/*'"),
            "{x}",
            vec![],
        ),
        ("a name that is both an export and a bind", bind("op='evaltest.chars', at='/ids/*'"), "{x}", vec![("x", "a")]),
    ]
}

#[test]
fn every_malformed_input_is_a_harness_error_for_every_check_kind_that_reads_it() {
    let fixture = runner(&json!({"counter": 1, "chars": {"a": {"name": "A"}}}));
    let result = runner(&json!({"counter": 5, "chars": {"a": {"name": "A"}, "fox": {"name": "Fox"}}}));
    for (kind, use_name) in name_users() {
        for (source, head, name, exports) in broken_name_sources() {
            let toml = format!("{head}[[check]]\nid='c'\n{}", use_name(name));
            let exports = exports.into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
            let report = Checker::new(fixture.clone(), result.clone(), Transcript::default())
                .with_exports(exports)
                .grade(&task(&toml));
            assert_eq!(report.results[0].outcome, Outcome::Error, "{kind} with {source}: {}", report.results[0].detail);
        }
    }
}

#[test]
fn malformed_arguments_pointers_and_answers_are_harness_errors_not_the_agents_fail() {
    let char_query = |args: &str| {
        format!(
            "[[check]]\nid='c'\nkind='query'\nop='evaltest.char'\nargs={args}\nat='/name'\nexpect={{ equals = 'A' }}\n"
        )
    };
    // Op arguments that do not decode against the op's schema: a missing field, the wrong type.
    for args in ["{}", "{ id = 5 }", "{ id = ['a'] }"] {
        assert_eq!(outcome(&char_query(args)), Outcome::Error, "{args}");
    }
    // The result project missing the object is the agent's.
    assert_eq!(outcome(&char_query("{ id = 'nobody' }")), Outcome::Fail);
    // An op nothing registers, and an edit op, as a query.
    for op in ["evaltest.nope", "evaltest.set_counter"] {
        let toml = format!("[[check]]\nid='c'\nkind='query'\nop='{op}'\nat='/x'\nexpect={{ equals = 1 }}\n");
        assert_eq!(outcome(&toml), Outcome::Error, "{op}");
    }
    // A gate op that does not answer `passed` as a boolean.
    let gate = "[[check]]\nid='c'\nkind='gate'\nops=['evaltest.doc']\n";
    assert_eq!(outcome(gate), Outcome::Error);
    // A binding that reaches a number mixed in with ids is not silently filtered down to the strings.
    let mixed = "[bind]\nx = { op='evaltest.doc', at='/*', pick='new' }\n\
                 [[check]]\nid='c'\nkind='log.applied'\nop='{x}'\n";
    assert_eq!(outcome(mixed), Outcome::Error);
}

/// One malformed form, the check text that carries it, and the field the message must name.
type Row = (&'static str, String, &'static str);

fn expect_row(label: &'static str, expect: &str, field: &'static str) -> Row {
    (label, query("/counter", expect), field)
}

fn pointer_row(label: &'static str, at: &str, field: &'static str) -> Row {
    (label, query(at, "{ equals = 1 }"), field)
}

fn malformed_rows() -> Vec<Row> {
    let on = |body: &str| format!("[[check]]\nid='c'\n{body}");
    vec![
        // The modifiers of `change`.
        expect_row("a negative at_least", "{ change = 'up', at_least = -0.04 }", "at_least"),
        expect_row("at_least without change", "{ equals = 5, at_least = 100.0 }", "at_least"),
        expect_row("relative without change", "{ equals = 5, relative = true }", "relative"),
        expect_row("relative without at_least", "{ change = 'up', relative = true }", "relative"),
        expect_row("change none with at_least", "{ change = 'none', at_least = 1.0 }", "none"),
        expect_row("change none with relative", "{ change = 'none', relative = true, at_least = 0.1 }", "none"),
        // Range and equals.
        expect_row("a range above itself", "{ range = [6.0, 4.0] }", "range"),
        expect_row("equals outside the range", "{ equals = 9, range = [4.0, 6.0] }", "equals"),
        expect_row("a string equals beside a range", "{ equals = 'five', range = [4.0, 6.0] }", "equals"),
        expect_row("a string equals beside a change", "{ equals = 'five', change = 'up' }", "equals"),
        expect_row("an expectation of nothing", "{}", "expect"),
        // Pointers, by RFC 6901.
        pointer_row("an escape that is neither ~0 nor ~1", "/chars/a~2b", "~"),
        pointer_row("a dangling escape", "/chars/a~", "~"),
        pointer_row("no leading slash", "chars/a", "starts with"),
        // The rest of the grammar.
        ("a gate naming nothing", on("kind='gate'\n"), "gate"),
        ("a gate op with an empty name", on("kind='gate'\nops=['']\n"), "ops"),
        ("a log op with an empty name", on("kind='log.applied'\nop=' '\n"), "op"),
        ("a log.within entry with an empty name", on("kind='log.within'\nops=['evaltest.char', '']\n"), "ops"),
        ("a query with an empty op", on("kind='query'\nop=''\nat='/x'\nexpect={ equals = 1 }\n"), "op"),
        ("doc.range with no bound", on("kind='doc.range'\npath='counter'\n"), "min"),
        ("doc.range above itself", on("kind='doc.range'\npath='counter'\nmin=5.0\nmax=1.0\n"), "min"),
        (
            "doc.change with a negative at_least",
            on("kind='doc.change'\npath='counter'\nchange='up'\nat_least=-1.0\n"),
            "at_least",
        ),
        (
            "doc.change with relative alone",
            on("kind='doc.change'\npath='counter'\nchange='up'\nrelative=true\n"),
            "relative",
        ),
        (
            "render.diff with no threshold",
            on("kind='render.diff'\nrender={ op='render.preview' }\nregion=[0.0, 0.0, 1.0, 1.0]\nexpect={}\n"),
            "threshold",
        ),
        (
            "render.diff with a fraction above one",
            on(
                "kind='render.diff'\nrender={ op='render.preview' }\nregion=[0.0, 0.0, 1.0, 1.0]\nexpect={ changed_fraction_at_least = 2.0 }\n",
            ),
            "changed_fraction_at_least",
        ),
        (
            "render.diff with its thresholds crossed",
            on(
                "kind='render.diff'\nrender={ op='render.preview' }\nregion=[0.0, 0.0, 1.0, 1.0]\nexpect={ changed_fraction_at_least = 0.8, changed_fraction_at_most = 0.2 }\n",
            ),
            "changed_fraction_at_least",
        ),
        (
            "a binding with an empty op",
            format!("[bind]\nx = {{ op='', at='/ids/*', pick='new' }}\n{}", on("kind='log.applied'\nop='{x}'\n")),
            "bind op",
        ),
    ]
}

#[test]
fn every_malformed_form_is_a_harness_error_naming_its_field_before_anything_is_graded() {
    for (label, toml, field) in malformed_rows() {
        let report = grade(&toml, Vec::new());
        let result = &report.results[0];
        assert_eq!(result.outcome, Outcome::Error, "{label}: {}", result.detail);
        assert!(result.detail.contains(field), "{label}: the message must name `{field}`: {}", result.detail);
        assert_eq!((result.before.is_none(), result.after.is_none(), result.score), (true, true, None), "{label}");
    }
}

#[test]
fn well_formed_expectations_and_pointers_are_not_errors() {
    let valid = [
        query("/counter", "{ equals = 5 }"),
        query("/counter", "{ equals = 5, range = [4.0, 6.0] }"),
        query("/counter", "{ range = [5.0, 5.0] }"),
        query("/counter", "{ change = 'up' }"),
        query("/counter", "{ change = 'up', at_least = 0.0 }"),
        query("/counter", "{ change = 'up', at_least = 0.5, relative = true }"),
        query("/counter", "{ change = 'none' }"),
        query("/counter", "{ equals = 5, change = 'up', at_least = 2.0 }"),
        query("", "{ equals = 1 }"),
        query("/chars/a/name", "{ equals = 'A' }"),
        query("/chars/*/name", "{ equals = 'A' }"),
        query("/chars/0", "{ equals = 'A' }"),
        query("/chars/01", "{ equals = 'A' }"),
        query("/chars/-1", "{ equals = 'A' }"),
        query("/chars/10", "{ equals = 'A' }"),
        query("/chars/a~0b/c~1d", "{ equals = 'A' }"),
        query("/chars/~01", "{ equals = 'A' }"),
        query("/chars//a", "{ equals = 'A' }"),
        query("/chars/a-b/x.y", "{ equals = 'A' }"),
    ];
    for toml in valid {
        let result = &grade(&toml, Vec::new()).results[0];
        assert_ne!(result.outcome, Outcome::Error, "{toml}: {}", result.detail);
    }
}

#[test]
fn a_check_never_runs_an_edit_or_a_job_whatever_scope_it_is_registered_with() {
    for op in ["evaltest.sneaky_edit", "evaltest.render_job"] {
        let query = format!("[[check]]\nid='c'\nkind='query'\nop='{op}'\nat='/passed'\nexpect={{ equals = true }}\n");
        let gate = format!("[[check]]\nid='c'\nkind='gate'\nops=['{op}']\n");
        let binding = format!(
            "[bind]\nx = {{ op='{op}', at='/passed', pick='new' }}\n[[check]]\nid='c'\nkind='log.applied'\nop='{{x}}'\n"
        );
        for toml in [query, gate, binding] {
            let report = grade(&toml, Vec::new());
            let result = &report.results[0];
            assert_eq!(result.outcome, Outcome::Error, "{op}: {}", result.detail);
            assert!(result.detail.contains("not a query"), "{op}: {}", result.detail);
        }
    }
    // The refused edit never ran: the result's counter is still the one the document held.
    let doc = MemDoc::from_value(json!({"counter": 5}));
    let result: Arc<dyn Runner> = Arc::new(MockRunner::new(doc));
    let refused = Checker::new(runner(&json!({"counter": 1})), result.clone(), Transcript::default())
        .grade(&task("[[check]]\nid='c'\nkind='query'\nop='evaltest.sneaky_edit'\nat='/x'\nexpect={ equals = 1 }\n"));
    assert_eq!(refused.results[0].outcome, Outcome::Error);
    let after = Checker::new(runner(&json!({"counter": 1})), result, Transcript::default())
        .grade(&task(&query("/counter", "{ equals = 5 }")));
    assert_eq!(after.results[0].outcome, Outcome::Pass, "the refused edit must not have run");
}

#[test]
fn a_pointer_indexing_an_array_must_be_canonical_but_an_object_key_may_look_like_a_number() {
    let keyed = |at: &str| doc_query(at, "{ equals = 'x' }");
    let doc = json!({"counter": 1, "chars": {"01": {"name": "x"}, "-1": {"name": "x"}}});
    let ok = |at: &str| {
        let report = Checker::new(runner(&doc), runner(&doc), Transcript::default()).grade(&task(&keyed(at)));
        report.results[0].outcome
    };
    assert_eq!(ok("/chars/01/name"), Outcome::Pass);
    assert_eq!(ok("/chars/-1/name"), Outcome::Pass);
    // An array, by contrast, is never indexed by a token that only resembles an index.
    let arrays = json!({"counter": 1, "chars": ["x", "y"]});
    let on_array = |at: &str| {
        Checker::new(runner(&arrays), runner(&arrays), Transcript::default()).grade(&task(&keyed(at))).results[0]
            .outcome
    };
    assert_eq!(on_array("/chars/0"), Outcome::Pass);
    for bad in ["/chars/01", "/chars/+0", "/chars/-1", "/chars/0.0"] {
        assert_eq!(on_array(bad), Outcome::Error, "{bad}");
    }
}
