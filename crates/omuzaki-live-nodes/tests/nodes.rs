//! `on.event` and `op.run` through the node harness: ports derived from a payload schema, `where` and `min` filters,
//! placeholders typed from an op's schema, the arguments decoded before anything is sent, the reply on `done` or
//! `failed`, and the ops a show may not run.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_core::time::Seconds;
use omuzaki_live_api::node::find_node_kind;
use omuzaki_live_api::value::{PortType, Val};
use omuzaki_live_mock::fixtures;
use omuzaki_live_mock::harness::NodeHarness;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ── Test ops: one a show may run, one it may not ───────────────────────────────────────────────────────────────

/// Say a line with a volume.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Say {
    /// The line.
    text: String,
    /// How loud.
    #[serde(default)]
    volume: f64,
    /// In which mood.
    #[serde(default)]
    mood: Option<Mood>,
}

/// A mood.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Mood {
    /// Happy.
    Happy,
    /// Sad.
    Sad,
}

impl Op for Say {
    fn meta() -> OpMeta {
        OpMeta { name: "test.say", summary: "Say a line.", kind: OpKind::Edit, scope: Scope::Perform, undo: Undo::None }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "said": self.text })))
    }
}
register_op!(Say);

/// Write a file.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Scribble {
    /// Where.
    path: String,
}

impl Op for Scribble {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.scribble",
            summary: "Write a file.",
            kind: OpKind::Edit,
            scope: Scope::Fs,
            undo: Undo::None,
        }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "path": self.path })))
    }
}
register_op!(Scribble);

/// A harness for a registered kind; links the crate so its registrations are in the test binary (ADR 0015).
fn node(kind: &str, params: Value) -> Result<NodeHarness, omuzaki_live_api::node::ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id(kind, params)
}

fn names(ports: &[omuzaki_live_api::node::PortDecl]) -> Vec<String> {
    ports.iter().map(|p| p.name.to_string()).collect()
}

// ── on.event ───────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_redeem_trigger_has_the_payloads_fields_as_sorted_ports_then_event_and_fired() {
    let h = node("on.event", json!({ "kind": "redeem" })).unwrap();
    let outputs = names(&h.ports().outputs);
    for port in ["user_name", "user_id", "user_roles", "reward_title", "reward_cost", "input", "redemption_id"] {
        assert!(outputs.contains(&port.to_owned()), "{port} missing from {outputs:?}");
    }
    let values = &outputs[..outputs.len() - 2];
    assert!(values.windows(2).all(|w| w[0] < w[1]), "value ports are sorted: {values:?}");
    assert_eq!(&outputs[outputs.len() - 2..], ["event", "fired"]);
    let ty = |name: &str| h.ports().output(name).unwrap().ty;
    assert_eq!((ty("reward_cost"), ty("user_roles"), ty("input")), (PortType::Number, PortType::List, PortType::Text));
    assert_eq!(h.listens().iter().map(ToString::to_string).collect::<Vec<_>>(), ["redeem"]);
}

#[test]
fn where_matches_the_reward_title_ignoring_case_and_sets_the_fields() {
    let mut h = node("on.event", json!({ "kind": "redeem", "where": { "reward.title": "BONK" } })).unwrap();
    h.trigger(fixtures::redeem("Kiri", "Hat"));
    h.trigger(fixtures::redeem("Kiri", "bonk"));
    let fired = h.fired();
    assert_eq!(fired.len(), 1, "only the bonk redeem passes");
    let values = &fired[0].values;
    let get = |port: &str| values.iter().find(|(name, _)| name.as_str() == port).map(|(_, v)| v.clone());
    assert_eq!(get("reward_title"), Some(Val::text("bonk")));
    assert_eq!(get("user_name"), Some(Val::text("Kiri")));
    assert_eq!(get("reward_cost"), Some(Val::Number(100.0)));
    assert_eq!(get("input"), Some(Val::text("")), "a missing optional field reads as its type's default");
    assert!(matches!(get("event"), Some(Val::Json(_))));
}

#[test]
fn min_keeps_only_cheers_with_enough_bits() {
    let mut h = node("on.event", json!({ "kind": "cheer", "min": { "bits": 100 } })).unwrap();
    h.trigger(fixtures::cheer("Kiri", 99));
    h.trigger(fixtures::cheer("Kiri", 100));
    h.trigger(fixtures::cheer("Kiri", 500));
    assert_eq!(h.fired().len(), 2);
}

#[test]
fn a_custom_kind_has_only_event_and_fired() {
    let mut h = node("on.event", json!({ "kind": "custom.bell" })).unwrap();
    assert_eq!(names(&h.ports().outputs), ["event", "fired"]);
    h.trigger(fixtures::custom("bell", json!({ "rings": 3 })));
    assert_eq!(h.fired().len(), 1);
}

