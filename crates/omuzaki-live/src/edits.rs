//! The document ops that edit a show (design D13; #1177, #1178, #1179): shows, graphs, groups, variable
//! declarations, nodes and wires. Each is an edit with `Undo::Auto`, one undo step, and its patch touches only the
//! leaves that changed, so dragging a node writes its `pos` and nothing else.
//!
//! Every op reads the show, changes it as a [`ShowDef`], and refuses the result when it breaks a rule of D18's first
//! column that the show didn't already break: a structural rule (`ShowDef::check`), or a catalogue rule the compile
//! finds (a kind that doesn't exist, parameters the kind refuses, a wire to a missing port, a type that never converts,
//! two sources on a value input, a `set` constant that doesn't fit, a value cycle). So an agent learns about a mistake
//! at once, and a show edited by hand on disk can still be edited.

use std::collections::BTreeSet;

use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_live_api::graph::{GraphDef, NodeDef, Wire};
use omuzaki_live_api::ids::{GraphId, GroupId, NodeId, NodeKindId, PortName, PortRef, ShowId, VarName};
use omuzaki_live_api::node::{DynNodeKind, find_node_kind};
use omuzaki_live_api::services::InertReason;
use omuzaki_live_api::show::{GroupDef, ShowDef, VarScope, VarType, VariableDef, paths};
use omuzaki_live_api::value::Val;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, json};

use crate::compile::compile;
use crate::core::english;
use crate::ops::{document_edit, resolve_show, service};

/// `Some(None)` for an explicit `null`, `None` when the field is absent.
#[allow(clippy::option_option)] // Absent, null and a value are three different requests.
fn double<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// Appends to `changes` what turns `before` into `after` under `path`: tables are compared key by key, and
/// any other value that differs is replaced whole, so a drag writes one `pos` and a removal removes one key.
fn diff(path: &DocPath, before: Option<&Value>, after: Option<&Value>, changes: &mut Patch) {
    match (before, after) {
        (Some(Value::Object(b)), Some(Value::Object(a))) => {
            let keys: BTreeSet<&String> = b.keys().chain(a.keys()).collect();
            for key in keys {
                diff(&path.join(key.as_str()), b.get(key), a.get(key), changes);
            }
        }
        (b, a) if b == a => {}
        (b, a) => {
            *changes = std::mem::take(changes).with(path.clone(), b.cloned(), a.cloned());
        }
    }
}

/// The node-level refusals of D18's first column that the compile reports.
fn refused_reason(why: &InertReason) -> bool {
    matches!(
        why,
        InertReason::UnknownKind | InertReason::BadParams | InertReason::OpNotAllowed | InertReason::BadWiring
    )
}

/// Refuses `after` when it breaks a first-column rule `before` didn't.
fn refuse(show: &ShowId, before: &ShowDef, after: &ShowDef) -> Result<(), OpError> {
    let old: Vec<_> = before.check().into_iter().filter(|f| f.refuses).map(|f| f.text).collect();
    if let Some(finding) = after.check().into_iter().find(|f| f.refuses && !old.contains(&f.text)) {
        return Err(OpError::invalid("show", english(&finding.text)));
    }
    let (was, now) = (compile(show, before), compile(show, after));
    for node in &now.nodes {
        let Some(why) = node.inert.as_ref().filter(|why| refused_reason(why)) else { continue };
        let same = was.nodes.iter().any(|o| o.graph == node.graph && o.id == node.id && o.inert.as_ref() == Some(why));
        if same {
            continue;
        }
        let text = now
            .findings
            .iter()
            .find(|f| f.graph.as_ref() == Some(&node.graph) && f.node.as_ref() == Some(&node.id))
            .map_or_else(|| format!("{} in {} can't run like this", node.id, node.graph), |f| english(&f.text));
        return Err(OpError::invalid(format!("{}/{}", node.graph, node.id), text));
    }
    Ok(())
}

