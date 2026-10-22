//! The show and automation ops (design D13): the document ops that make a show and its graphs, and the runtime ops
//! that start, stop, watch and test the running show.
//!
//! - **Document ops** (`show.create`, `automation.add_graph`, `automation.remove_graph`) are edits with
//!   `Undo::Auto`. Each checks the resulting show (`ShowDef::check`) and every new node against the catalogue before
//!   it returns its patch, so an agent learns about a misspelt kind or a bad parameter at once. They take the
//!   `edit:scene` scope until the `edit:show` scope exists in the ops contract.
//! - **Runtime ops** are live actions (`Undo::None`) on the [`Automation`] service: `automation.start` compiles the
//!   show from the document and starts it, `automation.stop` stops it, `automation.fire` publishes a test event that
//!   runs as the caller, and `automation.status` and `automation.values` report without waiting on the engine.

use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_live_api::graph::GraphDef;
use omuzaki_live_api::ids::{EventKind, GraphId, ShowId};
use omuzaki_live_api::node::{DynNodeKind, find_node_kind};
use omuzaki_live_api::services::AutomationControl;
use omuzaki_live_api::show::{ShowDef, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::compile::compile;
use crate::core::english;
use crate::service::Automation;

/// Make a new, empty show: the folder `shows/<id>` that holds its automations. The first show becomes the project's
/// default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": "main" }, { "id": "stream", "name": "Stream" }]))]
pub struct CreateShow {
    /// The show's id: lowercase letters, digits and dashes (`main`).
    pub id: ShowId,
    /// Its display name; the id when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Add an automation graph to a show: its settings and its nodes, wired at their destinations (`in`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{
        "show": "main",
        "id": "brb",
        "graph": {
            "graph": { "name": "Be right back" },
            "nodes": {
                "redeem": { "kind": "on.event", "params": { "kind": "redeem", "where": { "reward.title": "BRB" } } },
                "scene": {
                    "kind": "op.run",
                    "params": { "op": "obs.scene.set", "args": { "scene": "BRB" } },
                    "in": { "in": "redeem.fired" }
                }
            }
        }
    }],
    "x-omuzaki-fixture" = "show"
))]
pub struct AddGraph {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The graph's id (`brb`).
    pub id: GraphId,
    /// The graph: `graph` (name, enabled, group, summary) and `nodes`, each with its `kind`, `params`, wires (`in`,
    /// `"<node>.<port>"`) and constants (`set`).
    pub graph: GraphDef,
}

/// Remove an automation graph and all its nodes from a show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main", "graph": "brb" }], "x-omuzaki-fixture" = "show-graph"))]
pub struct RemoveGraph {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// The graph's id.
    pub graph: GraphId,
}

/// Start a show: its automations react to platform events, hotkeys and test events until it stops.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main" }], "x-omuzaki-fixture" = "show-graph"))]
pub struct Start {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
}

/// Stop the running show.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Stop {}

/// Report the running show: its seed, each graph's state, inert and faulted nodes, and what was dropped.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

/// The running show's values monitor: variables, groups, each graph's state, the last firings and events.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Values {}

/// Send a test event into the running show, as if it had arrived; the actions it causes run as you.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "kind": "custom.test", "payload": {} },
    { "kind": "redeem", "payload": {
        "platform": "twitch", "channel": { "id": "1", "name": "omuzaki" },
        "user": { "id": "2", "login": "kiri", "name": "Kiri", "roles": [], "badges": [] },
        "reward": { "id": "r1", "title": "BRB", "cost": 100 }, "redemption_id": "x1"
    } }
]))]
pub struct Fire {
    /// The event kind (`redeem`, `chat.message`, `custom.<name>`, ...).
    pub kind: EventKind,
    /// The payload, as the kind's schema describes it (`automation.event_kinds`).
    pub payload: Value,
}

fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a Automation, OpError> {
    ctx.service::<Automation>()
}

fn live_action(name: &'static str, summary: &'static str, scope: Scope) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope, undo: Undo::None }
}

fn query(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}

fn document_edit(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::EditScene, undo: Undo::Auto }
}