#[test]
fn an_unknown_kind_or_a_list_filter_is_refused() {
    let unknown = node("on.event", json!({ "kind": "bonk.storm" })).err().unwrap();
    assert_eq!(unknown.0[0].path, "kind");
    let list = node("on.event", json!({ "kind": "redeem", "where": { "user.roles": ["vip"] } })).err().unwrap();
    assert_eq!(list.0[0].path, "where.user.roles");
}

// ── op.run ─────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_placeholder_becomes_an_input_typed_from_the_ops_schema() {
    let h = node("op.run", json!({ "op": "test.say", "args": { "text": "$in.line", "volume": "$in.loud" } })).unwrap();
    let inputs = names(&h.ports().inputs);
    assert_eq!(inputs, ["in", "line", "loud"]);
    assert_eq!(h.ports().input("line").unwrap().ty, PortType::Text);
    assert_eq!(h.ports().input("loud").unwrap().ty, PortType::Number);
    assert_eq!(names(&h.ports().outputs), ["done", "failed", "output", "error"]);
}

#[test]
fn each_pulse_runs_the_op_with_its_inputs_and_the_reply_fires_done() {
    let mut h = node("op.run", json!({ "op": "test.say", "args": { "text": "$in.line", "volume": 0.5 } })).unwrap();
    h.respond("test.say", Seconds(0.25), |args| Ok(Some(json!({ "said": args["text"] }))));
    h.set_input("line", "hello");
    h.send("in");
    assert_eq!(h.calls().len(), 1);
    assert_eq!(h.calls()[0].args, json!({ "text": "hello", "volume": 0.5 }));
    assert!(h.fired().is_empty(), "nothing fires before the reply");
    h.advance(Seconds(0.25));
    let fired = h.fired();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].port.as_str(), "done");
    let output = fired[0].values.iter().find(|(p, _)| p.as_str() == "output").map(|(_, v)| v.to_json());
    assert_eq!(output, Some(json!({ "said": "hello" })));
}

#[test]
fn a_refused_reply_fires_failed_with_the_reason() {
    let mut h = node("op.run", json!({ "op": "test.say", "args": { "text": "hi" } })).unwrap();
    h.respond("test.say", Seconds(0.0), |_| Err(OpError::conflict("OBS isn't connected")));
    h.send("in");
    h.advance(Seconds(0.0));
    assert_eq!(h.fired()[0].port.as_str(), "failed");
    let error = h.fired()[0].values.iter().find(|(p, _)| p.as_str() == "error").map(|(_, v)| v.clone());
    assert_eq!(error, Some(Val::text("conflict: OBS isn't connected")));
}

#[test]
fn arguments_that_dont_decode_fail_without_reaching_the_executor() {
    // `mood` takes text, but only the names of the moods.
    let mut h = node("op.run", json!({ "op": "test.say", "args": { "text": "hi", "mood": "$in.mood" } })).unwrap();
    assert_eq!(h.ports().input("mood").unwrap().ty, PortType::Text);
    h.set_input("mood", "angry");
    h.send("in");
    assert!(h.calls().is_empty(), "nothing is sent");
    assert_eq!(h.fired()[0].port.as_str(), "failed");
}

#[test]
fn a_literal_dollar_is_written_with_two() {
    let mut h = node("op.run", json!({ "op": "test.say", "args": { "text": "$$in.line" } })).unwrap();
    assert_eq!(names(&h.ports().inputs), ["in"], "an escaped placeholder is no input");
    h.send("in");
    assert_eq!(h.calls()[0].args, json!({ "text": "$in.line" }));
}

#[test]
fn ops_a_show_may_not_run_and_misspelt_arguments_are_refused_at_compile() {
    let refused = node("op.run", json!({ "op": "test.scribble", "args": { "path": "a.txt" } })).err().unwrap();
    assert_eq!(refused.0[0].path, "op");
    let unknown = node("op.run", json!({ "op": "no.such_op" })).err().unwrap();
    assert_eq!(unknown.0[0].path, "op");
    let misspelt = node("op.run", json!({ "op": "test.say", "args": { "txt": "hi" } })).err().unwrap();
    assert_eq!(misspelt.0[0].path, "args.txt");
}

#[test]
fn a_perform_op_needs_only_the_op() {
    // Every show may perform; only the op itself is listed for the approval card.
    let h = node("op.run", json!({ "op": "test.say", "args": { "text": "hi" } })).unwrap();
    let needs = serde_json::to_value(h.needs()).unwrap();
    assert!(!needs.to_string().contains("perform") && needs.to_string().contains("test.say"), "{needs}");
}

#[test]
fn both_kinds_are_registered() {
    omuzaki_live_nodes::link();
    for id in ["on.event", "op.run"] {
        let registration = find_node_kind(id).unwrap();
        assert_eq!((registration.meta)().id, id);
    }
    let _: Value = (find_node_kind("op.run").unwrap().defaults)();
}
