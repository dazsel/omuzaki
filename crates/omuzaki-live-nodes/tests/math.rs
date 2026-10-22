//! The math nodes to their acceptance (#731): a table for every op and each domain error, and property tests that no
//! pure node's value is ever NaN or infinite, `map_range` is monotonic and exact at its ends, every curve runs from 0
//! to 1, and `random_range` stays in bounds and repeats with its seed.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::float_cmp)]

use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::harness::NodeHarness;
use omuzaki_live_nodes::math::Curve;
use proptest::prelude::*;
use serde_json::{Value, json};

fn node(kind: &str, params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id(kind, params)
}

fn number(v: Val) -> f64 {
    match v {
        Val::Number(x) => x,
        other => panic!("not a number: {other:?}"),
    }
}

/// `math.op` with `op` on the inputs: (value, ok, error).
fn op(name: &str, inputs: &[(&str, f64)]) -> (f64, bool, String) {
    let mut h = node("math.op", json!({ "op": name })).unwrap();
    // Only the inputs this op has: the property test offers all of them to every op.
    for (port, x) in inputs {
        if h.ports().input(port).is_some() {
            h.set_input(port, *x);
        }
    }
    let value = number(h.read("value").unwrap());
    let ok = h.read("ok").unwrap() == Val::Bool(true);
    let error = match h.read("error").unwrap() {
        Val::Text(t) => t.to_string(),
        other => panic!("{other:?}"),
    };
    (value, ok, error)
}

#[test]
fn every_op_gives_its_value() {
    let ab = |a, b| vec![("a", a), ("b", b)];
    for (name, inputs, want) in [
        ("add", ab(2.0, 3.0), 5.0),
        ("sub", ab(2.0, 3.0), -1.0),
        ("mul", ab(2.0, 3.0), 6.0),
        ("div", ab(3.0, 2.0), 1.5),
        ("min", ab(2.0, 3.0), 2.0),
        ("max", ab(2.0, 3.0), 3.0),
        ("mod", ab(7.0, 3.0), 1.0),
        ("mod", ab(-1.0, 3.0), 2.0),
        ("pow", ab(2.0, 10.0), 1024.0),
        ("abs", vec![("a", -4.5)], 4.5),
        ("round", vec![("a", 2.5)], 3.0),
        ("floor", vec![("a", 2.7)], 2.0),
        ("ceil", vec![("a", 2.1)], 3.0),
        ("clamp", vec![("a", 5.0), ("min", 0.0), ("max", 1.0)], 1.0),
    ] {
        let (value, ok, error) = op(name, &inputs);
        assert_eq!((value, ok, error.as_str()), (want, true, ""), "{name} {inputs:?}");
    }
}

#[test]
fn domain_errors_read_as_zero_not_ok_and_a_reason() {
    for (name, inputs, says) in [
        ("div", vec![("a", 1.0), ("b", 0.0)], "division by zero"),
        ("mod", vec![("a", 1.0), ("b", 0.0)], "division by zero"),
        ("pow", vec![("a", 10.0), ("b", 1000.0)], "finite"),
        ("pow", vec![("a", -8.0), ("b", 0.5)], "finite"),
        ("clamp", vec![("a", 0.5), ("min", 2.0), ("max", 1.0)], "above the maximum"),
    ] {
        let (value, ok, error) = op(name, &inputs);
        assert_eq!((value, ok), (0.0, false), "{name}");
        assert!(error.contains(says), "{name}: {error}");
    }
    let mut h = node("math.map_range", json!({})).unwrap();
    h.set_input("in_min", 1.0);
    h.set_input("in_max", 1.0);
    assert_eq!(h.read("value").unwrap(), Val::Number(0.0));
    assert_eq!(h.read("ok").unwrap(), Val::Bool(false));
}

#[test]
fn compare_numbers_with_epsilon_and_text_exactly() {
    let mut h = node("math.compare", json!({ "op": "==" })).unwrap();
    h.set_input("a", 0.1 + 0.2);
    h.set_input("b", 0.3);
    assert_eq!(h.read("value").unwrap(), Val::Bool(true), "within epsilon");
    let mut h = node("math.compare", json!({ "op": "<" })).unwrap();
    h.set_input("a", 1.0);
    h.set_input("b", 2.0);
    assert_eq!(h.read("value").unwrap(), Val::Bool(true));
    let mut h = node("math.compare", json!({ "op": "!=", "on": "text" })).unwrap();
    h.set_input("a", "Bonk");
    h.set_input("b", "bonk");
    assert_eq!(h.read("value").unwrap(), Val::Bool(true), "text compares exactly");
    let refused = node("math.compare", json!({ "op": "<", "on": "text" })).err().unwrap();
    assert_eq!(refused.0[0].path, "op");
}

