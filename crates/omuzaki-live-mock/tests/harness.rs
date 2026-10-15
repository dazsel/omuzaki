//! The node harness (#953), exercised through every method with an `echo` node (a value passed through, an op call,
//! a variable, a group, an emitted event, a live output), a `delay` node (wakes, the random stream, wall time) and a
//! trigger; the engine's ordering; the determinism check; and the `Unhandled` rule.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::payloads::ChatMessage;
use omuzaki_live_api::event::{Source, UtcMillis};
use omuzaki_live_api::ids::{EventKind, GroupId, VarName};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::{
    Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeErrorCode, NodeKind, NodeMeta, Ports,
};
use omuzaki_live_api::show::{VarScope, VarType, VariableDef};
use omuzaki_live_api::value::{PortType, Val};
use omuzaki_live_mock::fixtures;
use omuzaki_live_mock::harness::NodeHarness;
use omuzaki_ops_api::OpError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ── echo ───────────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `test.echo`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoParams {
    /// The op it calls with each value.
    #[serde(default)]
    op: String,
}

/// Passes `value` on, counts it in `count`, calls `op` with it and reports the reply on `done`; `fail` fails, `off`
/// switches the `redeems` group off.
struct Echo {
    op: String,
}

impl NodeKind for Echo {
    type Params = EchoParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("test.echo", "Passes a value on.", NodeClass::Active, Category::Flow)
    }

    fn ports(_: &EchoParams) -> Ports {
        Ports::new()
            .event_in("in", "Each value.")
            .value_in("value", PortType::Text, Val::text(""), "The value.")
            .event_out("out", "The value, passed on.")
            .event_out("done", "The op answered.")
            .event_out("failed", "It failed.")
            .value_out("value", PortType::Text, "The value.")
            .value_out("output", PortType::Json, "The op's answer.")
            .value_out("error", PortType::Text, "Why it failed.")
            .live_out("length", PortType::Number, "The value's length, in bytes.")
    }

    fn needs(params: &EchoParams) -> Vec<Need> {
        vec![Need::op(params.op.clone())]
    }

    fn new(params: EchoParams) -> Self {
        Self { op: if params.op.is_empty() { "chat.send".into() } else { params.op } }
    }
}

impl Echo {
    fn on_in(&self, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        let value = cx.input_text("value")?;
        match &*value {
            "fail" => return cx.fail("failed", omuzaki_live_api::l10n::internal("asked to fail")),
            "off" => {
                cx.switch_group(&GroupId::new("redeems").map_err(|e| NodeError::internal(e.to_string()))?, false)?;
            }
            _ => {}
        }
        let count = VarName::new("count").map_err(|e| NodeError::internal(e.to_string()))?;
        if let Some(Val::Number(n)) = cx.var(&count) {
            cx.set_var(&count, Val::Number(n + 1.0))?;
        }
        cx.set("value", value.clone())?;
        cx.emit(EventKind::from_static("custom.echoed"), json!({ "text": &*value }))?;
        cx.fire("out")?;
        cx.run_op(&self.op, json!({ "text": &*value }))?;
        Ok(())
    }
}

impl Node for Echo {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => self.on_in(cx),
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Reply { outcome, .. } => {
                let output = match outcome {
                    Ok(output) => output.unwrap_or_default(),
                    Err(error) => json!({ "error": error.to_string() }),
                };
                cx.set("output", output)?;
                cx.fire("done")
            }
            // Fired while stopping: the harness must ignore it.
            Input::Stop => cx.fire("out"),
            Input::Start | Input::Trigger { .. } | Input::Wake { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }

    fn eval(&self, port: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        match port {
            #[allow(clippy::cast_precision_loss)] // test values are short
            "length" => Ok(Val::Number(cx.input_text("value")?.len() as f64)),
            other => Err(NodeError::not_live(other)),
        }
    }
}

omuzaki_live_api::register_node!(Echo);

// ── delay ──────────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters of `test.delay`.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DelayParams {
    /// How long to wait.
    seconds: f64,
}

