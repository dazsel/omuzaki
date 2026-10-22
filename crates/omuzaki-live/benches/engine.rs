//! The engine's own cost (design D19, #776) through `ShowHarness` on fake time, so the numbers are engine CPU alone
//! (no thread hand-offs, no sleeping). The node kinds are the bench's own, shaped like `chat.command` and `op.run`
//! (which live in `omuzaki-live-nodes`, a crate this one may not depend on), so the cost is the engine's:
//!
//! - **one bonk**: `!bonk` in chat through a command trigger to an op call, and the op's reply coming back (target: the
//!   event step at most 15 µs median);
//! - **chat load**: 100 chat messages a second, one in ten a command, into 20 command graphs, for 60 seconds of fake
//!   time (target: at most 10 ms of engine CPU per second);
//! - **idle**: the same show with no events for 60 seconds of fake time (target: 0 steps).
//!
//! `cargo bench -p omuzaki-live --bench engine`

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use std::time::{Duration, Instant};

use omuzaki_core::time::Seconds;
use omuzaki_live::testing::ShowHarness;
use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, ShowId};
use omuzaki_live_api::node::{Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports};
use omuzaki_live_api::register_node;
use omuzaki_live_api::show::ShowDef;
use omuzaki_live_api::value::{PortType, Val};
use omuzaki_live_mock::fixtures;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Parameters of `bench.command`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommandParams {
    /// The command, without its `!`.
    #[serde(default)]
    name: String,
}

/// A chat command trigger: the first word is `!<name>`; puts the user and the rest on its outputs.
struct Command {
    word: String,
}

impl NodeKind for Command {
    type Params = CommandParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("bench.command", "A chat command.", NodeClass::Trigger, Category::Triggers)
    }
    fn ports(_: &CommandParams) -> Ports {
        Ports::new()
            .value_out("user", PortType::Text, "Who.")
            .value_out("rest", PortType::Text, "The rest.")
            .event_out("fired", "It matched.")
    }
    fn listens(_: &CommandParams) -> Vec<EventKind> {
        vec![EventKind::from_static("chat.message")]
    }
    fn new(params: CommandParams) -> Self {
        Self { word: format!("!{}", params.name) }
    }
}

impl Node for Command {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        let Input::Trigger { event, .. } = input else { return Ok(()) };
        let text = event.payload["text"].as_str().unwrap_or("");
        let (first, rest) = text.split_once(' ').unwrap_or((text, ""));
        if !first.eq_ignore_ascii_case(&self.word) {
            return Ok(());
        }
        let user = event.payload["user"]["name"].as_str().unwrap_or("").to_owned();
        cx.set("user", Val::text(user))?;
        cx.set("rest", Val::text(rest))?;
        cx.fire("fired")
    }
}
register_node!(Command);

/// Parameters of `bench.act`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ActParams {
    /// The op.
    #[serde(default)]
    op: String,
}

/// Calls its op with the user, and fires `done` with the reply.
struct Act {
    op: String,
}

impl NodeKind for Act {
    type Params = ActParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("bench.act", "Calls an op.", NodeClass::Active, Category::Actions)
    }
    fn ports(_: &ActParams) -> Ports {
        Ports::new()
            .event_in("in", "Run.")
            .value_in("user", PortType::Text, Val::text(""), "Who.")
            .event_out("done", "Answered.")
    }
    fn new(params: ActParams) -> Self {
        Self { op: params.op }
    }
}

impl Node for Act {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => {
                let user = cx.input_text("user")?;
                cx.run_op(&self.op, json!({ "user": &*user })).map(|_| ())
            }
            Input::Reply { .. } => cx.fire("done"),
            _ => Ok(()),
        }
    }
}
register_node!(Act);

/// A graph answering `!<name>` with `throw.item`.
fn command(name: &str) -> GraphDef {
    GraphDef::from_node(&json!({ "nodes": {
        "cmd": { "kind": "bench.command", "params": { "name": name } },
        "act": { "kind": "bench.act", "params": { "op": "throw.item" }, "in": { "in": "cmd.fired", "user": "cmd.user" } },
    }}))
    .unwrap()
}

fn harness(commands: usize) -> ShowHarness {
    let def = (0..commands).fold(ShowDef::new("Bench").with_seed(1), |def, i| {
        def.with_graph(GraphId::new(format!("command-{i}")).unwrap(), command(&format!("cmd{i}")))
    });
    let def = def.with_graph(GraphId::new("bonk").unwrap(), command("bonk"));
    let mut h = ShowHarness::new(ShowId::new("bench").unwrap(), def);
    h.respond("throw.item", Seconds(0.0), |_| Ok(Some(json!({}))));
    assert!(h.findings().iter().all(|f| !f.inert), "{:?}", h.findings());
    h
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort_unstable();
    times[times.len() / 2]
}

fn main() {
    omuzaki_live::link();

    // One bonk: the event step (with its request) and, at the next event, the reply's step.
    let mut h = harness(0);
    let runs = 20_000;
    let mut steps = Vec::with_capacity(runs);
    for i in 0..runs {
        let event = fixtures::at(fixtures::chat("kiri", "!bonk"), Seconds(i as f64));
        let started = Instant::now();
        h.publish(event);
        steps.push(started.elapsed());
    }
    assert_eq!(h.requests().len(), runs);
    println!("one bonk (event step plus the last reply's step), {runs} runs: median {:?}", median(steps));

    // Chat load: 100 a second into 20 command graphs, one in ten a command.
    let mut h = harness(20);
    let seconds = 60;
    let events: Vec<_> = (0..seconds * 100)
        .map(|i| {
            let text = if i % 10 == 0 { format!("!cmd{} now", (i / 10) % 20) } else { format!("just chatting {i}") };
            fixtures::at(fixtures::chat(fixtures::NAMES[i % fixtures::NAMES.len()], &text), Seconds(i as f64 / 100.0))
        })
        .collect();
    let started = Instant::now();
    for event in events {
        h.publish(event);
    }
    h.advance(Seconds(1.0));
    let cpu = started.elapsed();
    assert_eq!(h.requests().len(), seconds * 10);
    println!(
        "chat load, 100 a second into 20 command graphs for {seconds} s: {:?} of engine CPU per second",
        cpu / seconds as u32
    );

    // Idle: nothing happens, so nothing runs.
    let mut h = harness(20);
    h.advance(Seconds(60.0));
    println!("idle show, 60 s of fake time: {} steps", h.steps());
    assert_eq!(h.steps(), 0);
}