/// Reads the show `given` names (or the default), lets `change` edit it and answer the op's output, refuses a broken
/// result, and plans the patch of the leaves that changed.
fn edit_show(
    ctx: &OpCtx<'_>,
    given: Option<&ShowId>,
    change: impl FnOnce(&ShowId, &mut ShowDef) -> Result<Value, OpError>,
) -> Result<Plan, OpError> {
    let show = resolve_show(ctx, given)?;
    let path = paths::show(&show);
    let before_node =
        ctx.doc.get(&path).cloned().ok_or_else(|| {
            OpError::not_found(format!("there is no show `{show}`")).hint("show.create {id: \"main\"}")
        })?;
    let before = ShowDef::from_node(&before_node)
        .map_err(|e| OpError::invalid("show", format!("show `{show}` can't be read: {e}")))?;
    let mut after = before.clone();
    let output = change(&show, &mut after)?;
    refuse(&show, &before, &after)?;
    let mut after_node = before_node.clone();
    after.to_node(&mut after_node).map_err(|e| OpError::internal(e.to_string()))?;
    let mut changes = Patch::new();
    diff(&path, Some(&before_node), Some(&after_node), &mut changes);
    let mut plan = Plan::edit(changes);
    plan.output = Some(output);
    Ok(plan)
}

fn graph_mut<'a>(show: &ShowId, def: &'a mut ShowDef, graph: &GraphId) -> Result<&'a mut GraphDef, OpError> {
    def.graphs.get_mut(graph).ok_or_else(|| {
        OpError::not_found(format!("show `{show}` has no graph `{graph}`")).hint("show.describe lists the graphs")
    })
}

fn node_mut<'a>(graph_id: &GraphId, graph: &'a mut GraphDef, node: &NodeId) -> Result<&'a mut NodeDef, OpError> {
    graph.nodes.get_mut(node).ok_or_else(|| {
        OpError::not_found(format!("graph `{graph_id}` has no node `{node}`"))
            .hint("automation.describe lists the nodes")
    })
}

/// Merges `changes` into `map` by top-level key; `null` removes a key.
fn merge(map: &mut Map<String, Value>, changes: &Map<String, Value>) {
    for (key, value) in changes {
        if value.is_null() {
            map.remove(key);
        } else {
            map.insert(key.clone(), value.clone());
        }
    }
}

/// `params` as the kind normalises them, or the kind's refusal.
fn normalised(kind: &NodeKindId, field: &str, params: &Map<String, Value>) -> Result<Map<String, Value>, OpError> {
    let registration = find_node_kind(kind.as_str()).ok_or_else(|| {
        OpError::invalid(format!("{field}/kind"), format!("no node kind `{kind}` exists"))
            .hint("automation.node_kinds lists the kinds")
    })?;
    match registration.check(&Value::Object(params.clone())) {
        Ok(Value::Object(normal)) => Ok(normal),
        Ok(_) => Ok(params.clone()),
        Err(errors) => {
            let text = errors.0.iter().map(|e| english(&e.text)).collect::<Vec<_>>().join("; ");
            Err(OpError::invalid(format!("{field}/params"), text))
        }
    }
}

/// Removes the wires into `node` from ports its kind no longer has, answering the ports removed.
fn drop_dangling(graph: &mut GraphDef, node: &NodeId) -> Vec<PortName> {
    let Some(def) = graph.nodes.get(node) else { return Vec::new() };
    let Some(kind) = find_node_kind(def.kind.as_str()) else { return Vec::new() };
    let Ok(ports) = DynNodeKind::ports(kind, &Value::Object(def.params.clone())) else { return Vec::new() };
    let gone: Vec<PortName> = def.inputs.keys().filter(|p| ports.input(p.as_str()).is_none()).cloned().collect();
    let outputs_gone: Vec<PortName> = graph
        .nodes
        .values()
        .flat_map(|n| n.inputs.values().flat_map(Wire::sources))
        .filter(|s| &s.node == node && ports.output(s.port.as_str()).is_none())
        .map(|s| s.port.clone())
        .collect();
    if let Some(def) = graph.nodes.get_mut(node) {
        for port in &gone {
            def.inputs.remove(port);
        }
    }
    for port in &outputs_gone {
        let source = PortRef::new(node.clone(), port.clone());
        for other in graph.nodes.values_mut() {
            remove_source(other, &source);
        }
    }
    gone.into_iter().chain(outputs_gone).collect()
}

