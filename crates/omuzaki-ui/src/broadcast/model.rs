//! What the Broadcast workspace's panels read about the show (`docs/design/automation.md` D13, D17): the default show
//! as the document holds it, what the engine makes of it (`automation.describe`), the node catalogue
//! (`automation.node_kinds`), and while it runs its status and values (`automation.status`, `automation.values`).
//!
//! Everything here reads: the document through the runner's `get`, the rest through query ops run as the person. A
//! panel reads when the document changes ([`DocWatch`](crate::panels::io::DocWatch)) or on its own clock, never per
//! frame, and every change it makes is an op.

use std::collections::BTreeMap;

use omuzaki_doc_api::Value;
use omuzaki_live_api::graph::{GraphDef, NodeDef};
use omuzaki_live_api::ids::{GraphId, NodeId, PortName, ShowId};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::NodeClass;
use omuzaki_live_api::services::{InertReason, Status, ValuesSnapshot};
use omuzaki_live_api::show::{ShowDef, paths};
use omuzaki_live_api::value::PortType;
use omuzaki_ops_api::{Grants, OpError, Runner};
use serde::Deserialize;
use serde_json::json;

use crate::panels::io::Io;

/// The show the workspace works on: the project's default show.
#[derive(Clone, Debug)]
pub struct Show {
    /// Its id.
    pub id: ShowId,
    /// As the document holds it.
    pub def: ShowDef,
}

impl Show {
    /// Graph `id`, if the show has it.
    pub fn graph(&self, id: &GraphId) -> Option<&GraphDef> {
        self.def.graphs.get(id)
    }

    /// Node `node` of graph `graph`.
    pub fn node(&self, graph: &GraphId, node: &NodeId) -> Option<&NodeDef> {
        self.graph(graph).and_then(|g| g.nodes.get(node))
    }
}

/// The project's default show, or `None` when the project has none yet.
///
/// # Errors
/// What the runner refuses, or `invalid` when the document's show doesn't read as one (a show edited by hand).
pub fn read_show(runner: &dyn Runner) -> Result<Option<Show>, OpError> {
    let grants = Grants::person();
    let Some(default) = runner.get(&paths::default_show(), &grants)? else { return Ok(None) };
    let Some(id) = default.as_str() else { return Ok(None) };
    let id = ShowId::new(id).map_err(|e| OpError::invalid("show", e.to_string()))?;
    let Some(node) = runner.get(&paths::show(&id), &grants)? else { return Ok(None) };
    let def = ShowDef::from_node(&node).map_err(|e| OpError::invalid("show", e.to_string()))?;
    Ok(Some(Show { id, def }))
}

/// One port of a node, as the engine resolved it for the node's parameters.
#[derive(Clone, Debug, Deserialize)]
pub struct PortInfo {
    /// Its name.
    pub name: PortName,
    /// Its type.
    #[serde(rename = "type")]
    pub ty: PortType,
    /// Read at the moment it is used rather than carried by the event.
    #[serde(default)]
    pub live: bool,
    /// The value an unwired input takes.
    #[serde(default)]
    pub default: Option<Value>,
    /// One line on what it carries.
    #[serde(default)]
    pub summary: Option<String>,
}

/// One node as the engine sees it.
#[derive(Clone, Debug, Deserialize)]
pub struct NodeInfo {
    /// Its kind's id, when the document names a kind.
    #[serde(default)]
    pub kind: Option<String>,
    /// Trigger, pure or active.
    pub class: NodeClass,
    /// `"armed"`, or `{"inert": why}`.
    pub state: Value,
    /// Its inputs.
    #[serde(default)]
    pub inputs: Vec<PortInfo>,
    /// Its outputs.
    #[serde(default)]
    pub outputs: Vec<PortInfo>,
}

impl NodeInfo {
    /// Why the node won't run, when it won't.
    pub fn inert(&self) -> Option<InertReason> {
        self.state.get("inert").and_then(|why| serde_json::from_value(why.clone()).ok())
    }

    /// Input `port`.
    pub fn input(&self, port: &str) -> Option<&PortInfo> {
        self.inputs.iter().find(|p| p.name.as_str() == port)
    }

    /// Output `port`.
    pub fn output(&self, port: &str) -> Option<&PortInfo> {
        self.outputs.iter().find(|p| p.name.as_str() == port)
    }
}

/// A finding: something the engine noticed that doesn't stop the edit.
#[derive(Clone, Debug, Deserialize)]
pub struct FindingRow {
    /// The graph it is about.
    #[serde(default)]
    pub graph: Option<GraphId>,
    /// The node it is about.
    #[serde(default)]
    pub node: Option<NodeId>,
    /// In English, from the engine.
    pub text: String,
}

/// One of the show's needs and whether this machine approved it.
#[derive(Clone, Debug, Deserialize)]
pub struct NeedRow {
    /// The need.
    pub need: Need,
    /// Approved on this machine.
    pub approved: bool,
}

