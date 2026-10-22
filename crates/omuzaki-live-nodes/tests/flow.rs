//! The flow and logic nodes to their acceptance (#1274, #778), through the node harness on fake time.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::float_cmp)]

use omuzaki_core::time::Seconds;
use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::fixtures;
use omuzaki_live_mock::harness::NodeHarness;
use omuzaki_live_nodes::fold::fold;
use serde_json::{Value, json};

fn node(kind: &str, params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id(kind, params)
}

fn ports(h: &NodeHarness) -> Vec<String> {
    h.fired().iter().map(|f| f.port.to_string()).collect()
}

fn left(h: &NodeHarness, i: usize) -> Option<Val> {
    h.fired()[i].values.iter().find(|(p, _)| p.as_str() == "left").map(|(_, v)| v.clone())
}

// ── flow.wait ──────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_two_second_wait_fires_at_exactly_two_seconds() {
    let mut h = node("flow.wait", json!({})).unwrap();
    h.set_input("seconds", 2.0);
    h.send("in");
    h.advance(Seconds(1.999));
    assert_eq!(h.fired().len(), 0, "not before");
    h.advance(Seconds(0.001));
    assert_eq!(h.fired().len(), 1);
    assert!((h.fired()[0].at.0 - 2.0).abs() < 1e-9, "{:?}", h.fired()[0].at);
}

#[test]
fn overlapping_waits_fire_in_order_and_cancel_drops_them_all() {
    let mut h = node("flow.wait", json!({})).unwrap();
    h.set_input("seconds", 2.0);
    for _ in 0..3 {
        h.send("in");
        h.advance(Seconds(0.5));
    }
    h.advance(Seconds(2.0));
    let at: Vec<f64> = h.fired().iter().map(|f| f.at.0).collect();
    assert_eq!(at.len(), 3);
    assert!((at[0] - 2.0).abs() < 1e-9 && (at[1] - 2.5).abs() < 1e-9 && (at[2] - 3.0).abs() < 1e-9, "{at:?}");

    let mut h = node("flow.wait", json!({})).unwrap();
    h.set_input("seconds", 2.0);
    for _ in 0..3 {
        h.send("in");
    }
    h.send("cancel");
    h.advance(Seconds(5.0));
    assert_eq!(h.fired().len(), 0, "cancel drops every pending event");
}

// ── flow.random ────────────────────────────────────────────────────────────────────────────────────────────────

fn random_run(seed: u32) -> NodeHarness {
    let mut h = node("flow.random", json!({ "weights": [1.0, 3.0] })).unwrap().seed(seed);
    for _ in 0..10_000 {
        h.send("in");
    }
    h
}

#[test]
fn random_picks_by_weight_and_repeats_with_its_seed() {
    let h = random_run(7);
    let first = h.fired().iter().filter(|f| f.port.as_str() == "out_1").count();
    #[allow(clippy::cast_precision_loss)]
    let share = first as f64 / 10_000.0;
    assert!((share - 0.25).abs() <= 0.015, "out_1 took {share}");
    assert_eq!(h.log_json(), random_run(7).log_json(), "the same seed repeats byte for byte");
    assert_ne!(h.log_json(), random_run(8).log_json(), "another seed differs");
}

#[test]
fn random_refuses_bad_weights() {
    for weights in [json!([1.0]), json!([1.0, -1.0]), json!([0.0, 0.0])] {
        assert!(node("flow.random", json!({ "weights": weights })).is_err(), "{weights}");
    }
}

// ── flow.sequence and flow.switch ──────────────────────────────────────────────────────────────────────────────

#[test]
fn sequence_wraps_or_stops_and_reset_starts_again() {
    let mut h = node("flow.sequence", json!({ "count": 3 })).unwrap();
    for _ in 0..4 {
        h.send("in");
    }
    assert_eq!(ports(&h), ["out_1", "out_2", "out_3", "out_1"]);

    let mut h = node("flow.sequence", json!({ "count": 2, "wrap": false })).unwrap();
    for _ in 0..3 {
        h.send("in");
    }
    assert_eq!(ports(&h), ["out_1", "out_2"], "it stops at the end");
    h.send("reset");
    h.send("in");
    assert_eq!(ports(&h), ["out_1", "out_2", "out_1"]);
}

