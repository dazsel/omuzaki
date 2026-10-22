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
use omuzaki_mcp::{HttpOptions, InAppSpec};
use omuzaki_ops_api::{
    Actor, ConnectionService, Decision, Event, Grants, Mode, Need, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope,
    ServicesBuilder, Undo, register_op,
};
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

/// A service a host brings, as the studio brings its performer.
trait Greeting: Send + Sync {
    fn greeting(&self) -> String;
}

struct Hello;

impl Greeting for Hello {
    fn greeting(&self) -> String {
        "hello from the host".into()
    }
}

/// Sets the note to the host's greeting, and checks the core's own connections are there beside it.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Greet {}

impl Op for Greet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.greet",
            summary: "Set the project's note to the host's greeting.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.service::<dyn ConnectionService>()?;
        let greeting = ctx.service::<dyn Greeting>()?.greeting();
        let at = note_path();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(greeting)))))
    }
}
register_op!(Greet);

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

async fn call<H: ClientHandler>(client: &RunningService<RoleClient, H>, tool: &str, args: Value) -> CallToolResult {
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

#[test]
fn the_host_s_services_reach_ops_beside_the_core_s_own() {
    let tmp = tempfile::tempdir().unwrap();
    AppCore::create(tmp.path(), "demo", options()).unwrap().close().unwrap();
    let without = AppCore::open(tmp.path(), options()).unwrap();
    let refused = without.run("test.greet", json!({})).unwrap_err();
    assert_eq!(refused.code, omuzaki_ops_api::ErrorCode::Unavailable, "{refused}");
    without.close().unwrap();

    let host = ServicesBuilder::new().insert::<dyn Greeting>(std::sync::Arc::new(Hello)).build();
    let core = AppCore::open(tmp.path(), options().with_services(host)).unwrap();
    core.run("test.greet", json!({})).unwrap();
    assert_eq!(note(&core), Some(json!("hello from the host")));
    core.close().unwrap();
}

/// An MCP client named like a coding agent, as `omuzaki mcp` sees it on stdio.
struct Agent;

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("claude-code", "1"))
    }
}

/// Connects `Agent` through the bridge, as `omuzaki mcp` started by the agent does while the studio has the project.
fn bridge_in(
    core: &AppCore,
    data: &Path,
) -> (RunningService<RoleClient, Agent>, tokio::task::JoinHandle<Result<(), omuzaki_app::AppError>>) {
    let (agent_end, bridge_end) = tokio::io::duplex(1 << 16);
    let (read, write) = tokio::io::split(bridge_end);
    let (dir, data) = (core.dir().to_path_buf(), data.to_path_buf());
    let relay = core
        .runtime()
        .spawn(async move { omuzaki_app::bridge::relay(&dir, &data, tokio::io::BufReader::new(read), write).await });
    let agent = core
        .runtime()
        .block_on(Agent.serve_with_lifecycle(agent_end, ClientLifecycleMode::Initialize))
        .expect("the agent connects through the bridge");
    (agent, relay)
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never happened: {what}");
        std::thread::yield_now();
    }
}

fn port() -> HttpOptions {
    HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap())
}

