//! The group nodes to their acceptance (#938), through the node harness: switches land at the end of the step.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_live_api::ids::GroupId;
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::harness::NodeHarness;
use serde_json::{Value, json};

fn node(kind: &str, params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id(kind, params)
}

fn ports(h: &NodeHarness) -> Vec<String> {
    h.fired().iter().map(|f| f.port.to_string()).collect()
}

#[test]
fn enable_and_disable_switch_the_group() {
    let mut on = node("group.enable", json!({ "group": "chat_games" })).unwrap().with_group("chat_games", false);
    on.send("in");
    assert_eq!(on.group("chat_games"), Some(true));
    assert_eq!(ports(&on), ["done"]);

    let mut off = node("group.disable", json!({ "group": "chat_games" })).unwrap().with_group("chat_games", true);
    off.send("in");
    assert_eq!(off.group("chat_games"), Some(false));
}

#[test]
fn toggling_twice_returns_to_the_start() {
    let mut h = node("group.toggle", json!({ "group": "chat_games" })).unwrap().with_group("chat_games", true);
    h.send("in");
    assert_eq!(h.group("chat_games"), Some(false));
    h.send("in");
    assert_eq!(h.group("chat_games"), Some(true));
    assert_eq!(ports(&h), ["done", "done"]);
    assert_eq!(h.calls().len(), 0, "a group switch makes no op request");
}

#[test]
fn is_enabled_reads_the_group() {
    let mut h = node("group.is_enabled", json!({ "group": "chat_games" })).unwrap().with_group("chat_games", true);
    assert_eq!(h.read("value").unwrap(), Val::Bool(true));
    let mut h = node("group.is_enabled", json!({ "group": "chat_games" })).unwrap().with_group("chat_games", false);
    assert_eq!(h.read("value").unwrap(), Val::Bool(false));
}

#[test]
fn switching_an_undeclared_group_fails_with_a_reason() {
    let mut h = node("group.enable", json!({ "group": "ghost" })).unwrap();
    h.send("in");
    assert_eq!(ports(&h), ["failed"]);
    let error = format!("{:?}", h.failures()[0].text);
    assert!(error.contains("ghost"), "{error}");
}

#[test]
fn needs_are_exactly_the_group_and_a_fresh_node_asks_for_one_and_none_requests_an_op() {
    for kind in ["group.enable", "group.disable", "group.toggle", "group.is_enabled"] {
        let h = node(kind, json!({ "group": "chat_games" })).unwrap();
        assert_eq!(h.needs(), [Need::group(GroupId::new("chat_games").unwrap())], "{kind}");
        let Err(errors) = node(kind, json!({})) else { panic!("{kind} took no group") };
        assert_eq!(errors.0.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["group"], "{kind}");
    }
}
