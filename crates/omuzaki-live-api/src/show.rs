//! A show (design D2): [`ShowDef`] over `shows/<id>/`, the variable and group declarations in its `show.toml`, its
//! document [`paths`], the keys other features add under it ([`keys`]) and its structural [`check`](ShowDef::check).
//!
//! ```text
//! shows/<show>/show.toml                               name, seed, [variables.<name>], [groups.<id>]
//! shows/<show>/automations/<graph>/graph.toml          name, enabled, group, summary
//! shows/<show>/automations/<graph>/nodes/<node>.toml   kind, label, pos, [params], [in], [set]
//! shows/<show>/applied/<app>.toml                      library records (ADR 0019)
//! ```
//!
//! Runtime state (variable values, group switches, suspensions, approvals) is never in the document.

pub mod check;
pub mod keys;
pub mod paths;

use std::collections::BTreeMap;

use omuzaki_core::text::{edit_distance, nearest};
use omuzaki_doc_api::Value;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::graph::GraphDef;
use crate::ids::{GraphId, GroupId, VarName};
use crate::value::PortType;

pub use check::Finding;

/// A show: its settings, declarations and graphs, and the opaque sub-trees of the keys other features add.
/// Everything under `shows/<id>/` except the library's `applied` records. Built with [`ShowDef::new`] and the
/// `with_*` methods.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ShowDef {
    /// Display name.
    pub name: String,
    /// The seed every node's random stream derives from (design D6); drawn at start and reported when absent.
    pub seed: Option<u32>,
    /// The declared variables (design D10).
    pub variables: BTreeMap<VarName, VariableDef>,
    /// The declared groups (design D12).
    pub groups: BTreeMap<GroupId, GroupDef>,
    /// The graphs (`automations/<id>/`).
    pub graphs: BTreeMap<GraphId, GraphDef>,
    /// The sub-trees of keys other features add under the show (overlays, outputs, plugins), kept exactly as read
    /// and written back unchanged (design D21). Their content is checked only by their owner ([`keys`]).
    pub features: BTreeMap<String, Value>,
}

impl ShowDef {
    /// The keys of `shows/<id>` that make up a [`ShowDef`]: `show.toml` and the graphs.
    pub const NODE_KEYS: [&'static str; 2] = ["show", "automations"];

    /// The other built-in key: the library's `applied` records. [`from_node`](Self::from_node) skips it and
    /// [`to_node`](Self::to_node) leaves it alone.
    pub const SIBLING_KEYS: [&'static str; 1] = ["applied"];

    /// A show called `name`, with no seed, declarations, graphs or feature keys.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            seed: None,
            variables: BTreeMap::new(),
            groups: BTreeMap::new(),
            graphs: BTreeMap::new(),
            features: BTreeMap::new(),
        }
    }

    /// With this seed.
    #[must_use]
    pub fn with_seed(mut self, seed: u32) -> Self {
        self.seed = Some(seed);
        self
    }

    /// With this variable declared.
    #[must_use]
    pub fn with_variable(mut self, name: VarName, def: VariableDef) -> Self {
        self.variables.insert(name, def);
        self
    }

    /// With this group declared.
    #[must_use]
    pub fn with_group(mut self, id: GroupId, def: GroupDef) -> Self {
        self.groups.insert(id, def);
        self
    }

    /// With this graph.
    #[must_use]
    pub fn with_graph(mut self, id: GraphId, graph: GraphDef) -> Self {
        self.graphs.insert(id, graph);
        self
    }