/// Removes `source` from every input of `node`; an input left with no source goes.
fn remove_source(node: &mut NodeDef, source: &PortRef) {
    node.inputs.retain(|_, wire| {
        let kept: Vec<PortRef> = wire.sources().iter().filter(|s| *s != source).cloned().collect();
        *wire = match kept.len() {
            0 => return false,
            1 => Wire::One(kept[0].clone()),
            _ => Wire::Many(kept),
        };
        true
    });
}

// ── Shows (#1177) ─────────────────────────────────────────────────────────────────────────────────────────────────

/// Rename a show; its id never changes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "name": "Main show" }], "x-omuzaki-fixture" = "show"))]
pub struct RenameShow {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// Its new display name.
    pub name: String,
}

/// Make a show the one the project runs and edits by default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main" }], "x-omuzaki-fixture" = "show"))]
pub struct SetDefaultShow {
    /// The show.
    pub show: ShowId,
}

/// Delete a show and its automations. A running show is refused; the default moves to the first show left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main" }], "x-omuzaki-fixture" = "show-graph"))]
pub struct DeleteShow {
    /// The show.
    pub show: ShowId,
}

impl Op for RenameShow {
    fn meta() -> OpMeta {
        document_edit("show.rename", "Rename a show (its id stays).")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            let name = self.name.trim();
            if name.is_empty() {
                return Err(OpError::invalid("name", "a show's name can't be empty"));
            }
            name.clone_into(&mut def.name);
            Ok(json!({ "show": show, "name": name }))
        })
    }
}

impl Op for SetDefaultShow {
    fn meta() -> OpMeta {
        document_edit("show.set_default", "Make a show the project's default.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&paths::show(&self.show)).is_none() {
            return Err(OpError::not_found(format!("there is no show `{}`", self.show))
                .hint(format!("show.create {{id: \"{}\"}}", self.show)));
        }
        let path = paths::default_show();
        let before = ctx.doc.get(&path).cloned();
        let after = json!(self.show.as_str());
        let changes = if before.as_ref() == Some(&after) {
            Patch::new()
        } else if before.is_some() {
            Patch::new().with(path, before, Some(after))
        } else {
            crate::ops::create(ctx, &path, after)
        };
        let mut plan = Plan::edit(changes);
        plan.output = Some(json!({ "default": self.show }));
        Ok(plan)
    }
}