#[test]
fn switch_matches_a_number_against_its_text_case_and_falls_to_default() {
    let mut h = node("flow.switch", json!({ "cases": ["1", "2", "3"] })).unwrap();
    h.set_input("value", 3.0);
    h.send("in");
    h.set_input("value", "nine");
    h.send("in");
    assert_eq!(ports(&h), ["case_3", "default"]);
}

// ── fold ───────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn fold_lowercases_after_nfkc_and_leaves_japanese_and_korean_alone() {
    assert_eq!(fold("ＢＯＮＫ!"), "bonk!");
    assert_eq!(fold("こんにちは"), "こんにちは");
    assert_eq!(fold("안녕하세요"), "안녕하세요");
    assert_eq!(fold("ｶﾀｶﾅ"), "カタカナ", "half-width katakana widens under NFKC");
}

// ── flow.branch, flow.then, flow.gate ──────────────────────────────────────────────────────────────────────────

#[test]
fn branch_follows_its_condition() {
    let mut h = node("flow.branch", json!({})).unwrap();
    h.set_input("condition", true);
    h.send("in");
    h.set_input("condition", false);
    h.send("in");
    assert_eq!(ports(&h), ["true", "false"]);
}

#[test]
fn then_fires_its_outputs_in_order() {
    let mut h = node("flow.then", json!({ "count": 3 })).unwrap();
    h.send("in");
    assert_eq!(ports(&h), ["then_1", "then_2", "then_3"]);
}

#[test]
fn a_gate_started_closed_passes_nothing_until_opened() {
    let mut h = node("flow.gate", json!({})).unwrap();
    h.send("in");
    assert_eq!(h.fired().len(), 0);
    assert_eq!(h.read("is_open").unwrap(), Val::Bool(false));
    h.send("open");
    h.send("in");
    assert_eq!(ports(&h), ["out"]);
    h.send("toggle");
    h.send("in");
    assert_eq!(ports(&h), ["out"], "toggled closed");
    assert_eq!(h.read("is_open").unwrap(), Val::Bool(false));
}

// ── flow.cooldown ──────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_global_cooldown_lets_one_through_per_ten_seconds() {
    let mut h = node("flow.cooldown", json!({})).unwrap();
    h.send("in");
    h.advance_to(Seconds(4.0));
    h.send("in");
    h.advance_to(Seconds(10.0));
    h.send("in");
    assert_eq!(ports(&h), ["out", "cooling", "out"]);
    assert_eq!(left(&h, 1), Some(Val::Number(6.0)));
}

#[test]
fn a_per_user_cooldown_keys_on_the_user() {
    let mut h = node("flow.cooldown", json!({ "per": "user" })).unwrap();
    // The cooldown reads the root event, so each pulse is sent in a token rooted at a chat message.
    h.send_from("in", fixtures::at(fixtures::chat("kiri", "a"), Seconds(0.0)));
    h.send_from("in", fixtures::at(fixtures::chat("mochi", "b"), Seconds(2.0)));
    h.send_from("in", fixtures::at(fixtures::chat("kiri", "c"), Seconds(5.0)));
    assert_eq!(ports(&h), ["out", "out", "cooling"]);
    assert_eq!(left(&h, 2), Some(Val::Number(5.0)));
}

// ── logic ──────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn logic_truth_tables() {
    for (kind, table) in [("logic.and", [false, false, false, true]), ("logic.or", [false, true, true, true])] {
        let mut h = node(kind, json!({})).unwrap();
        for (i, (a, b)) in [(false, false), (false, true), (true, false), (true, true)].into_iter().enumerate() {
            h.set_input("a", a);
            h.set_input("b", b);
            assert_eq!(h.read("value").unwrap(), Val::Bool(table[i]), "{kind} {a} {b}");
        }
    }
    let mut h = node("logic.not", json!({})).unwrap();
    for a in [false, true] {
        h.set_input("a", a);
        assert_eq!(h.read("value").unwrap(), Val::Bool(!a));
    }
}