#[test]
fn random_range_refuses_a_backwards_range() {
    let mut h = node("math.random_range", json!({})).unwrap();
    h.set_input("min", 2.0);
    h.set_input("max", 1.0);
    h.send("in");
    assert_eq!(h.fired()[0].port.as_str(), "failed");
}

fn draws(seed: u32, integer: bool, min: f64, max: f64) -> Vec<f64> {
    let mut h = node("math.random_range", json!({ "integer": integer })).unwrap().seed(seed);
    h.set_input("min", min);
    h.set_input("max", max);
    for _ in 0..10_000 {
        h.send("in");
    }
    h.fired().iter().map(|f| number(f.values.iter().find(|(p, _)| p.as_str() == "value").unwrap().1.clone())).collect()
}

#[test]
fn random_range_stays_in_bounds_and_repeats_with_its_seed() {
    let reals = draws(3, false, -2.0, 5.0);
    assert!(reals.iter().all(|x| (-2.0..5.0).contains(x)));
    let ints = draws(3, true, 1.0, 6.0);
    assert!(ints.iter().all(|x| x.fract() == 0.0 && (1.0..=6.0).contains(x)));
    for face in 1..=6 {
        assert!(ints.contains(&f64::from(face)), "every face of the die comes up, {face} included");
    }
    assert_eq!(reals, draws(3, false, -2.0, 5.0), "the same seed repeats");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn no_op_ever_gives_nan_or_infinity(name in prop::sample::select(vec![
        "add", "sub", "mul", "div", "min", "max", "mod", "pow", "abs", "round", "floor", "ceil", "clamp",
    ]), a in -1e300f64..1e300, b in -1e300f64..1e300, c in -1e300f64..1e300) {
        let (value, _, _) = op(name, &[("a", a), ("b", b), ("min", b), ("max", c)]);
        prop_assert!(value.is_finite(), "{} {} {} {} gave {}", name, a, b, c, value);
    }

    #[test]
    fn map_range_is_monotonic_and_exact_at_its_ends(
        lo in -1e6f64..1e6, span in 1e-3f64..1e6, out_lo in -1e6f64..1e6, out_span in -1e6f64..1e6,
        x in 0.0f64..1.0, y in 0.0f64..1.0,
    ) {
        let mut h = node("math.map_range", json!({})).unwrap();
        h.set_input("in_min", lo);
        h.set_input("in_max", lo + span);
        h.set_input("out_min", out_lo);
        h.set_input("out_max", out_lo + out_span);
        let mut at = |v: f64| { h.set_input("value", v); number(h.read("value").unwrap()) };
        prop_assert_eq!(at(lo), out_lo);
        prop_assert_eq!(at(lo + span), out_lo + out_span);
        let (small, large) = if x <= y { (x, y) } else { (y, x) };
        let (fa, fb) = (at(lo + small * span), at(lo + large * span));
        if out_span >= 0.0 { prop_assert!(fa <= fb + 1e-9); } else { prop_assert!(fa + 1e-9 >= fb); }
    }

    #[test]
    fn ease_values_are_finite(t in -10.0f64..10.0, curve in prop::sample::select(vec![
        "linear", "smooth", "out_cubic", "in_out_cubic", "out_back", "out_elastic", "out_bounce",
    ])) {
        let mut h = node("math.ease", json!({ "curve": curve })).unwrap();
        h.set_input("t", t);
        prop_assert!(number(h.read("value").unwrap()).is_finite());
    }
}

#[test]
fn every_curve_runs_from_zero_to_one() {
    for curve in [
        Curve::Linear,
        Curve::Smooth,
        Curve::OutCubic,
        Curve::InOutCubic,
        Curve::OutBack,
        Curve::OutElastic,
        Curve::OutBounce,
    ] {
        assert!(curve.apply(0.0).abs() < 1e-12, "{curve:?} at 0 is {}", curve.apply(0.0));
        assert!((curve.apply(1.0) - 1.0).abs() < 1e-12, "{curve:?} at 1 is {}", curve.apply(1.0));
    }
}
