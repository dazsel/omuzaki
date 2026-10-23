//! The studio on the app core (#2269): a project the studio opens is the app core's, so a person at a panel and an
//! agent over MCP work on one character, each sees the other's change at once (the agent's in what History reads, the
//! person's in the agent's log), each undoes only their own, and a restart keeps the edits and every undo step.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_doc_api::DocPath;
use omuzaki_mcp::{HttpOptions, InAppSpec};
use omuzaki_ops_api::{Actor, Event, Grants, Mode, Request, Runner, Services};
use omuzaki_ui::wiring::{Core, Document, core_on};
use rmcp::model::{CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation};
use rmcp::service::{RoleClient, RunningService};
use rmcp::{ClientHandler, ClientLifecycleMode, ClientServiceExt};
use serde_json::{Value, json};

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

fn open(dir: &std::path::Path) -> Core {
    omuzaki_ui::link();
    core_on(Document::project(dir).unwrap(), Services::default()).unwrap()
}

/// The head's half width: the value the inspector's slider edits.
fn width_path(id: &CharId) -> DocPath {
    paths::part_params(id, &PartId::new("head").unwrap()).join("cranium").join("half_width")
}

fn width(runner: &dyn Runner, id: &CharId) -> f64 {
    runner.get(&width_path(id), &Grants::person()).unwrap().and_then(|v| v.as_f64()).unwrap()
}

fn set_param(id: &CharId, value: f64) -> Value {
    json!({"character": id.as_str(), "part": "head", "path": "cranium/half_width", "value": value})
}

/// The person's edit, as a panel makes it: through the runner, in the person's name.
fn person_sets(runner: &dyn Runner, id: &CharId, value: f64) {
    runner
        .run(Request {
            op: "part.set_param".into(),
            args: set_param(id, value),
            mode: Mode::Apply,
            actor: you(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        })
        .unwrap();
}

struct Client;

impl ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(ClientCapabilities::default(), Implementation::new("studio-core-test", "1"))
    }
}

async fn call<H: ClientHandler>(client: &RunningService<RoleClient, H>, tool: &str, args: Value) -> CallToolResult {
    let args = args.as_object().unwrap().clone();
    client.call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args)).await.expect("call")
}

fn text(result: &CallToolResult) -> String {
    result.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<Vec<_>>().join("\n")
}

/// Who applied the next op on `events`.
fn next_applied(events: &Receiver<Event>) -> Actor {
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
fn a_person_in_the_studio_and_an_agent_over_mcp_edit_one_character_and_a_restart_keeps_both() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("show");
    let (id, agent_width, person_width) = {
        let core = open(&dir);
        let app = Arc::clone(core.app().expect("a project opens through the app core"));
        let id = omuzaki_ui::studio::open_character(core.runner.as_ref()).unwrap();
        let start = width(core.runner.as_ref(), &id);
        let events = core.runner.subscribe(&Grants::person()).unwrap();
        let link = app.connect_in_app(InAppSpec::assistant(app.person().clone(), None)).unwrap();
        let client = app
            .runtime()
            .block_on(Client.serve_with_lifecycle(link.client_io, ClientLifecycleMode::Initialize))
            .expect("the agent connects");

        // The agent's edit lands in the studio at once: the panels' event stream sees it in the agent's name, and the
        // log History reads holds it.
        let agent_width = start * 1.2;
        let done = app.runtime().block_on(call(&client, "part_set_param", set_param(&id, agent_width)));
        assert_ne!(done.is_error, Some(true), "{}", text(&done));
        assert!(matches!(next_applied(&events), Actor::Agent { .. }));
        assert!((width(core.runner.as_ref(), &id) - agent_width).abs() < 1e-9);
        let log = core.runner.log(0, 100, &Grants::person()).unwrap();
        assert!(
            log.iter().any(|record| record.op == "part.set_param" && matches!(record.actor, Actor::Agent { .. })),
            "History's log holds the agent's step"
        );

        // The person's panel edit reaches the agent: its log has it.
        let person_width = start * 0.9;
        person_sets(core.runner.as_ref(), &id, person_width);
        assert_eq!(next_applied(&events), you());
        let agent_log = app.runtime().block_on(call(&client, "history_log", json!({})));
        assert!(text(&agent_log).contains("you"), "{}", text(&agent_log));

        // Undo is each actor's own: the agent's undo can't take back the person's edit on top of its own.
        let undo = app.runtime().block_on(call(&client, "history_undo", json!({})));
        assert!((width(core.runner.as_ref(), &id) - person_width).abs() < 1e-9, "{}", text(&undo));

        // The port opens for agents outside the studio, and `omuzaki mcp` in the folder finds it.
        let url = core.serve_mcp(HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap()));
        assert!(url.as_deref().is_some_and(|url| url.starts_with("http://127.0.0.1:")), "{url:?}");
        assert!(dir.join(".omuzaki").join("studio.json").exists(), "the port is published for the bridge");

        app.runtime().block_on(client.cancel()).unwrap();
        (id, agent_width, person_width)
    };
    assert!(!dir.join(".omuzaki").join("studio.json").exists(), "closing the studio withdraws the port");

    // Reopened: the person's edit is there, and so is their undo, back to the agent's value.
    let core = open(&dir);
    assert!((width(core.runner.as_ref(), &id) - person_width).abs() < 1e-9, "the edit survives a restart");
    let undone = core.runner.undo(&you(), &Grants::person()).unwrap();
    assert!(undone.is_some(), "the person's undo survives a restart");
    assert!((width(core.runner.as_ref(), &id) - agent_width).abs() < 1e-9);
}

