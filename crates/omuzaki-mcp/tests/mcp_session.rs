//! End-to-end: a real rmcp client talks to the server over in-memory pipes (the same async-read/write transport stdio
//! uses), against a fake runner holding the ops in `tests/common`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{FakeRunner, FakeRunnerExt, data, fake_runner, text, tool};
use omuzaki_mcp::{McpOptions, McpServer, ServerError};
use omuzaki_ops_api::{
    Actor, ApprovalId, Budget, Cause, CauseKind, Decision, EndReason, Ended, Grants, OpError, OpKind, PauseReason,
    PendingApproval, Resolution, Scope, SessionId, SessionInfo, SessionSpec, SessionState, SessionUndo, Sessions,
    Spend, Transport,
};
use rmcp::ClientLifecycleMode;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
    ReadResourceRequestParams, ResourceUpdatedNotificationParam, SubscribeRequestParams, SubscriptionFilter,
};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use rmcp::{ClientHandler, ClientServiceExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const BUILTINS: [&str; 6] = ["history_log", "history_redo", "history_undo", "tx_begin", "tx_commit", "tx_rollback"];

/// A client that records resource-update notifications.
struct TestClient {
    protocol: ProtocolVersion,
    updates: mpsc::UnboundedSender<String>,
}

impl ClientHandler for TestClient {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1.0"))
            .with_protocol_version(self.protocol.clone())
    }

    async fn on_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
        _context: NotificationContext<RoleClient>,
    ) {
        let _ = self.updates.send(params.uri);
    }
}

/// The two ways an MCP client can start a session.
#[derive(Copy, Clone, Debug)]
enum Lifecycle {
    /// `initialize` handshake (protocol 2025-11-25 and earlier): Claude Code and Codex today.
    Initialize,
    /// `server/discover` plus per-request metadata (protocol 2026-07-28).
    Discover,
}

impl Lifecycle {
    fn protocol(self) -> ProtocolVersion {
        match self {
            Self::Initialize => ProtocolVersion::LATEST_WITH_INITIALIZE,
            Self::Discover => ProtocolVersion::V_2026_07_28,
        }
    }

    fn mode(self) -> ClientLifecycleMode {
        match self {
            Self::Initialize => ClientLifecycleMode::Initialize,
            Self::Discover => ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        }
    }
}

struct Harness {
    runner: Arc<FakeRunner>,
    client: RunningService<RoleClient, TestClient>,
    updates: mpsc::UnboundedReceiver<String>,
    server: JoinHandle<Result<(), ServerError>>,
    /// The same server the client talks to, for awaiting its event log.
    mcp: McpServer,
}

impl Harness {
    /// Connects with the legacy `initialize` handshake (what Claude Code and Codex use today).
    async fn connect() -> Self {
        Self::connect_with(McpOptions::default(), Lifecycle::Initialize).await
    }

    async fn connect_with(options: McpOptions, lifecycle: Lifecycle) -> Self {
        let runner = Arc::new(fake_runner());
        let mcp = McpServer::new(runner.clone(), options).expect("server builds");
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let serving = mcp.clone();
        let server = tokio::spawn(async move { serving.serve(server_io).await });
        let (updates_tx, updates) = mpsc::unbounded_channel();
        let client = TestClient { protocol: lifecycle.protocol(), updates: updates_tx }
            .serve_with_lifecycle(client_io, lifecycle.mode())
            .await
            .expect("client connects");
        Self { runner, client, updates, server, mcp }
    }

    async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let args = args.as_object().expect("args are an object").clone();
        self.client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
    }

    async fn next_update(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(5), self.updates.recv()).await.expect("update in time").expect("open")
    }

    async fn disconnect(mut self) {
        self.client.close().await.expect("closes");
        self.server.await.expect("server task").expect("server ends cleanly");
    }
}

// ---- tool list -----------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_tool_list_matches_the_runner_ops_exactly() {
    let h = Harness::connect().await;
    let tools = h.client.list_all_tools().await.unwrap();

    // Tool names are op names with dots as underscores: Anthropic's rule is ^[a-zA-Z0-9_-]{1,64}$.
    // Ops withheld for a service this harness doesn't wire are not tools (see `WITHHELD_WITHOUT_SERVICE`).
    let mut expected: Vec<String> =
        common::offered_ops(h.runner.as_ref()).iter().map(|o| o.name.replace('.', "_")).collect();
    expected.extend(BUILTINS.map(str::to_owned));
    expected.sort();
    let listed: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(listed, expected, "tools/list must be exactly the offered Runner::ops() plus the built-ins, sorted");
    for name in &listed {
        assert!(
            (1..=64).contains(&name.len()) && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }

    for info in common::offered_ops(h.runner.as_ref()) {
        let t = tool(&tools, &info.name.replace('.', "_"));
        assert_eq!(t.title.as_deref(), Some(info.name), "the dotted op name is the tool's title");
        let op_schema = serde_json::to_value(&info.schema).unwrap();
        // A unit-struct op has no `properties` in its own schema.
        for property in op_schema["properties"].as_object().into_iter().flat_map(|p| p.keys()) {
            assert!(t.input_schema["properties"][property].is_object(), "{} lost property {property}", info.name);
        }
        let dry_run = &t.input_schema["properties"]["dry_run"];
        if info.kind == OpKind::Query {
            assert!(dry_run.is_null(), "{} is read-only: nothing to preview", info.name);
        } else {
            assert_eq!(dry_run["type"], "boolean", "{} has no dry_run", info.name);
        }
        let first_line = format!("{}: {}", info.name, info.summary);
        assert!(t.description.as_deref().unwrap().starts_with(&first_line), "{} description", info.name);
    }
    h.disconnect().await;
}

#[tokio::test]
async fn descriptions_and_annotations_tell_the_agent_what_a_tool_does() {
    let h = Harness::connect().await;
    let tools = h.client.list_all_tools().await.unwrap();

    let edit = tool(&tools, "face_set_blend");
    let description = edit.description.as_deref().unwrap();
    for part in
        ["Undoable with history_undo", "`edit:character`: granted", "dry_run=true", "`render_preview`", "`measure_*`"]
    {
        assert!(description.contains(part), "missing {part:?} in:\n{description}");
    }
    let annotations = edit.annotations.as_ref().unwrap();
    assert_eq!((annotations.read_only_hint, annotations.destructive_hint), (Some(false), Some(false)));
    assert_eq!(edit.input_schema["properties"]["blend"]["maximum"], 1.0);

    let query = tool(&tools, "character_describe_face");
    assert_eq!(query.annotations.as_ref().unwrap().read_only_hint, Some(true));
    assert!(query.input_schema["properties"].get("dry_run").is_none(), "nothing to preview on a query");
    assert!(!query.description.as_deref().unwrap().contains("dry_run"));

    let export = tool(&tools, "export_vrm");
    let export_description = export.description.as_deref().unwrap();
    assert!(export_description.contains("NOT granted") && export_description.contains("Long-running"));
    assert_eq!(export.annotations.as_ref().unwrap().open_world_hint, Some(true));

    let unit = tool(&tools, "project_ping");
    assert_eq!(unit.input_schema["type"], "object");

    let info = h.client.peer_info().unwrap();
    let guide = info.instructions.as_deref().unwrap();
    assert!(guide.contains("dry_run=true") && guide.contains("edit:character") && !guide.contains("publish"));
    let undo = tool(&tools, "history_undo").description.as_deref().unwrap().to_owned();
    assert!(undo.contains("YOUR most recent") && undo.contains("per actor"), "{undo}");
    h.disconnect().await;
}

