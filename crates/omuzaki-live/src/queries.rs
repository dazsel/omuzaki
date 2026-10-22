//! The queries agents build shows from (design D13, D17; #1177, #1180): `show.list` and `show.describe` for the
//! document, `automation.describe` for what the engine makes of a show (each node's ports, armed or inert and why,
//! the findings, the show's needs and which are approved), and `automation.node_kinds` and `automation.event_kinds`
//! for the catalogue.

use std::collections::BTreeSet;

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_live_api::event::event_kinds;
use omuzaki_live_api::ids::{GraphId, ShowId};
use omuzaki_live_api::node::{DynNodeKind, PortDecl, node_kinds};
use omuzaki_live_api::show::{ShowDef, paths};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::compile::{Arming, compile_with};
use crate::core::english;
use crate::ops::{query, read_show, resolve_show, service};

/// List the project's shows: ids, names, graph counts, which is the default and which is running.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}], "x-omuzaki-fixture" = "show"))]
pub struct ListShows {}

/// Describe a show as written: its settings, variables, groups and graphs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main" }], "x-omuzaki-fixture" = "show-graph"))]
pub struct DescribeShow {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
}

/// What the engine makes of a show: every node's ports, whether it is armed or inert and why, the findings, and the
/// show's needs with which are approved on this computer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "show": "main" }, { "show": "main", "graph": "brb" }], "x-omuzaki-fixture" = "show-graph"))]
pub struct DescribeAutomation {
    /// The show; the project's default show when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show: Option<ShowId>,
    /// Only this graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph: Option<GraphId>,
}

/// Every node kind: id, summary, class, category, parameter schema and the ports of its default parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "search": "wait" }, { "category": "flow" }]))]
pub struct NodeKinds {
    /// Only this palette group (`triggers`, `flow`, `math`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Words that must all appear in the kind's id or summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
}

/// Every event kind: its summary, payload schema and whether a full inbox may drop it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct EventKinds {}

fn ports_json(ports: &[PortDecl]) -> Value {
    ports
        .iter()
        .map(|p| {
            let mut port = json!({ "name": p.name, "type": p.ty.as_str() });
            if p.live {
                port["live"] = json!(true);
            }
            if let Some(default) = &p.default {
                port["default"] = default.to_json();
            }
            if let Some(summary) = p.summary {
                port["summary"] = json!(summary);
            } else if let Some(label) = &p.label {
                port["summary"] = json!(label);
            }
            port
        })
        .collect()
}