impl Op for DeleteShow {
    fn meta() -> OpMeta {
        document_edit("show.delete", "Delete a show and its automations.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = paths::show(&self.show);
        let before = ctx
            .doc
            .get(&path)
            .cloned()
            .ok_or_else(|| OpError::not_found(format!("there is no show `{}`", self.show)))?;
        if service(ctx).ok().and_then(super::service::Automation::running).is_some_and(|r| r.show == self.show) {
            return Err(OpError::conflict(format!("show `{}` is running", self.show))
                .hint("stop it first with automation.stop"));
        }
        let mut changes = Patch::new().with(path, Some(before), None);
        let default = paths::default_show();
        let current = ctx.doc.get(&default).cloned();
        let mut new_default = current.clone();
        if current.as_ref().and_then(Value::as_str) == Some(self.show.as_str()) {
            let shows = ctx.doc.get(&DocPath::root().join("shows")).and_then(Value::as_object);
            let next = shows.and_then(|s| s.keys().find(|k| k.as_str() != self.show.as_str()).cloned());
            new_default = next.map(Value::String);
            changes = changes.with(default, current, new_default.clone());
        }
        let mut plan = Plan::edit(changes);
        plan.output = Some(json!({ "deleted": self.show, "default": new_default }));
        Ok(plan)
    }
}

// ── Graphs, groups and variables (#1178) ──────────────────────────────────────────────────────────────────────────

/// Change a graph's settings: its name, whether it runs, its group and its summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "graph": "brb", "enabled": false }], "x-omuzaki-fixture" = "show-graph"))]
#[allow(clippy::option_option)] // `group`: absent leaves it, `null` takes the graph out of its group.
pub struct SetGraph {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The graph.
    pub graph: GraphId,
    /// Its display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Its group; `null` takes it out of its group.
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    pub group: Option<Option<GroupId>>,
    /// What it does, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Add a group: a switch that turns several graphs on and off together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "id": "intro", "name": "Intro" }], "x-omuzaki-fixture" = "show"))]
pub struct AddGroup {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The group's id.
    pub id: GroupId,
    /// Its display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether it is on when the show starts (on by default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Change a group's name or whether it is on when the show starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "group": "intro", "enabled": false }], "x-omuzaki-fixture" = "show-full"))]
pub struct SetGroup {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The group.
    pub group: GroupId,
    /// Its display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Whether it is on when the show starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Remove a group; its graphs stay and leave the group.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "group": "intro" }], "x-omuzaki-fixture" = "show-full"))]
pub struct RemoveGroup {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The group.
    pub group: GroupId,
}

/// Declare a variable (or change its declaration): its type, where its value lives, its default and summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "show": "main", "name": "bonks", "type": "number", "scope": "show", "default": 0, "summary": "Bonks so far." }
], "x-omuzaki-fixture" = "show"))]
pub struct DeclareVariable {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The variable's name.
    pub name: VarName,
    /// Its type: `bool`, `number`, `text` or `list`.
    #[serde(rename = "type")]
    pub ty: VarType,
    /// Where its value lives: `session` (this run), `show` (this show, on this computer) or `global`.
    pub scope: VarScope,
    /// Its starting value, of its type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// What it holds, in a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Remove a variable's declaration. Nodes that use it become inert until it is declared again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "name": "bonks" }], "x-omuzaki-fixture" = "show-var"))]
pub struct RemoveVariable {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The variable.
    pub name: VarName,
}

impl Op for SetGraph {
    fn meta() -> OpMeta {
        document_edit("automation.set_graph", "Change a graph's name, whether it runs, its group or summary.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            if let Some(Some(group)) = &self.group
                && !def.groups.contains_key(group)
            {
                return Err(OpError::invalid("group", format!("show `{show}` has no group `{group}`"))
                    .hint(format!("automation.add_group {{id: \"{group}\"}}")));
            }
            let graph = graph_mut(show, def, &self.graph)?;
            if let Some(name) = &self.name {
                graph.name = Some(name.clone());
            }
            if let Some(enabled) = self.enabled {
                graph.enabled = enabled;
            }
            if let Some(group) = &self.group {
                graph.group.clone_from(group);
            }
            if let Some(summary) = &self.summary {
                graph.summary = Some(summary.clone());
            }
            Ok(json!({ "show": show, "graph": self.graph, "enabled": graph.enabled, "group": graph.group }))
        })
    }
}

impl Op for AddGroup {
    fn meta() -> OpMeta {
        document_edit("automation.add_group", "Add a group that turns several graphs on and off together.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            if def.groups.contains_key(&self.id) {
                return Err(OpError::conflict(format!("show `{show}` already has a group `{}`", self.id)));
            }
            let mut group = GroupDef::default();
            group.name.clone_from(&self.name);
            group.enabled = self.enabled.unwrap_or(true);
            def.groups.insert(self.id.clone(), group);
            Ok(json!({ "show": show, "group": self.id }))
        })
    }
}

impl Op for SetGroup {
    fn meta() -> OpMeta {
        document_edit("automation.set_group", "Change a group's name or whether it starts on.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            let group = def
                .groups
                .get_mut(&self.group)
                .ok_or_else(|| OpError::not_found(format!("show `{show}` has no group `{}`", self.group)))?;
            if let Some(name) = &self.name {
                group.name = Some(name.clone());
            }
            if let Some(enabled) = self.enabled {
                group.enabled = enabled;
            }
            Ok(json!({ "show": show, "group": self.group, "enabled": group.enabled }))
        })
    }
}

