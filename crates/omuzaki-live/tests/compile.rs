//! The compile's catalogue checks and D11's arming rule (design D18, #1175), and hot reload (D5), through
//! `ShowHarness`: every second-column row of D18 makes exactly its node inert with a finding; a value cycle makes
//! both ends inert; approvals arm only the nodes that needed them; a program whose file changes after approval is
//! inert again; and a reload keeps every unchanged node's state while a changed node (or both ends of a changed wire)
//! starts again, its parked work dropped and counted.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::panic)]

mod kinds;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use kinds::{custom, harness, ops, show};
use omuzaki_core::time::Seconds;
use omuzaki_live::compile::ProgramHasher;
use omuzaki_live_api::ids::NodeId;
use omuzaki_live_api::need::{Invocation, Need, NetScheme};
use omuzaki_live_api::node::{Category, EvalCx, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports};
use omuzaki_live_api::register_node;
use omuzaki_live_api::services::{ApprovedProgram, GraphStatus, InertReason, ShowApprovals};
use omuzaki_live_api::value::{PortType, Val};
use omuzaki_ops_api::Scope;
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ── Kinds ──────────────────────────────────────────────────────────────────────────────────────────────────────

/// Parameters listing needs.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NeedyParams {
    /// What it needs.
    #[serde(default)]
    needs: Vec<Need>,
}

/// Needs what its parameters say; runs `op.needy` on each pulse.
struct Needy;

impl NodeKind for Needy {
    type Params = NeedyParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.needy", "Needs things.", NodeClass::Active, Category::Actions)
    }
    fn ports(_: &NeedyParams) -> Ports {
        Ports::new().event_in("in", "In.")
    }
    fn needs(params: &NeedyParams) -> Vec<Need> {
        params.needs.clone()
    }
    fn new(_: NeedyParams) -> Self {
        Self
    }
}

impl Node for Needy {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if let Input::Event { .. } = input {
            cx.run_op("op.needy", json!({}))?;
        }
        Ok(())
    }
}
register_node!(Needy);

/// Parameters with nothing in them.
#[derive(Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

/// A pure node: `y` is `x` plus one.
struct Pass;

impl NodeKind for Pass {
    type Params = NoParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.pass", "Adds one.", NodeClass::Pure, Category::Math)
    }
    fn ports(_: &NoParams) -> Ports {
        Ports::new().value_in("x", PortType::Number, Val::Number(0.0), "In.").live_out("y", PortType::Number, "Out.")
    }
    fn new(_: NoParams) -> Self {
        Self
    }
}

impl Node for Pass {
    fn handle(&mut self, _: Input<'_>, _: &mut NodeCx<'_>) -> Result<(), NodeError> {
        Ok(())
    }
    fn eval(&self, _: &str, cx: &mut EvalCx<'_>) -> Result<Val, NodeError> {
        Ok(Val::Number(cx.input_number("x")? + 1.0))
    }
}
register_node!(Pass);

/// Parameters of `test.tally`.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TallyParams {
    /// Added to the tally on each pulse.
    #[serde(default = "one")]
    step: u32,
}

fn one() -> u32 {
    1
}

impl Default for TallyParams {
    fn default() -> Self {
        Self { step: 1 }
    }
}

/// Counts its pulses in its own state and reports the tally through `op.tally`; reads `x` (a number) when wired.
struct Tally {
    step: u32,
    n: u32,
}

impl NodeKind for Tally {
    type Params = TallyParams;
    fn meta() -> NodeMeta {
        NodeMeta::new("test.tally", "Tallies.", NodeClass::Active, Category::Flow)
    }
    fn ports(_: &TallyParams) -> Ports {
        Ports::new()
            .event_in("in", "In.")
            .value_in("x", PortType::Number, Val::Number(0.0), "A number.")
            .event_out("out", "Out.")
    }
    fn new(params: TallyParams) -> Self {
        Self { step: params.step, n: 0 }
    }
}

