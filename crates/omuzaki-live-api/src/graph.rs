//! A graph (design D2): [`GraphDef`] over `shows/<show>/automations/<graph>/`, one [`NodeDef`] per node file, and the
//! [`Wire`] each input stores at its destination.
//!
//! The graph's node holds `graph` (`graph.toml`: name, enabled, group, summary) and `nodes` (one `<node>.toml` each).
//! [`GraphDef`] serialises in exactly that shape, so `automation.add_graph`'s `graph` argument, a template's step and
//! the document all read alike.

use std::collections::BTreeMap;

use omuzaki_doc_api::Value;
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Map;

use crate::ids::{GroupId, NodeId, NodeKindId, PortName, PortRef};
use crate::show::unknown_key;

/// A graph: its settings and its nodes. Built with [`GraphDef::new`] and the `with_*` methods.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct GraphDef {
    /// Display name; the id stands in when absent.
    pub name: Option<String>,
    /// The person's design-time switch (the Rules table's checkbox). Default `true`; written only when `false`.
    pub enabled: bool,
    /// The group it belongs to, at most one (design D12).
    pub group: Option<GroupId>,
    /// One line on what it does, for the Rules table and agents.
    pub summary: Option<String>,
    /// The nodes, by id.
    pub nodes: BTreeMap<NodeId, NodeDef>,
}

impl Default for GraphDef {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphDef {
    /// The keys of a graph node (`shows/<show>/automations/<graph>`).
    pub const NODE_KEYS: [&'static str; 2] = ["graph", "nodes"];

    /// An enabled graph with no name, group, summary or nodes.
    pub fn new() -> Self {
        Self { name: None, enabled: true, group: None, summary: None, nodes: BTreeMap::new() }
    }

    /// With this display name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Enabled or not.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// In this group.
    #[must_use]
    pub fn with_group(mut self, group: GroupId) -> Self {
        self.group = Some(group);
        self
    }

    /// With this summary.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// With this node (replacing one of the same id).
    #[must_use]
    pub fn with_node(mut self, id: NodeId, node: NodeDef) -> Self {
        self.nodes.insert(id, node);
        self
    }

    /// The graph stored in the graph node `node`. A key other than [`NODE_KEYS`](Self::NODE_KEYS) is refused with
    /// the nearest valid one, so a misspelt `node` can't read as an absent `nodes` and be lost on the next save.
    ///
    /// # Errors
    /// The [`serde_json::Error`] naming the unknown key or the field that doesn't match the format.
    pub fn from_node(node: &Value) -> Result<Self, serde_json::Error> {
        if let Value::Object(map) = node
            && let Some(unknown) = map.keys().find(|k| !Self::NODE_KEYS.contains(&k.as_str()))
        {
            return Err(unknown_key(unknown, &Self::NODE_KEYS, "a graph"));
        }
        serde_json::from_value(node.clone())
    }

    /// Writes this graph into the graph node `into`: `graph` always (a graph is at least its `graph.toml`), `nodes`
    /// only when there are any. A node that is not a table becomes one.
    ///
    /// # Errors
    /// The [`serde_json::Error`] if the graph can't be turned into a table (it can for any value the type holds).
    pub fn to_node(&self, into: &mut Value) -> Result<(), serde_json::Error> {
        let Value::Object(written) = serde_json::to_value(self)? else {
            return Err(<serde_json::Error as serde::ser::Error>::custom("a graph is a table"));
        };
        match into {
            Value::Object(node) => {
                node.retain(|key, _| !Self::NODE_KEYS.contains(&key.as_str()) || written.contains_key(key));
                node.extend(written);
            }
            other => *other = Value::Object(written),
        }
        Ok(())
    }
}

/// `graph.toml`.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GraphFile {
    /// Display name; the id stands in when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    /// Whether the graph runs; `true` when absent.
    #[serde(default = "enabled_default", skip_serializing_if = "is_true")]
    enabled: bool,
    /// The group it belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    group: Option<GroupId>,
    /// One line on what it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
}

/// A graph node: its `graph.toml` and its node files.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "GraphDef", description = "A graph: its settings (`graph`) and its nodes (`nodes`), by id.")]
struct GraphNode {
    /// The graph's settings (`graph.toml`).
    #[serde(default)]
    graph: GraphFile,
    /// The nodes, by id (`nodes/<id>.toml`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    nodes: BTreeMap<NodeId, NodeDef>,
}

/// An absent `graph.toml` is every setting absent: enabled, as an absent `enabled` is.
impl Default for GraphFile {
    fn default() -> Self {
        Self { name: None, enabled: enabled_default(), group: None, summary: None }
    }
}

fn enabled_default() -> bool {
    true
}

// serde's `skip_serializing_if` passes a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_true(b: &bool) -> bool {
    *b
}

impl Serialize for GraphDef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let graph = GraphFile {
            name: self.name.clone(),
            enabled: self.enabled,
            group: self.group.clone(),
            summary: self.summary.clone(),
        };
        GraphNode { graph, nodes: self.nodes.clone() }.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for GraphDef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let GraphNode { graph, nodes } = GraphNode::deserialize(deserializer)?;
        Ok(Self { name: graph.name, enabled: graph.enabled, group: graph.group, summary: graph.summary, nodes })
    }
}

impl JsonSchema for GraphDef {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "GraphDef".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        GraphNode::json_schema(generator)
    }
}

