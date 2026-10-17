//! `omuzaki mcp` over the real executor and its session registry (#2106; design `agent-sessions.md` §3, §7 and §7.6):
//! the executor and the server wired as the CLI wires them, with the agent working for the person who started it.
//!
//! The server's own tests run against a stand-in registry (`omuzaki-mcp/tests/common/registry.rs`); these pin that the
//! registry the app really uses behaves the same: a disconnect ends the session and abandons its transaction, a request
//! that needs a person queues for the studio's window and is answered through `approval_wait`, and with no window
//! (headless) the same request is refused with the hint that says how to grant it in advance.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a test file

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use omuzaki_cli::cli::try_parse_from;
use omuzaki_cli::session::{Session, open_executor_unshared, open_executor_with};
use omuzaki_doc_api::DocPath;
use omuzaki_mcp::{McpOptions, McpServer, ServerError};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, ApprovalId, Decision, EndReason, Event, Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope,
    SessionState, Sessions, Undo, register_op,
};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

/// How long a test waits for the executor to say something it must say; a hang fails instead of stalling the gate.
const PATIENCE: Duration = Duration::from_secs(30);

/// An op only a person may confirm, as one that locks or publishes the project would be: when an agent asks, the
/// studio queues the request for the person, and a headless host refuses it. No op in the CLI's binary asks for a
/// person yet.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Confirm {}

impl Op for Confirm {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.confirm",
            summary: "Lock the project.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("locking the project is the person's call")?;
        Ok(Plan::answer(json!({ "locked": true })))
    }
}
register_op!(Confirm);

/// An MCP client that only calls tools, as an agent does.
struct Named;

impl ClientHandler for Named {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("claude-code", "1.0"))
    }
}

/// One connected agent.
struct Agent {
    client: RunningService<RoleClient, Named>,
    served: JoinHandle<Result<(), ServerError>>,
}

impl Agent {
    async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let args = args.as_object().expect("args are an object").clone();
        self.client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
    }

    async fn disconnect(mut self) {
        self.client.close().await.expect("closes");
        self.served.await.expect("server task").expect("server ends cleanly");
    }
}

/// The person who started the server: the agent works for them.
fn person() -> Actor {
    Session::person("alice").actor
}

/// A server over `executor` and one agent connected to it, wired as `omuzaki mcp` wires them.
async fn connect(executor: &Arc<Executor>, session: &Session) -> Agent {
    let options = McpOptions::default()
        .with_connections(session.connections.clone())
        .with_sessions(executor.clone(), session.actor.clone());
    let mcp = McpServer::new(executor.clone(), options).expect("the server builds");
    let (server_io, client_io) = tokio::io::duplex(1 << 20);
    let served = tokio::spawn(async move { mcp.serve(server_io).await });
    let client = Named.serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize).await.expect("connects");
    Agent { client, served }
}

/// The project in a fresh folder and the command line that opens it.
fn project() -> (tempfile::TempDir, omuzaki_cli::cli::Cli) {
    let dir = tempfile::tempdir().expect("temp dir");
    omuzaki_doc::Project::create(dir.path(), "demo").expect("create the project");
    let cli = try_parse_from(["omuzaki", "--project", dir.path().to_str().unwrap(), "mcp"]).expect("a command line");
    (dir, cli)
}

fn data(result: &CallToolResult) -> &Value {
    result.structured_content.as_ref().expect("structured content")
}

fn text(result: &CallToolResult) -> String {
    result.content[0].as_text().expect("text content").text.clone()
}

/// Blocks until the executor announces that a session ended, and returns what ending it did.
fn session_ended(events: &Receiver<Event>) -> (omuzaki_ops_api::SessionInfo, omuzaki_ops_api::Ended) {
    loop {
        match events.recv_timeout(PATIENCE).expect("the executor never ended the session") {
            Event::SessionEnded { session, ended, .. } => return (session, ended),
            _ => {}
        }
    }
}