impl Node for Tally {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if let Input::Event { .. } = input {
            self.n += self.step;
            cx.run_op("op.tally", json!({ "n": self.n }))?;
            cx.fire("out")?;
        }
        Ok(())
    }
}
register_node!(Tally);

/// Hashes "files" from a table a test changes.
#[derive(Default)]
struct Files(Mutex<BTreeMap<String, Vec<u8>>>);

impl Files {
    fn put(&self, path: &str, bytes: &[u8]) {
        self.0.lock().unwrap().insert(path.to_owned(), bytes.to_vec());
    }
}

impl ProgramHasher for Files {
    fn hash(&self, path: &str) -> Option<ContentHash> {
        self.0.lock().unwrap().get(path).map(|b| ContentHash::of(b))
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────────────────────

fn inert(graphs: &[GraphStatus]) -> BTreeMap<String, InertReason> {
    graphs.iter().flat_map(|g| g.inert.iter().map(|n| (n.node.as_str().to_owned(), n.why.clone()))).collect()
}

fn host() -> Need {
    Need::host(NetScheme::Https, "example.com", 443)
}

fn needy(needs: &[Need]) -> Value {
    json!({ "kind": "test.needy", "params": { "needs": needs }, "in": { "in": "on.fired" } })
}

fn tally_args(h: &omuzaki_live::testing::ShowHarness) -> Vec<u64> {
    h.requests().iter().filter(|r| r.op == "op.tally").map(|r| r.args["n"].as_u64().unwrap()).collect()
}

// ── Tests ──────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn every_catalogue_rule_makes_exactly_its_node_inert_with_a_finding() {
    let def = show(
        &[],
        &[(
            "g",
            None,
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "ghost": { "kind": "test.nope", "in": { "in": "on.fired" } },
                "bad_params": { "kind": "test.tally", "params": { "step": "many" }, "in": { "in": "on.fired" } },
                "no_input": { "kind": "test.tally", "in": { "in": "on.fired", "missing": "on.fired" } },
                "no_output": { "kind": "test.tally", "in": { "in": "on.nothing" } },
                "event_to_value": { "kind": "test.tally", "in": { "in": "on.fired", "x": "on.fired" } },
                "bad_set": { "kind": "test.tally", "set": { "x": "three" }, "in": { "in": "on.fired" } },
                "undeclared": { "kind": "test.needy", "params": { "needs": [
                    { "variable": { "name": "nope", "ty": "number" } } ] }, "in": { "in": "on.fired" } },
                "wrong_type": { "kind": "test.needy", "params": { "needs": [
                    { "variable": { "name": "n", "ty": "text" } } ] }, "in": { "in": "on.fired" } },
                "no_group": { "kind": "test.needy", "params": { "needs": [
                    { "group": { "group": "nope" } } ] }, "in": { "in": "on.fired" } },
                "deaf": { "kind": "test.on", "params": { "kind": "nobody.sends.this" } },
                "risky": needy(&[host()]),
                "fine": { "kind": "test.tally", "in": { "in": "on.fired" } },
                "lonely": { "kind": "test.tally" },
            }),
        )],
    );
    let mut h = harness(def);
    let reasons = inert(&h.graphs());
    let expected: BTreeMap<String, InertReason> = [
        ("ghost", InertReason::UnknownKind),
        ("bad_params", InertReason::BadParams),
        ("no_input", InertReason::BadWiring),
        ("no_output", InertReason::BadWiring),
        ("event_to_value", InertReason::BadWiring),
        ("bad_set", InertReason::BadWiring),
        ("undeclared", InertReason::Undeclared),
        ("wrong_type", InertReason::Undeclared),
        ("no_group", InertReason::Undeclared),
        ("deaf", InertReason::UnknownEventKind),
        ("risky", InertReason::not_approved(vec![host()])),
    ]
    .into_iter()
    .map(|(n, r)| (n.to_owned(), r))
    .collect();
    assert_eq!(reasons, expected);
    // One inert finding per inert node, and one note: nothing is wired into `lonely`.
    let findings = h.findings().to_vec();
    let inert_findings: Vec<&str> =
        findings.iter().filter(|f| f.inert).filter_map(|f| f.node.as_ref().map(NodeId::as_str)).collect();
    assert_eq!(inert_findings.len(), expected.len(), "{findings:?}");
    let notes: Vec<&str> =
        findings.iter().filter(|f| !f.inert).filter_map(|f| f.node.as_ref().map(NodeId::as_str)).collect();
    assert_eq!(notes, ["lonely"]);
    // The rest of the show runs.
    h.publish(custom("go", 1.0));
    assert_eq!(tally_args(&h), [1]);
}

