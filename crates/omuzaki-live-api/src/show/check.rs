//! [`ShowDef::check`]: the structural rules of design D18 that need no node catalogue, shared by the document ops (which
//! refuse an edit that breaks one) and the engine's compile (which loads a show edited by hand with findings instead).
//!
//! The rules that need the catalogue (ports, types, value cycles, needs, `op.run` targets) are the engine's (#1175);
//! they report the same [`Finding`].

use std::collections::BTreeSet;

use omuzaki_l10n_api::Text;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ShowDef;
use super::keys::{self, KeyKind};
use crate::graph::GraphDef;
use crate::ids::{GraphId, NodeId, PortName};
use crate::l10n;
use crate::limits::{MAX_GRAPHS, MAX_NODES_PER_GRAPH, MAX_PARAMS_BYTES, MAX_PORTS_PER_NODE, MAX_VARIABLES};

/// Something wrong (or worth saying) about a show, at a graph, a node or a feature key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Finding {
    /// The graph it is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphId>,
    /// The node it is about, if any (within [`graph`](Self::graph)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<NodeId>,
    /// The feature key it is about, if any (design D21).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// What is wrong, for people.
    pub text: Text,
    /// Whether the thing it names can't run: the node, else the graph. The engine keeps it inert.
    pub inert: bool,
    /// Whether a document op refuses an edit that leaves this finding (design D18, first column).
    pub refuses: bool,
}

impl Finding {
    /// A rule the document ops refuse to break; the node or graph it names is inert when a show is loaded with it.
    pub fn refusal(text: Text) -> Self {
        Self { graph: None, node: None, key: None, text, inert: true, refuses: true }
    }

    /// Something the edit may leave, that makes the node or graph it names inert (design D18, second column).
    pub fn inert(text: Text) -> Self {
        Self { graph: None, node: None, key: None, text, inert: true, refuses: false }
    }

    /// Something worth saying that changes nothing.
    pub fn note(text: Text) -> Self {
        Self { graph: None, node: None, key: None, text, inert: false, refuses: false }
    }

    /// About this graph.
    #[must_use]
    pub fn at_graph(mut self, graph: &GraphId) -> Self {
        self.graph = Some(graph.clone());
        self
    }

    /// About this node of this graph.
    #[must_use]
    pub fn at_node(mut self, graph: &GraphId, node: &NodeId) -> Self {
        self.graph = Some(graph.clone());
        self.node = Some(node.clone());
        self
    }

    /// About this feature key.
    #[must_use]
    pub fn at_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Show-wide: refuses an edit, but names nothing that becomes inert.
    #[must_use]
    fn show_wide(mut self) -> Self {
        self.inert = false;
        self
    }
}

/// A count as a message argument; counts here are bounded far below `i64::MAX`.
fn int(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl ShowDef {
    /// The structural rules (design D18, first column, without the catalogue), plus each feature key's owner check or
    /// its "kept, not checked" note (design D21). Findings come in a stable order: show-wide first, then by graph and
    /// node, each in rule order.
    pub fn check(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        if self.graphs.len() > MAX_GRAPHS {
            let text = l10n::too_many_graphs(int(self.graphs.len()), int(MAX_GRAPHS));
            findings.push(Finding::refusal(text).show_wide());
        }
        if self.variables.len() > MAX_VARIABLES {
            let text = l10n::too_many_variables(int(self.variables.len()), int(MAX_VARIABLES));
            findings.push(Finding::refusal(text).show_wide());
        }
        for (name, def) in &self.variables {
            if def.default.as_ref().is_some_and(|value| !def.ty.holds(value)) {
                let text = l10n::variable_default_type(name.as_str(), def.ty.as_str());
                findings.push(Finding::refusal(text).show_wide());
            }
        }
        for (key, value) in &self.features {
            match keys::classify(key) {
                Ok(KeyKind::Registered(registration)) => {
                    let owned = registration.check.map(|check| check(value)).unwrap_or_default();
                    findings.extend(owned.into_iter().map(|f| f.at_key(key.as_str())));
                }
                Ok(KeyKind::Unchecked) => {
                    findings.push(Finding::note(l10n::key_kept_unchecked(key.as_str())).at_key(key.as_str()))
                }
                // `from_node` refuses these, so only a hand-built `ShowDef` reaches here.
                Ok(KeyKind::BuiltIn) => {
                    let text = l10n::key_refused(key.as_str(), "it is one of the show's own keys");
                    findings.push(Finding::refusal(text).at_key(key.as_str()).show_wide());
                }
                Err(why) => {
                    findings.push(
                        Finding::refusal(l10n::key_refused(key.as_str(), why.to_string()))
                            .at_key(key.as_str())
                            .show_wide(),
                    );
                }
            }
        }
        for (id, graph) in &self.graphs {
            self.check_graph(id, graph, &mut findings);
        }
        findings
    }

    fn check_graph(&self, id: &GraphId, graph: &GraphDef, findings: &mut Vec<Finding>) {
        if let Some(group) = &graph.group
            && !self.groups.contains_key(group)
        {
            findings.push(Finding::refusal(l10n::graph_unknown_group(id.as_str(), group.as_str())).at_graph(id));
        }
        if graph.nodes.len() > MAX_NODES_PER_GRAPH {
            let text = l10n::too_many_nodes(id.as_str(), int(graph.nodes.len()), int(MAX_NODES_PER_GRAPH));
            findings.push(Finding::refusal(text).at_graph(id));
        }
        for (node_id, node) in &graph.nodes {
            let at = |text: Text| Finding::refusal(text).at_node(id, node_id);
            for (port, wire) in &node.inputs {
                if wire.sources().is_empty() {
                    findings.push(at(l10n::wire_empty(id.as_str(), node_id.as_str(), port.as_str())));
                }
                for source in wire.sources().iter().filter(|s| !graph.nodes.contains_key(&s.node)) {
                    let text =
                        l10n::wire_missing_node(id.as_str(), node_id.as_str(), port.as_str(), source.to_string());
                    findings.push(at(text));
                }
                if node.set.contains_key(port) {
                    findings.push(at(l10n::port_wired_and_set(id.as_str(), node_id.as_str(), port.as_str())));
                }
            }
            let ports: BTreeSet<&str> = node.inputs.keys().chain(node.set.keys()).map(PortName::as_str).collect();
            if ports.len() > MAX_PORTS_PER_NODE {
                let text =
                    l10n::too_many_ports(id.as_str(), node_id.as_str(), int(ports.len()), int(MAX_PORTS_PER_NODE));
                findings.push(at(text));
            }
            let bytes = serde_json::to_vec(&node.params).map_or(0, |b| b.len());
            if bytes > MAX_PARAMS_BYTES {
                findings.push(at(l10n::params_too_large(
                    id.as_str(),
                    node_id.as_str(),
                    int(bytes),
                    int(MAX_PARAMS_BYTES),
                )));
            }
        }
    }
}
