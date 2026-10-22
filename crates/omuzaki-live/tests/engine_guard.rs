//! The loop guard and the engine limits (design D8, #776) through `ShowHarness` on fake time: a runaway loop trips at
//! exactly 2,001 activations and suspends its graph only, naming the last eight nodes; emitted events past eight hops
//! suspend the emitting graph; each graph starts at most 2,000 tokens at once and then 500 a second; an idle show
//! runs no steps.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::panic)]

mod kinds;

use kinds::{custom, harness, ops, show, var};
use omuzaki_core::time::Seconds;
use omuzaki_live_api::ids::GraphId;
use omuzaki_live_api::services::SuspendReason;
use serde_json::json;

fn suspension(h: &mut omuzaki_live::testing::ShowHarness, graph: &str) -> Option<SuspendReason> {
    let graphs = h.graphs();
    let status = graphs.iter().find(|g| g.graph == GraphId::new(graph).unwrap()).unwrap();
    status.suspended.as_ref().map(|s| s.reason.clone())
}

#[test]
fn a_loop_without_waits_trips_at_exactly_2001_activations_and_suspends_its_graph_only() {
    // `loop`: on -> a -> b -> a -> ... with no wait. `other` answers the same event with an op.
    let mut h = harness(show(
        &[],
        &[
            (
                "loop",
                None,
                json!({
                    "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                    "a": { "kind": "test.count", "in": { "in": ["on.fired", "b.out"] } },
                    "b": { "kind": "test.count", "in": { "in": "a.out" } },
                }),
            ),
            (
                "other",
                None,
                json!({
                    "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                    "say": { "kind": "test.call", "params": { "op": "op.other" }, "in": { "in": "on.fired" } },
                }),
            ),
        ],
    ));
    h.publish(custom("go", 1.0));
    // 2,000 activations ran: the trigger and 1,999 counts. The 2,001st tripped the guard before it ran.
    assert_eq!(var(&mut h, "n"), 1_999.0);
    let Some(SuspendReason::LoopGuard { last, .. }) = suspension(&mut h, "loop") else { panic!("not suspended") };
    let last: Vec<&str> = last.iter().map(omuzaki_live_api::ids::NodeId::as_str).collect();
    assert_eq!(last, ["a", "b", "a", "b", "a", "b", "a", "b"]);
    assert_eq!(suspension(&mut h, "other"), None);
    assert!(!h.graphs().iter().find(|g| g.graph.as_str() == "loop").unwrap().active);

    // The next event: the suspended graph stays out, the other graph handles it.
    let before = ops(&h).len();
    h.publish(custom("go", 2.0));
    assert_eq!(&ops(&h)[before..], ["op.other"]);
    assert_eq!(var(&mut h, "n"), 1_999.0);
    assert_eq!(h.dropped().inactive, 1);

    // `automation.resume_graph` lets it run (and trip) again.
    h.resume_graph(GraphId::new("loop").unwrap());
    assert_eq!(suspension(&mut h, "loop"), None);
    h.publish(custom("go", 3.0));
    assert_eq!(var(&mut h, "n"), 3_998.0);
    assert!(suspension(&mut h, "loop").is_some());
}

#[test]
fn emitted_events_past_eight_hops_are_dropped_and_the_emitting_graph_suspended() {
    // `echo` answers each `custom.ping` by emitting another one.
    let mut h = harness(show(
        &[],
        &[
            (
                "echo",
                None,
                json!({
                    "on": { "kind": "test.on", "params": { "kind": "custom.ping" } },
                    "count": { "kind": "test.count", "in": { "in": "on.fired" } },
                    "emit": { "kind": "test.emit", "params": { "kind": "custom.ping" }, "in": { "in": "count.out" } },
                }),
            ),
            (
                "other",
                None,
                json!({
                    "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                    "say": { "kind": "test.call", "params": { "op": "op.other" }, "in": { "in": "on.fired" } },
                }),
            ),
        ],
    ));
    h.publish(custom("ping", 1.0));
    // The outside ping (0 hops) and its echoes with 1 to 8 hops ran; the ninth echo was refused.
    assert_eq!(var(&mut h, "n"), 9.0);
    assert_eq!(suspension(&mut h, "echo"), Some(SuspendReason::EmitHops));
    h.publish(custom("go", 2.0));
    assert_eq!(ops(&h), ["op.other"]);
}

#[test]
fn a_graph_starts_2000_tokens_at_once_then_500_a_second_and_counts_the_rest() {
    let mut h = harness(show(
        &[],
        &[(
            "g",
            None,
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "count": { "kind": "test.count", "in": { "in": "on.fired" } },
            }),
        )],
    ));
    for _ in 0..3_000 {
        h.publish(custom("go", 0.0));
    }
    assert_eq!(var(&mut h, "n"), 2_000.0);
    assert_eq!(h.dropped().rate, 1_000);
    // A second later the bucket holds 500 again.
    for _ in 0..600 {
        h.publish(custom("go", 1.0));
    }
    assert_eq!(var(&mut h, "n"), 2_500.0);
    assert_eq!(h.dropped().rate, 1_100);
    // Half a second gives 250; the bucket never holds more than the burst.
    for _ in 0..300 {
        h.publish(custom("go", 1.5));
    }
    assert_eq!(var(&mut h, "n"), 2_750.0);
    h.advance_to(Seconds(100.0));
    for _ in 0..2_500 {
        h.publish(custom("go", 100.0));
    }
    assert_eq!(var(&mut h, "n"), 4_750.0);
}

#[test]
fn the_rate_is_per_graph() {
    let graph = || {
        json!({
            "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
            "count": { "kind": "test.count", "in": { "in": "on.fired" } },
        })
    };
    let mut h = harness(show(&[], &[("a", None, graph()), ("b", None, graph())]));
    for _ in 0..2_100 {
        h.publish(custom("go", 0.0));
    }
    // Each graph started 2,000 and refused 100.
    assert_eq!(var(&mut h, "n"), 4_000.0);
    assert_eq!(h.dropped().rate, 200);
}

#[test]
fn an_idle_show_runs_no_steps() {
    let mut h = harness(show(
        &[],
        &[(
            "g",
            None,
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "wait": { "kind": "test.wait", "params": { "seconds": 2.0 }, "in": { "in": "on.fired" } },
                "count": { "kind": "test.count", "in": { "in": "wait.out" } },
            }),
        )],
    ));
    h.advance(Seconds(60.0));
    assert_eq!(h.steps(), 0);
    // One event: its step, the wake two seconds later, and the coalesced `var.changed` after it.
    h.publish(custom("go", 61.0));
    h.advance(Seconds(60.0));
    assert_eq!(h.steps(), 3);
    assert_eq!(var(&mut h, "n"), 1.0);
}