    /// The show stored in the show node `node` (`shows/<id>`).
    ///
    /// Reads `show` and `automations`, skips `applied`, and keeps every key another feature adds as an opaque
    /// sub-tree in [`features`](Self::features), whether or not this binary links its owner (design D21). A key that
    /// isn't a slug, or that is a misspelt built-in key (within two edits of one), is refused with the key it
    /// resembles, so a misspelt `automatons` can't read as an absent `automations` and be lost on the next save. This
    /// checks the format only; [`check`](Self::check) checks the rules.
    ///
    /// # Errors
    /// The [`serde_json::Error`] naming the refused key or the field that doesn't match the format, or a node that is
    /// not a table.
    pub fn from_node(node: &Value) -> Result<Self, serde_json::Error> {
        let Value::Object(map) = node else {
            return Err(<serde_json::Error as serde::de::Error>::custom("a show is a table"));
        };
        let mut features = BTreeMap::new();
        for (key, value) in map {
            if Self::NODE_KEYS.contains(&key.as_str()) || Self::SIBLING_KEYS.contains(&key.as_str()) {
                continue;
            }
            keys::classify(key).map_err(<serde_json::Error as serde::de::Error>::custom)?;
            features.insert(key.clone(), value.clone());
        }
        let file = map
            .get("show")
            .ok_or_else(|| <serde_json::Error as serde::de::Error>::custom("a show needs its settings (`show`)"))?;
        let ShowFile { name, seed, variables, groups } = serde_json::from_value(file.clone())?;
        let mut graphs = BTreeMap::new();
        if let Some(automations) = map.get("automations") {
            let Value::Object(automations) = automations else {
                return Err(<serde_json::Error as serde::de::Error>::custom("`automations` is a table of graphs"));
            };
            for (id, graph) in automations {
                let id = GraphId::new(id.as_str()).map_err(<serde_json::Error as serde::de::Error>::custom)?;
                let graph = GraphDef::from_node(graph)
                    .map_err(|e| <serde_json::Error as serde::de::Error>::custom(format!("graph `{id}`: {e}")))?;
                graphs.insert(id, graph);
            }
        }
        Ok(Self { name, seed, variables, groups, graphs, features })
    }

    /// Writes this show into the show node `into`: `show` always, `automations` only when there are graphs, every
    /// feature key it holds exactly as held. Every other key (`applied`, and a feature key this value doesn't hold)
    /// is left as it was: nothing drops a key because its owner isn't linked. A node that is not a table becomes one.
    ///
    /// Reading the node back with [`from_node`](Self::from_node) gives an equal show.
    ///
    /// # Errors
    /// The [`serde_json::Error`] if the show can't be turned into a table (it can for any value the type holds).
    pub fn to_node(&self, into: &mut Value) -> Result<(), serde_json::Error> {
        let file = ShowFile {
            name: self.name.clone(),
            seed: self.seed,
            variables: self.variables.clone(),
            groups: self.groups.clone(),
        };
        let mut written = serde_json::Map::new();
        written.insert("show".to_owned(), serde_json::to_value(file)?);
        if !self.graphs.is_empty() {
            let mut automations = serde_json::Map::new();
            for (id, graph) in &self.graphs {
                let mut node = Value::Null;
                graph.to_node(&mut node)?;
                automations.insert(id.to_string(), node);
            }
            written.insert("automations".to_owned(), Value::Object(automations));
        }
        written.extend(self.features.iter().map(|(key, value)| (key.clone(), value.clone())));
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

/// `show.toml`.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShowFile {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    variables: BTreeMap<VarName, VariableDef>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    groups: BTreeMap<GroupId, GroupDef>,
}

/// A variable's declaration in `show.toml` (design D10). Declarations are document; values are state, kept on this
/// machine and never in the show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct VariableDef {
    /// Its type (`type` in the file). Every write is checked against it.
    #[serde(rename = "type")]
    pub ty: VarType,
    /// Who shares it and where it persists.
    pub scope: VarScope,
    /// The value a run starts from when nothing is stored; the type's default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// One line on what it counts or holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl VariableDef {
    /// A variable of type `ty` and scope `scope`, with no default or summary.
    pub fn new(ty: VarType, scope: VarScope) -> Self {
        Self { ty, scope, default: None, summary: None }
    }

    /// With this default.
    #[must_use]
    pub fn with_default(mut self, default: Value) -> Self {
        self.default = Some(default);
        self
    }

    /// With this summary.
    #[must_use]
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }

    /// The value a run starts from: the default, else the type's default.
    pub fn initial(&self) -> Value {
        self.default.clone().unwrap_or_else(|| self.ty.default_json())
    }
}

/// The type of a variable.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VarType {
    /// `true` or `false`.
    Bool,
    /// A finite number.
    Number,
    /// Text.
    Text,
    /// A list.
    List,
}