/// `automation.describe`'s answer.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Described {
    /// Each graph's nodes, by id.
    #[serde(default)]
    pub graphs: BTreeMap<GraphId, DescribedGraph>,
    /// The findings.
    #[serde(default)]
    pub findings: Vec<FindingRow>,
    /// The needs that need a person's approval.
    #[serde(default)]
    pub needs: Vec<NeedRow>,
}

/// One graph of [`Described`].
#[derive(Clone, Debug, Default, Deserialize)]
pub struct DescribedGraph {
    /// Its nodes.
    #[serde(default)]
    pub nodes: BTreeMap<NodeId, NodeInfo>,
}

impl Described {
    /// Node `node` of graph `graph`.
    pub fn node(&self, graph: &GraphId, node: &NodeId) -> Option<&NodeInfo> {
        self.graphs.get(graph).and_then(|g| g.nodes.get(node))
    }

    /// The needs not yet approved.
    pub fn missing(&self) -> Vec<&Need> {
        self.needs.iter().filter(|n| !n.approved).map(|n| &n.need).collect()
    }
}

/// What the engine makes of `show`.
///
/// # Errors
/// What the runner refuses, or `internal` when the answer doesn't read.
pub fn describe(io: &Io<'_>, show: &ShowId) -> Result<Described, OpError> {
    let answer = io.ask("automation.describe", json!({ "show": show }))?;
    serde_json::from_value(answer).map_err(|e| OpError::internal(format!("automation.describe: {e}")))
}

/// One node kind from the catalogue.
#[derive(Clone, Debug, Deserialize)]
pub struct KindInfo {
    /// Its id (`flow.cooldown`).
    pub id: String,
    /// One line on what it does, in English.
    pub summary: String,
    /// Trigger, pure or active.
    pub class: NodeClass,
    /// Its palette group.
    pub category: String,
    /// Its parameters' JSON Schema.
    #[serde(default)]
    pub params: Value,
    /// Its default parameters.
    #[serde(default)]
    pub defaults: Value,
}

/// The node catalogue, sorted by id.
///
/// # Errors
/// What the runner refuses, or `internal` when the answer doesn't read.
pub fn node_kinds(io: &Io<'_>) -> Result<Vec<KindInfo>, OpError> {
    #[derive(Deserialize)]
    struct Kinds {
        kinds: Vec<KindInfo>,
    }
    let answer = io.ask("automation.node_kinds", json!({}))?;
    serde_json::from_value::<Kinds>(answer)
        .map(|k| k.kinds)
        .map_err(|e| OpError::internal(format!("automation.node_kinds: {e}")))
}

/// The running show's status.
///
/// # Errors
/// What the runner refuses (no automation engine in this build), or `internal` when the answer doesn't read.
pub fn status(io: &Io<'_>) -> Result<Status, OpError> {
    let answer = io.ask("automation.status", json!({}))?;
    serde_json::from_value(answer).map_err(|e| OpError::internal(format!("automation.status: {e}")))
}

/// The running show's values, or `None` while no show runs.
///
/// # Errors
/// What the runner refuses, or `internal` when the answer doesn't read.
pub fn values(io: &Io<'_>) -> Result<Option<ValuesSnapshot>, OpError> {
    let answer = io.ask("automation.values", json!({}))?;
    if answer["running"].as_bool() != Some(true) {
        return Ok(None);
    }
    serde_json::from_value(answer["values"].clone())
        .map(Some)
        .map_err(|e| OpError::internal(format!("automation.values: {e}")))
}

/// A graph read as a straight chain (D17): one trigger, then one node after another, each on a single event wire from
/// the one before, with value inputs only from constants or the trigger. The Rules table shows such a graph as one
/// row and edits it inline; any other graph opens in the editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    /// The trigger.
    pub trigger: NodeId,
    /// The steps after it, in order.
    pub steps: Vec<NodeId>,
}

/// `graph` as a [`Chain`], when it is one. `nodes` is what the engine resolved for it (each node's class and ports).
pub fn chain(graph: &GraphDef, nodes: &BTreeMap<NodeId, NodeInfo>) -> Option<Chain> {
    let class = |id: &NodeId| nodes.get(id).map(|n| n.class);
    let mut triggers = graph.nodes.keys().filter(|id| class(id) == Some(NodeClass::Trigger));
    let trigger = triggers.next()?.clone();
    if triggers.next().is_some() {
        return None;
    }
    // Each step's one event source, and no value from anywhere but the trigger.
    let mut after: BTreeMap<NodeId, NodeId> = BTreeMap::new();
    for (id, node) in &graph.nodes {
        if *id == trigger {
            continue;
        }
        let info = nodes.get(id)?;
        let mut event_source = None;
        for (port, wire) in &node.inputs {
            let is_event = info.input(port.as_str()).is_some_and(|p| p.ty.is_event());
            let sources = wire.sources();
            if is_event {
                if event_source.is_some() || sources.len() != 1 {
                    return None;
                }
                event_source = Some(sources[0].node.clone());
            } else if sources.iter().any(|s| s.node != trigger) {
                return None;
            }
        }
        let from = event_source?;
        if after.insert(from, id.clone()).is_some() {
            return None;
        }
    }
    let mut steps = Vec::new();
    let mut at = trigger.clone();
    while let Some(next) = after.get(&at) {
        if steps.contains(next) {
            return None;
        }
        steps.push(next.clone());
        at = next.clone();
    }
    (steps.len() + 1 == graph.nodes.len()).then_some(Chain { trigger, steps })
}