fn characters(executor: &Executor) -> Value {
    let doc = executor.get(&DocPath::root(), &Grants::person()).unwrap().expect("the project has a document");
    doc["characters"].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disconnect_ends_the_session_and_abandons_its_transaction() {
    let (_dir, cli) = project();
    let session = Session::person("alice");
    let executor = open_executor_with(&cli, &session.connections).expect("the project opens");
    let events = executor.subscribe(&Grants::person()).unwrap();
    let agent = connect(&executor, &session).await;

    agent.call("tx_begin", json!({"label": "design"})).await;
    let created =
        agent.call("character_create", json!({"id": "kit", "name": "Kit", "template": "standard", "seed": 7})).await;
    assert_eq!(created.is_error, Some(false), "{}", text(&created));
    let [info] = executor.sessions().try_into().expect("one session");
    assert!(info.open_tx.is_some(), "the transaction is open");
    assert!(characters(&executor).get("kit").is_some(), "the edit is in the document while the transaction is open");

    agent.disconnect().await;
    let (ended_session, ended) = session_ended(&events);
    assert_eq!(ended_session.id, info.id);
    assert!(
        matches!(ended_session.state, SessionState::Ended { reason: EndReason::Disconnected, .. }),
        "{:?}",
        ended_session.state
    );
    assert_eq!(ended.abandoned.len(), 1, "the open transaction was abandoned through the registry: {ended:?}");
    assert!(ended.kept.is_empty(), "{ended:?}");
    assert!(ended_session.open_tx.is_none());
    assert!(characters(&executor).get("kit").is_none(), "the abandoned edit was reverted");
}

#[tokio::test(flavor = "multi_thread")]
async fn in_the_studio_a_request_that_needs_a_person_queues_and_approval_wait_returns_its_answer() {
    let (_dir, cli) = project();
    let session = Session::person("alice");
    // The studio has a window, so an approver: this is the executor `open_executor_with` builds, before it is told
    // there is nobody to ask.
    let executor = Arc::new(open_executor_unshared(&cli, &session.connections).expect("the project opens"));
    let agent = connect(&executor, &session).await;

    let queued = agent.call("test_confirm", json!({})).await;
    assert_eq!(queued.is_error, Some(true));
    assert_eq!(data(&queued)["code"], "waiting");
    assert_eq!(data(&queued)["detail"]["kind"], "queued");
    let words = text(&queued);
    assert!(words.contains("nothing ran") && words.contains("approval_wait"), "{words}");
    let [pending] = executor.approvals().try_into().expect("one approval is queued");
    let id: ApprovalId = pending.id;

    // The person answers in the window.
    executor.resolve(id, Decision::Approve, &person()).expect("the person approves");

    let waited = agent.call("approval_wait", json!({"id": id.0, "timeout_seconds": 10})).await;
    assert_eq!(waited.is_error, Some(false), "{}", text(&waited));
    assert_eq!(data(&waited)["ok"], true, "{}", text(&waited));
    assert_eq!(data(&waited)["approval"], id.0);
    assert_eq!(data(&waited)["output"]["locked"], true, "the approved request ran once: {}", text(&waited));
    assert!(executor.approvals().is_empty(), "the decision resolved the approval");
    agent.disconnect().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_the_same_request_is_refused_with_the_hint_and_nothing_is_queued() {
    let (_dir, cli) = project();
    let session = Session::person("alice");
    let executor = open_executor_with(&cli, &session.connections).expect("the project opens");
    let agent = connect(&executor, &session).await;

    let refused = agent.call("test_confirm", json!({})).await;
    assert_eq!(refused.is_error, Some(true));
    assert_eq!(data(&refused)["code"], "denied", "{}", text(&refused));
    let hint = data(&refused)["hint"].as_str().expect("a hint");
    assert!(hint.contains("Omuzaki Studio") && hint.contains("no one can be asked"), "{hint}");
    assert!(executor.approvals().is_empty(), "nothing is queued with nobody to ask");

    // There is no approval to wait for.
    let waited = agent.call("approval_wait", json!({"id": 1})).await;
    assert_eq!(waited.is_error, Some(true));
    assert_eq!(data(&waited)["code"], "not_found");
    agent.disconnect().await;
}