#[test]
fn an_agent_started_as_omuzaki_mcp_bridges_into_the_studio_after_one_pairing() {
    let (tmp, data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    core.serve_http(port()).unwrap();
    assert!(omuzaki_app::studio::rendezvous(tmp.path()).is_some(), "the studio says where it is");
    let events = core.subscribe();

    // The first time, the agent's session waits on a pairing card in the studio, for this client by name.
    let (agent, relay) = bridge_in(&core, data.path());
    let sessions = core.sessions();
    until("the pairing card", || !sessions.approvals().is_empty());
    let card = sessions.approvals().remove(0);
    assert_eq!(card.need, Need::Pairing { client: "claude-code".into() });
    sessions.resolve(card.id, Decision::Approve, &person()).unwrap();

    // Paired, the agent edits the studio's own document: the person sees it at once, and the agent sees theirs.
    let done = core.runtime().block_on(call(&agent, "test_set_note", json!({"note": "from the bridge"})));
    assert_ne!(done.is_error, Some(true), "{}", text(&done));
    assert!(matches!(next_applied(&events), Actor::Agent { .. }));
    assert_eq!(note(&core), Some(json!("from the bridge")));
    set(&core, "from the person");
    let log = core.runtime().block_on(call(&agent, "history_log", json!({})));
    assert!(text(&log).contains("from the person"), "{}", text(&log));
    core.runtime().block_on(agent.cancel()).unwrap();
    core.runtime().block_on(relay).unwrap().unwrap();
    core.close().unwrap();
    assert!(omuzaki_app::studio::rendezvous(tmp.path()).is_none(), "a closed studio takes no agents");

    // After a restart the client is still paired: no card, and it works at once.
    let core = AppCore::open(tmp.path(), options()).unwrap();
    core.serve_http(port()).unwrap();
    let (agent, relay) = bridge_in(&core, data.path());
    let again = core.runtime().block_on(call(&agent, "test_set_note", json!({"note": "again"})));
    assert_ne!(again.is_error, Some(true), "{}", text(&again));
    assert_eq!(core.sessions().approvals(), []);
    assert_eq!(note(&core), Some(json!("again")));
    core.runtime().block_on(agent.cancel()).unwrap();
    core.runtime().block_on(relay).unwrap().unwrap();
    core.close().unwrap();
}

#[test]
fn without_a_studio_taking_agents_the_bridge_says_what_to_do() {
    let (tmp, data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    let input = tokio::io::BufReader::new(&b"{}\n"[..]);
    let refused =
        core.runtime().block_on(omuzaki_app::bridge::relay(tmp.path(), data.path(), input, tokio::io::sink()));
    let refused = refused.unwrap_err().to_op_error();
    assert_eq!(refused.code, omuzaki_ops_api::ErrorCode::Conflict, "{refused:?}");
    core.close().unwrap();
}

/// Options whose saves wait a minute, so a test sees only the saves it asks for.
fn slow_saves() -> AppOptions {
    options().with_journal(
        omuzaki_doc::journal::JournalOptions::default()
            .with_save_delays(Duration::from_secs(60), Duration::from_secs(60)),
    )
}

#[test]
fn project_save_makes_the_files_current_and_a_dry_run_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    AppCore::create(tmp.path(), "demo", options()).unwrap().close().unwrap();
    let core = AppCore::open(tmp.path(), slow_saves()).unwrap();
    set(&core, "unsaved");
    assert_eq!(note_on_disk(tmp.path()), None, "files trail the document");
    let runner = core.runner();
    runner.run(core.request("project.save", json!({}), Mode::DryRun)).unwrap();
    assert_eq!(note_on_disk(tmp.path()), None, "a dry run saves nothing");
    let saved = core.run("project.save", json!({})).unwrap();
    assert_eq!(saved.plan.output, Some(json!({"saved": true})));
    assert_eq!(note_on_disk(tmp.path()), Some(json!("unsaved")), "the files match when it returns");
    drop(runner);
    core.close().unwrap();
}

#[test]
fn project_reload_takes_an_outside_edit_back_in_as_one_undoable_step_and_keeps_its_text() {
    let tmp = tempfile::tempdir().unwrap();
    AppCore::create(tmp.path(), "demo", options()).unwrap().close().unwrap();
    let core = AppCore::open(tmp.path(), slow_saves()).unwrap();
    set(&core, "ours");
    core.run("project.save", json!({})).unwrap();
    // Someone edits the file in a text editor, with a comment of their own.
    let file = tmp.path().join("project.toml");
    let theirs = std::fs::read_to_string(&file).unwrap().replace("\"ours\"", "\"theirs\" # edited by hand");
    std::fs::write(&file, &theirs).unwrap();

    let reloaded = core.run("project.reload", json!({ "paths": ["project.toml"] })).unwrap();
    assert_eq!(reloaded.plan.output, Some(json!({"reloaded": ["project"]})));
    assert_eq!(note(&core), Some(json!("theirs")));
    core.run("project.save", json!({})).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), theirs, "the person's text is kept, not rewritten");

    // Undo puts the document back, and the next save writes it over the file again.
    core.undo().unwrap();
    assert_eq!(note(&core), Some(json!("ours")));
    core.run("project.save", json!({})).unwrap();
    assert_eq!(note_on_disk(tmp.path()), Some(json!("ours")));
    core.close().unwrap();
}

#[test]
fn a_named_version_is_a_snapshot_whose_ref_exists_when_take_returns() {
    let tmp = tempfile::tempdir().unwrap();
    let core = AppCore::create(tmp.path(), "demo", options()).unwrap();
    set(&core, "before the oval face");
    let snapshots = core.snapshots();
    let id = snapshots.take(omuzaki_history_api::Trigger::Named, Some("before the oval face")).unwrap();
    let listed = snapshots.list().unwrap();
    let named = listed.iter().find(|info| info.id == id).expect("the ref exists when take returns");
    assert_eq!(named.name.as_deref(), Some("before the oval face"));
    assert_eq!(snapshots.tree(&id).unwrap()["project"]["note"], json!("before the oval face"));
    assert!(snapshots.take(omuzaki_history_api::Trigger::Named, None).is_err(), "a named version needs a name");
    let usage = snapshots.usage().unwrap();
    assert!(usage.snapshots.count > 0 && usage.journal.count > 0, "{usage:?}");
    core.close().unwrap();
}