// ---- calling ops ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn dry_run_previews_apply_changes_and_undo_reverts() {
    let h = Harness::connect().await;

    let preview = h.call("face_set_blend", json!({"character": "mira", "blend": 0.6, "dry_run": true})).await;
    assert_eq!(preview.is_error, Some(false));
    assert_eq!((data(&preview)["mode"].clone(), data(&preview)["applied"].clone()), (json!("dry_run"), json!(false)));
    assert_eq!(data(&preview)["changes"][0]["path"], "characters/mira/face/blend");
    assert!(text(&preview).contains("Nothing was applied") && text(&preview).contains("again without dry_run"));
    assert_eq!(h.runner.blend(), None, "a preview must not change the project");

    let applied = h.call("face_set_blend", json!({"character": "mira", "blend": 0.6})).await;
    assert_eq!(data(&applied)["applied"], true);
    assert_eq!(data(&applied)["log"]["seq"], 1);
    assert!(text(&applied).contains("Applied face_set_blend (log #1") && text(&applied).contains("history_undo"));
    assert!(text(&applied).contains("`render_preview`"), "edits point at how to verify them");
    assert_eq!(h.runner.blend(), Some(json!(0.6_f32)));

    let undone = h.call("history_undo", json!({})).await;
    assert!(text(&undone).starts_with("Undid \"Set how far"), "{}", text(&undone));
    assert_eq!(h.runner.blend(), None);
    let redone = h.call("history_redo", json!({})).await;
    assert_eq!(data(&redone)["done"], true);
    assert_eq!(h.runner.blend(), Some(json!(0.6_f32)));
    let nothing = h.call("history_redo", json!({})).await;
    assert_eq!((nothing.is_error, text(&nothing)), (Some(false), "Nothing to redo.".to_owned()));
    h.disconnect().await;
}

#[tokio::test]
async fn queries_return_their_answer_in_text_and_json() {
    let h = Harness::connect().await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.25})).await;
    let answer = h.call("character_describe_face", json!({"character": "mira"})).await;
    assert_eq!(data(&answer)["output"], json!({"blend": 0.25}));
    assert!(text(&answer).ends_with("Output:\n{\n  \"blend\": 0.25\n}"), "{}", text(&answer));
    assert!(data(&answer).get("mode").is_none() && data(&answer).get("applied").is_none());
    let ping = h.call("project_ping", json!({})).await;
    assert_eq!(data(&ping)["output"], "pong");
    // Queries don't advertise `dry_run`, but agents add it by habit; it changes nothing.
    let habit = h.call("character_describe_face", json!({"character": "mira", "dry_run": true})).await;
    assert_eq!((habit.is_error, data(&habit)["output"].clone()), (Some(false), json!({"blend": 0.25})));
    h.disconnect().await;
}

#[tokio::test]
async fn large_answers_arrive_whole_in_the_text() {
    let h = Harness::connect().await;
    let small = h.call("character_list_parts", json!({"count": 120})).await;
    assert!(text(&small).contains("part-00119") && !text(&small).contains("large"));

    let big = h.call("character_list_parts", json!({"count": 8000})).await;
    let text = text(&big);
    let first = text.lines().next().unwrap();
    assert!(first.contains("The output is large (") && first.ends_with("all of it follows."), "{first}");
    let (_, json_part) = text.split_once("Output:\n").unwrap();
    let parsed: Value = serde_json::from_str(json_part).expect("the text holds the complete JSON answer");
    assert_eq!(parsed["parts"].as_array().unwrap().len(), 8000);
    assert_eq!(parsed, data(&big)["output"]);
    h.disconnect().await;
}

#[tokio::test]
async fn jobs_report_their_cost_when_previewed_and_need_their_scope() {
    let h = Harness::connect().await;
    let preview = h.call("export_vrm", json!({"character": "mira", "dry_run": true})).await;
    assert!(text(&preview).contains("Estimated cost: about 12 s, 2048 MiB of GPU memory."), "{}", text(&preview));
    assert_eq!(data(&preview)["cost"]["gpu_mib"], 2048);

    let denied = h.call("export_vrm", json!({"character": "mira"})).await;
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(data(&denied)["code"], "denied");
    assert!(data(&denied)["hint"].as_str().unwrap().contains("grant the `publish` scope"));
    h.disconnect().await;

    let grants = Grants::agent_default().with(Scope::Publish);
    let granted = Harness::connect_with(McpOptions::with_grants(grants), Lifecycle::Initialize).await;
    let tools = granted.client.list_all_tools().await.unwrap();
    assert!(tool(&tools, "export_vrm").description.as_deref().unwrap().contains("`publish`: granted"));
    let done = granted.call("export_vrm", json!({"character": "mira"})).await;
    assert_eq!(done.is_error, Some(false));
    granted.disconnect().await;
}

#[tokio::test]
async fn without_the_read_scope_every_call_is_refused_and_the_tools_say_so() {
    let grants = Grants::of([Scope::EditCharacter]);
    let h = Harness::connect_with(McpOptions::with_grants(grants), Lifecycle::Initialize).await;
    let tools = h.client.list_all_tools().await.unwrap();
    assert!(tool(&tools, "face_set_blend").description.as_deref().unwrap().contains("missing `read`"));
    assert!(h.client.peer_info().unwrap().instructions.as_deref().unwrap().contains("WARNING: `read` is not granted"));
    let refused = h.call("face_set_blend", json!({"character": "mira", "blend": 0.5, "dry_run": true})).await;
    assert_eq!(data(&refused)["code"], "denied");
    assert!(data(&refused)["hint"].as_str().unwrap().contains("grant the `read` scope"));
    h.disconnect().await;
}

// ---- errors ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn op_errors_carry_code_field_hint_and_the_expected_call() {
    let h = Harness::connect().await;

    let out_of_range = h.call("face_set_blend", json!({"character": "mira", "blend": 2.0})).await;
    assert_eq!(out_of_range.is_error, Some(true));
    let e = data(&out_of_range);
    assert_eq!(
        (&e["ok"], &e["code"], &e["field"], &e["hint"]),
        (&json!(false), &json!("invalid"), &json!("blend"), &json!("use a value from 0 to 1"))
    );
    assert!(e["signature"].as_str().unwrap().starts_with("face_set_blend("));
    assert!(
        text(&out_of_range).contains("Hint: use a value from 0 to 1")
            && text(&out_of_range).contains("Expected: face_set_blend(")
    );

    let missing = h.call("face_set_blend", json!({"character": "nobody", "blend": 0.5})).await;
    assert_eq!(
        (data(&missing)["code"].clone(), data(&missing)["hint"].clone()),
        (json!("not_found"), json!("create it first"))
    );

    // Top-level mistakes name the argument; serde's own errors don't.
    let wrong_type = h.call("face_set_blend", json!({"character": "mira", "blend": "high"})).await;
    assert_eq!(data(&wrong_type)["field"], "blend");
    assert_eq!(data(&wrong_type)["message"], "`blend` must be number[0..1], got string \"high\"");
    let missing_arg = h.call("face_set_blend", json!({"character": "mira"})).await;
    assert_eq!(
        (data(&missing_arg)["field"].clone(), data(&missing_arg)["hint"].clone()),
        (json!("blend"), json!("pass `blend`: number[0..1]"))
    );

    // What the schema can't express (a slug's characters) fails in the op's decoder: the same message points at the
    // expected call instead of the CLI, and the field names the argument at fault.
    let bad_slug = h.call("face_set_blend", json!({"character": "Mira!", "blend": 0.5})).await;
    assert_eq!(data(&bad_slug)["field"], "character");
    let slug_text = text(&bad_slug);
    assert!(
        slug_text.contains("Hint: match the `Expected:` call below") && slug_text.contains("Expected: face_set_blend(")
    );
    assert!(!slug_text.contains("omuzaki help"), "{slug_text}");
    assert_eq!(h.runner.blend(), None, "failed calls change nothing");
    h.disconnect().await;
}

