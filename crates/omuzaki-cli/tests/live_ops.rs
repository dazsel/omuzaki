//! The show document ops and the queries agents build shows from (#1177 to #1180), on the real executor over a saved
//! project: an agent builds a whole rule with ops alone, each edit is one undo step that restores the show exactly, a
//! drag writes only the position, wiring mistakes are refused with the reason, and the queries say what the engine
//! makes of the show.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

mod common;

use std::sync::Arc;

use omuzaki_cli::project::SavedProject;
use omuzaki_core::time::SystemClock;
use omuzaki_doc_api::DocPath;
use omuzaki_live::Automation;
use omuzaki_live_api::services::AutomationControl;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, ErrorCode, Grants, Limits, Mode, OpError, Request, Runner, ServicesBuilder, SessionSpec, Sessions,
    Transport,
};
use serde_json::{Value, json};

fn rig() -> (Arc<Executor>, Arc<Automation>, tempfile::TempDir) {
    omuzaki_live::link();
    omuzaki_live_nodes::link();
    omuzaki_obs::link();
    let automation = Arc::new(Automation::new());
    let services = ServicesBuilder::new().insert::<Automation>(Arc::clone(&automation)).build();
    let dir = common::new_project();
    let doc = SavedProject::open(dir.path()).expect("the project opens");
    let runner = Arc::new(
        Executor::new(Box::new(doc), Arc::new(SystemClock::new()))
            .expect("the executor builds")
            .with_services(services),
    );
    automation.attach(&(Arc::clone(&runner) as Arc<dyn Runner>));
    // The builder is an agent, and agents need a session.
    let session = runner
        .open(SessionSpec {
            transport: Transport::Stdio,
            client: "builder".to_owned(),
            model: None,
            on_behalf_of: Actor::Person { name: "tester".to_owned() },
            grants: Grants::person(),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        })
        .expect("the session opens");
    assert_eq!(session.actor, agent());
    (runner, automation, dir)
}

fn agent() -> Actor {
    Actor::Agent { id: "builder#1".to_owned(), model: None }
}

fn try_run(runner: &Executor, op: &str, args: Value) -> Result<Value, OpError> {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: agent(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).map(|outcome| outcome.plan.output.unwrap_or(Value::Null))
}

fn run(runner: &Executor, op: &str, args: Value) -> Value {
    try_run(runner, op, args).unwrap_or_else(|e| panic!("{op} failed: {} ({:?})", e.message, e.hint))
}

fn show(runner: &Executor) -> Value {
    let path = DocPath::root().join("shows").join("main");
    runner.get(&path, &Grants::person()).expect("readable").expect("the show exists")
}

/// The redeem-to-scene rule, built node by node as an agent would.
fn build(runner: &Executor) {
    run(runner, "show.create", json!({ "id": "main" }));
    run(
        runner,
        "automation.declare_variable",
        json!({ "name": "bonks", "type": "number", "scope": "show", "default": 0 }),
    );
    run(runner, "automation.add_group", json!({ "id": "intro" }));
    run(runner, "automation.add_graph", json!({ "id": "brb", "graph": { "nodes": {} } }));
    let added = run(
        runner,
        "automation.add_node",
        json!({ "graph": "brb", "kind": "on.event", "params": { "kind": "redeem", "where": { "reward.title": "BRB" } } }),
    );
    assert_eq!(added["node"], "event", "the id is the kind's last word");
    run(
        runner,
        "automation.add_node",
        json!({ "graph": "brb", "kind": "op.run", "id": "scene", "params": { "op": "obs.scene.set", "args": { "scene": "BRB" } } }),
    );
    run(runner, "automation.connect", json!({ "graph": "brb", "from": "event.fired", "to": "scene.in" }));
}

#[test]
fn an_agent_builds_a_rule_with_ops_alone_and_each_edit_undoes_exactly() {
    let (runner, _automation, _dir) = rig();
    build(&runner);
    let built = show(&runner);
    assert_eq!(built["automations"]["brb"]["nodes"]["scene"]["in"]["in"], "event.fired", "{built}");

    // A drag writes the position and nothing else.
    run(&runner, "automation.set_node", json!({ "graph": "brb", "node": "scene", "pos": [200, 40] }));
    let log = runner.log(0, 1_000, &Grants::person()).expect("the log");
    let drag = log.last().expect("the drag is logged");
    let paths: Vec<String> = drag.patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert_eq!(paths, ["shows/main/automations/brb/nodes/scene/pos"]);

    // Each edit is one undo step, and undo restores the show exactly.
    runner.undo(&agent(), &Grants::person()).expect("undo");
    assert_eq!(show(&runner), built);
    run(&runner, "automation.remove_node", json!({ "graph": "brb", "node": "event" }));
    let removed = show(&runner);
    assert!(removed["automations"]["brb"]["nodes"]["scene"].get("in").is_none(), "the wire went with its node");
    runner.undo(&agent(), &Grants::person()).expect("undo");
    assert_eq!(show(&runner), built);
}

