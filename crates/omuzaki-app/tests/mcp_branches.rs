//! Agents on branches over MCP (`docs/design/branches.md` 8.1 to 8.3), through the app core's own servers: a client
//! makes a branch, moves its session onto it, edits and asks for review; a second client on main never sees those
//! edits; the person's send-back opens the first client's next result; switching the folder is refused to an agent
//! with the way it can move instead; and the tool descriptions say what is undoable.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_history_api::branch::{BranchName, Branches, ReviewDecision, ReviewState};
use omuzaki_mcp::McpServer;
use omuzaki_ops_api::{
    Actor, EndReason, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, SessionState, Undo, register_op,
};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Sets the project's note.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetNote {
    note: String,
}

impl Op for SetNote {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.set_note",
            summary: "Set the project's note.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = DocPath::parse("project/note").unwrap();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.note)))))
    }
}
register_op!(SetNote);

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

struct Client(&'static str);

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new(self.0, "1"))
    }
}

/// A client connected to `server` as `name`, served on the core's runtime.
fn connect(core: &AppCore, server: McpServer, name: &'static str) -> RunningService<RoleClient, Client> {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    core.runtime().spawn(async move { server.serve(server_io).await });
    core.runtime().block_on(Client(name).serve_with_lifecycle(client_io, ClientLifecycleMode::Initialize)).unwrap()
}

fn call(core: &AppCore, client: &RunningService<RoleClient, Client>, tool: &str, args: Value) -> CallToolResult {
    let Value::Object(args) = args else { panic!("{tool}: the arguments are an object") };
    core.runtime()
        .block_on(client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)))
        .expect("the call")
}

fn text(result: &CallToolResult) -> String {
    result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n")
}

fn ok(core: &AppCore, client: &RunningService<RoleClient, Client>, tool: &str, args: Value) -> String {
    let result = call(core, client, tool, args);
    assert_ne!(result.is_error, Some(true), "{tool}: {}", text(&result));
    text(&result)
}

fn note(core: &AppCore, client: &RunningService<RoleClient, Client>) -> String {
    ok(core, client, "doc_get", json!({"path": "project/note"}))
}

#[test]
fn an_agent_works_on_its_own_branch_over_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", AppOptions::studio(person())).unwrap();
    core.run("test.set_note", json!({"note": "base"})).unwrap();
    let designer = connect(&core, core.mcp(), "designer");
    let watcher = connect(&core, core.mcp(), "watcher");

    // The tool list offers the branch built-ins, and says what is undoable.
    let tools = core.runtime().block_on(designer.list_all_tools()).unwrap();
    let described = |name: &str| {
        tools.iter().find(|t| t.name == name).and_then(|t| t.description.clone()).unwrap_or_default().to_string()
    };
    for tool in ["branch_create", "branch_checkpoint", "branch_switch", "branch_work"] {
        assert!(!described(tool).is_empty(), "{tool} is listed");
    }
    assert!(described("branch_create").contains("not an undo step"), "{}", described("branch_create"));
    assert!(described("branch_work").contains("Not an undo step"), "{}", described("branch_work"));
    assert!(ok(&core, &designer, "session_info", json!({})).contains("Branch: main"));

    // Make a branch and move onto it.
    let made = ok(&core, &designer, "branch_create", json!({"name": "designer/oval", "focus": ["project"]}));
    assert!(made.contains("Made branch designer/oval"), "{made}");
    let moved = ok(&core, &designer, "branch_work", json!({"branch": "designer/oval"}));
    assert!(moved.contains("now works on designer/oval"), "{moved}");
    assert!(ok(&core, &designer, "session_info", json!({})).contains("Branch: designer/oval"));
    let sessions = core.sessions().sessions();
    let ended = sessions.iter().find(|s| s.client == "designer" && matches!(s.state, SessionState::Ended { .. }));
    let live = sessions.iter().find(|s| s.client == "designer" && !matches!(s.state, SessionState::Ended { .. }));
    let (ended, live) = (ended.expect("the old session ended"), live.expect("the new one is live"));
    assert!(matches!(&ended.state, SessionState::Ended { reason: EndReason::Closed, .. }), "{:?}", ended.state);
    assert_eq!(live.grants, ended.grants, "the same grants");
    assert!(moved.contains(&format!("Session {} ended", ended.id.0)), "{moved}");

    // Its edits are the branch's: the watcher on main doesn't see them, and the person's version is untouched.
    ok(&core, &designer, "test_set_note", json!({"note": "oval"}));
    assert!(note(&core, &designer).contains("oval"));
    assert!(note(&core, &watcher).contains("base"), "{}", note(&core, &watcher));
    assert_eq!(core.get(&DocPath::parse("project/note").unwrap()), Some(json!("base")));

    // Switching the folder is the person's; the refusal says how to move instead.
    let refused = call(&core, &designer, "branch_switch", json!({"branch": "designer/oval"}));
    assert_eq!(refused.is_error, Some(true));
    assert!(text(&refused).contains("branch_work"), "{}", text(&refused));

    // Ready for review, then sent back: the comment opens the designer's next result.
    let ready =
        ok(&core, &designer, "branch_checkpoint", json!({"label": "oval A", "ready": true, "note": "the chin"}));
    assert!(ready.contains("is ready"), "{ready}");
    let oval = core.branches().by_name(&BranchName::new("designer/oval").unwrap()).unwrap();
    assert_eq!(oval.review.as_ref().map(|r| r.state), Some(ReviewState::Ready));
    core.branches().review(&oval.id, ReviewDecision::send_back("rounder, please"), &person()).unwrap();
    let next = note(&core, &designer);
    assert!(next.contains("Review: the person sent back designer/oval") && next.contains("rounder, please"), "{next}");
    assert!(!note(&core, &designer).contains("Review:"), "each outcome is told once");
    assert!(!note(&core, &watcher).contains("Review:"), "only the branch's sessions are told");

    // Back to main: the person's version, and the watcher's edits reach it there.
    ok(&core, &designer, "branch_work", json!({"branch": "main"}));
    assert!(note(&core, &designer).contains("base"));
    drop((designer, watcher));
    core.close().unwrap();
}