impl Default for DelayParams {
    fn default() -> Self {
        Self { seconds: 1.0 }
    }
}

/// Fires `out` `seconds` after each `in`, with a die roll and the times it saw; `cancel` drops the waiting ones.
struct Delay {
    seconds: f64,
    pending: Vec<omuzaki_live_api::node::WakeId>,
}

impl NodeKind for Delay {
    type Params = DelayParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("test.delay", "Waits.", NodeClass::Active, Category::Time)
    }

    fn ports(_: &DelayParams) -> Ports {
        Ports::new()
            .event_in("in", "Each event.")
            .event_in("cancel", "Drops the waiting ones.")
            .event_out("out", "After the wait.")
            .value_out("asked", PortType::Number, "When it was asked, on the runner clock.")
            .value_out("roll", PortType::Number, "A die roll.")
            .value_out("utc", PortType::Number, "The wall time when it fired.")
            .value_out("now", PortType::Number, "The runner time when it fired.")
    }

    fn new(params: DelayParams) -> Self {
        Self { seconds: params.seconds, pending: Vec::new() }
    }
}

impl Node for Delay {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let now = cx.now().0;
                cx.set("asked", now)?;
                #[allow(clippy::cast_precision_loss)] // 1..=6
                let roll = (cx.rng().next_u64() % 6 + 1) as f64;
                cx.set("roll", roll)?;
                self.pending.push(cx.wake_after(Seconds(self.seconds))?);
                Ok(())
            }
            Input::Event { port: "cancel", .. } => {
                self.pending.drain(..).for_each(|id| cx.cancel_wake(id));
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Wake { id, .. } => {
                self.pending.retain(|p| *p != id);
                #[allow(clippy::cast_precision_loss)] // test times
                let utc = cx.utc().0 as f64;
                let now = cx.now().0;
                cx.set("utc", utc)?;
                cx.set("now", now)?;
                cx.fire("out")
            }
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}

// ── a trigger, and a node that both waits and calls ───────────────────────────────────────────────────────────

/// Starts on each chat message.
struct OnChat;

impl NodeKind for OnChat {
    type Params = EchoParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("test.on_chat", "On chat.", NodeClass::Trigger, Category::Triggers)
    }

    fn ports(_: &EchoParams) -> Ports {
        Ports::new().event_out("fired", "A message came.").value_out("text", PortType::Text, "Its text.")
    }

    fn listens(_: &EchoParams) -> Vec<EventKind> {
        vec![EventKind::from_static("chat.message")]
    }

    fn new(_: EchoParams) -> Self {
        Self
    }
}

impl Node for OnChat {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Trigger { event, .. } => {
                let message = event.decode::<ChatMessage>().ok_or_else(|| NodeError::internal("not chat"))?;
                let message = message.map_err(|e| NodeError::internal(e.to_string()))?;
                cx.set("text", message.text)?;
                cx.fire("fired")
            }
            Input::Start | Input::Stop | Input::Event { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}

/// Calls an op, then asks for a wake, both due one second later.
struct Both;

impl NodeKind for Both {
    type Params = EchoParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("test.both", "Both.", NodeClass::Active, Category::Flow)
    }

    fn ports(_: &EchoParams) -> Ports {
        Ports::new().event_in("in", "Go.").event_out("woke", "The wake.").event_out("replied", "The reply.")
    }

    fn new(_: EchoParams) -> Self {
        Self
    }
}

impl Node for Both {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { .. } => {
                cx.run_op("probe", json!({}))?;
                cx.wake_after(Seconds(1.0)).map(|_| ())
            }
            Input::Wake { .. } => cx.fire("woke"),
            Input::Reply { .. } => cx.fire("replied"),
            Input::Start | Input::Stop | Input::Trigger { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}

/// Handles nothing: every input reaches its wildcard arm.
struct Careless;

impl NodeKind for Careless {
    type Params = EchoParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("test.careless", "Careless.", NodeClass::Active, Category::Flow)
    }