#[test]
fn a_project_open_in_another_studio_is_refused_before_anything_starts() {
    let tmp = tempfile::tempdir().unwrap();
    let first = open(tmp.path());
    let refused = Document::project(tmp.path()).expect_err("the folder is locked");
    assert!(matches!(refused, omuzaki_doc::StoreError::Locked { .. }), "{refused}");
    drop(first);
    assert!(Document::project(tmp.path()).is_ok(), "the lock goes with the studio that held it");
}

#[test]
fn an_in_memory_document_has_no_app_core_and_serves_no_port() {
    omuzaki_ui::link();
    let core = core_on(Document::empty(), Services::default()).unwrap();
    assert!(core.app().is_none());
    assert_eq!(core.serve_mcp(HttpOptions::default()), None);
}

/// A colourway the person installs: one palette step, owned and free to use.
const COLOURWAY: &str = "schema = 1\nid = \"me/colourways/dusk\"\nversion = \"1.0.0\"\nsection = \"colourways\"\n\
    kind = \"setting\"\ntarget = \"character\"\nslot = \"colourway\"\ntitle = \"Dusk\"\nsummary = \"A test colourway.\"\n\
    origin = \"studio test\"\ntags = [\"test\"]\n[rights]\nauthor = \"Omuzaki\"\nlicence = \"CC0-1.0\"\n\
    commercial = \"ok\"\nowned = true\n[[step]]\nop = \"palette.set\"\n[step.args]\ncharacter = \"$character\"\n\
    slot = \"hair_root\"\ncolour = \"#2a2346\"\n";

fn install(runner: &dyn Runner) -> Result<omuzaki_ops_api::Outcome, omuzaki_ops_api::OpError> {
    runner.run(Request {
        op: "library.install".into(),
        args: json!({"manifest": COLOURWAY}),
        mode: Mode::Apply,
        actor: you(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    })
}

#[test]
fn the_studio_s_data_folder_keeps_the_library_items_the_person_installs() {
    omuzaki_ui::link();
    let (tmp, data) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let with =
        core_on(Document::project(&tmp.path().join("a")).unwrap().with_data_dir(data.path()), Services::default())
            .unwrap();
    install(with.runner.as_ref()).unwrap_or_else(|e| panic!("installs with a data folder: {e}"));
    assert!(data.path().join("library").exists(), "the item is kept in the data folder");
    let without = core_on(Document::project(&tmp.path().join("b")).unwrap(), Services::default()).unwrap();
    assert!(install(without.runner.as_ref()).is_err(), "without one there is nowhere to keep it");
}
