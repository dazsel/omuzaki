//! Compiling a [`ShowDef`] against the node catalogue: every node gets its ports, the event kinds it listens to and a
//! fresh instance; every wire is resolved to node indices; a node that can't run (an unknown kind, parameters its kind
//! refuses, a wire to a node or port that isn't there) is kept **inert** with a reason, so one bad node never stops
//! the show (design D5, D18).
//!
//! Nodes are numbered in (graph id, node id) order, so ascending index *is* the ordering the engine's rules use:
//! triggers run in ascending (graph, node), and a pulse goes to its destinations in ascending (node, port).

use std::collections::BTreeMap;

use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, NodeId, PortName, ShowId, VarName};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::{DynNodeKind, Node, NodeClass, Ports, find_node_kind};
use omuzaki_live_api::services::InertReason;
use omuzaki_live_api::show::{Finding, ShowDef};
use serde_json::Value;

/// One node as the engine runs it: everything but the instance, which the engine holds apart so a node can be
/// borrowed mutably while the rest of the show is read.
#[derive(Debug)]
pub(crate) struct CompiledNode {
    pub graph: GraphId,
    pub id: NodeId,
    pub ports: Ports,
    pub class: NodeClass,
    /// Event kinds a trigger listens to.
    pub listens: Vec<EventKind>,
    /// The variables it reads or writes, from its needs: a global whose type another show settled makes it inert.
    pub vars: Vec<VarName>,
    /// Each wired value input's source: (node index, output port).
    pub value_wires: BTreeMap<PortName, (usize, PortName)>,
    /// Constants for unwired value inputs.
    pub set: BTreeMap<PortName, Value>,
    /// Each event output's destinations, ascending (node index, input port).
    pub outs: BTreeMap<PortName, Vec<(usize, PortName)>>,
    /// Why it can't run, if it can't.
    pub inert: Option<InertReason>,
    /// Whether its graph runs: enabled and its group on (design D12).
    pub active: bool,
}

/// A compiled show: the nodes, their instances (`None` for an inert node), the trigger index and the findings.
pub(crate) struct Compiled {
    pub show: ShowId,
    pub def: ShowDef,
    pub nodes: Vec<CompiledNode>,
    pub instances: Vec<Option<Box<dyn Node>>>,
    /// For each event kind, the triggers listening to it, ascending.
    pub triggers: BTreeMap<EventKind, Vec<usize>>,
    /// What the structural check found.
    pub findings: Vec<Finding>,
}

/// Whether a graph runs at start: enabled, and in no group or in a group that is on.
fn graph_active(def: &ShowDef, graph: &GraphDef) -> bool {
    graph.enabled && graph.group.as_ref().is_none_or(|g| def.groups.get(g).is_none_or(|group| group.enabled))
}

/// Compiles `def`. Never fails: whatever can't run is inert, and the structural findings ride along for the caller
/// to show (or refuse to start on).
pub(crate) fn compile(show: &ShowId, def: &ShowDef) -> Compiled {
    let findings = def.check();
    let mut index: BTreeMap<(&GraphId, &NodeId), usize> = BTreeMap::new();
    let mut nodes = Vec::new();
    let mut instances = Vec::new();
    for (graph_id, graph) in &def.graphs {
        let active = graph_active(def, graph);
        for (node_id, node) in &graph.nodes {
            index.insert((graph_id, node_id), nodes.len());
            let params = Value::Object(node.params.clone());
            let built = find_node_kind(node.kind.as_str()).ok_or(InertReason::UnknownKind).and_then(|kind| {
                let meta = kind.meta();
                let ports = DynNodeKind::ports(kind, &params).map_err(|_| InertReason::BadParams)?;
                let listens = kind.listens(&params).map_err(|_| InertReason::BadParams)?;
                let instance = kind.instantiate(&params).map_err(|_| InertReason::BadParams)?;
                let vars = kind
                    .needs(&params)
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|need| match need {
                        Need::Variable { name, .. } => Some(name),
                        _ => None,
                    })
                    .collect();
                Ok((meta.class, ports, listens, vars, instance))
            });
            let (class, ports, listens, vars, instance, inert) = match built {
                Ok((class, ports, listens, vars, instance)) => (class, ports, listens, vars, Some(instance), None),
                Err(why) => (NodeClass::Active, Ports::new(), Vec::new(), Vec::new(), None, Some(why)),
            };
            let inert = inert.or_else(|| {
                findings
                    .iter()
                    .any(|f| f.inert && f.graph.as_ref() == Some(graph_id) && f.node.as_ref() == Some(node_id))
                    .then_some(InertReason::BadParams)
            });
            nodes.push(CompiledNode {
                graph: graph_id.clone(),
                id: node_id.clone(),
                ports,
                class,
                listens,
                vars,
                value_wires: BTreeMap::new(),
                set: node.set.clone(),
                outs: BTreeMap::new(),
                inert,
                active,
            });
            instances.push(instance);
        }
    }
    // Wires are written at their destination; resolve each to indices and file event wires under their source.
    let mut event_wires: Vec<(usize, PortName, usize, PortName)> = Vec::new();
    for (graph_id, graph) in &def.graphs {
        for (node_id, node) in &graph.nodes {
            let Some(&dest) = index.get(&(graph_id, node_id)) else { continue };
            for (port, wire) in &node.inputs {
                for source in wire.sources() {
                    let Some(&src) = index.get(&(graph_id, &source.node)) else { continue };
                    let Some(input) = nodes[dest].ports.input(port.as_str()) else { continue };
                    let Some(output) = nodes[src].ports.output(source.port.as_str()) else { continue };
                    if input.ty.is_event() != output.ty.is_event() {
                        continue;
                    }
                    if input.ty.is_event() {
                        event_wires.push((src, source.port.clone(), dest, port.clone()));
                    } else {
                        nodes[dest].value_wires.entry(port.clone()).or_insert((src, source.port.clone()));
                    }
                }
            }
        }
    }
    for (src, out, dest, input) in event_wires {
        nodes[src].outs.entry(out).or_default().push((dest, input));
    }
    for node in &mut nodes {
        for dests in node.outs.values_mut() {
            dests.sort();
            dests.dedup();
        }
    }
    let mut triggers: BTreeMap<EventKind, Vec<usize>> = BTreeMap::new();
    for (i, node) in nodes.iter().enumerate() {
        if node.class == NodeClass::Trigger && node.inert.is_none() {
            for kind in &node.listens {
                triggers.entry(kind.clone()).or_default().push(i);
            }
        }
    }
    Compiled { show: show.clone(), def: def.clone(), nodes, instances, triggers, findings }
}
