//! `on.event` and `event.emit` to their acceptance (#781): the redeem trigger's ports are exactly the golden list from
//! `Redeem`, `where` folds case and width, `min` is inclusive, an unregistered kind is refused, and `event.emit` sends
//! `custom.<name>` events and fails on a payload that isn't an object.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_mock::fixtures;
use omuzaki_live_mock::harness::NodeHarness;
use serde_json::{Value, json};

fn node(kind: &str, params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id(kind, params)
}

#[test]
fn the_redeem_triggers_ports_are_the_golden_list() {
    let h = node("on.event", json!({ "kind": "redeem" })).unwrap();
    let outputs: Vec<String> = h.ports().outputs.iter().map(|p| p.name.to_string()).collect();
    assert_eq!(
        outputs,
        [
            "channel_id",
            "channel_name",
            "input",
            "platform",
            "redemption_id",
            "reward_cost",
            "reward_id",
            "reward_title",
            "user_badges",
            "user_colour",
            "user_id",
            "user_login",
            "user_name",
            "user_roles",
            "event",
            "fired",
        ]
    );
}

#[test]
fn where_folds_case_and_width() {
    let mut h = node("on.event", json!({ "kind": "redeem", "where": { "reward.title": "bonk" } })).unwrap();
    h.trigger(fixtures::redeem("Kiri", "Bonk"));
    h.trigger(fixtures::redeem("Kiri", "ＢＯＮＫ"));
    h.trigger(fixtures::redeem("Kiri", "bonks"));
    assert_eq!(h.fired().len(), 2, "Bonk and the full-width BONK match; bonks does not");
}

#[test]
fn min_passes_100_and_not_99() {
    let mut h = node("on.event", json!({ "kind": "cheer", "min": { "bits": 100 } })).unwrap();
    h.trigger(fixtures::cheer("Kiri", 99));
    assert!(h.fired().is_empty());
    h.trigger(fixtures::cheer("Kiri", 100));
    assert_eq!(h.fired().len(), 1);
}

#[test]
fn an_unregistered_kind_fails_check() {
    let errors = node("on.event", json!({ "kind": "storm" })).err().unwrap();
    assert_eq!(errors.0[0].path, "kind");
}

#[test]
fn emit_sends_a_custom_event_and_fires_done() {
    let mut h = node("event.emit", json!({ "kind": "custom.x" })).unwrap();
    h.set_input("payload", json!({ "count": 3 }));
    h.send("in");
    assert_eq!(h.emitted().len(), 1);
    assert_eq!(h.emitted()[0].kind.as_str(), "custom.x");
    assert_eq!(h.emitted()[0].payload, json!({ "count": 3 }));
    assert_eq!(h.fired()[0].port.as_str(), "done");
}

#[test]
fn emit_with_no_payload_sends_an_empty_object() {
    let mut h = node("event.emit", json!({ "kind": "custom.x" })).unwrap();
    h.send("in");
    assert_eq!(h.emitted()[0].payload, json!({}));
}

#[test]
fn emit_with_a_payload_that_is_not_an_object_fails() {
    let mut h = node("event.emit", json!({ "kind": "custom.x" })).unwrap();
    h.set_input("payload", json!([1, 2]));
    h.send("in");
    assert!(h.emitted().is_empty());
    assert_eq!(h.fired()[0].port.as_str(), "failed");
}

#[test]
fn emit_only_sends_custom_kinds() {
    let errors = node("event.emit", json!({ "kind": "redeem" })).err().unwrap();
    assert_eq!(errors.0[0].path, "kind");
}