impl Op for RemoveGroup {
    fn meta() -> OpMeta {
        document_edit("automation.remove_group", "Remove a group; its graphs stay and leave it.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            if def.groups.remove(&self.group).is_none() {
                return Err(OpError::not_found(format!("show `{show}` has no group `{}`", self.group)));
            }
            let mut left = Vec::new();
            for (id, graph) in &mut def.graphs {
                if graph.group.as_ref() == Some(&self.group) {
                    graph.group = None;
                    left.push(id.clone());
                }
            }
            Ok(json!({ "show": show, "removed": self.group, "graphs": left }))
        })
    }
}

impl Op for DeclareVariable {
    fn meta() -> OpMeta {
        document_edit("automation.declare_variable", "Declare a variable: its type, scope and default.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            let mut declared = VariableDef::new(self.ty, self.scope);
            if let Some(default) = &self.default {
                let fits = Val::from_json(default, self.ty.port_type())
                    .is_ok_and(|v| v.ty() == self.ty.port_type() && v.check().is_ok());
                if !fits {
                    return Err(OpError::invalid("default", format!("the default isn't a {}", self.ty.as_str())));
                }
                declared = declared.with_default(default.clone());
            }
            if let Some(summary) = &self.summary {
                declared = declared.with_summary(summary.clone());
            }
            let replaced = def.variables.insert(self.name.clone(), declared).is_some();
            Ok(json!({ "show": show, "variable": self.name, "replaced": replaced }))
        })
    }
}

impl Op for RemoveVariable {
    fn meta() -> OpMeta {
        document_edit("automation.remove_variable", "Remove a variable's declaration.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            if def.variables.remove(&self.name).is_none() {
                return Err(OpError::not_found(format!("show `{show}` declares no variable `{}`", self.name)));
            }
            Ok(json!({ "show": show, "removed": self.name }))
        })
    }
}

// ── Nodes and wires (#1179) ───────────────────────────────────────────────────────────────────────────────────────

/// Add a node to a graph. Its id defaults to the kind's last word (`wait`, then `wait-2`); its parameters are checked
/// by the kind and written normalised.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "show": "main", "graph": "brb", "kind": "flow.wait", "set": { "seconds": 2 }, "pos": [240, 0] }
], "x-omuzaki-fixture" = "show-graph"))]
pub struct AddNode {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The graph.
    pub graph: GraphId,
    /// The node kind (`flow.wait`); `automation.node_kinds` lists them.
    pub kind: NodeKindId,
    /// The node's id; the kind's last word when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<NodeId>,
    /// The kind's parameters.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    /// Where the editor shows it, in points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f32; 2]>,
    /// What the editor shows instead of the kind's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Constants for its value inputs.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub set: Map<String, Value>,
}

/// Change a node: its parameters and constants (merged by key; `null` removes one), its position or label. Wires into
/// ports the change takes away are removed and listed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "graph": "brb", "node": "scene", "pos": [200, 40] }], "x-omuzaki-fixture" = "show-graph"))]
#[allow(clippy::option_option)] // `label`: absent leaves it, `null` removes it.
pub struct SetNode {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The graph.
    pub graph: GraphId,
    /// The node.
    pub node: NodeId,
    /// Parameters to change; `null` removes one.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub params: Map<String, Value>,
    /// Where the editor shows it, in points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pos: Option<[f32; 2]>,
    /// Its label; `null` removes it.
    #[serde(default, deserialize_with = "double", skip_serializing_if = "Option::is_none")]
    pub label: Option<Option<String>>,
    /// Constants to change; `null` removes one.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub set: Map<String, Value>,
}

/// Remove a node and every wire that names it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "graph": "brb", "node": "scene" }], "x-omuzaki-fixture" = "show-graph"))]
pub struct RemoveNode {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The graph.
    pub graph: GraphId,
    /// The node.
    pub node: NodeId,
}

/// Wire an output to an input (`"<node>.<port>"`). An event input takes several sources; a value input takes one,
/// unless `replace` swaps it. Types that never convert, an event wired to a value and a value cycle are refused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "graph": "brb", "from": "redeem.fired", "to": "wait.in" }], "x-omuzaki-fixture" = "show-full"))]
pub struct Connect {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The graph.
    pub graph: GraphId,
    /// The output (`redeem.fired`).
    pub from: PortRef,
    /// The input (`wait.in`).
    pub to: PortRef,
    /// Replace a value input's current source.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replace: bool,
}