/// The show an op names, else the project's default, else the only show there is.
fn resolve_show(ctx: &OpCtx<'_>, given: Option<&ShowId>) -> Result<ShowId, OpError> {
    if let Some(id) = given {
        return Ok(id.clone());
    }
    if let Some(Value::String(id)) = ctx.doc.get(&paths::default_show()) {
        return ShowId::new(id.as_str()).map_err(|e| OpError::invalid("show", e.to_string()));
    }
    let shows = ctx.doc.get(&DocPath::root().join("shows")).and_then(Value::as_object);
    match shows.map(|s| s.keys().collect::<Vec<_>>()).as_deref() {
        Some([only]) => ShowId::new(only.as_str()).map_err(|e| OpError::invalid("show", e.to_string())),
        Some([]) | None => Err(OpError::not_found("the project has no show").hint("make one with show.create")),
        Some(_) => Err(OpError::invalid("show", "the project has several shows and no default; name one")),
    }
}

/// The show stored at `shows/<id>`.
fn read_show(ctx: &OpCtx<'_>, id: &ShowId) -> Result<ShowDef, OpError> {
    let node = ctx
        .doc
        .get(&paths::show(id))
        .ok_or_else(|| OpError::not_found(format!("there is no show `{id}`")).hint("list shows under `shows`"))?;
    ShowDef::from_node(node).map_err(|e| OpError::invalid("show", format!("show `{id}` can't be read: {e}")))
}

/// Refuses a show the document ops must not leave: a structural refusal, or a node whose kind or parameters the
/// catalogue refuses.
fn refuse_broken(def: &ShowDef, graph: &GraphId) -> Result<(), OpError> {
    if let Some(finding) = def.check().into_iter().find(|f| f.refuses) {
        return Err(OpError::invalid("graph", english(&finding.text)));
    }
    let Some(graph_def) = def.graphs.get(graph) else { return Ok(()) };
    for (id, node) in &graph_def.nodes {
        let field = format!("graph/nodes/{id}");
        let kind = find_node_kind(node.kind.as_str()).ok_or_else(|| {
            OpError::invalid(format!("{field}/kind"), format!("no node kind `{}` exists", node.kind))
                .hint("automation.node_kinds lists the kinds")
        })?;
        if let Err(errors) = kind.check(&Value::Object(node.params.clone())) {
            let text = errors.0.iter().map(|e| english(&e.text)).collect::<Vec<_>>().join("; ");
            let path = errors.0.first().map(|e| e.path.replace('.', "/")).unwrap_or_default();
            return Err(OpError::invalid(format!("{field}/params/{path}").trim_end_matches('/').to_owned(), text));
        }
    }
    Ok(())
}

/// A patch that writes `value` at `path`, creating the missing parent tables on the way (the document applies a
/// change only under a parent that exists).
fn create(ctx: &OpCtx<'_>, path: &DocPath, value: Value) -> Patch {
    let mut at = path.clone();
    let mut value = value;
    while let Some((parent, key)) = at.split_last() {
        if parent.segments().is_empty() || ctx.doc.get(&parent).is_some() {
            break;
        }
        value = json!({ key: value });
        at = parent;
    }
    Patch::new().with(at, None, Some(value))
}

fn to_node(def: &ShowDef) -> Result<Value, OpError> {
    let mut node = Value::Null;
    def.to_node(&mut node).map_err(|e| OpError::internal(e.to_string()))?;
    Ok(node)
}

impl Op for CreateShow {
    fn meta() -> OpMeta {
        document_edit("show.create", "Make a new, empty show to hold automations.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::show(&self.id);
        if ctx.doc.get(&at).is_some() {
            return Err(OpError::conflict(format!("show `{}` already exists", self.id)));
        }
        let def = ShowDef::new(self.name.clone().unwrap_or_else(|| self.id.to_string()));
        let mut patch = create(ctx, &at, to_node(&def)?);
        let default = paths::default_show();
        let becomes_default = ctx.doc.get(&default).is_none();
        if becomes_default {
            patch.extend(create(ctx, &default, json!(self.id.as_str())));
        }
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({ "show": self.id, "default": becomes_default }));
        Ok(plan)
    }
}

