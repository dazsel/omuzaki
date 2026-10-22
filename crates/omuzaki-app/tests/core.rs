//! The app core end to end: a person and an agent over MCP edit one document and see each other's changes; the
//! document and every undo step survive a clean restart; a crash loses nothing that was done, and drops what was left
//! unfinished.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;
use std::time::Duration;

use omuzaki_app::{AppCore, AppOptions};
use omuzaki_doc::{Project, StoreError};
use omuzaki_doc_api::{DocPath, DocRead, Patch};
use omuzaki_mcp::InAppSpec;
use omuzaki_ops_api::{Actor, Event, Grants, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Sets the project's note: the one edit these tests need.
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
        let at = note_path();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.note)))))
    }
}
register_op!(SetNote);

fn note_path() -> DocPath {
    DocPath::parse("project/note").unwrap()
}

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

fn options() -> AppOptions {
    AppOptions::studio(person())
}

fn note(core: &AppCore) -> Option<Value> {
    core.get(&note_path())
}

fn note_on_disk(dir: &Path) -> Option<Value> {
    Project::open(dir).unwrap().get(&note_path()).cloned()
}

fn set(core: &AppCore, note: &str) {
    core.run("test.set_note", json!({ "note": note })).unwrap();
}

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("test-client", "1"))
    }
}

async fn call(client: &RunningService<RoleClient, Client>, tool: &str, args: Value) -> CallToolResult {
    let args = args.as_object().unwrap().clone();
    client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
}

fn text(result: &CallToolResult) -> String {
    result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n")
}

/// The next `OpApplied` on `events`, by whom.
fn next_applied(events: &std::sync::mpsc::Receiver<Event>) -> Actor {
    loop {
        let event = events.recv_timeout(Duration::from_secs(10)).expect("an event");
        if let Ok(Value::Object(fields)) = serde_json::to_value(&event)
            && fields.get("event") == Some(&json!("op_applied"))
        {
            return serde_json::from_value(fields["record"]["actor"].clone()).unwrap();
        }
    }
}

#[test]
fn a_person_and_an_agent_edit_one_document_and_each_sees_the_other() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let events = core.subscribe();
    let link = core.connect_in_app(InAppSpec::assistant(person(), None)).unwrap();
    let client = core
        .runtime()
        .block_on(Client.serve_with_lifecycle(link.client_io, ClientLifecycleMode::Initialize))
        .expect("the assistant connects");

    // The agent's edit is applied by the same executor: the UI's event stream sees it at once, in the agent's name.
    let done = core.runtime().block_on(call(&client, "test_set_note", json!({"note": "from the agent"})));
    assert_ne!(done.is_error, Some(true), "{}", text(&done));
    assert!(matches!(next_applied(&events), Actor::Agent { .. }));
    assert_eq!(note(&core), Some(json!("from the agent")));

    // The person's edit reaches the agent: its next result opens with a heads-up naming the person, and its log has it.
    set(&core, "from the person");
    assert_eq!(next_applied(&events), person());
    let preview = core.runtime().block_on(call(&client, "test_set_note", json!({"note": "x", "dry_run": true})));
    assert!(text(&preview).starts_with("Heads-up"), "{}", text(&preview));
    assert!(text(&preview).contains("pat"), "{}", text(&preview));
    let log = core.runtime().block_on(call(&client, "history_log", json!({})));
    assert!(text(&log).contains("from the person"), "{}", text(&log));

    // Undo is per actor: the agent's undo can't take back the person's edit, which is on top of its own.
    let undo = core.runtime().block_on(call(&client, "history_undo", json!({})));
    assert_eq!(note(&core), Some(json!("from the person")), "{}", text(&undo));
    assert_eq!(core.undo().unwrap().as_deref(), Some("Set the project's note."));
    core.runtime().block_on(client.cancel()).unwrap();
    core.close().unwrap();
}

#[test]
fn the_document_and_its_history_survive_a_clean_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(&core, "first");
    set(&core, "second");
    core.close().unwrap();
    assert_eq!(note_on_disk(tmp.path()), Some(json!("second")), "close saves the files");

    let core = AppCore::open(tmp.path(), options()).unwrap();
    assert!(core.recovery().is_empty(), "{:?}", core.recovery());
    assert_eq!(note(&core), Some(json!("second")));
    // Undo reaches across the restart, and so does redo.
    assert!(core.undo().unwrap().is_some());
    assert_eq!(note(&core), Some(json!("first")));
    assert!(core.redo().unwrap().is_some());
    assert_eq!(note(&core), Some(json!("second")));
    core.undo().unwrap();
    core.close().unwrap();
    assert_eq!(note_on_disk(tmp.path()), Some(json!("first")));
}

#[test]
fn a_crash_loses_nothing_done_and_drops_what_was_unfinished() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(&core, "kept");
    // An unfinished transaction: begun, an edit in it, never committed.
    let runner = core.runner();
    let tx = runner.begin("half done", &person());
    let mut request = core.request("test.set_note", json!({"note": "unfinished"}), Mode::Apply);
    request.tx = Some(tx);
    runner.run(request).unwrap();
    drop(runner);
    core.abandon();

    let core = AppCore::open(tmp.path(), options()).unwrap();
    let recovery = core.recovery();
    assert!(recovery.unclean, "{recovery:?}");
    assert_eq!(recovery.replayed, 3, "the op, the begin and the op in the transaction: {recovery:?}");
    assert_eq!(recovery.rolled_back, ["half done"]);
    assert_eq!(note(&core), Some(json!("kept")));
    // The committed edit is still the person's to undo.
    assert!(core.undo().unwrap().is_some());
    assert_eq!(note(&core), None);
    core.close().unwrap();
    assert_eq!(note_on_disk(tmp.path()), None);

    // And a clean open after that has nothing to recover.
    let core = AppCore::open(tmp.path(), options()).unwrap();
    assert!(core.recovery().is_empty(), "{:?}", core.recovery());
    core.close().unwrap();
}

#[test]
fn a_project_open_in_one_core_is_refused_to_another() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let Err(refused) = AppCore::open(tmp.path(), options()) else { panic!("a second core opened the project") };
    assert!(matches!(refused, omuzaki_app::AppError::Store(StoreError::Locked { .. })), "{refused}");
    // The first core is untouched by the refusal.
    set(&core, "still works");
    let runner = core.runner();
    assert_eq!(runner.get(&note_path(), &Grants::person()).unwrap(), Some(json!("still works")));
    drop(runner);
    core.close().unwrap();
}
