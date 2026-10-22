//! `audio.bus.*`: the mixer's buses and duck rules as document edits (design section 7.1), under the egress rule
//! (section 7.2).
//!
//! The graph lives in the document at `audio/graph` (a [`GraphSpec`]), so every bus edit is an ordinary undoable edit
//! (`Undo::Auto`) that a person and an agent share; [`follow`](crate::follow) applies the document's graph to the live
//! engine whenever it changes. Before the first bus edit the document has no graph, and the ops start from the
//! engine's (or from an empty one).
//!
//! **The egress rule:** every edit computes the (captured source → output) pairs before and after. If the set grows,
//! the edit needs a person: an agent's edit is refused (or queued for a person's approval) naming the new pairs.
//! Making things quieter or more private (mute, unroute, lower, delete) never grows the set, so it is always allowed.
//! The pairs are counted as if every input and output the graph names were bound and the mic were live, so an agent
//! can't route a microphone today that only reaches the stream once a person binds a device tomorrow.
//!
//! **The gain ceiling:** an agent may set a bus's gain up to the ceiling a person set for it (default 0 dB); only a
//! person sets the ceiling, at `audio/ceilings/<bus>`.

use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{
    Bindings, BusSpec, CapturedSource, Channels, DuckRule, DuckTrigger, Egress, GRAPH_DOC_PATH, GraphSpec, MAX_GAIN_DB,
    MicMode, NodeSpec, PROGRAM_BUS, SendSpec, SourceRef, egress_pairs,
};
use omuzaki_audio_api::ids::{BusId, DeviceRole, NodeId, NodeKindId};
use omuzaki_doc_api::{Change, DocPath, DocRead, Patch, Value};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// The ceiling an agent's gain stays under when a person has set none.
pub const DEFAULT_CEILING_DB: f32 = 0.0;

/// Where the document keeps the graph.
#[must_use]
pub fn graph_path() -> DocPath {
    GRAPH_DOC_PATH.iter().fold(DocPath::root(), |path, segment| path.join(*segment))
}

fn ceiling_path(bus: &BusId) -> DocPath {
    DocPath::root().join("audio").join("ceilings").join(bus.as_str())
}

/// The graph the document holds, if a bus edit has stored one.
///
/// # Errors
/// [`OpError::invalid`] when the stored value is not a graph.
pub fn stored_graph(doc: &dyn DocRead) -> Result<Option<GraphSpec>, OpError> {
    doc.get(&graph_path())
        .map(|value| {
            serde_json::from_value(value.clone()).map_err(|e| {
                OpError::invalid("graph", format!("the project's audio graph is not in the graph format: {e}"))
                    .hint("repair `audio/graph` in the project file, or undo the edit that broke it")
            })
        })
        .transpose()
}

/// The graph an edit starts from: the document's, else the engine's, else an empty one.
fn current(ctx: &OpCtx<'_>) -> Result<GraphSpec, OpError> {
    if let Some(graph) = stored_graph(ctx.doc)? {
        return Ok(graph);
    }
    Ok(ctx.service::<dyn AudioEngine>().map(AudioEngine::graph).unwrap_or_default())
}

/// The person-set gain ceiling of `bus`.
fn ceiling(ctx: &OpCtx<'_>, bus: &BusId) -> f32 {
    #[allow(clippy::cast_possible_truncation)] // a dB value the op stored from an f32.
    ctx.doc.get(&ceiling_path(bus)).and_then(Value::as_f64).map_or(DEFAULT_CEILING_DB, |db| db as f32)
}

/// Every input and output role either graph names, and a live mic: the widest the routing could ever carry.
fn widest(graphs: [&GraphSpec; 2]) -> Bindings {
    let mut bindings = Bindings { mic_mode: MicMode::Live, ..Bindings::default() };
    for bus in graphs.iter().flat_map(|g| &g.buses) {
        for source in &bus.sources {
            if let SourceRef::Device { role } = source {
                bindings.inputs.insert(role.clone());
            }
        }
        bindings.outputs.extend(bus.outputs.iter().cloned());
    }
    bindings
}

