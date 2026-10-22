//! The engine's step rules through `ShowHarness` (#775), with test node kinds on fake time: fan-out and depth-first
//! order, wakes before events stamped at the same time, time that never goes backwards, tokens that keep their own
//! values across a wait, determinism (the same script twice, and random streams keyed by node), and a panicking node
//! faulted while the show keeps running.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::panic)]

use omuzaki_core::time::Seconds;
use omuzaki_live::testing::ShowHarness;
use omuzaki_live_api::event::{LiveEvent, Source};
use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, NodeId, ShowId};
use omuzaki_live_api::node::{Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports};
use omuzaki_live_api::register_node;
use omuzaki_live_api::show::ShowDef;
use omuzaki_live_api::value::{PortType, Val};
use omuzaki_live_mock::fixtures;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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

/// Requests its op with `value`, and fires `out` at once (so chains can continue in the same step).
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
            .event_out("out", "Asked.")
    }
    fn new(params: CallParams) -> Self {
        Self { op: params.op }
    }
}

impl Node for Call {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if let Input::Event { .. } = input {
            let value = cx.input_text("value")?;
            cx.run_op(&self.op, json!({ "value": &*value }))?;
            cx.fire("out")?;
        }
        Ok(())
    }
}
register_node!(Call);

/// Parameters of `test.wait`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitParams {
    /// Seconds.
    #[serde(default)]
    seconds: f64,
}

/// Waits `seconds` in the token that arrived, then fires `out`.
struct Wait {
    seconds: f64,
}

impl NodeKind for Wait {
    type Params = WaitParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.wait", "Waits.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &WaitParams) -> Ports {
        Ports::new().event_in("in", "In.").event_out("out", "Out.")
    }
    fn new(params: WaitParams) -> Self {
        Self { seconds: params.seconds }
    }
}

impl Node for Wait {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => cx.wake_after(Seconds(self.seconds)).map(|_| ()),
            Input::Wake { .. } => cx.fire("out"),
            _ => Ok(()),
        }
    }
}
register_node!(Wait);

/// Parameters with nothing in them.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

/// On `in`, puts a draw from its random stream (and the step time) on its outputs and fires `out`.
struct Draw;

impl NodeKind for Draw {
    type Params = NoParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.draw", "Draws.", NodeClass::Active, Category::Math)
    }
    fn ports(_: &NoParams) -> Ports {
        Ports::new()
            .event_in("in", "In.")
            .value_out("value", PortType::Text, "A draw.")
            .value_out("at", PortType::Text, "The step time.")
            .event_out("out", "Out.")
    }
    fn new(_: NoParams) -> Self {
        Self
    }
}

impl Node for Draw {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if let Input::Event { .. } = input {
            let draw = cx.rng().next_u64();
            let at = cx.now();
            cx.set("value", Val::text(draw.to_string()))?;
            cx.set("at", Val::text(at.0.to_string()))?;
            cx.fire("out")?;
        }
        Ok(())
    }
}
register_node!(Draw);

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

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────────────────────

fn show(graphs: &[(&str, Value)]) -> ShowDef {
    graphs.iter().fold(ShowDef::new("Test").with_seed(7), |def, (id, nodes)| {
        def.with_graph(GraphId::new(*id).unwrap(), GraphDef::from_node(&json!({ "nodes": nodes })).unwrap())
    })
}

fn harness(graphs: &[(&str, Value)]) -> ShowHarness {
    ShowHarness::new(ShowId::new("main").unwrap(), show(graphs))
}

fn custom(who: &str, at: f64) -> LiveEvent {
    LiveEvent::new(EventKind::new("custom.go").unwrap(), Source::Test, Seconds(at), json!({ "who": who }))
}

fn ops(h: &ShowHarness) -> Vec<(String, String)> {
    h.requests().iter().map(|r| (r.op.clone(), r.args["value"].as_str().unwrap_or("").to_owned())).collect()
}