#[test]
fn a_value_cycle_makes_both_ends_inert_and_nothing_else() {
    let def = show(
        &[],
        &[(
            "g",
            None,
            json!({
                "a": { "kind": "test.pass", "in": { "x": "b.y" } },
                "b": { "kind": "test.pass", "in": { "x": "a.y" } },
                "c": { "kind": "test.pass", "in": { "x": "a.y" } },
            }),
        )],
    );
    let mut h = harness(def);
    let reasons = inert(&h.graphs());
    assert_eq!(
        reasons,
        BTreeMap::from([("a".to_owned(), InertReason::BadWiring), ("b".to_owned(), InertReason::BadWiring)])
    );
}

#[test]
fn approving_a_host_arms_only_the_nodes_that_needed_it() {
    let other = Need::host(NetScheme::Https, "other.example", 443);
    let def = show(
        &[],
        &[(
            "g",
            None,
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "one": needy(&[host()]),
                "two": needy(std::slice::from_ref(&other)),
                "both": needy(&[host(), other.clone()]),
                "op": needy(&[Need::Scope(Scope::Broadcast), Need::op("obs.scene.set")]),
                "base": needy(&[Need::Scope(Scope::Perform)]),
            }),
        )],
    );
    let mut h = harness(def.clone()).approvals(ShowApprovals::default().with_granted(host()));
    let reasons = inert(&h.graphs());
    assert_eq!(reasons.keys().map(String::as_str).collect::<Vec<_>>(), ["both", "op", "two"]);
    assert_eq!(reasons["both"], InertReason::not_approved(vec![other]));
    h.publish(custom("go", 1.0));
    assert_eq!(ops(&h), ["op.needy", "op.needy"], "`one` and `base` ran");
}

#[test]
fn a_program_runs_only_while_its_file_hashes_as_approved() {
    let files = Arc::new(Files::default());
    files.put("/bin/tool", b"version 1");
    files.put("/scripts/go.lua", b"print('go')");
    let invocation = Invocation::new("/bin/tool").with_script("/scripts/go.lua");
    let pinned =
        ApprovedProgram::new(invocation.clone(), ContentHash::of(b"version 1"), Some(ContentHash::of(b"print('go')")));
    let def = show(
        &[],
        &[(
            "g",
            None,
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "run": needy(&[Need::Program(invocation.clone())]),
            }),
        )],
    );
    let approvals = ShowApprovals::default().with_program(pinned);
    let mut h = harness(def.clone()).approvals(approvals.clone()).hasher(Arc::clone(&files) as Arc<dyn ProgramHasher>);
    assert!(inert(&h.graphs()).is_empty());

    // The script changes on disk: the next compile (a reload) no longer trusts it.
    files.put("/scripts/go.lua", b"os.execute('something else')");
    h.reload(def, None);
    assert_eq!(inert(&h.graphs())["run"], InertReason::not_approved(vec![Need::Program(invocation)]));
    h.publish(custom("go", 1.0));
    assert_eq!(ops(&h), Vec::<String>::new());
}