/// The (captured source → output) pairs `after` has and `before` lacks, as `mic to program_out`.
#[must_use]
pub fn new_egress(before: &GraphSpec, after: &GraphSpec) -> Vec<String> {
    let bindings = widest([before, after]);
    let old = egress_pairs(before, &bindings);
    egress_pairs(after, &bindings)
        .difference(&old)
        .map(|(CapturedSource(input), egress)| match egress {
            Egress::Output { role } => format!("{input} to {role}"),
            other => format!("{input} to {other:?}"),
        })
        .collect()
}

/// The bus `id` of `graph`.
fn find<'g>(graph: &'g GraphSpec, id: &BusId, field: &str) -> Result<&'g BusSpec, OpError> {
    graph.bus(id).ok_or_else(|| missing(graph, id, field))
}

fn missing(graph: &GraphSpec, id: &BusId, field: &str) -> OpError {
    let mut error = OpError::not_found(format!("there is no bus `{id}`"));
    error.field = Some(field.to_owned());
    let names: Vec<&str> = graph.buses.iter().map(|b| b.id.as_str()).collect();
    error.hint(if names.is_empty() {
        "the mixer has no buses yet: make one with audio.bus.create".to_owned()
    } else {
        format!("the buses are: {}", names.join(", "))
    })
}

fn bus_mut<'g>(graph: &'g mut GraphSpec, id: &BusId, field: &str) -> Result<&'g mut BusSpec, OpError> {
    let error = missing(graph, id, field);
    graph.buses.iter_mut().find(|b| &b.id == id).ok_or(error)
}

/// A gain an agent may set on `bus`, or the person it needs.
fn check_gain(ctx: &OpCtx<'_>, bus: &BusId, gain_db: f32, field: &str) -> Result<(), OpError> {
    if !gain_db.is_finite() || gain_db > MAX_GAIN_DB {
        return Err(OpError::invalid(field, format!("a gain is a number of dB up to {MAX_GAIN_DB}, got {gain_db}"))
            .hint("mute the bus to silence it"));
    }
    let ceiling = ceiling(ctx, bus);
    if gain_db > ceiling {
        ctx.require_person(format!("raising `{bus}` to {gain_db} dB, above the {ceiling} dB a person allowed"))?;
    }
    Ok(())
}

/// The edit from `before` to `after`: validated, held to the egress rule, stored at `audio/graph`, with `extra` values
/// set beside it.
fn edit(
    ctx: &OpCtx<'_>,
    before: &GraphSpec,
    after: &GraphSpec,
    extra: Vec<(DocPath, Value)>,
    summary: Value,
) -> Result<Plan, OpError> {
    after.validate_registered().map_err(|e| {
        OpError::invalid("graph", format!("the mixer would be invalid: {e}"))
            .hint("list the buses with audio.bus.list; sends must not loop, and every bus id once")
    })?;
    let grown = new_egress(before, after);
    if !grown.is_empty() {
        ctx.require_person(format!("this would send {}", grown.join(", ")))?;
    }
    let value = serde_json::to_value(after).map_err(|e| OpError::internal(e.to_string()))?;
    let mut changes = Vec::new();
    for (path, value) in std::iter::once((graph_path(), value)).chain(extra) {
        set(ctx, &mut changes, path, value);
    }
    Ok(Plan { output: Some(summary), ..Plan::edit(Patch { changes }) })
}

/// Adds the change that sets `path` to `value`, after the ones that create the tables above it that neither the
/// document nor `changes` has yet (`audio`, `audio/ceilings`), so undo removes them too.
fn set(ctx: &OpCtx<'_>, changes: &mut Vec<Change>, path: DocPath, value: Value) {
    let mut missing = Vec::new();
    let mut at = path.split_last().map(|(table, _)| table);
    while let Some(table) = at {
        if table == DocPath::root() || ctx.doc.get(&table).is_some() || changes.iter().any(|c| c.path == table) {
            break;
        }
        at = table.split_last().map(|(up, _)| up);
        missing.push(table);
    }
    for table in missing.into_iter().rev() {
        changes.push(Change { path: table, before: None, after: Some(Value::Object(serde_json::Map::new())) });
    }
    changes.push(Change { before: ctx.doc.get(&path).cloned(), path, after: Some(value) });
}