/// A node's name for people: its label, else its kind's summary up to the first full stop or colon, else its id.
pub fn node_name(id: &NodeId, node: &NodeDef, kinds: &[KindInfo]) -> String {
    if let Some(label) = &node.label {
        return label.clone();
    }
    let summary = kinds.iter().find(|k| k.id == node.kind.as_str()).map(|k| k.summary.as_str());
    match summary {
        Some(summary) => {
            let cut = summary.find(['.', ':']).unwrap_or(summary.len());
            summary[..cut].to_owned()
        }
        None => id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(name: &'static str, ty: PortType) -> PortInfo {
        PortInfo { name: PortName::from_static(name), ty, live: false, default: None, summary: None }
    }

    fn info(class: NodeClass, inputs: Vec<PortInfo>) -> NodeInfo {
        NodeInfo { kind: None, class, state: json!("armed"), inputs, outputs: Vec::new() }
    }

    fn id(s: &str) -> NodeId {
        NodeId::new(s).unwrap_or_else(|e| panic!("{e}"))
    }

    fn graph(nodes: &Value) -> GraphDef {
        GraphDef::from_node(&json!({ "nodes": nodes })).unwrap_or_else(|e| panic!("{e}"))
    }

    fn infos() -> BTreeMap<NodeId, NodeInfo> {
        let step = || info(NodeClass::Active, vec![port("in", PortType::Event), port("text", PortType::Text)]);
        BTreeMap::from([
            (id("cmd"), info(NodeClass::Trigger, Vec::new())),
            (id("wait"), step()),
            (id("say"), step()),
            (id("other"), step()),
        ])
    }

    #[test]
    fn a_trigger_then_steps_on_single_event_wires_is_a_chain_in_order() {
        let g = graph(&json!({
            "cmd": { "kind": "chat.command" },
            "say": { "kind": "op.run", "in": { "in": "wait.out", "text": "cmd.user" } },
            "wait": { "kind": "flow.wait", "in": { "in": "cmd.fired" } },
        }));
        assert_eq!(chain(&g, &infos()), Some(Chain { trigger: id("cmd"), steps: vec![id("wait"), id("say")] }));
    }

    #[test]
    fn a_fan_out_a_value_from_a_step_or_two_triggers_is_not() {
        let fan = graph(&json!({
            "cmd": { "kind": "chat.command" },
            "say": { "kind": "op.run", "in": { "in": "cmd.fired" } },
            "wait": { "kind": "flow.wait", "in": { "in": "cmd.fired" } },
        }));
        assert_eq!(chain(&fan, &infos()), None, "two steps after the trigger");
        let value = graph(&json!({
            "cmd": { "kind": "chat.command" },
            "say": { "kind": "op.run", "in": { "in": "wait.out", "text": "wait.text" } },
            "wait": { "kind": "flow.wait", "in": { "in": "cmd.fired" } },
        }));
        assert_eq!(chain(&value, &infos()), None, "a value from a step, not the trigger");
        let mut two = infos();
        two.insert(id("other"), info(NodeClass::Trigger, Vec::new()));
        let triggers = graph(&json!({ "cmd": { "kind": "chat.command" }, "other": { "kind": "chat.command" } }));
        assert_eq!(chain(&triggers, &two), None);
        let loose = graph(&json!({ "cmd": { "kind": "chat.command" }, "say": { "kind": "op.run" } }));
        assert_eq!(chain(&loose, &infos()), None, "a step nothing runs");
    }

    #[test]
    fn a_node_is_named_by_its_label_or_its_kinds_first_clause() {
        let kinds = vec![KindInfo {
            id: "op.run".into(),
            summary: "Run an op: switch a scene.".into(),
            class: NodeClass::Active,
            category: "actions".into(),
            params: Value::Null,
            defaults: Value::Null,
        }];
        let node = NodeDef::new(omuzaki_live_api::ids::NodeKindId::from_static("op.run"));
        assert_eq!(node_name(&id("say"), &node, &kinds), "Run an op");
        assert_eq!(node_name(&id("say"), &node.clone().with_label("Say hi"), &kinds), "Say hi");
        assert_eq!(node_name(&id("say"), &node, &[]), "say");
    }
}