#[tokio::test]
async fn misspelt_arguments_are_rejected_instead_of_silently_applied() {
    let h = Harness::connect().await;
    // serde would drop `dryrun`, so this used to apply for real.
    let typo = h.call("face_set_blend", json!({"character": "mira", "blend": 0.5, "dryrun": true})).await;
    assert_eq!(typo.is_error, Some(true));
    assert_eq!(data(&typo)["field"], "dryrun");
    assert!(data(&typo)["hint"].as_str().unwrap().contains("did you mean `dry_run`?"));
    assert_eq!(h.runner.blend(), None);

    let not_bool = h.call("face_set_blend", json!({"character": "mira", "blend": 0.5, "dry_run": "yes"})).await;
    assert_eq!(data(&not_bool)["field"], "dry_run");
    assert_eq!(h.runner.blend(), None);

    let undo_preview = h.call("history_undo", json!({"dry_run": true})).await;
    assert_eq!(undo_preview.is_error, Some(true));
    assert!(data(&undo_preview)["hint"].as_str().unwrap().contains("takes no arguments"));
    h.disconnect().await;
}

#[tokio::test]
async fn the_dotted_op_name_from_a_description_also_works_and_errors_use_the_tool_name() {
    let h = Harness::connect().await;
    let dotted = h.call("face.set_blend", json!({"character": "mira", "blend": 0.5, "dry_run": true})).await;
    assert_eq!(data(&dotted)["op"], "face_set_blend", "answers use the name the agent sees");
    assert!(text(&dotted).contains("call face_set_blend again without dry_run"));

    // The runner's own message names the op with a dot; the agent reads the tool name.
    let bad = h.call("face_set_blend", json!({"character": "Mira!", "blend": 0.5})).await;
    let bad_text = text(&bad);
    assert!(
        bad_text.contains("arguments for face_set_blend don't match") && !bad_text.contains("face.set_blend"),
        "{bad_text}"
    );
    assert!(bad_text.contains("Expected: face_set_blend("), "{bad_text}");
    assert_eq!(data(&bad)["op"], "face_set_blend");
    h.disconnect().await;
}

#[tokio::test]
async fn unknown_tools_are_tool_errors_with_suggestions() {
    let h = Harness::connect().await;
    let result = h.call("set_blend", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(data(&result)["code"], "not_found");
    assert!(data(&result)["hint"].as_str().unwrap().contains("did you mean `face_set_blend`?"));
    h.disconnect().await;
}

#[tokio::test]
async fn a_panicking_op_is_an_internal_error_and_the_session_survives() {
    let h = Harness::connect().await;
    let crashed = h.call("debug_crash", json!({})).await;
    assert_eq!(crashed.is_error, Some(true));
    assert_eq!(data(&crashed)["code"], "internal");
    assert!(data(&crashed)["hint"].as_str().unwrap().contains("bug in Omuzaki"));
    let after = h.call("project_ping", json!({})).await;
    assert_eq!(data(&after)["output"], "pong", "the session keeps working");
    h.disconnect().await;
}

// ---- transactions ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_transaction_makes_several_edits_one_undo_step() {
    let h = Harness::connect().await;
    let begin = h.call("tx_begin", json!({"label": "oval face"})).await;
    assert_eq!(data(&begin)["label"], "oval face");

    let first = h.call("face_set_blend", json!({"character": "mira", "blend": 0.3})).await;
    assert!(text(&first).contains("open transaction") && text(&first).contains("tx_commit"), "{}", text(&first));
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.7})).await;
    assert!(h.runner.has_open_tx());

    let commit = h.call("tx_commit", json!({})).await;
    assert!(text(&commit).contains("one undo step"));
    assert!(!h.runner.has_open_tx());

    let undone = h.call("history_undo", json!({})).await;
    assert_eq!(data(&undone)["label"], "oval face");
    assert_eq!(h.runner.blend(), None, "both edits undo together");
    h.disconnect().await;
}

#[tokio::test]
async fn every_tool_names_its_kind_in_meta() {
    let h = Harness::connect().await;
    let tools = h.client.list_all_tools().await.unwrap();
    let kind = |name: &str| tool(&tools, name).meta.as_ref().and_then(|m| m.0.get("omuzaki/kind")).cloned();
    assert_eq!(kind("face_set_blend"), Some(json!("edit")));
    assert_eq!(kind("character_describe_face"), Some(json!("query")));
    assert_eq!(kind("export_vrm"), Some(json!("job")));
    for name in BUILTINS {
        assert_eq!(kind(name), Some(json!("builtin")), "{name}");
    }
    h.disconnect().await;
}

#[tokio::test]
async fn a_transaction_stamps_its_cause_on_every_edit_in_it() {
    let h = Harness::connect().await;
    h.call("tx_begin", json!({"label": "oval face", "cause": "make her face more oval"})).await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.3})).await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.7})).await;
    h.call("tx_commit", json!({})).await;

    let cause = Some(Cause { kind: CauseKind::Prompt, text: Some("make her face more oval".to_owned()) });
    let log = h.runner.records();
    assert_eq!(log.len(), 2);
    assert!(log.iter().all(|record| record.cause == cause), "{log:?}");
    h.disconnect().await;
}

#[tokio::test]
async fn edits_without_a_cause_record_none_including_one_after_commit() {
    let h = Harness::connect().await;
    h.call("tx_begin", json!({"label": "oval face"})).await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.3})).await;
    h.call("tx_commit", json!({})).await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.5})).await;

    let none = Some(Cause { kind: CauseKind::Prompt, text: None });
    let log = h.runner.records();
    assert_eq!(log.len(), 2);
    assert!(log.iter().all(|record| record.cause == none), "{log:?}");
    h.disconnect().await;
}

#[tokio::test]
async fn a_blank_cause_counts_as_absent() {
    let h = Harness::connect().await;
    h.call("tx_begin", json!({"label": "oval face", "cause": "   "})).await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.3})).await;
    h.call("tx_commit", json!({})).await;

    let log = h.runner.records();
    assert_eq!(log[0].cause, Some(Cause { kind: CauseKind::Prompt, text: None }));
    h.disconnect().await;
}

#[tokio::test]
async fn a_cause_is_limited_to_200_characters_and_the_error_names_it() {
    let h = Harness::connect().await;
    // Characters, not bytes: each `é` is two bytes, so 200 of them are 400 bytes and still pass.
    let too_long = h.call("tx_begin", json!({"label": "oval face", "cause": "é".repeat(201)})).await;
    assert_eq!(too_long.is_error, Some(true));
    assert_eq!(data(&too_long)["code"], "invalid");
    assert_eq!(data(&too_long)["field"], "cause");
    assert!(text(&too_long).contains("201 characters"), "{}", text(&too_long));
    assert!(!h.runner.has_open_tx(), "a refused begin opens nothing");

    let at_limit = h.call("tx_begin", json!({"label": "oval face", "cause": "é".repeat(200)})).await;
    assert_eq!(at_limit.is_error, Some(false), "{}", text(&at_limit));
    h.disconnect().await;
}

#[tokio::test]
async fn a_job_runs_outside_the_open_transaction_while_edits_still_join_it() {
    let grants = Grants::agent_default().with(Scope::Publish);
    let h = Harness::connect_with(McpOptions::with_grants(grants), Lifecycle::Initialize).await;
    let begun = h.call("tx_begin", json!({"label": "oval face", "cause": "make her face more oval"})).await;
    let tx = data(&begun)["tx"].as_u64().unwrap();
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.3})).await;

    let export = h.call("export_vrm", json!({"character": "mira"})).await;
    assert_eq!(export.is_error, Some(false), "{}", text(&export));
    assert!(h.runner.has_open_tx(), "the job leaves the transaction open");

    let log = h.runner.records();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].tx.0, tx, "the edit joins the transaction");
    assert_ne!(log[1].tx.0, tx, "the job never joins it");
    assert_eq!(log[1].cause, Some(Cause { kind: CauseKind::Prompt, text: None }));
    h.call("tx_commit", json!({})).await;
    h.disconnect().await;
}