/// A bus as the list shows it.
fn describe(bus: &BusSpec, ceiling: f32) -> Value {
    json!({
        "bus": bus.id,
        "channels": bus.channels,
        "sources": bus.sources,
        "gain_db": bus.gain_db,
        "ceiling_db": ceiling,
        "muted": bus.muted,
        "sends": bus.sends,
        "outputs": bus.outputs,
        "chain": bus.chain.iter().map(|n| json!({ "id": n.id, "kind": n.kind, "enabled": n.enabled })).collect::<Vec<_>>(),
    })
}

/// List the mixer's buses, duck rules, and where captured audio goes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}], "x-omuzaki-fixture" = "audio"))]
pub struct BusList {}

impl Op for BusList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.bus.list",
            summary: "List the audio mixer's buses (gain, mute, routing, chain), its duck rules, and where captured audio goes.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let graph = current(ctx)?;
        let routes = new_egress(&GraphSpec::default(), &graph);
        Ok(Plan::answer(json!({
            "buses": graph.buses.iter().map(|b| describe(b, ceiling(ctx, &b.id))).collect::<Vec<_>>(),
            "ducks": graph.ducks,
            "captured_audio_reaches": routes,
        })))
    }
}

/// Add a bus to the audio mixer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "bus": "game", "channels": "stereo", "sends": [{ "to": "program" }] },
    { "bus": "sfx", "channels": "mono" }
], "x-omuzaki-fixture" = "audio"))]
pub struct BusCreate {
    /// The new bus's id (lowercase, `a-z0-9_`).
    pub bus: BusId,
    /// `mono` or `stereo`.
    pub channels: Channels,
    /// What feeds it: devices by role, feeds, players. Default: nothing (other buses can send to it).
    #[serde(default)]
    pub sources: Vec<SourceRef>,
    /// Where it sends, by bus id. Default: nowhere.
    #[serde(default)]
    pub sends: Vec<SendSpec>,
    /// The output roles it plays on (`program_out`, `monitor_out`). Default: none.
    #[serde(default)]
    pub outputs: Vec<DeviceRole>,
    /// Its fader in dB. Default 0.
    #[serde(default)]
    pub gain_db: f32,
}

impl Op for BusCreate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.bus.create",
            summary: "Add a bus to the audio mixer, with its sources, sends and outputs.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = current(ctx)?;
        if before.bus(&self.bus).is_some() {
            let mut error = OpError::conflict(format!("there is already a bus `{}`", self.bus));
            error.field = Some("bus".to_owned());
            return Err(error.hint("change it with audio.bus.route, or pick another id"));
        }
        check_gain(ctx, &self.bus, self.gain_db, "gain_db")?;
        let bus = BusSpec {
            id: self.bus.clone(),
            channels: self.channels,
            sources: self.sources.clone(),
            chain: default_chain(&self.bus),
            gain_db: self.gain_db,
            muted: false,
            sends: self.sends.clone(),
            outputs: self.outputs.clone(),
        };
        let summary = json!({ "created": describe(&bus, ceiling(ctx, &self.bus)) });
        let mut after = before.clone();
        after.buses.push(bus);
        edit(ctx, &before, &after, Vec::new(), summary)
    }
}

/// The kind of the program bus's safety limiter (`omuzaki-audio-fx`, #921).
pub const PROGRAM_LIMITER: &str = "fx.limiter";

/// A new bus's chain: on `program`, the -1 dBTP true-peak safety limiter, on by default (ADR 0018, review answer 6),
/// when this build has the kind; every other bus starts empty. Turn it off with `on = 0` or remove it from the chain.
fn default_chain(bus: &BusId) -> Vec<NodeSpec> {
    let Ok(kind) = NodeKindId::new(PROGRAM_LIMITER) else { return Vec::new() };
    if bus.as_str() != PROGRAM_BUS || omuzaki_audio_api::node::node_kind(&kind).is_none() {
        return Vec::new();
    }
    let Ok(id) = NodeId::new("limiter") else { return Vec::new() };
    vec![NodeSpec {
        id,
        kind,
        config: omuzaki_audio_api::node::NodeConfig::default(),
        params: std::collections::BTreeMap::new(),
        enabled: true,
        sidechain: None,
    }]
}