#[test]
fn compiling_twice_gives_the_same_findings_and_states() {
    let def = show(
        &[],
        &[
            (
                "a",
                None,
                json!({ "on": { "kind": "test.on", "params": { "kind": "custom.go" } }, "x": needy(&[host()]) }),
            ),
            ("b", None, json!({ "p": { "kind": "test.pass", "in": { "x": "q.y" } }, "q": { "kind": "test.pass" } })),
        ],
    );
    let (mut one, mut two) = (harness(def.clone()), harness(def));
    assert_eq!(one.graphs(), two.graphs());
    assert_eq!(serde_json::to_string(one.findings()).unwrap(), serde_json::to_string(two.findings()).unwrap());
}

#[test]
fn a_reload_keeps_unchanged_nodes_and_restarts_the_changed_one() {
    let nodes = |step: u32| {
        json!({
            "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
            "keep": { "kind": "test.tally", "in": { "in": "on.fired" } },
            "change": { "kind": "test.tally", "params": { "step": step }, "in": { "in": "on.fired" } },
        })
    };
    let mut h = harness(show(&[], &[("g", None, nodes(1))]));
    h.publish(custom("go", 1.0));
    h.publish(custom("go", 2.0));
    // `change` then `keep` (ascending node id): (1, 1), (2, 2).
    assert_eq!(tally_args(&h), [1, 1, 2, 2]);

    h.reload(show(&[], &[("g", None, nodes(10))]), None);
    h.publish(custom("go", 3.0));
    // `change` was rebuilt (10 from nothing); `keep` carried its tally on (3).
    assert_eq!(tally_args(&h)[4..], [10, 3]);
    assert_eq!(h.dropped().reload, 0);
}

#[test]
fn changing_a_wire_rebuilds_both_ends_and_drops_their_parked_work() {
    let nodes = |wired: bool| {
        let mut nodes = json!({
            "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
            "src": { "kind": "test.tally", "in": { "in": "on.fired" } },
            "wait": { "kind": "test.wait", "params": { "seconds": 5.0 }, "in": { "in": "on.fired" } },
            "dst": { "kind": "test.tally", "in": { "in": "on.fired" } },
            "other": { "kind": "test.tally", "in": { "in": "on.fired" } },
        });
        if wired {
            nodes["dst"]["in"]["in"] = json!(["on.fired", "src.out"]);
            nodes["wait"]["params"]["seconds"] = json!(6.0);
        }
        nodes
    };
    let mut h = harness(show(&[], &[("g", None, nodes(false))]));
    h.publish(custom("go", 1.0));
    // dst, other, src, each 1; `wait` parks a wake for t = 6.
    assert_eq!(tally_args(&h), [1, 1, 1]);

    h.reload(show(&[], &[("g", None, nodes(true))]), None);
    assert_eq!(h.dropped().reload, 1, "the rebuilt wait's wake is dropped and counted");
    h.publish(custom("go", 2.0));
    // dst (rebuilt) 1, other (kept) 2, src (rebuilt) 1, then src.out pulses dst again: 2.
    assert_eq!(tally_args(&h)[3..], [1, 2, 1, 2]);
    h.advance_to(Seconds(10.0));
    assert_eq!(h.dropped().reload, 1);
}

#[test]
fn approving_while_the_show_runs_arms_the_node_without_a_restart() {
    let def = show(
        &[],
        &[(
            "g",
            None,
            json!({
                "on": { "kind": "test.on", "params": { "kind": "custom.go" } },
                "risky": needy(&[host()]),
                "keep": { "kind": "test.tally", "in": { "in": "on.fired" } },
            }),
        )],
    );
    let mut h = harness(def.clone());
    h.publish(custom("go", 1.0));
    assert_eq!(ops(&h), ["op.tally"]);
    h.reload(def, Some(ShowApprovals::default().with_granted(host())));
    assert!(inert(&h.graphs()).is_empty());
    h.publish(custom("go", 2.0));
    assert_eq!(ops(&h), ["op.tally", "op.tally", "op.needy"]);
    assert_eq!(tally_args(&h), [1, 2], "the tally kept its count across the re-arm");
}