/// One node: a file `nodes/<id>.toml`. Built with [`NodeDef::new`] and the `with_*` methods.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct NodeDef {
    /// The node kind (`flow.wait`).
    pub kind: NodeKindId,
    /// What the editor shows instead of the kind's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The editor position, in points. Written as the digits a person would type.
    #[serde(default, serialize_with = "short_pos")]
    pub pos: [f32; 2],
    /// The kind's parameters: they decide its ports, ids and templates and are checked against its schema.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    /// The wires into its inputs (`in` in the file): one source for a value input, one or more for an event input.
    #[serde(rename = "in", default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<PortName, Wire>,
    /// Constants for unwired value inputs.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub set: BTreeMap<PortName, Value>,
}

impl NodeDef {
    /// A node of `kind` at the origin, with no label, parameters, wires or constants.
    pub fn new(kind: NodeKindId) -> Self {
        Self { kind, label: None, pos: [0.0, 0.0], params: Map::new(), inputs: BTreeMap::new(), set: BTreeMap::new() }
    }

    /// With this label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// At this editor position.
    #[must_use]
    pub fn with_pos(mut self, pos: [f32; 2]) -> Self {
        self.pos = pos;
        self
    }

    /// With this parameter.
    #[must_use]
    pub fn with_param(mut self, key: impl Into<String>, value: Value) -> Self {
        self.params.insert(key.into(), value);
        self
    }

    /// With this wire into `port`.
    #[must_use]
    pub fn with_input(mut self, port: PortName, wire: Wire) -> Self {
        self.inputs.insert(port, wire);
        self
    }

    /// With this constant for `port`.
    #[must_use]
    pub fn with_set(mut self, port: PortName, value: Value) -> Self {
        self.set.insert(port, value);
        self
    }
}

/// Writes each coordinate as the shortest decimal that reads back as the same `f32`, so `0.9` stays `0.9` instead of
/// becoming the `f64` widening `0.899999976158142`.
// serde's `serialize_with` passes a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn short_pos<S: Serializer>(pos: &[f32; 2], serializer: S) -> Result<S::Ok, S::Error> {
    let short = pos.map(|x| x.to_string().parse::<f64>().unwrap_or(f64::from(x)));
    short.serialize(serializer)
}

/// The wires into one input, written at the destination (design D2): a string for one source, a list for several.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Wire {
    /// One source: `in.text = "line.text"`.
    One(PortRef),
    /// Several sources, for an event input: `in.in = ["redeem.fired", "cmd.fired"]`.
    Many(Vec<PortRef>),
}

impl Wire {
    /// The sources, in the order written.
    pub fn sources(&self) -> &[PortRef] {
        match self {
            Wire::One(source) => std::slice::from_ref(source),
            Wire::Many(sources) => sources,
        }
    }
}

impl From<PortRef> for Wire {
    fn from(source: PortRef) -> Self {
        Wire::One(source)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn omitted_settings_take_their_defaults() {
        let only_nodes = json!({"nodes": {"cool": {"kind": "flow.cooldown"}}});
        for node in [only_nodes, json!({"graph": {}}), json!({})] {
            let graph = GraphDef::from_node(&node).unwrap();
            assert!(graph.enabled, "{node}");
            assert_eq!((graph.name.as_deref(), graph.group.as_ref(), graph.summary.as_deref()), (None, None, None));
        }
        let off = GraphDef::from_node(&json!({"graph": {"enabled": false}})).unwrap();
        assert!(!off.enabled);
    }

    #[test]
    fn a_graph_is_its_settings_and_its_nodes() {
        let node = json!({"graph": {"name": "Bonk", "group": "redeems"}, "nodes": {"cool": {"kind": "flow.cooldown", "pos": [220.0, 0.0], "in": {"in": "redeem.fired"}}}});
        let graph = GraphDef::from_node(&node).unwrap();
        assert!(graph.enabled);
        assert_eq!(graph.nodes[&NodeId::new("cool").unwrap()].inputs[&PortName::from_static("in")].sources().len(), 1);
        let mut back = Value::Null;
        graph.to_node(&mut back).unwrap();
        assert_eq!(back, node);
    }

    #[test]
    fn a_misspelt_key_names_the_nearest() {
        let err = GraphDef::from_node(&json!({"graph": {}, "node": {}})).unwrap_err().to_string();
        assert!(err.contains("`node`") && err.contains("`nodes`"), "{err}");
        let err = GraphDef::from_node(&json!({"graph": {"nmae": "x"}})).unwrap_err().to_string();
        assert!(err.contains("nmae"), "{err}");
    }

    #[test]
    fn an_empty_graph_keeps_its_graph_file_and_a_disabled_one_says_so() {
        let mut node = Value::Null;
        GraphDef::new().with_enabled(false).to_node(&mut node).unwrap();
        assert_eq!(node, json!({"graph": {"enabled": false}}));
        GraphDef::new().to_node(&mut node).unwrap();
        assert_eq!(node, json!({"graph": {}}));
    }

    #[test]
    fn positions_keep_the_digits_written() {
        let node = NodeDef::new(NodeKindId::from_static("flow.wait")).with_pos([0.9, -80.0]);
        assert_eq!(serde_json::to_value(&node).unwrap()["pos"], json!([0.9, -80.0]));
    }

    #[test]
    fn a_wire_is_a_string_or_a_list() {
        let many: Wire = serde_json::from_value(json!(["redeem.fired", "cmd.fired"])).unwrap();
        assert_eq!(many.sources().len(), 2);
        assert_eq!(
            serde_json::to_value(Wire::from("line.text".parse::<PortRef>().unwrap())).unwrap(),
            json!("line.text")
        );
    }
}
