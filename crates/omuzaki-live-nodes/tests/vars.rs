//! The variable nodes to their acceptance (#732), through the node harness.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::{LiveEvent, Source};
use omuzaki_live_api::ids::{EventKind, VarName};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::show::{VarScope, VarType, VariableDef};
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::harness::NodeHarness;
use serde_json::{Value, json};

fn node(kind: &str, params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id(kind, params)
}

fn def(ty: VarType) -> VariableDef {
    VariableDef::new(ty, VarScope::Session)
}

fn changed(name: &str, old: &Value, new: &Value) -> LiveEvent {
    let kind = EventKind::new("var.changed").unwrap();
    LiveEvent::new(
        kind,
        Source::Test,
        Seconds(0.0),
        json!({ "name": name, "scope": "session", "old": old, "new": new }),
    )
}

fn value(h: &NodeHarness, i: usize, port: &str) -> Option<Val> {
    h.fired()[i].values.iter().find(|(p, _)| p.as_str() == port).map(|(_, v)| v.clone())
}

#[test]
fn three_adds_from_zero_count_one_two_three() {
    let mut h = node("var.add", json!({ "variable": "bonks", "type": "number" })).unwrap().with_var(
        "bonks",
        def(VarType::Number),
        Val::Number(0.0),
    );
    for _ in 0..3 {
        h.send("in");
    }
    let counts: Vec<_> = (0..3).map(|i| (h.fired()[i].port.to_string(), value(&h, i, "value"))).collect();
    assert_eq!(counts, [1.0, 2.0, 3.0].map(|n| ("done".to_owned(), Some(Val::Number(n)))), "{:?}", h.log_json());
    assert_eq!(h.var("bonks"), Some(Val::Number(3.0)));
}

#[test]
fn add_takes_away_with_a_negative_step() {
    let mut h = node("var.add", json!({ "variable": "hp", "type": "number" })).unwrap().with_var(
        "hp",
        def(VarType::Number),
        Val::Number(10.0),
    );
    h.set_input("by", -2.5);
    h.send("in");
    assert_eq!(h.var("hp"), Some(Val::Number(7.5)));
}

#[test]
fn toggling_twice_returns_to_the_start() {
    let mut h = node("var.toggle", json!({ "variable": "lights", "type": "bool" })).unwrap().with_var(
        "lights",
        def(VarType::Bool),
        Val::Bool(false),
    );
    h.send("in");
    assert_eq!(h.var("lights"), Some(Val::Bool(true)));
    h.send("in");
    assert_eq!(h.var("lights"), Some(Val::Bool(false)));
    assert_eq!(value(&h, 1, "value"), Some(Val::Bool(false)));
}

#[test]
fn set_writes_and_get_reads() {
    let mut set = node("var.set", json!({ "variable": "title", "type": "text" })).unwrap().with_var(
        "title",
        def(VarType::Text),
        Val::text(""),
    );
    set.set_input("value", Val::text("cozy stream"));
    set.send("in");
    assert_eq!(set.var("title"), Some(Val::text("cozy stream")));
    assert_eq!(set.fired()[0].port.as_str(), "done");

    let mut get = node("var.get", json!({ "variable": "title", "type": "text" })).unwrap().with_var(
        "title",
        def(VarType::Text),
        Val::text("cozy stream"),
    );
    assert_eq!(get.read("value").unwrap(), Val::text("cozy stream"));
}

#[test]
fn a_write_to_an_undeclared_variable_fails_with_a_reason() {
    let mut h = node("var.add", json!({ "variable": "ghost", "type": "number" })).unwrap();
    h.send("in");
    assert_eq!(h.fired().len(), 1);
    assert_eq!(h.fired()[0].port.as_str(), "failed");
    assert_eq!(h.failures().len(), 1);
    let error = format!("{:?}", h.failures()[0].text);
    assert!(error.contains("ghost"), "{error}");
}

#[test]
fn add_and_toggle_refuse_the_wrong_type_naming_type() {
    for (kind, ty) in [("var.add", "text"), ("var.toggle", "number")] {
        let Err(errors) = node(kind, json!({ "variable": "x", "type": ty })) else { panic!("{kind} took {ty}") };
        assert_eq!(errors.0.len(), 1, "{kind}: {errors:?}");
        assert_eq!(errors.0[0].path, "type", "{kind}: {errors:?}");
    }
}

#[test]
fn needs_are_exactly_the_variable() {
    for kind in ["var.get", "var.set", "var.add", "var.changed"] {
        let h = node(kind, json!({ "variable": "bonks", "type": "number" })).unwrap();
        assert_eq!(h.needs(), [Need::variable(VarName::new("bonks").unwrap(), VarType::Number)], "{kind}");
    }
}

#[test]
fn changed_fires_for_its_own_variable_only() {
    let mut h = node("var.changed", json!({ "variable": "bonks", "type": "number" })).unwrap();
    h.trigger(changed("other", &json!(0), &json!(1)));
    assert_eq!(h.fired().len(), 0);
    h.trigger(changed("bonks", &json!(4), &json!(5)));
    assert_eq!(h.fired().len(), 1);
    assert_eq!(value(&h, 0, "old"), Some(Val::Number(4.0)));
    assert_eq!(value(&h, 0, "new"), Some(Val::Number(5.0)));
}

#[test]
fn a_fresh_node_asks_for_a_variable() {
    let Err(errors) = node("var.get", json!({})) else { panic!("var.get took no variable") };
    assert_eq!(errors.0.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(), ["variable"]);
}
