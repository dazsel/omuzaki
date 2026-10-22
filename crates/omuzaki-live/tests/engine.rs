//! The engine through its service, with test node kinds and a fake runner: an event runs the wired action with its
//! own values, a reply resumes the token that asked, the loop guard suspends a runaway graph, a panic faults one node,
//! redeliveries are dropped, a test event acts as its caller, and stop and start are clean.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_live::Automation;
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError, Source};
use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, ShowId};
use omuzaki_live_api::node::{Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports};
use omuzaki_live_api::register_node;
use omuzaki_live_api::services::{AutomationControl, InertReason, SuspendReason};
use omuzaki_live_api::show::ShowDef;
use omuzaki_live_api::value::{PortType, Val};
use omuzaki_live_mock::fixtures;
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Plan, Request, Runner, TxId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ── Test node kinds ────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `test.on`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OnParams {
    /// The kind.
    #[serde(default)]
    kind: String,
}

/// A trigger that puts the user's name (or a custom event's `who`) on `who`.
struct On;

impl NodeKind for On {
    type Params = OnParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.on", "A trigger.", NodeClass::Trigger, Category::Triggers)
    }
    fn ports(_: &OnParams) -> Ports {
        Ports::new().value_out("who", PortType::Text, "Who.").event_out("fired", "It fired.")
    }
    fn listens(params: &OnParams) -> Vec<EventKind> {
        vec![EventKind::new(params.kind.as_str()).unwrap()]
    }
    fn new(_: OnParams) -> Self {
        Self
    }
}

impl Node for On {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if let Input::Trigger { event, .. } = input {
            let p = &event.payload;
            let who = p["user"]["name"].as_str().or_else(|| p["who"].as_str()).unwrap_or("").to_owned();
            cx.set("who", who)?;
            cx.fire("fired")?;
        }
        Ok(())
    }
}
register_node!(On);

/// Parameters of `test.call`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CallParams {
    /// The op.
    #[serde(default)]
    op: String,
}

/// Runs its op with `value`, then fires `done` or `failed`.
struct Call {
    op: String,
}

impl NodeKind for Call {
    type Params = CallParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.call", "Calls an op.", NodeClass::Active, Category::Actions)
    }
    fn ports(_: &CallParams) -> Ports {
        Ports::new()
            .event_in("in", "Run.")
            .value_in("value", PortType::Text, Val::text(""), "The value.")
            .event_out("done", "Done.")
            .event_out("failed", "Failed.")
            .value_out("output", PortType::Json, "The answer.")
            .value_out("error", PortType::Text, "Why.")
    }
    fn new(params: CallParams) -> Self {
        Self { op: params.op }
    }
}

impl Node for Call {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => {
                let value = cx.input_text("value")?;
                cx.run_op(&self.op, json!({ "value": &*value }))?;
                Ok(())
            }
            Input::Reply { outcome, .. } => match outcome {
                Ok(output) => {
                    cx.set("output", Val::from(output.unwrap_or(Value::Null)))?;
                    cx.fire("done")
                }
                Err(e) => cx.fail("failed", omuzaki_live_api::l10n::internal(e.message)),
            },
            _ => Ok(()),
        }
    }
}
register_node!(Call);

/// Parameters with nothing in them.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

/// Fires `out` on every `in`: wired to itself, a runaway loop.
struct Echo;

impl NodeKind for Echo {
    type Params = NoParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.echo", "Passes the pulse on.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &NoParams) -> Ports {
        Ports::new().event_in("in", "In.").event_out("out", "Out.")
    }
    fn new(_: NoParams) -> Self {
        Self
    }
}

impl Node for Echo {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => cx.fire("out"),
            _ => Ok(()),
        }
    }
}
register_node!(Echo);

/// Waits a tenth of a second, then fires `out`.
struct Later;

impl NodeKind for Later {
    type Params = NoParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.later", "Waits.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &NoParams) -> Ports {
        Ports::new().event_in("in", "In.").event_out("out", "Out.")
    }
    fn new(_: NoParams) -> Self {
        Self
    }
}

impl Node for Later {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => cx.wake_after(Seconds(0.1)).map(|_| ()),
            Input::Wake { .. } => cx.fire("out"),
            _ => Ok(()),
        }
    }
}
register_node!(Later);