/// The event kinds in `omuzaki://log`, oldest first, once at least `at_least` have arrived (events reach the log a
/// moment after the call that caused them). Waiting for the last expected event also catches stray earlier ones,
/// because the log is ordered.
async fn logged_events(h: &Harness, at_least: usize) -> Vec<String> {
    h.mcp.wait_for_log(at_least as u64).await;
    let log = h.client.read_resource(ReadResourceRequestParams::new("omuzaki://log")).await.unwrap();
    let rmcp::model::ResourceContents::TextResourceContents { text, .. } = &log.contents[0] else { panic!("text") };
    let body: Value = serde_json::from_str(text).unwrap();
    body["events"].as_array().unwrap().iter().map(|e| e["event"]["event"].as_str().unwrap().to_owned()).collect()
}

#[tokio::test(start_paused = true)]
async fn a_second_tx_begin_is_refused_without_touching_the_runner() {
    let h = Harness::connect().await;
    let first = h.call("tx_begin", json!({"label": "first"})).await;
    let first_id = data(&first)["tx"].as_u64().unwrap();

    let second = h.call("tx_begin", json!({"label": "second"})).await;
    assert_eq!(second.is_error, Some(true));
    assert_eq!(data(&second)["code"], "conflict");
    assert!(
        text(&second).contains(&format!("transaction {first_id} (\"first\") is already open")),
        "{}",
        text(&second)
    );
    assert!(text(&second).contains("tx_commit") && text(&second).contains("tx_rollback"));

    h.call("tx_commit", json!({})).await;
    assert_eq!(logged_events(&h, 1).await, ["tx_committed"], "no phantom begin/rollback in the log");
    let next = h.call("tx_begin", json!({"label": "third"})).await;
    assert_eq!(data(&next)["tx"].as_u64().unwrap(), first_id + 1, "the refused begin didn't burn a transaction id");
    h.disconnect().await;
}

#[tokio::test(start_paused = true)]
async fn racing_tx_begins_open_exactly_one_transaction() {
    let h = Harness::connect().await;
    let (a, b) = tokio::join!(h.call("tx_begin", json!({"label": "a"})), h.call("tx_begin", json!({"label": "b"})));
    assert_eq!([a.is_error, b.is_error].iter().filter(|e| **e == Some(true)).count(), 1, "one wins, one is refused");
    h.call("tx_rollback", json!({})).await;
    assert_eq!(logged_events(&h, 1).await, ["tx_rolled_back"], "only the winner's transaction ever existed");
    h.disconnect().await;
}

#[tokio::test]
async fn rollback_discards_the_transaction_and_misuse_explains_itself() {
    let h = Harness::connect().await;
    let none_open = h.call("tx_commit", json!({})).await;
    assert_eq!(
        (data(&none_open)["code"].clone(), data(&none_open)["hint"].clone()),
        (json!("not_found"), json!("start one with tx_begin"))
    );
    let no_label = h.call("tx_begin", json!({})).await;
    assert_eq!(data(&no_label)["field"], "label");

    h.call("tx_begin", json!({"label": "try it"})).await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.9})).await;
    assert_eq!(h.runner.blend(), Some(json!(0.9_f32)));
    let rollback = h.call("tx_rollback", json!({})).await;
    assert!(text(&rollback).contains("reverted"));
    assert_eq!(h.runner.blend(), None);

    let after = h.call("face_set_blend", json!({"character": "mira", "blend": 0.1})).await;
    assert!(!text(&after).contains("open transaction"), "the transaction is closed: edits stand alone again");
    h.disconnect().await;
}

#[tokio::test(start_paused = true)]
async fn a_transaction_left_open_is_rolled_back_when_the_agent_disconnects() {
    let h = Harness::connect().await;
    h.call("tx_begin", json!({"label": "abandoned"})).await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.9})).await;
    let runner = h.runner.clone();
    h.disconnect().await;
    // The session is dropped by the service task just after the connection closes.
    for _ in 0..100 {
        if !runner.has_open_tx() {
            break;
        }
        tokio::time::advance(Duration::from_millis(20)).await;
    }
    assert!(!runner.has_open_tx());
    assert_eq!(runner.blend(), None, "the abandoned edits were reverted");
}

// ---- identity and protocol versions -------------------------------------------------------------------------------

/// A registry that refuses every session, and keeps what it was asked for. With `live` set it admits that session
/// instead, which a test can change afterwards (pause it, narrow its grants).
#[derive(Default)]
struct OneSession {
    asked: std::sync::Mutex<Vec<SessionSpec>>,
    live: std::sync::Mutex<Option<SessionInfo>>,
    /// Questions about the live session so far, and after how many of them it reads as paused (see
    /// `a_pause_between_the_check_and_the_begin_leaves_no_transaction_behind`).
    asked_about: std::sync::atomic::AtomicUsize,
    pause_after: std::sync::Mutex<Option<(usize, Actor)>>,
}

impl Sessions for OneSession {
    fn open(&self, spec: SessionSpec) -> Result<SessionInfo, OpError> {
        self.asked.lock().unwrap().push(spec);
        self.live.lock().unwrap().clone().ok_or_else(|| OpError::unsupported("not needed by this test"))
    }
    fn sessions(&self) -> Vec<SessionInfo> {
        Vec::new()
    }
    fn session(&self, _: SessionId) -> Option<SessionInfo> {
        None
    }
    fn session_of(&self, _: &Actor) -> Option<SessionInfo> {
        let n = self.asked_about.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut info = self.live.lock().unwrap().clone();
        if let (Some(info), Some((after, by))) = (info.as_mut(), self.pause_after.lock().unwrap().as_ref())
            && n >= *after
        {
            info.state = SessionState::Paused { by: by.clone(), reason: PauseReason::Person };
        }
        info
    }
    fn pause(&self, _: SessionId, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("no"))
    }
    fn resume(&self, _: SessionId, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("no"))
    }
    fn set_grants(&self, _: SessionId, _: Grants, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("no"))
    }
    fn set_budget(&self, _: SessionId, _: Budget, _: &Actor) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("no"))
    }
    fn charge(&self, _: SessionId, _: Spend) -> Result<SessionInfo, OpError> {
        Err(OpError::unsupported("no"))
    }
    fn end(&self, _: SessionId, _: EndReason) -> Result<Ended, OpError> {
        Err(OpError::unsupported("no"))
    }
    fn undo_session(&self, _: SessionId, _: &Actor) -> Result<SessionUndo, OpError> {
        Err(OpError::unsupported("no"))
    }
    fn approvals(&self) -> Vec<PendingApproval> {
        Vec::new()
    }
    fn resolve(&self, _: ApprovalId, _: Decision, _: &Actor) -> Result<Resolution, OpError> {
        Err(OpError::unsupported("no"))
    }
}

#[tokio::test]
async fn a_connection_asks_the_registry_for_its_session_and_runs_nothing_if_refused() {
    let registry = Arc::new(OneSession::default());
    let alice = Actor::Person { name: "alice".into() };
    let options = McpOptions::default().with_sessions(registry.clone(), alice.clone());
    let h = Harness::connect_with(options, Lifecycle::Initialize).await;
    let result = h.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    // The refusal is kept for the connection: a second call does not ask again, and runs nothing either.
    let again = h.call("face_set_blend", json!({"character": "mira", "blend": 0.5})).await;

    {
        // The registry saw the client, the person and the server's grants, once.
        let specs = registry.asked.lock().unwrap();
        assert_eq!(specs.len(), 1, "one session per connection");
        assert_eq!((specs[0].client.as_str(), &specs[0].on_behalf_of), ("test-client", &alice));
        assert_eq!(specs[0].grants, Grants::agent_default());
    }
    // Without a session the agent has no name that could match a registered one, so nothing reaches the runner.
    assert_eq!((result.is_error, again.is_error), (Some(true), Some(true)));
    assert!(text(&result).contains("not needed by this test"), "{}", text(&result));
    assert_eq!(h.runner.records().len(), 0);
    // Resources are closed too: they are authorised by the server's grants, which a refused session must not borrow.
    let log = h.client.read_resource(ReadResourceRequestParams::new("omuzaki://log")).await;
    assert!(log.is_err(), "the op log was readable without a session");
    assert!(h.client.list_resources(None).await.is_err(), "resources were listed without a session");
    h.disconnect().await;
}