/// Remove a bus from the audio mixer, with the sends into it and the duck rules that name it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "bus": "music" }], "x-omuzaki-fixture" = "audio"))]
pub struct BusDelete {
    /// The bus to remove.
    pub bus: BusId,
}

impl Op for BusDelete {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.bus.delete",
            summary: "Remove a bus from the audio mixer, with the sends into it and its duck rules.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = current(ctx)?;
        find(&before, &self.bus, "bus")?;
        let mut after = before.clone();
        after.buses.retain(|b| b.id != self.bus);
        for bus in &mut after.buses {
            bus.sends.retain(|s| s.to != self.bus);
            for node in &mut bus.chain {
                if node.sidechain.as_ref() == Some(&self.bus) {
                    node.sidechain = None;
                }
            }
        }
        after.ducks.retain(|d| d.target != self.bus && d.trigger != DuckTrigger::Bus { id: self.bus.clone() });
        let rules = before.ducks.len() - after.ducks.len();
        edit(ctx, &before, &after, Vec::new(), json!({ "deleted": self.bus, "duck_rules_removed": rules }))
    }
}

/// Set a bus's fader, and (a person only) the ceiling agents may raise it to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "bus": "music", "gain_db": -12.0 }], "x-omuzaki-fixture" = "audio"))]
pub struct BusSetGain {
    /// The bus.
    pub bus: BusId,
    /// The fader in dB, at most 12. An agent may go up to the bus's ceiling (default 0 dB).
    pub gain_db: f32,
    /// The highest gain an agent may set on this bus, in dB. Only a person sets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gain_db: Option<f32>,
}

impl Op for BusSetGain {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.bus.set_gain",
            summary: "Set an audio bus's fader in dB; agents stay under the ceiling a person set (default 0 dB).",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = current(ctx)?;
        let mut extra = Vec::new();
        if let Some(max) = self.max_gain_db {
            ctx.require_person(format!("setting how loud agents may make `{}`", self.bus))?;
            if !max.is_finite() || max > MAX_GAIN_DB {
                return Err(OpError::invalid(
                    "max_gain_db",
                    format!("a ceiling is a number of dB up to {MAX_GAIN_DB}"),
                ));
            }
            extra.push((ceiling_path(&self.bus), json!(max)));
        }
        check_gain(ctx, &self.bus, self.gain_db, "gain_db")?;
        let mut after = before.clone();
        bus_mut(&mut after, &self.bus, "bus")?.gain_db = self.gain_db;
        let summary = json!({ "bus": self.bus, "gain_db": self.gain_db, "max_gain_db": self.max_gain_db });
        edit(ctx, &before, &after, extra, summary)
    }
}

/// Mute or unmute a bus. Muting is always allowed; unmuting a bus that carries captured audio out needs a person.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "bus": "music", "muted": true }], "x-omuzaki-fixture" = "audio"))]
pub struct BusMute {
    /// The bus.
    pub bus: BusId,
    /// `true` mutes it (everything leaving it, pre-fader sends included); `false` unmutes it.
    pub muted: bool,
}

impl Op for BusMute {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.bus.mute",
            summary: "Mute or unmute an audio bus; unmuting captured audio that leaves the device needs a person.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = current(ctx)?;
        let mut after = before.clone();
        bus_mut(&mut after, &self.bus, "bus")?.muted = self.muted;
        edit(ctx, &before, &after, Vec::new(), json!({ "bus": self.bus, "muted": self.muted }))
    }
}

/// Change what feeds a bus, where it sends and which outputs it plays on. Each list given replaces the bus's own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "bus": "music", "sends": [{ "to": "program", "gain_db": -6.0 }] },
    { "bus": "music", "outputs": [] }
], "x-omuzaki-fixture" = "audio"))]
pub struct BusRoute {
    /// The bus.
    pub bus: BusId,
    /// The new sources. Default: unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<SourceRef>>,
    /// The new sends. Default: unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sends: Option<Vec<SendSpec>>,
    /// The new output roles. Default: unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<DeviceRole>>,
}