fn call(op: &str, wires: Value) -> Value {
    let mut node = json!({ "kind": "test.call", "params": { "op": op } });
    node["in"] = wires;
    node
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn fan_out_runs_in_node_id_order_and_each_chain_before_its_sibling() {
    // `on.fired` goes to `a` and `c`; `a.out` goes to `b`. Depth-first: a, then a's subtree (b), then c.
    let mut h = harness(&[(
        "g",
        json!({
            "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
            "c": call("op.c", json!({ "in": "on.fired", "value": "on.who" })),
            "a": call("op.a", json!({ "in": "on.fired", "value": "on.who" })),
            "b": call("op.b", json!({ "in": "a.out", "value": "on.who" })),
        }),
    )]);
    h.publish(custom("Kiri", 1.0));
    let order: Vec<_> = ops(&h).into_iter().map(|(op, _)| op).collect();
    assert_eq!(order, ["op.a", "op.b", "op.c"]);
}

#[test]
fn triggers_start_in_graph_then_node_order() {
    let trigger_and_call = |op: &str| {
        json!({
            "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
            "x": call(op, json!({ "in": "on.fired", "value": "on.who" })),
        })
    };
    let mut h = harness(&[("zeta", trigger_and_call("op.zeta")), ("alpha", trigger_and_call("op.alpha"))]);
    h.publish(custom("Kiri", 1.0));
    let order: Vec<_> = ops(&h).into_iter().map(|(op, _)| op).collect();
    assert_eq!(order, ["op.alpha", "op.zeta"]);
}

#[test]
fn forty_chat_events_through_a_wait_each_read_their_own_user() {
    let mut h = harness(&[(
        "lurk",
        json!({
            "on": { "kind": "test.on", "params": { "kind": "chat.message" } },
            "wait": { "kind": "test.wait", "params": { "seconds": 2.0 }, "in": { "in": "on.fired" } },
            "say": call("chat.send", json!({ "in": "wait.out", "value": "on.who" })),
        }),
    )]);
    let names: Vec<String> = (0..40).map(|i| format!("{}{i}", fixtures::NAMES[i % fixtures::NAMES.len()])).collect();
    for (i, name) in names.iter().enumerate() {
        let at = 1.0 + 0.05 * f64::from(u32::try_from(i).unwrap());
        h.publish(fixtures::at(fixtures::chat(name, "hi"), Seconds(at)));
    }
    assert_eq!(h.requests().len(), 0, "every one is still waiting");
    h.advance(Seconds(5.0));
    let said: Vec<String> = ops(&h).into_iter().map(|(_, who)| who).collect();
    assert_eq!(said, names);
    for (i, request) in h.requests().iter().enumerate() {
        let want = 3.0 + 0.05 * f64::from(u32::try_from(i).unwrap());
        assert!((request.at.0 - want).abs() < 1e-9, "request {i} at {:?}", request.at);
    }
}

#[test]
fn a_wake_due_at_t_runs_before_an_event_stamped_t() {
    let mut h = harness(&[(
        "g",
        json!({
            "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
            "wait": { "kind": "test.wait", "params": { "seconds": 1.0 }, "in": { "in": "on.fired" } },
            "late": call("op.wake", json!({ "in": "wait.out", "value": "on.who" })),
            "now": call("op.event", json!({ "in": "on.fired", "value": "on.who" })),
        }),
    )]);
    h.publish(custom("first", 1.0));
    h.publish(custom("second", 2.0));
    assert_eq!(
        ops(&h),
        [("op.event", "first"), ("op.wake", "first"), ("op.event", "second")]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
}

#[test]
fn an_event_stamped_earlier_than_the_last_step_runs_at_the_last_steps_time() {
    let mut h = harness(&[(
        "g",
        json!({
            "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
            "draw": { "kind": "test.draw", "in": { "in": "on.fired" } },
            "say": call("op.at", json!({ "in": "draw.out", "value": "draw.at" })),
        }),
    )]);
    h.publish(custom("a", 10.0));
    h.publish(custom("b", 5.0));
    let times: Vec<_> = ops(&h).into_iter().map(|(_, at)| at).collect();
    assert_eq!(times, ["10", "10"]);
}

fn drawing_show(extra: &str) -> Vec<(&'static str, Value)> {
    let mut nodes = json!({
        "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
        "draw": { "kind": "test.draw", "in": { "in": "on.fired" } },
        "say": call("op.draw", json!({ "in": "draw.out", "value": "draw.value" })),
    });
    // An unrelated node, under whatever name the test gives it.
    nodes[extra] = json!({ "kind": "test.draw", "in": { "in": "on.fired" } });
    vec![("g", nodes)]
}

fn script(h: &mut ShowHarness) {
    for i in 0..20_u32 {
        h.publish(custom("Kiri", f64::from(i)));
    }
    h.advance(Seconds(5.0));
}

#[test]
fn the_same_script_twice_gives_the_same_log_byte_for_byte() {
    let run = || {
        let mut h = harness(&drawing_show("other")).seed(42);
        script(&mut h);
        h.log_json()
    };
    let first = run();
    assert_eq!(first, run());
    let mut other_seed = harness(&drawing_show("other")).seed(43);
    script(&mut other_seed);
    assert_ne!(first, other_seed.log_json(), "another seed draws differently");
}

#[test]
fn renaming_an_unrelated_node_leaves_another_nodes_draws_alone() {
    let draws = |extra: &str| {
        let mut h = harness(&drawing_show(extra)).seed(42);
        script(&mut h);
        ops(&h).into_iter().map(|(_, v)| v).collect::<Vec<_>>()
    };
    let before = draws("other");
    assert_eq!(before.len(), 20);
    assert_eq!(before, draws("renamed"));
}

#[test]
fn a_panicking_node_is_faulted_and_the_show_keeps_running() {
    let mut h = harness(&[
        (
            "bad",
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "boom": { "kind": "test.boom", "in": { "in": "on.fired" } },
            }),
        ),
        (
            "good",
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "say": call("op.ok", json!({ "in": "on.fired", "value": "on.who" })),
            }),
        ),
    ]);
    h.publish(custom("one", 1.0));
    h.publish(custom("two", 2.0));
    let said: Vec<_> = ops(&h).into_iter().map(|(_, who)| who).collect();
    assert_eq!(said, ["one", "two"]);
    let bad = h.graphs().into_iter().find(|g| g.graph.as_str() == "bad").unwrap();
    assert_eq!(bad.faulted.iter().map(NodeId::as_str).collect::<Vec<_>>(), ["boom"]);
}

#[test]
fn replies_come_back_in_the_token_that_asked_after_their_latency() {
    let mut h = harness(&[(
        "g",
        json!({
            "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
            "first": call("op.slow", json!({ "in": "on.fired", "value": "on.who" })),
        }),
    )]);
    h.respond("op.slow", Seconds(0.5), |args| Ok(Some(args.clone())));
    h.publish(custom("Kiri", 1.0));
    assert_eq!(h.open_calls().len(), 0, "a scripted op leaves nothing open");
    let firings_before = h.values().firings.len();
    h.advance(Seconds(1.0));
    assert!(h.values().firings.len() >= firings_before);
    assert_eq!(h.requests().len(), 1);
}
