//! Variables that outlive a run (design D10, #954) through `ShowHarness`: a burst of writes is one save plus the
//! final flush; `show` and `global` values come back after a restart and `session` ones don't; a damaged values file
//! starts from the defaults with a finding; values the show no longer declares stay in the file; and three sets of one
//! variable in a step give one `var.changed` in the next step, with the first old value and the last new one.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::panic)]
// The damaged-file test writes its fixture file in a temporary folder.
#![allow(clippy::disallowed_methods)]

mod kinds;

use std::collections::BTreeMap;
use std::sync::Arc;

use kinds::{custom, harness, var};
use omuzaki_live::testing::ShowHarness;
use omuzaki_live::vars::DiskVariables;
use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, NodeId, ShowId, VarName};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::{Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports};
use omuzaki_live_api::register_node;
use omuzaki_live_api::services::{InertReason, PersistScope, ShowKey, VariableStore};
use omuzaki_live_api::show::{ShowDef, VarScope, VarType, VariableDef};
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::variables::MemoryVariables;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Parameters of `test.add`.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AddParams {
    /// How many times one pulse adds one to `n`.
    #[serde(default)]
    times: u32,
}

/// Adds one to `n`, `times` times, on each pulse: each add a write of its own, as `var.add` makes.
struct Add {
    times: u32,
}

impl NodeKind for Add {
    type Params = AddParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.add", "Adds.", NodeClass::Active, Category::Variables)
    }
    fn ports(_: &AddParams) -> Ports {
        Ports::new().event_in("in", "In.")
    }
    fn new(params: AddParams) -> Self {
        Self { times: params.times }
    }
}

impl Node for Add {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if let Input::Event { .. } = input {
            let n = VarName::new("n").unwrap();
            for _ in 0..self.times {
                let Some(Val::Number(x)) = cx.var(&n) else { return Ok(()) };
                cx.set_var(&n, Val::Number(x + 1.0))?;
            }
        }
        Ok(())
    }
}
register_node!(Add);

/// Parameters with nothing in them.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

/// Adds one to the global `g` on each pulse, and says so in its needs, as the `var.*` nodes do.
struct BumpGlobal;

impl NodeKind for BumpGlobal {
    type Params = NoParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.bump_global", "Bumps g.", NodeClass::Active, Category::Variables)
    }
    fn ports(_: &NoParams) -> Ports {
        Ports::new().event_in("in", "In.")
    }
    fn needs(_: &NoParams) -> Vec<Need> {
        vec![Need::variable(VarName::new("g").unwrap(), VarType::Number)]
    }
    fn new(_: NoParams) -> Self {
        Self
    }
}

impl Node for BumpGlobal {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if let Input::Event { .. } = input {
            let g = VarName::new("g").unwrap();
            if let Some(Val::Number(x)) = cx.var(&g) {
                cx.set_var(&g, Val::Number(x + 1.0))?;
            }
        }
        Ok(())
    }
}
register_node!(BumpGlobal);

/// A trigger on `var.changed` that hands the payload to `op.changed`.
struct Seen;

impl NodeKind for Seen {
    type Params = NoParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.seen", "Sees changes.", NodeClass::Trigger, Category::Triggers)
    }
    fn ports(_: &NoParams) -> Ports {
        Ports::new()
    }
    fn listens(_: &NoParams) -> Vec<EventKind> {
        vec![EventKind::from_static("var.changed")]
    }
    fn new(_: NoParams) -> Self {
        Self
    }
}

impl Node for Seen {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if let Input::Trigger { event, .. } = input {
            let payload = event.payload.clone();
            cx.run_op("op.changed", payload)?;
        }
        Ok(())
    }
}
register_node!(Seen);

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────────────────────

const PROJECT: &str = "/projects/demo";

fn name(s: &str) -> VarName {
    VarName::new(s).unwrap()
}

fn show_scope() -> PersistScope {
    PersistScope::Show(ShowKey::new(PROJECT, ShowId::new("main").unwrap()))
}

/// `n` in the show's scope, `g` global and `s` per session, all numbers from 0; `custom.go` adds `times` to `n`.
fn def(times: u32) -> ShowDef {
    let number = |scope| VariableDef::new(VarType::Number, scope).with_default(json!(0));
    let graph = GraphDef::from_node(&json!({ "nodes": {
        "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
        "add": { "kind": "test.add", "params": { "times": times }, "in": { "in": "on.fired" } },
        "seen": { "kind": "test.seen" },
    }}))
    .unwrap();
    ShowDef::new("Test")
        .with_seed(7)
        .with_variable(name("n"), number(VarScope::Show))
        .with_variable(name("g"), number(VarScope::Global))
        .with_variable(name("s"), number(VarScope::Session))
        .with_graph(GraphId::new("g").unwrap(), graph)
}

fn run(store: &Arc<MemoryVariables>, times: u32) -> ShowHarness {
    harness(def(times)).variables(Arc::clone(store) as Arc<dyn VariableStore>, Some(PROJECT))
}