fn admitted_session(alice: &Actor) -> SessionInfo {
    SessionInfo {
        id: SessionId(1),
        actor: Actor::Agent { id: "test-client#1".into(), model: None },
        on_behalf_of: alice.clone(),
        transport: Transport::Stdio,
        client: "test-client".into(),
        model: None,
        parent: None,
        role: None,
        grants: Grants::agent_default(),
        fence: Vec::new(),
        budget: Budget::default(),
        spent: Spend::default(),
        limits: omuzaki_ops_api::Limits::default(),
        state: SessionState::Active,
        open_tx: None,
        pending: Vec::new(),
        steps: 0,
        started_at: omuzaki_core::time::Seconds(0.0),
        last_active_at: omuzaki_core::time::Seconds(0.0),
    }
}

#[tokio::test]
async fn resources_follow_the_sessions_current_state_and_grants() {
    let registry = Arc::new(OneSession::default());
    let alice = Actor::Person { name: "alice".into() };
    *registry.live.lock().unwrap() = Some(admitted_session(&alice));
    let options = McpOptions::default().with_sessions(registry.clone(), alice.clone());
    let h = Harness::connect_with(options, Lifecycle::Initialize).await;
    let log = || h.client.read_resource(ReadResourceRequestParams::new("omuzaki://log"));
    assert!(log().await.is_ok(), "an active session with read reads the log");

    // The person narrows the session below `read`: the connection's next read is refused.
    registry.live.lock().unwrap().as_mut().unwrap().grants = Grants::of([Scope::EditCharacter]);
    assert!(log().await.is_err(), "a revoked read is honoured by the next resource request");
    registry.live.lock().unwrap().as_mut().unwrap().grants = Grants::agent_default();
    assert!(log().await.is_ok());

    // A paused session still reads (design §4.3); one that has ended reads nothing.
    let paused = SessionState::Paused { by: alice, reason: PauseReason::Person };
    registry.live.lock().unwrap().as_mut().unwrap().state = paused;
    assert!(log().await.is_ok(), "the log stays readable while paused");
    assert!(h.client.list_resources(None).await.is_ok());
    let over = SessionState::Ended { reason: omuzaki_ops_api::EndReason::Closed, at: omuzaki_core::time::Seconds(1.0) };
    registry.live.lock().unwrap().as_mut().unwrap().state = over;
    assert!(log().await.is_err(), "an ended session's resources are closed");
    assert!(h.client.list_resources(None).await.is_err());
    h.disconnect().await;
}

/// A harness whose session the test can change, with `alice` as the person.
async fn admitted_harness() -> (Harness, Arc<OneSession>, Actor) {
    let registry = Arc::new(OneSession::default());
    let alice = Actor::Person { name: "alice".into() };
    *registry.live.lock().unwrap() = Some(admitted_session(&alice));
    let options = McpOptions::default().with_sessions(registry.clone(), alice.clone());
    (Harness::connect_with(options, Lifecycle::Initialize).await, registry, alice)
}

#[tokio::test]
async fn the_log_tool_follows_the_sessions_current_grants() {
    let (h, registry, _) = admitted_harness().await;
    assert_ne!(h.call("history_log", json!({})).await.is_error, Some(true));
    // The person narrows the session below `read`.
    registry.live.lock().unwrap().as_mut().unwrap().grants = Grants::of([Scope::EditCharacter]);
    let log = h.call("history_log", json!({})).await;
    assert_eq!(log.is_error, Some(true), "a revoked read closes the log tool too");
    assert!(text(&log).contains("`read` scope"), "{}", text(&log));
    h.disconnect().await;
}

#[tokio::test]
async fn tx_begin_is_refused_while_the_session_is_paused() {
    let (h, registry, alice) = admitted_harness().await;
    registry.live.lock().unwrap().as_mut().unwrap().state =
        SessionState::Paused { by: alice, reason: PauseReason::Person };
    let begin = h.call("tx_begin", json!({"label": "oval face"})).await;
    assert_eq!(begin.is_error, Some(true));
    assert!(text(&begin).contains("paused"), "{}", text(&begin));
    assert!(!h.runner.has_open_tx(), "nothing was reserved with the runner");
    // Resumed, it opens as usual (the registry reports the transaction the runner opened, `TxId(1)`).
    {
        let mut live = registry.live.lock().unwrap();
        let live = live.as_mut().unwrap();
        live.state = SessionState::Active;
        live.open_tx = Some(omuzaki_ops_api::SessionTx { tx: omuzaki_ops_api::TxId(1), label: "oval face".into() });
    }
    assert_ne!(h.call("tx_begin", json!({"label": "oval face"})).await.is_error, Some(true));
    h.disconnect().await;
}

/// `Runner::begin` cannot refuse: a pause that lands after the standing check but before the runner opens the
/// transaction leaves an id that was never opened. The registry (no open transaction for the session) says so, and
/// `tx_begin` reports the pause instead of storing an id every later edit would fail on.
#[tokio::test]
async fn a_pause_between_the_check_and_the_begin_leaves_no_transaction_behind() {
    let (h, registry, alice) = admitted_harness().await;
    // The connect asked once (the session opened at initialization); the next question is the standing check, which
    // still sees it active; every question after that sees the pause.
    let seen = registry.asked_about.load(std::sync::atomic::Ordering::SeqCst);
    *registry.pause_after.lock().unwrap() = Some((seen + 1, alice));
    let begin = h.call("tx_begin", json!({"label": "oval face"})).await;
    assert_eq!(begin.is_error, Some(true));
    assert!(text(&begin).contains("paused"), "{}", text(&begin));
    // Nothing was kept: once resumed, a transaction opens (a second begin is not refused as "already open").
    *registry.pause_after.lock().unwrap() = None;
    registry.live.lock().unwrap().as_mut().unwrap().open_tx =
        Some(omuzaki_ops_api::SessionTx { tx: omuzaki_ops_api::TxId(2), label: "oval face".into() });
    let again = h.call("tx_begin", json!({"label": "oval face"})).await;
    assert_ne!(again.is_error, Some(true), "{}", text(&again));
    h.disconnect().await;
}

#[tokio::test]
async fn a_connection_that_only_initialized_is_already_in_the_registry() {
    let registry = Arc::new(OneSession::default());
    let alice = Actor::Person { name: "alice".into() };
    *registry.live.lock().unwrap() = Some(admitted_session(&alice));
    let options = McpOptions::default().with_sessions(registry.clone(), alice);
    let h = Harness::connect_with(options, Lifecycle::Initialize).await;
    assert_eq!(registry.asked.lock().unwrap().len(), 1, "opened at initialization, before any tool or resource call");
    h.disconnect().await;
}

#[tokio::test]
async fn a_subscription_goes_quiet_when_read_is_revoked() {
    let (mut h, registry, _) = admitted_harness().await;
    #[allow(deprecated)] // the legacy subscribe call is what clients on the `initialize` lifecycle use
    h.client.subscribe(SubscribeRequestParams::new("omuzaki://log")).await.unwrap();
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.5})).await;
    assert_eq!(h.next_update().await, "omuzaki://log");
    registry.live.lock().unwrap().as_mut().unwrap().grants = Grants::of([Scope::EditCharacter]);
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.6})).await;
    let none = tokio::time::timeout(Duration::from_millis(300), h.updates.recv()).await;
    assert!(none.is_err(), "no notification names the document after read was revoked: {none:?}");
    h.disconnect().await;
}