/// Unwire an input: one source, or every source when `from` is absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "graph": "brb", "to": "scene.in", "from": "wait.out" }], "x-omuzaki-fixture" = "show-full"))]
pub struct Disconnect {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The graph.
    pub graph: GraphId,
    /// The input (`scene.in`).
    pub to: PortRef,
    /// The one source to remove; every source when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<PortRef>,
}

/// `kind`'s last word as a node id, numbered past the ids `graph` already has.
fn fresh_id(graph: &GraphDef, kind: &NodeKindId) -> Result<NodeId, OpError> {
    let stem = kind.as_str().rsplit('.').next().unwrap_or("node").replace('_', "-");
    let first = NodeId::new(stem.as_str()).map_err(|e| OpError::internal(e.to_string()))?;
    if !graph.nodes.contains_key(&first) {
        return Ok(first);
    }
    for n in 2.. {
        let id = NodeId::new(format!("{stem}-{n}")).map_err(|e| OpError::internal(e.to_string()))?;
        if !graph.nodes.contains_key(&id) {
            return Ok(id);
        }
    }
    Err(OpError::internal("no free node id"))
}

fn port_map(set: &Map<String, Value>) -> Result<std::collections::BTreeMap<PortName, Value>, OpError> {
    set.iter()
        .map(|(port, value)| {
            let name =
                PortName::new(port.as_str()).map_err(|e| OpError::invalid(format!("set/{port}"), e.to_string()))?;
            Ok((name, value.clone()))
        })
        .collect()
}

impl Op for AddNode {
    fn meta() -> OpMeta {
        document_edit("automation.add_node", "Add a node to a graph.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            let graph = graph_mut(show, def, &self.graph)?;
            let id = match &self.id {
                Some(id) if graph.nodes.contains_key(id) => {
                    return Err(OpError::conflict(format!("graph `{}` already has a node `{id}`", self.graph)));
                }
                Some(id) => id.clone(),
                None => fresh_id(graph, &self.kind)?,
            };
            let mut node = NodeDef::new(self.kind.clone());
            node.params = normalised(&self.kind, "node", &self.params)?;
            node.pos = self.pos.unwrap_or([0.0, 0.0]);
            node.label.clone_from(&self.label);
            node.set = port_map(&self.set)?;
            graph.nodes.insert(id.clone(), node);
            Ok(json!({ "show": show, "graph": self.graph, "node": id }))
        })
    }
}

impl Op for SetNode {
    fn meta() -> OpMeta {
        document_edit("automation.set_node", "Change a node's parameters, constants, position or label.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            let graph = graph_mut(show, def, &self.graph)?;
            let node = node_mut(&self.graph, graph, &self.node)?;
            if !self.params.is_empty() {
                let mut params = node.params.clone();
                merge(&mut params, &self.params);
                node.params = normalised(&node.kind, "params", &params)?;
            }
            if let Some(pos) = self.pos {
                node.pos = pos;
            }
            if let Some(label) = &self.label {
                node.label.clone_from(label);
            }
            if !self.set.is_empty() {
                let mut set: Map<String, Value> =
                    node.set.iter().map(|(p, v)| (p.as_str().to_owned(), v.clone())).collect();
                merge(&mut set, &self.set);
                node.set = port_map(&set)?;
            }
            let removed = if self.params.is_empty() { Vec::new() } else { drop_dangling(graph, &self.node) };
            Ok(json!({ "show": show, "graph": self.graph, "node": self.node, "unwired": removed }))
        })
    }
}