    fn ports(_: &EchoParams) -> Ports {
        Ports::new().event_in("in", "Go.")
    }

    fn new(_: EchoParams) -> Self {
        Self
    }
}

impl Node for Careless {
    fn handle(&mut self, input: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        Err(NodeError::unhandled(&input))
    }
}

// ── tests ──────────────────────────────────────────────────────────────────────────────────────────────────────

fn counter() -> VariableDef {
    VariableDef::new(VarType::Number, VarScope::Show)
}

fn echo() -> NodeHarness {
    NodeHarness::of::<Echo>(EchoParams::default())
        .with_var("count", counter(), Val::Number(3.0))
        .with_group("redeems", true)
}

fn ports(h: &NodeHarness) -> Vec<&str> {
    h.fired().iter().map(|f| f.port.as_str()).collect()
}

#[test]
fn echo_passes_its_value_through_and_answers_in_the_same_token() {
    let mut h = echo();
    h.respond("chat.send", Seconds(0.5), |args| Ok(Some(json!({ "sent": args["text"] }))));
    h.set_input("value", "こんにちは");
    h.send("in");
    assert_eq!(ports(&h), ["out"]);
    assert_eq!(h.fired()[0].values["value"], Val::text("こんにちは"));
    assert_eq!(h.calls().len(), 1);
    assert_eq!((h.calls()[0].op.as_str(), &h.calls()[0].args), ("chat.send", &json!({ "text": "こんにちは" })));
    assert_eq!(h.var("count"), Some(Val::Number(4.0)));
    assert_eq!(h.read("length"), Ok(Val::Number(15.0)));
    assert_eq!(h.read("value").unwrap_err().code, NodeErrorCode::NotLive);

    let emitted = &h.emitted()[0];
    assert_eq!(
        (emitted.kind.as_str(), emitted.hops, &emitted.payload),
        ("custom.echoed", 1, &json!({ "text": "こんにちは" }))
    );
    assert!(matches!(emitted.source, Source::Graph { .. }));

    h.advance(Seconds(0.4));
    assert_eq!(ports(&h), ["out"]);
    h.advance(Seconds(0.1));
    assert_eq!(ports(&h), ["out", "done"]);
    let done = &h.fired()[1];
    assert_eq!(done.at, Seconds(0.5));
    assert_eq!(done.values["value"], Val::text("こんにちは"), "the reply resumes the token that asked");
    assert_eq!(done.values["output"], Val::from(json!({ "sent": "こんにちは" })));
}

#[test]
fn each_send_is_a_fresh_token() {
    let mut h = echo();
    h.set_input("value", "a");
    h.send("in");
    h.set_input("value", "b");
    h.send("in");
    let calls: Vec<_> = h.calls().iter().map(|c| c.id).collect();
    h.reply(calls[1], Ok(Some(json!(2))));
    h.reply(calls[0], Err(OpError::denied("no")));
    let done: Vec<(Val, Val)> = h
        .fired()
        .iter()
        .filter(|f| f.port.as_str() == "done")
        .map(|f| (f.values["value"].clone(), f.values["output"].clone()))
        .collect();
    assert_eq!(done[0], (Val::text("b"), Val::from(json!(2))));
    assert_eq!(done[1].0, Val::text("a"));
}

#[test]
fn a_failure_sets_the_english_error_and_fires_failed() {
    let mut h = echo();
    h.set_input("value", "fail");
    h.send("in");
    assert_eq!(ports(&h), ["failed"]);
    assert_eq!(h.failures().len(), 1);
    assert_eq!(h.failures()[0].text.id, "live-api-internal");
    let error = &h.fired()[0].values["error"];
    assert_eq!(error, &Val::text("Something went wrong inside the node: asked to fail"));
}

#[test]
fn group_switches_land_at_the_end_of_the_step() {
    let mut h = echo();
    assert_eq!(h.group("redeems"), Some(true));
    h.set_input("value", "off");
    h.send("in");
    assert_eq!(h.group("redeems"), Some(false));
    assert_eq!(h.group("nope"), None);
}