impl Op for ListShows {
    fn meta() -> OpMeta {
        query("show.list", "List the project's shows, the default and the running one.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let default = ctx.doc.get(&paths::default_show()).and_then(Value::as_str).map(str::to_owned);
        let running = service(ctx).ok().and_then(super::service::Automation::running).map(|r| r.show);
        let ids: Vec<String> = ctx
            .doc
            .get(&DocPath::root().join("shows"))
            .and_then(Value::as_object)
            .map(|shows| shows.keys().cloned().collect())
            .unwrap_or_default();
        let mut shows = Vec::new();
        for id in ids {
            let Ok(show) = ShowId::new(id.as_str()) else { continue };
            let def = ctx.doc.get(&paths::show(&show)).and_then(|n| ShowDef::from_node(n).ok());
            shows.push(json!({
                "id": show,
                "name": def.as_ref().map(|d| d.name.clone()),
                "graphs": def.as_ref().map_or(0, |d| d.graphs.len()),
                "default": default.as_deref() == Some(id.as_str()),
                "running": running.as_ref() == Some(&show),
            }));
        }
        Ok(Plan::answer(json!({ "shows": shows })))
    }
}

impl Op for DescribeShow {
    fn meta() -> OpMeta {
        query("show.describe", "Describe a show: settings, variables, groups and graphs.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let show = resolve_show(ctx, self.show.as_ref())?;
        let def = read_show(ctx, &show)?;
        let graphs: serde_json::Map<String, Value> = def
            .graphs
            .iter()
            .map(|(id, g)| {
                let nodes: serde_json::Map<String, Value> =
                    g.nodes.iter().map(|(n, node)| (n.to_string(), json!(node.kind.as_str()))).collect();
                let about = json!({
                    "name": g.name, "enabled": g.enabled, "group": g.group, "summary": g.summary, "nodes": nodes,
                });
                (id.to_string(), about)
            })
            .collect();
        Ok(Plan::answer(json!({
            "show": show,
            "name": def.name,
            "seed": def.seed,
            "variables": def.variables,
            "groups": def.groups,
            "graphs": graphs,
        })))
    }
}

impl Op for DescribeAutomation {
    fn meta() -> OpMeta {
        query("automation.describe", "Describe what the engine makes of a show: ports, states, findings, needs.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let show = resolve_show(ctx, self.show.as_ref())?;
        let def = read_show(ctx, &show)?;
        if let Some(graph) = &self.graph
            && !def.graphs.contains_key(graph)
        {
            return Err(OpError::not_found(format!("show `{show}` has no graph `{graph}`")));
        }
        let (approvals, hasher) = match service(ctx) {
            Ok(automation) => automation.arming_for(&show),
            Err(_) => (
                omuzaki_live_api::services::ShowApprovals::default(),
                std::sync::Arc::new(crate::compile::FileHasher) as _,
            ),
        };
        let compiled = compile_with(&show, &def, Arming { approvals: &approvals, hasher: hasher.as_ref() }, None);
        let wanted = |g: &GraphId| self.graph.as_ref().is_none_or(|only| only == g);
        let mut graphs = serde_json::Map::new();
        let mut needs = BTreeSet::new();
        for node in compiled.nodes.iter().filter(|n| wanted(&n.graph)) {
            needs.extend(node.needs.iter().filter(|n| !n.is_declaration()).cloned());
            let kind = def.graphs.get(&node.graph).and_then(|g| g.nodes.get(&node.id)).map(|n| n.kind.as_str());
            let state = match &node.inert {
                None => json!("armed"),
                Some(why) => json!({ "inert": why }),
            };
            let entry = graphs.entry(node.graph.to_string()).or_insert_with(|| json!({ "nodes": {} }));
            entry["nodes"][node.id.as_str()] = json!({
                "kind": kind,
                "class": node.class,
                "state": state,
                "inputs": ports_json(&node.ports.inputs),
                "outputs": ports_json(&node.ports.outputs),
            });
        }
        let findings: Vec<Value> = compiled
            .findings
            .iter()
            .filter(|f| f.graph.as_ref().is_none_or(wanted))
            .map(|f| json!({ "graph": f.graph, "node": f.node, "inert": f.inert, "text": english(&f.text) }))
            .collect();
        let needs: Vec<Value> = needs
            .into_iter()
            .map(|need| {
                let approved = approvals.missing([&need]).is_empty()
                    || compiled.nodes.iter().any(|n| n.needs.contains(&need) && n.inert.is_none());
                json!({ "need": need, "approved": approved })
            })
            .collect();
        Ok(Plan::answer(json!({ "show": show, "graphs": graphs, "findings": findings, "needs": needs })))
    }
}

impl Op for NodeKinds {
    fn meta() -> OpMeta {
        query("automation.node_kinds", "List the node kinds: ports, parameters and what each does.")
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        let words: Vec<String> =
            self.search.as_deref().unwrap_or("").split_whitespace().map(str::to_lowercase).collect();
        let mut kinds: Vec<_> = node_kinds().collect();
        kinds.sort_by_key(|k| (k.meta)().id);
        let listed: Vec<Value> = kinds
            .into_iter()
            .filter_map(|kind| {
                let meta = DynNodeKind::meta(kind);
                if self.category.as_deref().is_some_and(|c| c != meta.category.as_str()) {
                    return None;
                }
                let haystack = format!("{} {}", meta.id, meta.summary).to_lowercase();
                if !words.iter().all(|w| haystack.contains(w.as_str())) {
                    return None;
                }
                let defaults = kind.defaults();
                let ports = DynNodeKind::ports(kind, &defaults).ok();
                Some(json!({
                    "id": meta.id,
                    "summary": meta.summary,
                    "class": meta.class,
                    "category": meta.category.as_str(),
                    "params": kind.schema(),
                    "defaults": defaults,
                    "inputs": ports.as_ref().map(|p| ports_json(&p.inputs)),
                    "outputs": ports.as_ref().map(|p| ports_json(&p.outputs)),
                }))
            })
            .collect();
        Ok(Plan::answer(json!({ "kinds": listed })))
    }
}

impl Op for EventKinds {
    fn meta() -> OpMeta {
        query("automation.event_kinds", "List the event kinds a trigger can listen to, with their payloads.")
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut kinds: Vec<_> = event_kinds().collect();
        kinds.sort_by_key(|k| k.kind);
        let listed: Vec<Value> = kinds
            .into_iter()
            .map(|k| json!({ "kind": k.kind, "summary": k.summary, "payload": (k.schema)(), "droppable": k.droppable }))
            .collect();
        Ok(Plan::answer(json!({ "kinds": listed, "custom": "custom.<name>: any show's own events" })))
    }
}

register_op!(ListShows);
register_op!(DescribeShow);
register_op!(DescribeAutomation);
register_op!(NodeKinds);
register_op!(EventKinds);