impl VarType {
    /// The name as written in `show.toml` (`number`).
    pub const fn as_str(self) -> &'static str {
        match self {
            VarType::Bool => "bool",
            VarType::Number => "number",
            VarType::Text => "text",
            VarType::List => "list",
        }
    }

    /// The port type a variable of this type is read as.
    pub const fn port_type(self) -> PortType {
        match self {
            VarType::Bool => PortType::Bool,
            VarType::Number => PortType::Number,
            VarType::Text => PortType::Text,
            VarType::List => PortType::List,
        }
    }

    /// True if `value` has this type.
    pub fn holds(self, value: &Value) -> bool {
        match self {
            VarType::Bool => value.is_boolean(),
            VarType::Number => value.is_number(),
            VarType::Text => value.is_string(),
            VarType::List => value.is_array(),
        }
    }

    /// The type's default value: `false`, `0`, `""`, `[]`.
    pub fn default_json(self) -> Value {
        match self {
            VarType::Bool => Value::Bool(false),
            VarType::Number => Value::from(0),
            VarType::Text => Value::String(String::new()),
            VarType::List => Value::Array(Vec::new()),
        }
    }
}

/// Who shares a variable and where its value persists (design D10).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VarScope {
    /// The graphs of the running show, from start to stop; persisted nowhere.
    Session,
    /// The graphs of this show, across runs; persisted in the project's `.omuzaki/` folder on this machine.
    Show,
    /// Every show of every project of this OS user; persisted in the user's data folder.
    Global,
}

impl VarScope {
    /// True if values of this scope outlive a run.
    pub const fn is_persisted(self) -> bool {
        !matches!(self, VarScope::Session)
    }
}

/// A group of graphs, declared in `show.toml` (design D12).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct GroupDef {
    /// Display name; the id stands in when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Its state when the show starts. Always written: it is the point of the declaration.
    #[serde(default = "group_enabled_default")]
    pub enabled: bool,
}

fn group_enabled_default() -> bool {
    true
}

impl Default for GroupDef {
    fn default() -> Self {
        Self::new()
    }
}

impl GroupDef {
    /// A group with no name, on when the show starts.
    pub fn new() -> Self {
        Self { name: None, enabled: true }
    }

    /// With this display name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// On or off when the show starts.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// The error for a key `within` (`a show`, `a graph`) doesn't have: the nearest valid key when it is close enough to
/// be a typo (a third of its length plus one edit), otherwise the valid keys.
pub(crate) fn unknown_key(key: &str, valid: &[&str], within: &str) -> serde_json::Error {
    let close = key.chars().count() / 3 + 1;
    let message = match nearest(key, valid.iter().copied()) {
        Some(near) if edit_distance(key, near) <= close => {
            format!("unknown field `{key}` in {within}, did you mean `{near}`?")
        }
        _ => format!(
            "unknown field `{key}` in {within}, expected one of {}",
            valid.iter().map(|k| format!("`{k}`")).collect::<Vec<_>>().join(", ")
        ),
    };
    <serde_json::Error as serde::de::Error>::custom(message)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_show_reads_its_settings_and_graphs_and_skips_applied() {
        let node = json!({
            "show": {"name": "Main", "groups": {"redeems": {"enabled": true, "name": "Redeems"}},
                     "variables": {"bonks": {"type": "number", "scope": "show", "default": 0}}},
            "automations": {"bonk": {"graph": {"name": "Bonk", "group": "redeems"}}},
            "applied": {"bonk": {"item": "omuzaki/automations/bonk"}}
        });
        let show = ShowDef::from_node(&node).unwrap();
        assert_eq!(show.name, "Main");
        assert_eq!(show.variables[&VarName::new("bonks").unwrap()].initial(), json!(0));
        assert!(show.features.is_empty());
        let mut back = node.clone();
        show.to_node(&mut back).unwrap();
        assert_eq!(back, node);
    }

    #[test]
    fn a_show_without_graphs_writes_no_automations() {
        let mut node = json!({"show": {"name": "x"}, "automations": {"a": {"graph": {}}}});
        ShowDef::new("x").to_node(&mut node).unwrap();
        assert_eq!(node, json!({"show": {"name": "x"}}));
    }

    #[test]
    fn variable_types_know_their_values() {
        assert!(VarType::Number.holds(&json!(1.5)) && !VarType::Number.holds(&json!("1")));
        assert_eq!(VarType::List.default_json(), json!([]));
        assert_eq!(VarType::Text.port_type(), PortType::Text);
        assert!(!VarScope::Session.is_persisted() && VarScope::Global.is_persisted());
    }
}