#[tokio::test]
async fn edits_are_attributed_to_the_connected_client() {
    let h = Harness::connect().await;
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.4})).await;
    let record = h.runner.records().remove(0);
    assert_eq!(record.actor, Actor::Agent { id: "test-client#1".into(), model: None });
    assert_eq!(record.cause.unwrap().kind, omuzaki_ops_api::CauseKind::Prompt);
    h.disconnect().await;
}

/// Connects one more client to `server` over in-memory pipes.
async fn attach(server: &McpServer) -> RunningService<RoleClient, TestClient> {
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let server = server.clone();
    tokio::spawn(async move { server.serve(server_io).await });
    let (updates, _) = mpsc::unbounded_channel();
    TestClient { protocol: Lifecycle::Initialize.protocol(), updates }
        .serve_with_lifecycle(client_io, Lifecycle::Initialize.mode())
        .await
        .unwrap()
}

/// Two agents connected to one server (and so one runner).
struct Pair {
    runner: Arc<FakeRunner>,
    first: RunningService<RoleClient, TestClient>,
    second: RunningService<RoleClient, TestClient>,
}

impl Pair {
    async fn connect() -> Self {
        let runner = Arc::new(fake_runner());
        let server = McpServer::new(runner.clone(), McpOptions::default()).unwrap();
        let first = attach(&server).await;
        let second = attach(&server).await;
        Self { runner, first, second }
    }
}

async fn call_on(client: &RunningService<RoleClient, TestClient>, tool: &str, args: Value) -> CallToolResult {
    let args = args.as_object().unwrap().clone();
    client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.unwrap()
}

#[tokio::test]
async fn each_connection_is_its_own_agent_with_its_own_transaction() {
    let pair = Pair::connect().await;
    call_on(&pair.first, "tx_begin", json!({"label": "mine"})).await;
    // The second connection has no transaction, even though the first opened one.
    let other = call_on(&pair.second, "tx_commit", json!({})).await;
    assert_eq!(data(&other)["code"], "not_found");
    call_on(&pair.second, "face_set_blend", json!({"character": "mira", "blend": 0.5})).await;
    let actors: Vec<Actor> = pair.runner.records().into_iter().map(|r| r.actor).collect();
    assert_eq!(actors, [Actor::Agent { id: "test-client#2".into(), model: None }]);
    call_on(&pair.first, "tx_commit", json!({})).await;
}

#[tokio::test]
async fn undo_only_reverts_the_agents_own_changes() {
    let pair = Pair::connect().await;
    call_on(&pair.first, "face_set_blend", json!({"character": "mira", "blend": 0.2})).await;
    call_on(&pair.second, "face_set_blend", json!({"character": "mira", "blend": 0.8})).await;

    // The first agent has its own earlier step to undo, but it conflicts with the second agent's edit.
    let first_undo = call_on(&pair.first, "history_undo", json!({})).await;
    assert_eq!(first_undo.is_error, Some(true), "{}", text(&first_undo));
    assert_eq!(pair.runner.blend(), Some(json!(0.8_f32)), "the other agent's edit is untouched");
    let nothing_left = call_on(&pair.first, "history_undo", json!({})).await;
    // (The second agent's edit may be announced here or in the result before: when the log gets it is not the point.)
    assert!(
        text(&nothing_left).ends_with("Nothing to undo."),
        "a step that can't be undone is dropped, not retried forever: {}",
        text(&nothing_left)
    );

    let second_undo = call_on(&pair.second, "history_undo", json!({})).await;
    assert_eq!(second_undo.is_error, Some(false));
    assert_eq!(pair.runner.blend(), Some(json!(0.2_f32)));
}

#[tokio::test]
async fn calls_work_under_both_protocol_lifecycles() {
    for lifecycle in [Lifecycle::Initialize, Lifecycle::Discover] {
        let h = Harness::connect_with(McpOptions::default(), lifecycle).await;
        assert!(h.client.list_all_tools().await.unwrap().len() > BUILTINS.len(), "tools listed under {lifecycle:?}");
        let result = h.call("face_set_blend", json!({"character": "mira", "blend": 0.2})).await;
        assert_eq!(data(&result)["applied"], true, "apply works under {lifecycle:?}");
        assert_eq!(h.runner.records()[0].actor, Actor::Agent { id: "test-client#1".into(), model: None });
        h.disconnect().await;
    }
}

// ---- events ------------------------------------------------------------------------------------------------------

#[tokio::test]
async fn subscribers_are_told_when_the_op_log_changes() {
    let mut h = Harness::connect().await;
    let resources = h.client.list_all_resources().await.unwrap();
    let uris = resources.iter().map(|r| r.uri.as_str()).collect::<Vec<_>>();
    assert_eq!(uris, ["omuzaki://log", "omuzaki://project", "omuzaki://character/mira"]);

    #[allow(deprecated)] // the legacy subscribe call is exactly what clients on the `initialize` lifecycle use
    h.client.subscribe(SubscribeRequestParams::new("omuzaki://log")).await.unwrap();
    h.call("face_set_blend", json!({"character": "mira", "blend": 0.5})).await;
    assert_eq!(h.next_update().await, "omuzaki://log");

    let log = h.client.read_resource(ReadResourceRequestParams::new("omuzaki://log")).await.unwrap();
    let body: Value = match &log.contents[0] {
        rmcp::model::ResourceContents::TextResourceContents { text, .. } => serde_json::from_str(text).unwrap(),
        other => panic!("expected text, got {other:?}"),
    };
    assert_eq!(body["latest"], 1);
    assert_eq!(body["events"][0]["n"], 1);
    assert_eq!(body["events"][0]["event"]["event"], "op_applied");
    assert_eq!(body["events"][0]["event"]["record"]["op"], "face.set_blend");

    let unknown = h.client.read_resource(ReadResourceRequestParams::new("omuzaki://nope")).await;
    assert!(unknown.is_err());
    h.disconnect().await;
}

#[tokio::test]
async fn the_op_log_needs_the_read_scope_to_read_or_to_be_notified_of() {
    // Only `edit:character`: no `read`.
    let options = McpOptions::with_grants(Grants::of([Scope::EditCharacter]));
    let h = Harness::connect_with(options.clone(), Lifecycle::Initialize).await;
    let read = h.client.read_resource(ReadResourceRequestParams::new("omuzaki://log")).await.unwrap_err();
    assert!(read.to_string().contains("needs the `read` scope"), "{read}");
    #[allow(deprecated)]
    let subscribed = h.client.subscribe(SubscribeRequestParams::new("omuzaki://log")).await;
    assert!(subscribed.unwrap_err().to_string().contains("`read` scope"));
    h.disconnect().await;

    // 2026 clients: the log is left out of what the server agrees to send, and a real event is not forwarded.
    let h = Harness::connect_with(options, Lifecycle::Discover).await;
    let filter = SubscriptionFilter::builder().resource_subscription("omuzaki://log").build();
    let mut subscription = h.client.listen(filter).await.unwrap();
    assert_eq!(subscription.acknowledged().resource_subscriptions, None);
    h.runner.person_sets_blend(0.5);
    let quiet = tokio::time::timeout(Duration::from_millis(300), subscription.next()).await;
    assert!(quiet.is_err(), "no notification reaches a session without `read`");
    subscription.cancel().await.unwrap();
    h.disconnect().await;
}

#[tokio::test]
async fn protocol_2026_clients_listen_for_log_updates() {
    let h = Harness::connect_with(McpOptions::default(), Lifecycle::Discover).await;
    let filter = SubscriptionFilter::builder().resource_subscription("omuzaki://log").build();
    let mut subscription = h.client.listen(filter).await.unwrap();
    assert_eq!(subscription.acknowledged().resource_subscriptions.as_deref(), Some(&["omuzaki://log".to_owned()][..]));

    h.call("face_set_blend", json!({"character": "mira", "blend": 0.5})).await;
    let note = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await.expect("in time").unwrap();
    assert!(note.is_some(), "a notification arrived on the subscription stream");
    subscription.cancel().await.unwrap();
    h.disconnect().await;
}

