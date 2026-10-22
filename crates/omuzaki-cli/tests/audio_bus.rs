//! The mixer as document edits, end to end (#1157): `audio.bus.*` ops on the real executor change the project, the
//! follower applies the project's graph to the audio engine, and undo takes the engine back with the document. An
//! agent's edit that would send captured audio somewhere new waits for a person.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::GraphSpec;
use omuzaki_audio_api::ids::BusId;
use omuzaki_audio_mock::MockEngine;
use omuzaki_cli::project::SavedProject;
use omuzaki_core::time::SystemClock;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, ErrorCode, Grants, Limits, Mode, OpError, Request, Runner, ServicesBuilder, SessionSpec, Sessions,
    Transport,
};
use serde_json::{Value, json};

/// How long the follower may take to apply a change before the test calls it lost.
const PATIENCE: Duration = Duration::from_secs(10);

fn rig() -> (Arc<Executor>, Arc<MockEngine>, Receiver<GraphSpec>, tempfile::TempDir) {
    omuzaki_audio_engine::link();
    omuzaki_audio_fx::link();
    omuzaki_audio_mock::link();
    let engine = Arc::new(MockEngine::new());
    let services =
        ServicesBuilder::new().insert::<dyn AudioEngine>(Arc::clone(&engine) as Arc<dyn AudioEngine>).build();
    let dir = common::new_project();
    let doc = SavedProject::open(dir.path()).expect("the project opens");
    let runner = Arc::new(
        Executor::new(Box::new(doc), Arc::new(SystemClock::new()))
            .expect("the executor builds")
            .with_services(services),
    );
    let (sender, applied) = channel();
    omuzaki_audio_engine::follow_reporting(
        Arc::clone(&engine) as Arc<dyn AudioEngine>,
        &(Arc::clone(&runner) as Arc<dyn Runner>),
        Some(sender),
    )
    .expect("the follower starts");
    // It applies the engine's own graph first.
    applied.recv_timeout(PATIENCE).expect("the follower synced at start");
    (runner, engine, applied, dir)
}

fn person() -> Actor {
    Actor::Person { name: "tester".to_owned() }
}

fn run(runner: &Executor, actor: &Actor, op: &str, args: Value) -> Result<Value, OpError> {
    let request = Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request).map(|outcome| outcome.plan.output.unwrap_or(Value::Null))
}

fn bus_ids(graph: &GraphSpec) -> Vec<&str> {
    graph.buses.iter().map(|b| b.id.as_str()).collect()
}

#[test]
fn a_bus_edit_reaches_the_engine_and_undo_takes_it_back() {
    let (runner, engine, applied, _dir) = rig();
    let program = json!({ "bus": "program", "channels": "stereo", "outputs": ["program_out"] });
    run(&runner, &person(), "audio.bus.create", program).expect("a person creates program");
    let graph = applied.recv_timeout(PATIENCE).expect("the follower applied the new graph");
    assert_eq!(bus_ids(&graph), ["program"]);
    assert_eq!(engine.graph(), graph);
    // The -1 dBTP safety limiter is on `program` from the start (#921).
    let kinds: Vec<&str> = graph.buses[0].chain.iter().map(|n| n.kind.as_str()).collect();
    assert_eq!(kinds, ["fx.limiter"]);

    let music = json!({ "bus": "music", "channels": "stereo", "sends": [{ "to": "program" }] });
    run(&runner, &person(), "audio.bus.create", music).expect("a person creates music");
    assert_eq!(bus_ids(&applied.recv_timeout(PATIENCE).expect("applied")), ["program", "music"]);
    run(&runner, &person(), "audio.bus.set_gain", json!({ "bus": "music", "gain_db": -9.0 })).expect("lowered");
    let lowered = applied.recv_timeout(PATIENCE).expect("applied");
    assert_eq!(lowered.bus(&BusId::new("music").expect("an id")).map(|b| b.gain_db), Some(-9.0));

    runner.undo(&person(), &Grants::person()).expect("undo").expect("a step to undo");
    let restored = applied.recv_timeout(PATIENCE).expect("the undo reached the engine");
    assert_eq!(restored.bus(&BusId::new("music").expect("an id")).map(|b| b.gain_db), Some(0.0));
    assert_eq!(engine.graph(), restored);
}

#[test]
fn an_agent_routing_a_microphone_to_the_stream_needs_a_person() {
    let (runner, engine, applied, _dir) = rig();
    let program = json!({ "bus": "program", "channels": "stereo", "outputs": ["program_out"] });
    run(&runner, &person(), "audio.bus.create", program).expect("a person creates program");
    applied.recv_timeout(PATIENCE).expect("applied");
    // Agents work in sessions, on behalf of a person.
    let agent = runner
        .open(SessionSpec {
            transport: Transport::Stdio,
            client: "helper".to_owned(),
            model: None,
            on_behalf_of: person(),
            grants: Grants::person(),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        })
        .expect("the session opens")
        .actor;
    let mic = json!({
        "bus": "mic2", "channels": "mono",
        "sources": [{ "type": "device", "role": "mic" }], "sends": [{ "to": "program" }],
    });
    // The agent's request waits for the person's approval, and runs only if they give it.
    let waiting = run(&runner, &agent, "audio.bus.create", mic).expect_err("an agent can't route the mic out alone");
    assert_eq!(waiting.code, ErrorCode::Waiting);
    let asks = matches!(waiting.detail.as_deref(), Some(omuzaki_ops_api::ErrorDetail::Queued {
        need: omuzaki_ops_api::Need::Person { reason }, ..
    }) if reason.contains("mic to program_out"));
    assert!(asks, "it asks the person why: {waiting:?}");
    assert_eq!(bus_ids(&engine.graph()), ["program"], "the engine never saw it");
}
