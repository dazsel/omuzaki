//! The text nodes to their acceptance (#779): templates fill in numbers and text, `{{` writes a brace and a bad name
//! is refused; `to_number` takes plain numbers and refuses the rest with a reason; `contains` folds case and width.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::harness::NodeHarness;
use serde_json::{Value, json};

fn node(kind: &str, params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id(kind, params)
}

fn format(template: &str, inputs: &[(&str, Val)]) -> Val {
    let mut h = node("text.format", json!({ "template": template })).unwrap();
    for (port, value) in inputs {
        h.set_input(port, value.clone());
    }
    h.read("text").unwrap()
}

#[test]
fn format_fills_in_numbers_and_text() {
    let text = format("Bonk #{n} from {user}!", &[("n", Val::Number(4.0)), ("user", Val::text("Kiri"))]);
    assert_eq!(text, Val::text("Bonk #4 from Kiri!"));
    assert_eq!(format("{{x}}", &[]), Val::text("{x}"));
    assert_eq!(format("{sum}", &[("sum", Val::Number(0.1 + 0.2))]), Val::text("0.30000000000000004"));
    assert_eq!(format("こんにちは、{name}さん", &[("name", Val::text("キリ"))]), Val::text("こんにちは、キリさん"));
    assert_eq!(format("[{unset}]", &[]), Val::text("[]"), "an unset input is empty");
}

#[test]
fn format_has_one_input_per_placeholder_and_refuses_a_bad_name() {
    let h = node("text.format", json!({ "template": "{a} {b} {a}" })).unwrap();
    let inputs: Vec<String> = h.ports().inputs.iter().map(|p| p.name.to_string()).collect();
    assert_eq!(inputs, ["a", "b"]);
    for bad in ["{Bad Name}", "{open", "close}"] {
        let errors = node("text.format", json!({ "template": bad })).err().unwrap();
        assert_eq!(errors.0[0].path, "template", "{bad}");
    }
}

fn to_number(text: &str) -> (String, Option<Val>) {
    let mut h = node("text.to_number", json!({})).unwrap();
    h.set_input("text", text);
    h.send("in");
    let fired = &h.fired()[0];
    let value = fired.values.iter().find(|(p, _)| p.as_str() == "value").map(|(_, v)| v.clone());
    (fired.port.to_string(), value)
}

#[test]
fn to_number_takes_plain_numbers_and_refuses_the_rest() {
    for (text, want) in [("42", 42.0), (" 4.5 ", 4.5), ("1e3", 1000.0), ("-0.25", -0.25)] {
        assert_eq!(to_number(text), ("done".to_owned(), Some(Val::Number(want))), "{text:?}");
    }
    for text in ["abc", "", "NaN", "inf", "1e999"] {
        let mut h = node("text.to_number", json!({})).unwrap();
        h.set_input("text", text);
        h.send("in");
        assert_eq!(h.fired()[0].port.as_str(), "failed", "{text:?}");
        let error = h.fired()[0].values.iter().find(|(p, _)| p.as_str() == "error").map(|(_, v)| v.clone());
        assert!(matches!(error, Some(Val::Text(t)) if !t.is_empty()), "{text:?} gives a reason");
    }
}

#[test]
fn contains_folds_unless_case_sensitive() {
    let mut h = node("text.contains", json!({})).unwrap();
    h.set_input("text", "ＢＯＮＫ!");
    h.set_input("part", "bonk");
    assert_eq!(h.read("value").unwrap(), Val::Bool(true));
    let mut h = node("text.contains", json!({ "case_sensitive": true })).unwrap();
    h.set_input("text", "ＢＯＮＫ!");
    h.set_input("part", "bonk");
    assert_eq!(h.read("value").unwrap(), Val::Bool(false));
}