// ---- what others changed since your last call --------------------------------------------------------------------

/// Agents connected to one server. A test waits for an event to reach the log through the server (events arrive a
/// moment after the op that caused them), without reading the log itself, which would move the position of the agents
/// under test.
struct Room {
    runner: Arc<FakeRunner>,
    agents: Vec<RunningService<RoleClient, TestClient>>,
    server: McpServer,
}

impl Room {
    async fn connect(agents: usize) -> Self {
        let runner = Arc::new(fake_runner());
        let server = McpServer::new(runner.clone(), McpOptions::default()).unwrap();
        let mut connected = Vec::new();
        for _ in 0..agents {
            connected.push(attach(&server).await);
        }
        Self { runner, agents: connected, server }
    }

    /// Waits until the log holds event `n`.
    async fn wait_for_event(&self, n: u64) {
        self.server.wait_for_log(n).await;
    }

    async fn set_blend(&self, agent: usize, blend: f32) -> CallToolResult {
        call_on(&self.agents[agent], "face_set_blend", json!({"character": "mira", "blend": f64::from(blend)})).await
    }

    async fn ping(&self, agent: usize) -> CallToolResult {
        call_on(&self.agents[agent], "project_ping", json!({})).await
    }
}

/// The structured heads-up of a result, if it has one.
fn heads_up(result: &CallToolResult) -> Option<&Value> {
    data(result).get("since_your_last_call")
}

/// Valid blends to cycle through, so repeated edits are real changes.
const BLENDS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];

const PONG: &str = "Ran project_ping (read-only).\nOutput:\n\"pong\"";

#[tokio::test(start_paused = true)]
async fn another_agents_edit_shows_up_in_the_next_result_exactly_once() {
    let room = Room::connect(2).await;
    let idle = room.ping(0).await;
    assert!(heads_up(&idle).is_none(), "an idle project adds nothing");
    assert_eq!(text(&idle), PONG, "and the text is just the result");

    room.set_blend(1, 0.4).await;
    room.wait_for_event(1).await;
    let told = room.ping(0).await;
    let text = text(&told);
    assert!(
        text.starts_with(
            "Heads-up: since your last call, others changed the project (1 change; the log is now at n=1).\n\
             - agent test-client#2 applied face_set_blend (n=1): characters/mira/face/blend\n\
             Re-read what you rely on before editing it."
        ),
        "{text}"
    );
    assert!(text.ends_with(&format!("\n\n{PONG}")), "the result itself follows the heads-up: {text}");
    let report = heads_up(&told).expect("structured copy");
    assert_eq!(
        (report["total"].clone(), report["after_n"].clone(), report["up_to_n"].clone()),
        (json!(1), json!(0), json!(1))
    );
    assert_eq!(report["changes"][0]["actor"], "agent test-client#2");
    assert_eq!(report["changes"][0]["paths"], json!(["characters/mira/face/blend"]));

    let again = room.ping(0).await;
    assert!(heads_up(&again).is_none() && self::text(&again) == PONG, "told once, not again");
    // The editor itself is not told about its own edit.
    assert!(heads_up(&room.ping(1).await).is_none());
}

#[tokio::test(start_paused = true)]
async fn an_agents_own_edits_and_undos_are_never_repeated_back() {
    let room = Room::connect(2).await;
    room.set_blend(0, 0.2).await;
    room.wait_for_event(1).await;
    assert!(heads_up(&room.ping(0).await).is_none(), "own edit");

    call_on(&room.agents[0], "history_undo", json!({})).await;
    room.wait_for_event(2).await;
    assert!(heads_up(&room.ping(0).await).is_none(), "own undo");

    room.set_blend(1, 0.9).await;
    room.wait_for_event(3).await;
    let told = room.ping(0).await;
    let report = heads_up(&told).expect("the other agent's edit");
    assert_eq!((report["total"].clone(), report["after_n"].clone()), (json!(1), json!(2)));
    assert!(!text(&told).contains("test-client#1"), "{}", text(&told));
}

#[tokio::test(start_paused = true)]
async fn the_user_editing_in_the_app_is_reported_as_a_user() {
    let room = Room::connect(1).await;
    room.runner.person_sets_blend(0.5);
    room.wait_for_event(1).await;
    let told = room.ping(0).await;
    assert!(
        text(&told).contains("- user tester applied face_set_blend (n=1): characters/mira/face/blend"),
        "{}",
        text(&told)
    );
}

#[tokio::test(start_paused = true)]
async fn undo_and_rollback_by_others_are_reported_with_the_paths_they_touched() {
    let room = Room::connect(2).await;
    room.set_blend(1, 0.4).await;
    call_on(&room.agents[1], "history_undo", json!({})).await;
    call_on(&room.agents[1], "tx_begin", json!({"label": "oval face"})).await;
    room.set_blend(1, 0.8).await;
    call_on(&room.agents[1], "tx_rollback", json!({})).await;
    room.wait_for_event(4).await;

    let text = text(&room.ping(0).await);
    assert!(text.contains("(4 changes; the log is now at n=4)"), "{text}");
    assert!(
        text.contains("- agent test-client#2 applied face_set_blend x2 (n=1-3): characters/mira/face/blend"),
        "{text}"
    );
    assert!(
        text.contains(
            "- agent test-client#2 undid \"Set how far a character's face follow...\" (n=2): characters/mira/face/blend"
        ),
        "{text}"
    );
    assert!(
        text.contains("- agent test-client#2 rolled back transaction 2 (n=4): characters/mira/face/blend"),
        "{text}"
    );
}

#[tokio::test(start_paused = true)]
async fn failed_calls_carry_the_heads_up_too() {
    let room = Room::connect(2).await;
    room.set_blend(1, 0.4).await;
    room.wait_for_event(1).await;
    let failed = call_on(&room.agents[0], "face_set_blend", json!({"character": "mira", "blend": 7.0})).await;
    assert_eq!(failed.is_error, Some(true));
    let text = text(&failed);
    assert!(text.starts_with("Heads-up:") && text.contains("agent test-client#2 applied face_set_blend"), "{text}");
    assert!(text.contains("face_set_blend failed [invalid] (argument `blend`)"), "the error follows it: {text}");
    assert_eq!(data(&failed)["code"], "invalid");
    assert_eq!(heads_up(&failed).unwrap()["total"], 1);
}

#[tokio::test(start_paused = true)]
async fn reading_the_log_catches_a_session_up() {
    let room = Room::connect(2).await;
    room.set_blend(1, 0.4).await;
    room.wait_for_event(1).await;
    let log = room.agents[0].read_resource(ReadResourceRequestParams::new("omuzaki://log")).await.unwrap();
    assert_eq!(log.contents.len(), 1);
    assert!(heads_up(&room.ping(0).await).is_none(), "it has just read exactly that");
    room.set_blend(1, 0.5).await;
    room.wait_for_event(2).await;
    assert_eq!(heads_up(&room.ping(0).await).unwrap()["after_n"], 1, "and only what is newer is reported");
}

#[tokio::test(start_paused = true)]
async fn history_log_gives_the_log_resource_body_to_clients_that_cannot_read_resources() {
    let h = Harness::connect().await;
    for blend in [0.1, 0.2, 0.3] {
        h.call("face_set_blend", json!({"character": "mira", "blend": blend})).await;
    }
    logged_events(&h, 3).await;
    let resource = h.client.read_resource(ReadResourceRequestParams::new("omuzaki://log")).await.unwrap();
    let rmcp::model::ResourceContents::TextResourceContents { text: body, .. } = &resource.contents[0] else {
        panic!("text")
    };
    assert_eq!(text(&h.call("history_log", json!({})).await), *body, "the same JSON as the resource");

    let after = h.call("history_log", json!({"after_n": 1})).await;
    let events: Vec<u64> = serde_json::from_str::<Value>(&text(&after)).unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["n"].as_u64().unwrap())
        .collect();
    assert_eq!(events, [2, 3], "after_n=1 keeps only the events numbered above 1");
    h.disconnect().await;
}