#[test]
fn a_registered_kind_is_built_by_id_like_the_engine_builds_it() {
    let mut h = NodeHarness::by_id("test.echo", json!({ "op": "tts.say" })).unwrap();
    assert_eq!(h.needs(), [Need::op("tts.say")]);
    h.send("in");
    assert_eq!(h.calls()[0].op, "tts.say");
    assert_eq!(h.calls()[0].args, json!({ "text": "" }), "the input's default");
    let unknown = NodeHarness::by_id("test.missing", json!({})).err().unwrap();
    assert_eq!((unknown.0[0].path.as_str(), unknown.0[0].text.id.as_ref()), ("kind", "live-api-unknown-node-kind"));
    assert_eq!(NodeHarness::by_id("test.echo", json!({ "opp": 1 })).err().unwrap().0[0].path, "opp");
}

#[test]
fn delay_wakes_in_order_and_cancels() {
    let mut h = NodeHarness::of::<Delay>(DelayParams { seconds: 2.0 });
    h.send("in");
    h.advance(Seconds(0.5));
    h.send("in");
    h.advance(Seconds(1.6));
    assert_eq!(h.fired().len(), 1);
    assert_eq!(h.fired()[0].at, Seconds(2.0));
    assert_eq!(h.fired()[0].values["asked"], Val::Number(0.0));
    h.send("cancel");
    h.advance_to(Seconds(10.0));
    assert_eq!(h.fired().len(), 1);
    assert_eq!(h.now(), Seconds(10.0));
    h.advance_to(Seconds(5.0));
    assert_eq!(h.now(), Seconds(10.0), "time never goes back");
}

#[test]
fn a_wake_due_at_t_runs_before_a_reply_arriving_at_t() {
    let mut h = NodeHarness::of::<Both>(EchoParams::default());
    h.respond("probe", Seconds(1.0), |_| Ok(None));
    h.send("in");
    h.advance(Seconds(1.0));
    assert_eq!(ports(&h), ["woke", "replied"]);
    assert!(h.fired().iter().all(|f| f.at == Seconds(1.0)));
}

#[test]
fn a_wall_clock_jump_moves_utc_but_not_now() {
    let mut h = NodeHarness::of::<Delay>(DelayParams { seconds: 1.0 })
        .starting_at(Seconds(100.0), UtcMillis(1_700_000_000_000));
    h.send("in");
    h.wall().jump(3_600_000);
    h.advance(Seconds(1.0));
    let fired = &h.fired()[0].values;
    assert_eq!(fired["now"], Val::Number(101.0));
    assert_eq!(fired["utc"], Val::Number(1_700_000_000_000.0 + 1_000.0 + 3_600_000.0));
    assert_eq!(fired["asked"], Val::Number(100.0));
}

#[test]
fn a_trigger_gets_its_events_in_tokens_rooted_at_them() {
    let mut h = NodeHarness::of::<OnChat>(EchoParams::default());
    assert_eq!(h.listens(), [EventKind::from_static("chat.message")]);
    for name in fixtures::NAMES {
        h.trigger(fixtures::at(fixtures::chat(name, &format!("hi from {name}")), Seconds(1.0)));
    }
    let texts: Vec<Val> = h.fired().iter().map(|f| f.values["text"].clone()).collect();
    assert_eq!(texts.len(), fixtures::NAMES.len());
    assert_eq!(texts[3], Val::text("hi from ことり"));
}

/// The same script, with randomness, waits, calls, a variable and a group.
fn script(seed: u32) -> String {
    let mut h = NodeHarness::of::<Delay>(DelayParams { seconds: 0.25 }).seed(seed);
    for _ in 0..5 {
        h.send("in");
        h.advance(Seconds(0.1));
    }
    h.advance(Seconds(1.0));
    let mut e = echo().seed(seed);
    e.respond("chat.send", Seconds(0.3), |args| Ok(Some(args.clone())));
    for text in ["a", "ことり", "fail", "off"] {
        e.set_input("value", text);
        e.send("in");
        e.advance(Seconds(0.2));
    }
    e.advance(Seconds(1.0));
    format!("{}\n{}", h.log_json(), e.log_json())
}

