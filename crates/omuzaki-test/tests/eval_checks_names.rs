//! The `log.*` checks read a transcript that carries MCP tool names (`evaltest_set_counter`) in the op names a task
//! writes (`evaltest.set_counter`), through the server's own catalog; a name nothing explains is the harness's error.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_mcp::{McpOptions, McpServer};
use omuzaki_ops_api::{Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo};
use omuzaki_ops_mock::MockRunner;
use omuzaki_test::eval::check::{Checker, Outcome};
use omuzaki_test::eval::task::Task;
use omuzaki_test::eval::transcript::{ToolCall, ToolNames, Transcript};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, JsonSchema)]
struct SetCounter {
    value: i64,
}
impl Op for SetCounter {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.set_counter",
            summary: "Test op.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = DocPath::parse("counter").unwrap();
        Ok(Plan::edit(Patch::new().with(path.clone(), ctx.doc.get(&path).cloned(), Some(json!(self.value)))))
    }
}
omuzaki_ops_api::register_op!(SetCounter);

#[derive(Serialize, Deserialize, JsonSchema)]
struct Peek {}
impl Op for Peek {
    fn meta() -> OpMeta {
        OpMeta { name: "evaltest.peek", summary: "Test op.", kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
    }
    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({})))
    }
}
omuzaki_ops_api::register_op!(Peek);

fn runner() -> Arc<dyn Runner> {
    Arc::new(MockRunner::new(MemDoc::from_value(json!({"counter": 1}))))
}

fn task(checks: &str) -> Task {
    let head = "id = 't'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    toml::from_str(&format!("{head}{checks}")).unwrap()
}

fn call(tool: &str, args: &Value, mode: Mode, ok: bool) -> ToolCall {
    let call = ToolCall::new(tool, args.clone(), mode, ok);
    if mode == Mode::Apply && ok { call.with_application() } else { call }
}

/// The outcomes of `checks` over a transcript of MCP tool names, with the catalog of a server over the same ops.
fn outcomes(calls: Vec<ToolCall>, checks: &str, with_catalog: bool) -> Vec<Outcome> {
    let server = McpServer::new(runner(), McpOptions::default()).unwrap();
    let mut checker = Checker::new(runner(), runner(), Transcript::new(calls));
    if with_catalog {
        checker = checker.with_tool_names(ToolNames::of_server(&server));
    }
    checker.grade(&task(checks)).results.iter().map(|r| r.outcome).collect()
}

const APPLIED: &str = "[[check]]\nid='a'\nkind='log.applied'\nop='evaltest.set_counter'\n";
const WITHIN: &str = "[[check]]\nid='w'\nkind='log.within'\nops=['evaltest.set_counter']\n";
const PREVIEWED: &str = "[[check]]\nid='p'\nkind='log.previewed'\n";

#[test]
fn a_transcript_of_tool_names_satisfies_checks_written_in_op_names() {
    let dry = call("evaltest_set_counter", &json!({"value": 4, "dry_run": true}), Mode::DryRun, true);
    let applied = call("evaltest_set_counter", &json!({"value": 4}), Mode::Apply, true);
    let checks = format!("{APPLIED}{WITHIN}{PREVIEWED}");
    assert_eq!(outcomes(vec![dry, applied], &checks, true), [Outcome::Pass; 3]);
}

#[test]
fn within_rejects_an_op_outside_the_list_even_when_named_as_a_tool() {
    let applied = call("evaltest_set_counter", &json!({"value": 4}), Mode::Apply, true);
    let checks = "[[check]]\nid='w'\nkind='log.within'\nops=['evaltest.peek']\n";
    assert_eq!(outcomes(vec![applied], checks, true), [Outcome::Fail]);
}

#[test]
fn previewed_cannot_pass_by_not_recognising_the_edits() {
    // An apply with no dry run before it: with the catalog it is seen as an edit and fails; it never passes vacuously.
    let applied = call("evaltest_set_counter", &json!({"value": 4}), Mode::Apply, true);
    assert_eq!(outcomes(vec![applied], PREVIEWED, true), [Outcome::Fail]);
}

#[test]
fn op_names_in_the_transcript_are_still_read_as_they_are() {
    let applied = call("evaltest.set_counter", &json!({"value": 4}), Mode::Apply, true);
    assert_eq!(outcomes(vec![applied], APPLIED, false), [Outcome::Pass]);
}

#[test]
fn a_tool_name_without_the_catalog_or_a_name_nothing_explains_is_the_harness_s_error() {
    let applied = call("evaltest_set_counter", &json!({"value": 4}), Mode::Apply, true);
    let all = format!("{APPLIED}{WITHIN}{PREVIEWED}[[check]]\nid='e'\nkind='log.max_errors'\nn=3\n");
    assert_eq!(outcomes(vec![applied], &all, false), [Outcome::Error; 4]);
    let stray = call("evaltest_never_heard_of", &json!({}), Mode::Apply, true);
    assert_eq!(outcomes(vec![stray], &all, true), [Outcome::Error; 4]);
}

#[test]
fn built_in_tools_are_known_to_the_catalog() {
    let undo = call("history_undo", &json!({}), Mode::Apply, true);
    let checks = "[[check]]\nid='w'\nkind='log.not_applied'\nop='evaltest.set_counter'\n";
    assert_eq!(outcomes(vec![undo], checks, true), [Outcome::Pass]);
}