#[tokio::test]
async fn history_log_needs_the_read_scope() {
    let options = McpOptions::with_grants(Grants::of([Scope::EditCharacter]));
    let h = Harness::connect_with(options, Lifecycle::Initialize).await;
    let refused = h.call("history_log", json!({})).await;
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(data(&refused)["code"], "denied");
    assert!(text(&refused).contains("needs the `read` scope"), "{}", text(&refused));
    h.disconnect().await;
}

#[tokio::test(start_paused = true)]
async fn a_client_that_never_reads_resources_fetches_the_values_behind_a_heads_up_with_history_log() {
    let room = Room::connect(2).await;
    room.set_blend(1, 0.4).await;
    room.wait_for_event(1).await;
    room.set_blend(1, 0.7).await;
    room.wait_for_event(2).await;
    // Agent 0 only makes calls: it has never read a resource, so the heads-up is its only pointer.
    let heads = heads_up(&room.ping(0).await).expect("a heads-up about the other agent's edits").clone();
    assert_eq!(heads["total"], 2);
    let after = heads["after_n"].as_u64().unwrap();
    let values = call_on(&room.agents[0], "history_log", json!({"after_n": after})).await;
    let body: Value = serde_json::from_str(&text(&values)).unwrap();
    let events = body["events"].as_array().unwrap();
    assert_eq!(events.iter().map(|e| e["n"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2]);
    let edit = &events[1];
    assert_eq!(edit["event"]["record"]["op"], "face.set_blend");
    let change = &edit["event"]["record"]["patch"]["changes"][0];
    let tenth = |v: &Value| v.as_f64().map(|v| (v * 10.0).round() / 10.0);
    assert!(change["path"].to_string().contains("blend"), "{change}");
    assert_eq!(tenth(&change["before"]), Some(0.4), "the old value: {change}");
    assert_eq!(tenth(&change["after"]), Some(0.7), "the new value: {change}");
    assert!(heads_up(&room.ping(0).await).is_none(), "reading the log catches the reader up");
}

#[tokio::test(start_paused = true)]
async fn history_log_keeps_the_heads_up_position_so_later_edits_are_still_reported() {
    let room = Room::connect(2).await;
    room.set_blend(1, 0.4).await;
    room.wait_for_event(1).await;
    // A successful history_log is plain text: no heads-up in front of it.
    let log = call_on(&room.agents[0], "history_log", json!({})).await;
    assert!(serde_json::from_str::<Value>(&text(&log)).is_ok(), "the whole text is the log's JSON");
    // The dotted alias resolves to the same tool, so it must get the same plain text.
    let alias = call_on(&room.agents[0], "history.log", json!({})).await;
    assert_eq!(alias.is_error, Some(false), "{}", text(&alias));
    assert_eq!(text(&alias), text(&log), "the alias returns the same text");
    room.set_blend(1, 0.5).await;
    room.wait_for_event(2).await;
    let heads = heads_up(&room.ping(0).await).expect("the later edit is reported").clone();
    assert_eq!(heads["after_n"], 1, "only what came after the log read is new");
    assert_eq!(heads["total"], 1);
}

#[tokio::test(start_paused = true)]
async fn a_session_without_read_is_told_nothing_about_the_project() {
    let runner = Arc::new(fake_runner());
    let server = McpServer::new(runner.clone(), McpOptions::default()).unwrap();
    let blind = attach(&server.with_grants(Grants::of([Scope::EditCharacter])).unwrap()).await;
    runner.person_sets_blend(0.5);
    let watching = Room { runner, agents: vec![], server };
    watching.wait_for_event(1).await;

    let refused = call_on(&blind, "face_set_blend", json!({"character": "mira", "blend": 0.1})).await;
    assert_eq!(data(&refused)["code"], "denied");
    assert!(heads_up(&refused).is_none() && !text(&refused).contains("Heads-up"), "{}", text(&refused));
}

#[tokio::test(start_paused = true)]
async fn a_busy_project_lists_the_newest_lines_and_counts_the_rest() {
    let room = Room::connect(1).await;
    for i in 1..=12 {
        room.runner.sets_blend_as(Actor::Person { name: format!("user{i:02}") }, 0.5);
    }
    room.wait_for_event(12).await;
    let told = room.ping(0).await;
    let text = text(&told);
    assert_eq!(text.lines().filter(|l| l.starts_with("- user")).count(), 10, "{text}");
    assert!(text.contains("- ...and 2 earlier changes (n=1-2) not listed here."), "{text}");
    assert!(text.contains("(12 changes;") && !text.contains("user01") && text.contains("user12"), "{text}");
    let report = heads_up(&told).unwrap();
    assert_eq!((report["not_listed"].clone(), report["changes"].as_array().unwrap().len()), (json!(2), 10));
}

#[tokio::test(start_paused = true)]
async fn a_burst_by_one_actor_is_one_line_and_events_past_the_log_window_are_counted() {
    let room = Room::connect(1).await;
    for i in 0..300_usize {
        room.runner.person_sets_blend(BLENDS[i % BLENDS.len()]);
    }
    room.wait_for_event(300).await;
    let told = room.ping(0).await;
    let text = text(&told);
    assert!(text.contains("- 44 older events have left the log (it keeps the last 256)"), "{text}");
    assert!(
        text.contains("- user tester applied face_set_blend x256 (n=45-300): characters/mira/face/blend"),
        "{text}"
    );
    assert!(text.lines().count() < 12, "bounded however busy the project is: {text}");
    assert_eq!(
        (heads_up(&told).unwrap()["total"].clone(), heads_up(&told).unwrap()["missed"].clone()),
        (json!(256), json!(44))
    );
}

/// What a result says others changed.
fn told_total(result: &CallToolResult) -> u64 {
    heads_up(result).map_or(0, |report| report["total"].as_u64().unwrap())
}

/// Two agents and the user edit in a seeded, shuffled order while the agents look now and then. Each agent hears about
/// every edit by the others exactly once (in whichever result comes next, an edit's own included), and never about
/// its own.
#[tokio::test(start_paused = true)]
async fn interleaved_clients_each_hear_of_the_others_edits_exactly_once() {
    let room = Room::connect(2).await;
    let (mut seed, mut edits, mut heard) = (0x5EED_u64, [0_u64; 3], [0_u64; 2]);
    for step in 0..60_u64 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        let who = usize::try_from((seed >> 33) % 3).unwrap();
        let blend = BLENDS[usize::try_from(step).unwrap() % BLENDS.len()];
        if who < 2 {
            heard[who] += told_total(&room.set_blend(who, blend).await);
        } else {
            room.runner.person_sets_blend(blend);
        }
        edits[who] += 1;
        room.wait_for_event(step + 1).await;
        if (seed >> 40) % 4 == 0 {
            for (agent, heard) in heard.iter_mut().enumerate() {
                let told = room.ping(agent).await;
                *heard += told_total(&told);
                assert!(!text(&told).contains(&format!("test-client#{}", agent + 1)), "own edit repeated back");
            }
        }
    }
    for (agent, heard) in heard.iter_mut().enumerate() {
        *heard += told_total(&room.ping(agent).await);
        assert!(heads_up(&room.ping(agent).await).is_none(), "nothing is told twice");
    }
    let total: u64 = edits.iter().sum();
    assert_eq!(heard[0], total - edits[0], "agent 1 heard of everything but its own edits");
    assert_eq!(heard[1], total - edits[1], "agent 2 heard of everything but its own edits");
}