fn changes(h: &ShowHarness) -> Vec<Value> {
    h.requests().iter().filter(|r| r.op == "op.changed").map(|r| r.args.clone()).collect()
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn ten_thousand_adds_in_a_second_are_one_save_plus_the_flush() {
    let store = Arc::new(MemoryVariables::new());
    let mut h = run(&store, 100);
    // 100 events in the first second, each adding 100 times.
    for i in 0..100 {
        h.publish(custom("go", f64::from(i) / 100.0));
    }
    assert_eq!(var(&mut h, "n"), 10_000.0);
    assert_eq!(store.saves(), 0);
    h.advance_to(omuzaki_core::time::Seconds(1.0));
    assert_eq!(store.saves(), 1);
    assert_eq!(store.stored(&show_scope())[&name("n")], json!(10_000));
    h.advance_to(omuzaki_core::time::Seconds(30.0));
    assert_eq!(store.saves(), 1);
    h.stop();
    assert_eq!(store.saves(), 2);
}

#[test]
fn show_and_global_values_come_back_after_a_restart_and_session_values_do_not() {
    let store = Arc::new(MemoryVariables::new());
    let mut first = run(&store, 1);
    for v in ["n", "g", "s"] {
        first.set_variable(name(v), Val::Number(5.0));
    }
    first.stop();

    let mut again = run(&store, 1);
    assert_eq!(var(&mut again, "n"), 5.0);
    assert_eq!(var(&mut again, "g"), 5.0);
    assert_eq!(var(&mut again, "s"), 0.0);
    assert_eq!(again.findings().len(), 0);

    // Another project's show of the same id shares the globals only.
    let mut other = harness(def(1)).variables(Arc::clone(&store) as Arc<dyn VariableStore>, Some("/projects/other"));
    assert_eq!(var(&mut other, "g"), 5.0);
    assert_eq!(var(&mut other, "n"), 0.0);
}

#[test]
fn values_the_show_no_longer_declares_stay_in_the_file() {
    let stored = BTreeMap::from([(name("retired"), json!("keep me")), (name("n"), json!(1))]);
    let store = Arc::new(MemoryVariables::new().with(show_scope(), stored));
    let mut h = run(&store, 1);
    assert_eq!(var(&mut h, "n"), 1.0);
    h.publish(custom("go", 1.0));
    h.stop();
    let saved = store.stored(&show_scope());
    assert_eq!(saved[&name("retired")], json!("keep me"));
    assert_eq!(saved[&name("n")], json!(2));
}

#[test]
fn a_stored_value_of_another_type_starts_from_the_default_with_a_finding() {
    let store = Arc::new(MemoryVariables::new().with(show_scope(), BTreeMap::from([(name("n"), json!("three"))])));
    let mut h = run(&store, 1);
    assert_eq!(var(&mut h, "n"), 0.0);
    assert_eq!(h.findings().len(), 1);
    assert!(!h.findings()[0].inert);
}

#[test]
fn a_global_another_show_declared_with_another_type_makes_its_nodes_inert_and_is_never_overwritten() {
    // Another show declared `g` as text and saved it; this show declares it as a number.
    let globals = BTreeMap::from([(name("g"), json!("hello"))]);
    let store = Arc::new(MemoryVariables::new().with(PersistScope::Global, globals));
    let graph = GraphDef::from_node(&json!({ "nodes": {
        "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
        "bump": { "kind": "test.bump_global", "in": { "in": "on.fired" } },
        "add": { "kind": "test.add", "params": { "times": 1 }, "in": { "in": "on.fired" } },
    }}))
    .unwrap();
    let def = def(1).with_graph(GraphId::new("other").unwrap(), graph);
    let mut h = harness(def).variables(Arc::clone(&store) as Arc<dyn VariableStore>, Some(PROJECT));
    h.publish(custom("go", 1.0));
    assert_eq!(h.findings().len(), 1);
    assert!(h.findings()[0].inert);
    let other = h.graphs().into_iter().find(|g| g.graph.as_str() == "other").unwrap();
    assert_eq!(other.inert.len(), 1);
    assert_eq!(other.inert[0].node, NodeId::new("bump").unwrap());
    assert_eq!(other.inert[0].why, InertReason::Undeclared);
    // The rest of the show runs: `add` bumped `n`, and a person's write to `g` is refused too.
    assert_eq!(var(&mut h, "n"), 2.0);
    h.set_variable(name("g"), Val::Number(9.0));
    h.stop();
    assert_eq!(store.stored(&PersistScope::Global)[&name("g")], json!("hello"));
    assert_eq!(store.stored(&show_scope())[&name("n")], json!(2));
}

#[test]
fn a_damaged_values_file_starts_from_the_defaults_with_a_finding_and_never_fails_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(DiskVariables::new(dir.path().join("data")));
    let path = store.path(&PersistScope::Global);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "g = = 3\n").unwrap();
    let project = dir.path().join("project");
    let mut h = harness(def(1)).variables(Arc::clone(&store) as Arc<dyn VariableStore>, project.to_str());
    assert_eq!(var(&mut h, "g"), 0.0);
    assert_eq!(h.findings().len(), 1);
    // The show runs, and its values reach the disk.
    h.publish(custom("go", 1.0));
    h.set_variable(name("g"), Val::Number(9.0));
    h.stop();
    let again = harness(def(1)).variables(Arc::clone(&store) as Arc<dyn VariableStore>, project.to_str());
    let mut again = again;
    assert_eq!(var(&mut again, "g"), 9.0);
    assert_eq!(var(&mut again, "n"), 1.0);
}

#[test]
fn three_sets_in_one_step_give_one_change_event_with_the_first_old_and_last_new_value() {
    let store = Arc::new(MemoryVariables::new());
    let mut h = run(&store, 3);
    h.publish(custom("go", 1.0));
    let seen = changes(&h);
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0]["name"], json!("n"));
    assert_eq!(seen[0]["old"], json!(0));
    assert_eq!(seen[0]["new"], json!(3));
}