impl Op for BusRoute {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.bus.route",
            summary: "Change an audio bus's sources, sends or outputs; sending captured audio somewhere new needs a person.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.sources.is_none() && self.sends.is_none() && self.outputs.is_none() {
            return Err(OpError::invalid("bus", "name at least one of sources, sends and outputs"));
        }
        let before = current(ctx)?;
        let mut after = before.clone();
        let bus = bus_mut(&mut after, &self.bus, "bus")?;
        if let Some(sources) = &self.sources {
            bus.sources.clone_from(sources);
        }
        if let Some(sends) = &self.sends {
            bus.sends.clone_from(sends);
        }
        if let Some(outputs) = &self.outputs {
            bus.outputs.clone_from(outputs);
        }
        let summary = json!({ "routed": describe(bus, ceiling(ctx, &self.bus)) });
        edit(ctx, &before, &after, Vec::new(), summary)
    }
}

/// Set or remove a duck rule: lower a bus while another bus is loud (or a gate is open).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "target": "music", "trigger": { "type": "bus", "id": "mic" }, "depth_db": 12.0 },
    { "target": "music", "trigger": { "type": "bus", "id": "mic" }, "remove": true }
], "x-omuzaki-fixture" = "audio"))]
pub struct BusDuck {
    /// The bus to lower.
    pub target: BusId,
    /// What lowers it: a bus's level (`{"type": "bus", "id": "mic"}`) or a gate a sound opens.
    pub trigger: DuckTrigger,
    /// How far, in dB. Default 12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_db: Option<f32>,
    /// The trigger bus's level that starts it, in dBFS. Default -40.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_db: Option<f32>,
    /// How fast it goes down, in ms. Default 10.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_ms: Option<f32>,
    /// How long it stays down after the trigger stops, in ms. Default 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<f32>,
    /// How fast it comes back, in ms. Default 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_ms: Option<f32>,
    /// `true` removes the rule for this target and trigger instead.
    #[serde(default)]
    pub remove: bool,
}

impl Op for BusDuck {
    fn meta() -> OpMeta {
        OpMeta {
            name: "audio.bus.duck",
            summary: "Set or remove a duck rule: lower one audio bus while another is loud (music under the voice).",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = current(ctx)?;
        find(&before, &self.target, "target")?;
        let mut after = before.clone();
        let at = after.ducks.iter().position(|d| d.target == self.target && d.trigger == self.trigger);
        let old = at.map(|i| after.ducks.remove(i));
        if self.remove {
            if old.is_none() {
                return Err(OpError::not_found(format!("`{}` has no duck rule for that trigger", self.target))
                    .hint("list the rules with audio.bus.list"));
            }
            let summary = json!({ "target": self.target, "trigger": self.trigger, "removed": true });
            return edit(ctx, &before, &after, Vec::new(), summary);
        }
        let base = old.unwrap_or(DuckRule {
            trigger: self.trigger.clone(),
            target: self.target.clone(),
            depth_db: 12.0,
            threshold_db: -40.0,
            attack_ms: 10.0,
            hold_ms: 200.0,
            release_ms: 400.0,
        });
        let rule = DuckRule {
            depth_db: self.depth_db.unwrap_or(base.depth_db),
            threshold_db: self.threshold_db.unwrap_or(base.threshold_db),
            attack_ms: self.attack_ms.unwrap_or(base.attack_ms),
            hold_ms: self.hold_ms.unwrap_or(base.hold_ms),
            release_ms: self.release_ms.unwrap_or(base.release_ms),
            ..base
        };
        let summary = json!({ "rule": rule });
        after.ducks.insert(at.unwrap_or(after.ducks.len()), rule);
        edit(ctx, &before, &after, Vec::new(), summary)
    }
}

register_op!(BusList);
register_op!(BusCreate);
register_op!(BusDelete);
register_op!(BusSetGain);
register_op!(BusMute);
register_op!(BusRoute);
register_op!(BusDuck);
