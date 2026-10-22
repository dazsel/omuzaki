//! Groups and enabling at run time (design D12, #776) through `ShowHarness`: a switch lands when the step that asked
//! closes, so its chain completes; a graph switched off drops its wakes, ignores its replies and refuses new events,
//! all counted; switched back on, its nodes get `Start` again; runtime switches reset at the next start; a graph
//! disabled in the document never runs.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::panic)]

mod kinds;

use kinds::{custom, harness, ops, show, var};
use omuzaki_core::time::Seconds;
use omuzaki_live_api::ids::{GraphId, GroupId, VarName};
use omuzaki_live_api::value::Val;
use serde_json::json;

/// The "stream starting" pattern: on `custom.go` the graph switches its own group off, then calls `op.intro`; a
/// counter in it counts its `Start`s. `ctl` (in no group) switches the group back on at `custom.on`.
fn intro_show() -> omuzaki_live_api::show::ShowDef {
    show(
        &["intro"],
        &[
            (
                "intro",
                Some("intro"),
                json!({
                    "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                    "off": { "kind": "test.switch", "params": { "group": "intro", "on": false }, "in": { "in": "on.fired" } },
                    "say": { "kind": "test.call", "params": { "op": "op.intro" }, "in": { "in": "off.out" } },
                    "starts": { "kind": "test.count" },
                }),
            ),
            (
                "ctl",
                None,
                json!({
                    "on": { "kind": "test.on", "params": { "kind": "custom.on" } },
                    "switch": { "kind": "test.switch", "params": { "group": "intro", "on": true }, "in": { "in": "on.fired" } },
                }),
            ),
        ],
    )
}

fn group_on(h: &mut omuzaki_live::testing::ShowHarness, group: &str) -> bool {
    h.values().groups.iter().find(|g| g.group.as_str() == group).unwrap().on
}

fn active(h: &mut omuzaki_live::testing::ShowHarness, graph: &str) -> bool {
    h.graphs().iter().find(|g| g.graph.as_str() == graph).unwrap().active
}

#[test]
fn a_node_switching_its_own_group_off_completes_its_chain_and_start_comes_again_when_it_is_on() {
    let mut h = harness(intro_show());
    assert_eq!(var(&mut h, "starts"), 1.0);

    h.publish(custom("go", 1.0));
    // The chain that switched completed: the op after the switch was asked for.
    assert_eq!(ops(&h), ["op.intro"]);
    assert!(!group_on(&mut h, "intro"));
    assert!(!active(&mut h, "intro"));

    // Later events don't reach the group.
    h.publish(custom("go", 2.0));
    assert_eq!(ops(&h), ["op.intro"]);
    assert_eq!(h.dropped().inactive, 1);

    // Switched on from another graph: its nodes get `Start` again, and it handles events.
    h.publish(custom("on", 3.0));
    assert!(group_on(&mut h, "intro"));
    assert_eq!(var(&mut h, "starts"), 2.0);
    h.publish(custom("go", 4.0));
    assert_eq!(ops(&h), ["op.intro", "op.intro"]);
}

#[test]
fn switching_a_graph_off_drops_its_wakes_and_ignores_its_replies() {
    let mut h = harness(show(
        &["g"],
        &[(
            "w",
            Some("g"),
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "wait": { "kind": "test.wait", "params": { "seconds": 5.0 }, "in": { "in": "on.fired" } },
                "late": { "kind": "test.call", "params": { "op": "op.late" }, "in": { "in": "wait.out" } },
                "ask": { "kind": "test.call", "params": { "op": "op.ask" }, "in": { "in": "on.fired" } },
                "count": { "kind": "test.count", "in": { "in": "ask.done" } },
            }),
        )],
    ));
    h.publish(custom("go", 1.0));
    assert_eq!(ops(&h), ["op.ask"]);
    let call = h.open_calls()[0];

    h.switch_group(GroupId::new("g").unwrap(), false);
    // The wake went with the switch.
    assert_eq!(h.dropped().inactive, 1);
    // The reply is ignored and counted, and nothing after it runs.
    h.reply(call, Ok(None));
    assert_eq!(h.dropped().inactive, 2);
    assert_eq!(var(&mut h, "n"), 0.0);
    h.advance(Seconds(10.0));
    assert_eq!(ops(&h), ["op.ask"]);

    // Back on: nothing half-finished resumes; a new event runs the whole graph.
    h.switch_group(GroupId::new("g").unwrap(), true);
    h.advance(Seconds(10.0));
    assert_eq!(ops(&h), ["op.ask"]);
    h.publish(custom("go", 30.0));
    h.advance(Seconds(5.0));
    assert_eq!(ops(&h), ["op.ask", "op.ask", "op.late"]);
}

#[test]
fn runtime_switches_reset_at_the_next_start() {
    let def = intro_show();
    let mut h = harness(def.clone());
    h.publish(custom("go", 1.0));
    assert!(!group_on(&mut h, "intro"));
    h.stop();
    // A new run of the same show starts from the document's state.
    let mut again = harness(def);
    assert!(group_on(&mut again, "intro"));
    again.publish(custom("go", 1.0));
    assert_eq!(ops(&again), ["op.intro"]);
}

#[test]
fn a_group_off_in_the_document_keeps_its_graphs_out_until_switched_on() {
    let mut def = intro_show();
    def.groups.get_mut(&GroupId::new("intro").unwrap()).unwrap().enabled = false;
    let mut h = harness(def);
    // No `Start` for a graph that isn't active.
    assert_eq!(var(&mut h, "starts"), 0.0);
    h.publish(custom("go", 1.0));
    assert_eq!(ops(&h), Vec::<String>::new());
    h.switch_group(GroupId::new("intro").unwrap(), true);
    assert_eq!(var(&mut h, "starts"), 1.0);
    h.publish(custom("go", 2.0));
    assert_eq!(ops(&h), ["op.intro"]);
}

#[test]
fn a_disabled_graph_never_runs_even_when_its_group_is_switched_on() {
    let mut def = intro_show();
    let graph = def.graphs.get_mut(&GraphId::new("intro").unwrap()).unwrap();
    *graph = std::mem::take(graph).with_enabled(false);
    let mut h = harness(def);
    h.publish(custom("go", 1.0));
    h.publish(custom("on", 2.0));
    h.publish(custom("go", 3.0));
    assert_eq!(ops(&h), Vec::<String>::new());
    assert_eq!(var(&mut h, "starts"), 0.0);
    assert!(!active(&mut h, "intro"));
}

#[test]
fn a_variable_set_from_outside_lands_as_its_own_step_and_is_typed() {
    let mut h = harness(intro_show());
    h.set_variable(VarName::new("n").unwrap(), Val::Number(41.0));
    assert_eq!(var(&mut h, "n"), 41.0);
    // A value of the wrong type is ignored (the service refuses it before it is queued).
    h.set_variable(VarName::new("n").unwrap(), Val::text("x"));
    assert_eq!(var(&mut h, "n"), 41.0);
    let changed = h.values().variables.into_iter().find(|v| v.name.as_str() == "n").unwrap();
    assert_eq!(changed.value, json!(41));
    assert_eq!(changed.changed_by, None);
}