/// Panics on every pulse.
struct Boom;

impl NodeKind for Boom {
    type Params = NoParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.boom", "Panics.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &NoParams) -> Ports {
        Ports::new().event_in("in", "In.")
    }
    fn new(_: NoParams) -> Self {
        Self
    }
}

impl Node for Boom {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => panic!("a node that panics"),
            _ => Ok(()),
        }
    }
}
register_node!(Boom);

// ── A fake runner ──────────────────────────────────────────────────────────────────────────────────────────────

/// Records every request and answers `{echo: value}`; `test.fail` fails; a value of `hold` waits until released.
#[derive(Default)]
struct FakeRunner {
    requests: Mutex<Vec<Request>>,
    held: Mutex<bool>,
    release: Condvar,
}

impl FakeRunner {
    fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.release.notify_all();
    }
}

impl Runner for FakeRunner {
    fn ops(&self) -> Vec<OpInfo> {
        Vec::new()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.requests.lock().unwrap().push(req.clone());
        let held = self.held.lock().unwrap();
        drop(self.release.wait_while(held, |held| *held).unwrap());
        if req.op == "test.fail" {
            return Err(OpError::conflict("refused"));
        }
        Ok(Outcome::new(Plan::answer(json!({ "echo": req.args["value"] }))))
    }
    fn begin(&self, _: &str, _: &Actor) -> TxId {
        TxId(0)
    }
    fn commit(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn rollback(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn subscribe(&self, _: &Grants) -> Result<Receiver<Event>, OpError> {
        Err(OpError::unsupported("no events"))
    }
    fn get(&self, _: &DocPath, _: &Grants) -> Result<Option<Value>, OpError> {
        Ok(None)
    }
    fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────────────────────

fn graph(nodes: &Value) -> GraphDef {
    GraphDef::from_node(&json!({ "nodes": nodes })).unwrap()
}

fn show(graphs: &[(&str, Value)]) -> ShowDef {
    graphs.iter().fold(ShowDef::new("Test").with_seed(7), |def, (id, nodes)| {
        def.with_graph(GraphId::new(*id).unwrap(), graph(nodes))
    })
}

/// An engine with the fake runner attached; the runner `Arc` is kept alive by the caller.
fn engine() -> (Automation, Arc<FakeRunner>, Arc<dyn Runner>) {
    let fake = Arc::new(FakeRunner::default());
    let runner: Arc<dyn Runner> = fake.clone();
    let automation = Automation::new();
    automation.attach(&runner);
    (automation, fake, runner)
}

/// Waits for `cond`, checking every millisecond, for at most five seconds.
fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn redeem_and_call() -> Value {
    json!({
        "on": { "kind": "test.on", "params": { "kind": "redeem" } },
        "call": { "kind": "test.call", "params": { "op": "test.op" }, "in": { "in": "on.fired", "value": "on.who" } },
        "again": { "kind": "test.call", "params": { "op": "test.again" }, "in": { "in": "call.done", "value": "on.who" } }
    })
}

fn id(s: &str) -> ShowId {
    ShowId::new(s).unwrap()
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_redeem_runs_the_wired_action_with_its_own_values_in_the_shows_name() {
    let (automation, fake, _runner) = engine();
    automation.start_def(&id("main"), &show(&[("bonk", redeem_and_call())])).unwrap();
    automation.publish(fixtures::redeem("Kiri", "Bonk")).unwrap();
    until("both calls", || fake.requests().len() == 2);
    let requests = fake.requests();
    assert_eq!((requests[0].op.as_str(), requests[1].op.as_str()), ("test.op", "test.again"));
    assert_eq!(requests[0].args, json!({ "value": "Kiri" }));
    assert_eq!(requests[0].actor, Actor::System);
    let cause = requests[0].cause.as_ref().unwrap().text.clone().unwrap();
    assert!(cause.starts_with("main/bonk/call on redeem #"), "{cause}");
    assert!(!cause.contains("Kiri"), "the cause never carries what a viewer wrote or who they are");
    automation.stop(&Actor::System).unwrap();
}

#[test]
fn a_reply_resumes_the_token_that_asked_so_each_event_keeps_its_own_values() {
    let (automation, fake, _runner) = engine();
    automation.start_def(&id("main"), &show(&[("bonk", redeem_and_call())])).unwrap();
    fake.hold();
    automation.publish(fixtures::redeem("Kiri", "Bonk")).unwrap();
    automation.publish(fixtures::redeem("Mori", "Bonk")).unwrap();
    // Both events have run their triggers (the second one latched `Mori` on `on.who`) before any reply comes back.
    until("both events", || automation.values().is_some_and(|v| v.events.len() >= 2));
    fake.release();
    until("four calls", || fake.requests().len() == 4);
    let again: Vec<Value> =
        fake.requests().into_iter().filter(|r| r.op == "test.again").map(|r| r.args["value"].clone()).collect();
    assert_eq!(again, [json!("Kiri"), json!("Mori")], "each reply reads its own token, not the last value");
    automation.stop(&Actor::System).unwrap();
}

#[test]
fn a_runaway_loop_suspends_its_graph_and_the_other_graphs_keep_running() {
    let (automation, fake, _runner) = engine();
    let looping = json!({
        "on": { "kind": "test.on", "params": { "kind": "custom.spin" } },
        "echo": { "kind": "test.echo", "in": { "in": ["on.fired", "echo.out"] } }
    });
    automation.start_def(&id("main"), &show(&[("spin", looping), ("bonk", redeem_and_call())])).unwrap();
    automation.publish(fixtures::custom("spin", json!({}))).unwrap();
    until("the suspension", || automation.status().graphs.iter().any(|g| g.suspended.is_some()));
    let status = automation.status();
    let spin = status.graphs.iter().find(|g| g.graph.as_str() == "spin").unwrap();
    assert!(matches!(spin.suspended.as_ref().unwrap().reason, SuspendReason::LoopGuard { .. }));
    assert!(!spin.active);
    automation.publish(fixtures::redeem("Kiri", "Bonk")).unwrap();
    until("the bonk calls", || fake.requests().len() == 2);
    automation.stop(&Actor::System).unwrap();
}

#[test]
fn a_panicking_node_is_faulted_and_an_unknown_kind_is_inert() {
    let (automation, fake, _runner) = engine();
    let broken = json!({
        "on": { "kind": "test.on", "params": { "kind": "redeem" } },
        "boom": { "kind": "test.boom", "in": { "in": "on.fired" } },
        "ghost": { "kind": "test.ghost" }
    });
    automation.start_def(&id("main"), &show(&[("broken", broken), ("bonk", redeem_and_call())])).unwrap();
    let inert = automation.status().graphs.into_iter().find(|g| g.graph.as_str() == "broken").unwrap().inert;
    assert_eq!(inert.len(), 1);
    assert_eq!((inert[0].node.as_str(), &inert[0].why), ("ghost", &InertReason::UnknownKind));
    automation.publish(fixtures::redeem("Kiri", "Bonk")).unwrap();
    until("the fault", || automation.status().graphs.iter().any(|g| !g.faulted.is_empty()));
    until("the bonk calls", || fake.requests().len() == 2);
    automation.stop(&Actor::System).unwrap();
}

#[test]
fn a_wake_fires_later_in_the_same_token() {
    let (automation, fake, _runner) = engine();
    let later = json!({
        "on": { "kind": "test.on", "params": { "kind": "redeem" } },
        "later": { "kind": "test.later", "in": { "in": "on.fired" } },
        "call": { "kind": "test.call", "params": { "op": "test.op" }, "in": { "in": "later.out", "value": "on.who" } }
    });
    automation.start_def(&id("main"), &show(&[("later", later)])).unwrap();
    let sent = Instant::now();
    automation.publish(fixtures::redeem("Kiri", "Bonk")).unwrap();
    until("the call", || fake.requests().len() == 1);
    assert!(sent.elapsed() >= Duration::from_millis(100), "the call waits for the wake");
    assert_eq!(fake.requests()[0].args, json!({ "value": "Kiri" }));
    automation.stop(&Actor::System).unwrap();
}

#[test]
fn a_platform_redelivery_runs_once() {
    let (automation, fake, _runner) = engine();
    automation.start_def(&id("main"), &show(&[("bonk", redeem_and_call())])).unwrap();
    let event = fixtures::redeem("Kiri", "Bonk");
    assert!(matches!(event.source, Source::Platform { .. }) && event.origin_id.is_some());
    automation.publish(event.clone()).unwrap();
    automation.publish(event).unwrap();
    automation.publish(fixtures::redeem("Mori", "Bonk")).unwrap();
    until("the calls", || fake.requests().len() == 4);
    let users: Vec<Value> =
        fake.requests().into_iter().filter(|r| r.op == "test.op").map(|r| r.args["value"].clone()).collect();
    assert_eq!(users, [json!("Kiri"), json!("Mori")]);
    automation.stop(&Actor::System).unwrap();
}

#[test]
fn publish_refuses_manual_and_acting_events_and_unknown_kinds() {
    let automation = Automation::new();
    let person = Actor::Person { name: "Kiri".into() };
    let manual =
        LiveEvent::new(EventKind::new("custom.x").unwrap(), Source::manual(person.clone()), Seconds(0.0), json!({}));
    assert_eq!(automation.publish(manual), Err(PublishError::ManualSource));
    let acting = fixtures::custom("x", json!({})).with_acting(person);
    assert_eq!(automation.publish(acting), Err(PublishError::Acting));
    let unknown = LiveEvent::new(EventKind::new("bonk.storm").unwrap(), Source::Test, Seconds(0.0), json!({}));
    assert_eq!(automation.publish(unknown), Err(PublishError::UnknownKind));
}

#[test]
fn a_test_event_runs_its_actions_as_the_caller() {
    let (automation, fake, _runner) = engine();
    let custom = json!({
        "on": { "kind": "test.on", "params": { "kind": "custom.ping" } },
        "call": { "kind": "test.call", "params": { "op": "test.op" }, "in": { "in": "on.fired", "value": "on.who" } }
    });
    automation.start_def(&id("main"), &show(&[("ping", custom)])).unwrap();
    let person = Actor::Person { name: "Kiri".into() };
    automation.fire(EventKind::new("custom.ping").unwrap(), json!({ "who": "me" }), &person).unwrap();
    until("the call", || fake.requests().len() == 1);
    assert_eq!(fake.requests()[0].actor, person);
    automation.stop(&Actor::System).unwrap();
}

#[test]
fn a_refused_call_fires_failed_and_the_show_keeps_running() {
    let (automation, fake, _runner) = engine();
    let failing = json!({
        "on": { "kind": "test.on", "params": { "kind": "redeem" } },
        "call": { "kind": "test.call", "params": { "op": "test.fail" }, "in": { "in": "on.fired" } },
        "after": { "kind": "test.call", "params": { "op": "test.op" }, "in": { "in": "call.failed", "value": "call.error" } }
    });
    automation.start_def(&id("main"), &show(&[("fail", failing)])).unwrap();
    automation.publish(fixtures::redeem("Kiri", "Bonk")).unwrap();
    until("the follow-up", || fake.requests().len() == 2);
    let error = fake.requests()[1].args["value"].as_str().unwrap().to_owned();
    assert!(error.ends_with("refused"), "`error` carries the English reason: {error}");
    automation.stop(&Actor::System).unwrap();
}

#[test]
fn one_show_at_a_time_and_stop_then_start_again() {
    let (automation, fake, _runner) = engine();
    let def = show(&[("bonk", redeem_and_call())]);
    let run = automation.start_def(&id("main"), &def).unwrap();
    assert_eq!(run.seed, 7, "the show's pinned seed");
    let again = automation.start_def(&id("other"), &def).unwrap_err();
    assert!(again.message.contains("main"), "{}", again.message);
    automation.stop(&Actor::System).unwrap();
    assert!(automation.running().is_none());
    automation.publish(fixtures::redeem("Kiri", "Bonk")).unwrap();
    assert!(automation.stop(&Actor::System).is_err(), "nothing to stop");
    automation.start_def(&id("main"), &def).unwrap();
    automation.publish(fixtures::redeem("Mori", "Bonk")).unwrap();
    until("the calls", || fake.requests().len() == 2);
    assert_eq!(fake.requests()[0].args, json!({ "value": "Mori" }), "the event sent while stopped never ran");
    automation.stop(&Actor::System).unwrap();
}