#[test]
fn wiring_mistakes_are_refused_with_the_reason() {
    let (runner, _automation, _dir) = rig();
    build(&runner);
    for id in ["m1", "m2", "m3"] {
        run(&runner, "automation.add_node", json!({ "graph": "brb", "kind": "math.op", "id": id }));
    }
    // An event into a value input.
    let refused =
        try_run(&runner, "automation.connect", json!({ "graph": "brb", "from": "event.fired", "to": "m1.a" }))
            .expect_err("an event can't feed a number");
    assert_eq!(refused.code, ErrorCode::Invalid);
    assert!(refused.message.contains("number"), "{}", refused.message);
    // A value cycle.
    run(&runner, "automation.connect", json!({ "graph": "brb", "from": "m1.value", "to": "m2.a" }));
    let cycle = try_run(&runner, "automation.connect", json!({ "graph": "brb", "from": "m2.value", "to": "m1.a" }))
        .expect_err("a value cycle");
    assert!(cycle.message.contains("circle"), "{}", cycle.message);
    // A second source on a value input, unless it replaces the first.
    let taken = try_run(&runner, "automation.connect", json!({ "graph": "brb", "from": "m3.value", "to": "m2.a" }))
        .expect_err("m2.a already has a source");
    assert_eq!(taken.code, ErrorCode::Conflict);
    run(&runner, "automation.connect", json!({ "graph": "brb", "from": "m3.value", "to": "m2.a", "replace": true }));
    assert_eq!(show(&runner)["automations"]["brb"]["nodes"]["m2"]["in"]["a"], "m3.value");
    // A kind that doesn't exist, and parameters the kind refuses.
    let ghost = try_run(&runner, "automation.add_node", json!({ "graph": "brb", "kind": "flow.nope" }))
        .expect_err("no such kind");
    assert_eq!(ghost.hint.as_deref(), Some("automation.node_kinds lists the kinds"));
    try_run(&runner, "automation.add_node", json!({ "graph": "brb", "kind": "math.op", "params": { "op": "sqrtt" } }))
        .expect_err("no such operation");
}

#[test]
fn groups_variables_and_shows_change_and_go() {
    let (runner, automation, _dir) = rig();
    build(&runner);
    run(&runner, "automation.set_graph", json!({ "graph": "brb", "group": "intro", "summary": "Be right back." }));
    assert_eq!(show(&runner)["automations"]["brb"]["graph"]["group"], "intro");
    let gone = run(&runner, "automation.remove_group", json!({ "group": "intro" }));
    assert_eq!(gone["graphs"], json!(["brb"]));
    assert!(show(&runner)["automations"]["brb"]["graph"].get("group").is_none());
    let bad = try_run(
        &runner,
        "automation.declare_variable",
        json!({ "name": "x", "type": "number", "scope": "show", "default": "three" }),
    );
    assert!(bad.is_err(), "a default of the wrong type is refused");
    run(&runner, "automation.remove_variable", json!({ "name": "bonks" }));
    assert!(show(&runner)["show"].get("variables").is_none());

    // A second show; deleting the default moves the default; a running show can't be deleted.
    run(&runner, "show.create", json!({ "id": "backup", "name": "Backup" }));
    run(&runner, "show.rename", json!({ "show": "backup", "name": "Spare" }));
    let listed = run(&runner, "show.list", json!({}));
    assert_eq!(listed["shows"][0]["id"], "backup");
    assert_eq!(listed["shows"][0]["name"], "Spare");
    assert_eq!(listed["shows"][1]["default"], true);
    automation
        .start_def(
            &omuzaki_live_api::ids::ShowId::new("main").expect("an id"),
            &omuzaki_live_api::show::ShowDef::new("Main"),
        )
        .expect("starts");
    let running = try_run(&runner, "show.delete", json!({ "show": "main" })).expect_err("it runs");
    assert_eq!(running.code, ErrorCode::Conflict);
    automation.stop(&agent()).expect("stops");
    let deleted = run(&runner, "show.delete", json!({ "show": "main" }));
    assert_eq!(deleted["default"], "backup");
}

#[test]
fn the_queries_say_what_the_engine_makes_of_the_show() {
    let (runner, _automation, _dir) = rig();
    build(&runner);
    let described = run(&runner, "automation.describe", json!({}));
    let scene = &described["graphs"]["brb"]["nodes"]["scene"];
    assert_eq!(scene["kind"], "op.run");
    assert_eq!(scene["state"]["inert"]["kind"], "not_approved", "{described}");
    assert_eq!(scene["inputs"][0]["name"], "in");
    assert_eq!(described["graphs"]["brb"]["nodes"]["event"]["state"], "armed");
    let needs = described["needs"].as_array().expect("needs");
    assert!(needs.iter().any(|n| n["need"] == json!({ "scope": "broadcast" }) && n["approved"] == false), "{needs:?}");
    let texts = described["findings"].to_string();
    assert!(texts.contains("waits for your approval"), "{texts}");

    let kinds = run(&runner, "automation.node_kinds", json!({ "search": "wait" }));
    let ids: Vec<&str> = kinds["kinds"].as_array().expect("kinds").iter().filter_map(|k| k["id"].as_str()).collect();
    assert!(ids.contains(&"flow.wait"), "{ids:?}");
    let wait = kinds["kinds"].as_array().expect("kinds").iter().find(|k| k["id"] == "flow.wait").expect("flow.wait");
    assert_eq!(wait["outputs"][0]["name"], "out");

    let events = run(&runner, "automation.event_kinds", json!({}));
    assert!(events["kinds"].as_array().expect("kinds").iter().any(|k| k["kind"] == "redeem"));
    let described = run(&runner, "show.describe", json!({}));
    assert_eq!(described["graphs"]["brb"]["nodes"]["scene"], "op.run");
}