impl Op for AddGraph {
    fn meta() -> OpMeta {
        document_edit("automation.add_graph", "Add an automation graph (its nodes and wires) to a show.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let show = resolve_show(ctx, self.show.as_ref())?;
        let def = read_show(ctx, &show)?;
        if def.graphs.contains_key(&self.id) {
            return Err(OpError::conflict(format!("show `{show}` already has a graph `{}`", self.id))
                .hint("remove it first, or choose another id"));
        }
        let def = def.with_graph(self.id.clone(), self.graph.clone());
        refuse_broken(&def, &self.id)?;
        let mut node = Value::Null;
        self.graph.to_node(&mut node).map_err(|e| OpError::internal(e.to_string()))?;
        let mut plan = Plan::edit(create(ctx, &paths::graph(&show, &self.id), node));
        plan.output = Some(json!({ "show": show, "graph": self.id, "nodes": self.graph.nodes.len() }));
        Ok(plan)
    }
}

impl Op for RemoveGraph {
    fn meta() -> OpMeta {
        document_edit("automation.remove_graph", "Remove an automation graph and its nodes from a show.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let show = resolve_show(ctx, self.show.as_ref())?;
        let path = paths::graph(&show, &self.graph);
        let before = ctx
            .doc
            .get(&path)
            .cloned()
            .ok_or_else(|| OpError::not_found(format!("show `{show}` has no graph `{}`", self.graph)))?;
        let mut plan = Plan::edit(Patch::new().with(path, Some(before), None));
        plan.output = Some(json!({ "show": show, "removed": self.graph }));
        Ok(plan)
    }
}

impl Op for Start {
    fn meta() -> OpMeta {
        live_action(
            "automation.start",
            "Start a show: its automations react to events until it stops.",
            Scope::Broadcast,
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let automation = service(ctx)?;
        let show = resolve_show(ctx, self.show.as_ref())?;
        let def = read_show(ctx, &show)?;
        if ctx.dry_run {
            let compiled = compile(&show, &def);
            let inert: Vec<_> = compiled
                .nodes
                .iter()
                .filter_map(|n| n.inert.as_ref().map(|why| json!({ "graph": n.graph, "node": n.id, "why": why })))
                .collect();
            return Ok(Plan::answer(json!({
                "applied": false,
                "show": show,
                "graphs": def.graphs.len(),
                "inert": inert,
                "findings": compiled.findings.iter().map(|f| english(&f.text)).collect::<Vec<_>>(),
                "running": automation.running().map(|r| r.show),
            })));
        }
        let run = automation.start_def(&show, &def)?;
        let status = automation.status();
        let findings: Vec<_> = automation.start_findings().iter().map(|f| english(&f.text)).collect();
        Ok(Plan::answer(json!({ "started": run, "graphs": status.graphs, "findings": findings })))
    }
}

impl Op for Stop {
    fn meta() -> OpMeta {
        live_action("automation.stop", "Stop the running show.", Scope::Perform)
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let automation = service(ctx)?;
        let running = automation.running();
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "running": running.map(|r| r.show) })));
        }
        automation.stop(ctx.actor)?;
        Ok(Plan::answer(json!({ "stopped": running.map(|r| r.show) })))
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        query("automation.status", "Report the running show: its graphs, inert or faulted nodes, and what was dropped.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let status = service(ctx)?.status();
        Ok(Plan::answer(serde_json::to_value(status).map_err(|e| OpError::internal(e.to_string()))?))
    }
}

impl Op for Values {
    fn meta() -> OpMeta {
        query("automation.values", "The running show's variables, groups, last firings and events.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let values = service(ctx)?.values();
        let answer = match values {
            Some(snapshot) => json!({ "running": true, "values": *snapshot }),
            None => json!({ "running": false }),
        };
        Ok(Plan::answer(answer))
    }
}

impl Op for Fire {
    fn meta() -> OpMeta {
        live_action(
            "automation.fire",
            "Send a test event into the running show; its actions run as you.",
            Scope::Perform,
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let automation = service(ctx)?;
        omuzaki_live_api::event::check_payload(&self.kind, &self.payload)
            .map_err(|e| OpError::invalid("payload", e.to_string()))?;
        if ctx.dry_run {
            let running = automation.running().map(|r| r.show);
            return Ok(Plan::answer(json!({ "applied": false, "kind": self.kind, "running": running })));
        }
        let id = automation.fire(self.kind.clone(), self.payload.clone(), ctx.actor)?;
        Ok(Plan::answer(json!({ "event": id, "kind": self.kind })))
    }
}

register_op!(CreateShow);
register_op!(AddGraph);
register_op!(RemoveGraph);
register_op!(Start);
register_op!(Stop);
register_op!(Status);
register_op!(Values);
register_op!(Fire);