#[test]
fn the_same_script_gives_byte_identical_logs() {
    let first = script(7);
    assert_eq!(first, script(7));
    assert_ne!(first, script(8), "the seed reaches the node's stream");
    assert!(first.contains("\"roll\""));
    let parsed: serde_json::Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
    let keys: Vec<&String> = parsed.as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn outputs_fired_while_stopping_are_ignored() {
    let mut h = echo();
    h.start();
    h.stop();
    assert_eq!(h.fired(), []);
}

#[test]
#[should_panic(expected = "doesn't handle the `start` input")]
fn a_node_that_doesnt_handle_an_input_fails_the_test() {
    let mut h = NodeHarness::of::<Careless>(EchoParams::default());
    h.send("in");
}

#[test]
fn node_errors_are_recorded_as_faults() {
    let mut h = NodeHarness::of::<Echo>(EchoParams::default()).with_var(
        "count",
        VariableDef::new(VarType::Number, VarScope::Show),
        Val::Number(1.0),
    );
    h.set_input("value", "x".repeat(omuzaki_live_api::limits::MAX_TEXT_BYTES + 1));
    h.send("in");
    assert_eq!(h.faults().len(), 1);
    assert_eq!(h.faults()[0].error.code, NodeErrorCode::Type);
    assert!(h.fired().is_empty() && h.calls().is_empty());
}

// ── design D4's `flow.wait`, verbatim, through the harness (#952) ─────────────────────────────────────────────

/// Parameters of `flow.wait`: none; the duration is an input so it can vary per event.
#[derive(Serialize, Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub struct WaitParams {}

/// Delays each event on `in` by `seconds`; every event waits on its own, and `cancel` drops the waiting ones.
pub struct Wait {
    pending: Vec<omuzaki_live_api::node::WakeId>,
}

impl NodeKind for Wait {
    type Params = WaitParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("flow.wait", "Wait, then pass the event on.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &WaitParams) -> Ports {
        Ports::new()
            .event_in("in", "Each event to delay.")
            .event_in("cancel", "Drops every event still waiting.")
            .value_in("seconds", PortType::Number, Val::Number(1.0), "How long to wait.")
            .event_out("out", "The event, after the wait.")
    }
    fn new(_: WaitParams) -> Self {
        Self { pending: Vec::new() }
    }
}

impl Node for Wait {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let seconds = cx.input_number("seconds")?.max(0.0);
                self.pending.push(cx.wake_after(Seconds(seconds))?);
                Ok(())
            }
            Input::Event { port: "cancel", .. } => {
                self.pending.drain(..).for_each(|id| cx.cancel_wake(id));
                Ok(())
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Wake { id, .. } => {
                self.pending.retain(|p| *p != id);
                cx.fire("out")
            }
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Reply { .. } => Ok(()), // not used here
            other => Err(NodeError::unhandled(&other)), // an input added after this node (D21)
        }
    }
}
omuzaki_live_api::register_node!(Wait);

#[test]
fn the_designs_flow_wait_runs_in_the_harness() {
    let mut h = NodeHarness::by_id("flow.wait", json!({})).unwrap();
    h.set_input("seconds", 2.0);
    h.send("in");
    h.advance(Seconds(1.0));
    h.set_input("seconds", 0.5);
    h.send("in");
    h.advance(Seconds(0.5));
    assert_eq!(h.fired().iter().map(|f| f.at).collect::<Vec<_>>(), [Seconds(1.5)]);
    h.send("cancel");
    h.advance(Seconds(5.0));
    assert_eq!(h.fired().len(), 1, "cancel drops the event still waiting");
    assert_eq!(h.faults(), []);
}