impl Op for RemoveNode {
    fn meta() -> OpMeta {
        document_edit("automation.remove_node", "Remove a node and every wire that names it.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            let graph = graph_mut(show, def, &self.graph)?;
            if graph.nodes.remove(&self.node).is_none() {
                return Err(OpError::not_found(format!("graph `{}` has no node `{}`", self.graph, self.node)));
            }
            let mut unwired = Vec::new();
            for (id, other) in &mut graph.nodes {
                let before = other.inputs.len();
                let named: Vec<PortRef> =
                    other.inputs.values().flat_map(Wire::sources).filter(|s| s.node == self.node).cloned().collect();
                for source in &named {
                    remove_source(other, source);
                }
                if !named.is_empty() || other.inputs.len() != before {
                    unwired.push(id.clone());
                }
            }
            Ok(json!({ "show": show, "graph": self.graph, "removed": self.node, "unwired": unwired }))
        })
    }
}

impl Op for Connect {
    fn meta() -> OpMeta {
        document_edit("automation.connect", "Wire a node's output to another node's input.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            let graph = graph_mut(show, def, &self.graph)?;
            if !graph.nodes.contains_key(&self.from.node) {
                return Err(OpError::not_found(format!("graph `{}` has no node `{}`", self.graph, self.from.node)));
            }
            let source_kind = graph.nodes.get(&self.from.node).map(|n| (n.kind.clone(), n.params.clone()));
            let event_source = source_kind
                .and_then(|(kind, params)| {
                    let ports = DynNodeKind::ports(find_node_kind(kind.as_str())?, &Value::Object(params)).ok()?;
                    Some(ports.output(self.from.port.as_str())?.ty.is_event())
                })
                .unwrap_or(false);
            let node = node_mut(&self.graph, graph, &self.to.node)?;
            let wire = match node.inputs.remove(&self.to.port) {
                None => Wire::One(self.from.clone()),
                Some(existing) if existing.sources().contains(&self.from) => existing,
                Some(existing) if event_source => {
                    let mut sources = existing.sources().to_vec();
                    sources.push(self.from.clone());
                    Wire::Many(sources)
                }
                Some(_) if self.replace => Wire::One(self.from.clone()),
                Some(existing) => {
                    let current = existing.sources().first().map(ToString::to_string).unwrap_or_default();
                    return Err(OpError::conflict(format!("{} already takes its value from {current}", self.to))
                        .hint("pass replace: true to swap it"));
                }
            };
            node.set.remove(&self.to.port);
            node.inputs.insert(self.to.port.clone(), wire);
            Ok(json!({ "show": show, "graph": self.graph, "from": self.from, "to": self.to }))
        })
    }
}

impl Op for Disconnect {
    fn meta() -> OpMeta {
        document_edit("automation.disconnect", "Unwire a node's input.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        edit_show(ctx, self.show.as_ref(), |show, def| {
            let graph = graph_mut(show, def, &self.graph)?;
            let node = node_mut(&self.graph, graph, &self.to.node)?;
            let removed: Vec<PortRef> = match (&self.from, node.inputs.get(&self.to.port)) {
                (None, Some(wire)) => {
                    let all = wire.sources().to_vec();
                    node.inputs.remove(&self.to.port);
                    all
                }
                (Some(from), Some(wire)) if wire.sources().contains(from) => {
                    let mut only = NodeDef::new(node.kind.clone());
                    only.inputs.insert(self.to.port.clone(), wire.clone());
                    remove_source(&mut only, from);
                    match only.inputs.remove(&self.to.port) {
                        Some(rest) => node.inputs.insert(self.to.port.clone(), rest),
                        None => node.inputs.remove(&self.to.port),
                    };
                    vec![from.clone()]
                }
                (Some(_), Some(_)) | (_, None) => Vec::new(),
            };
            Ok(json!({ "show": show, "graph": self.graph, "to": self.to, "removed": removed }))
        })
    }
}

register_op!(RenameShow);
register_op!(SetDefaultShow);
register_op!(DeleteShow);
register_op!(SetGraph);
register_op!(AddGroup);
register_op!(SetGroup);
register_op!(RemoveGroup);
register_op!(DeclareVariable);
register_op!(RemoveVariable);
register_op!(AddNode);
register_op!(SetNode);
register_op!(RemoveNode);
register_op!(Connect);
register_op!(Disconnect);
