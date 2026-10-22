//! [`AutomationControl`]: what the runtime ops call (design D13), and what `automation.status` ([`Status`]) and
//! `automation.values` ([`ValuesSnapshot`], design D10) return.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Actor, OpError, SessionId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{EventId, Source};
use crate::ids::{EventKind, GraphId, GroupId, NodeId, PortName, ShowId, VarName};
use crate::limits::MONITOR_VALUE_CHARS;
use crate::need::Need;
use crate::show::{VarScope, VarType};

/// The running show, as the runtime ops see it. One show runs at a time per studio (design D5). Implemented by the
/// engine (`omuzaki-live`); each method is what its op does, and each change says who asked (`by`).
pub trait AutomationControl: Send + Sync {
    /// Compiles and starts `show`, opening its automation session.
    ///
    /// # Errors
    /// An [`OpError`] if another show runs, the show doesn't exist or doesn't compile.
    fn start(&self, show: &ShowId, by: &Actor) -> Result<RunInfo, OpError>;
    /// Stops the running show: `show.stopping`, up to [`STOP_GRACE_SECONDS`](crate::limits::STOP_GRACE_SECONDS) for
    /// calls in flight, `Stop` to every node, a final flush, the session ended.
    ///
    /// # Errors
    /// An [`OpError`] if no show runs.
    fn stop(&self, by: &Actor) -> Result<(), OpError>;
    /// Pauses the running show (the panic button): no new tokens, wakes and replies dropped and counted, the session
    /// paused (design D12).
    ///
    /// # Errors
    /// An [`OpError`] if no show runs.
    fn pause(&self, by: &Actor) -> Result<(), OpError>;
    /// Resumes a paused show; variables and group switches survive a pause.
    ///
    /// # Errors
    /// An [`OpError`] if no show is paused.
    fn resume(&self, by: &Actor) -> Result<(), OpError>;
    /// The running show's state.
    fn status(&self) -> Status;
    /// The latest values snapshot, refreshed at most [`MONITOR_REFRESH_PER_SECOND`](crate::limits::MONITOR_REFRESH_PER_SECOND)
    /// times a second while someone reads it; `None` when no show runs. Never waits on the engine.
    fn values(&self) -> Option<Arc<ValuesSnapshot>>;
    /// Sets a variable, type-checked; announced as `var.changed`.
    ///
    /// # Errors
    /// An [`OpError`] for an undeclared variable or a value of the wrong type.
    fn set_variable(&self, name: &VarName, value: Value, by: &Actor) -> Result<(), OpError>;
    /// Switches a group's runtime state (design D12).
    ///
    /// # Errors
    /// An [`OpError`] for an undeclared group.
    fn switch_group(&self, group: &GroupId, on: bool, by: &Actor) -> Result<(), OpError>;
    /// Clears a graph's loop-guard suspension.
    ///
    /// # Errors
    /// An [`OpError`] for a graph the show doesn't have.
    fn resume_graph(&self, graph: &GraphId, by: &Actor) -> Result<(), OpError>;
    /// Publishes a test event that runs as `by` (design D7): `Source::Manual` with `acting = by`.
    ///
    /// # Errors
    /// An [`OpError`] for an unknown kind, a bad payload or a full inbox.
    fn fire(&self, kind: EventKind, payload: Value, by: &Actor) -> Result<EventId, OpError>;
}

/// A run of a show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RunInfo {
    /// The show.
    pub show: ShowId,
    /// The seed every node's stream derives from; pin it in `show.toml` to reproduce the run.
    pub seed: u32,
    /// The run's automation session.
    pub session: SessionId,
    /// When it started, on the runner clock.
    pub started_at: Seconds,
}

impl RunInfo {
    /// The run of `show` with `seed` in `session`, started at `started_at`.
    pub fn new(show: ShowId, seed: u32, session: SessionId, started_at: Seconds) -> Self {
        Self { show, seed, session, started_at }
    }
}

/// What `automation.status` returns.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Status {
    /// The running show, if any.
    pub running: Option<RunInfo>,
    /// Whether it is paused.
    pub paused: bool,
    /// Each graph's state, by graph id.
    pub graphs: Vec<GraphStatus>,
    /// What was dropped, and why.
    pub dropped: Dropped,
    /// The needs nobody approved yet: what the approval card lists.
    pub missing: Vec<Need>,
}

impl Status {
    /// Running `run`.
    #[must_use]
    pub fn with_running(mut self, run: RunInfo) -> Self {
        self.running = Some(run);
        self
    }

    /// Paused or not.
    #[must_use]
    pub fn with_paused(mut self, paused: bool) -> Self {
        self.paused = paused;
        self
    }

    /// With these graphs.
    #[must_use]
    pub fn with_graphs(mut self, graphs: Vec<GraphStatus>) -> Self {
        self.graphs = graphs;
        self
    }

    /// With these counters.
    #[must_use]
    pub fn with_dropped(mut self, dropped: Dropped) -> Self {
        self.dropped = dropped;
        self
    }

    /// With these missing approvals.
    #[must_use]
    pub fn with_missing(mut self, missing: Vec<Need>) -> Self {
        self.missing = missing;
        self
    }
}

/// One graph's state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct GraphStatus {
    /// The graph.
    pub graph: GraphId,
    /// Whether it runs: enabled, its group on, not suspended, the show running (design D12).
    pub active: bool,
    /// Its loop-guard suspension, if any.
    pub suspended: Option<Suspension>,
    /// Its inert nodes and why.
    pub inert: Vec<InertNode>,
    /// Its faulted nodes (a panic inside them), inert until the show restarts.
    pub faulted: Vec<NodeId>,
    /// How many `NodeError`s its nodes returned.
    pub errors: u64,
}

impl GraphStatus {
    /// An inactive graph with nothing to report.
    pub fn new(graph: GraphId) -> Self {
        Self { graph, active: false, suspended: None, inert: Vec::new(), faulted: Vec::new(), errors: 0 }
    }

    /// Active or not.
    #[must_use]
    pub fn with_active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Suspended.
    #[must_use]
    pub fn with_suspension(mut self, suspension: Suspension) -> Self {
        self.suspended = Some(suspension);
        self
    }

    /// With these inert nodes.
    #[must_use]
    pub fn with_inert(mut self, inert: Vec<InertNode>) -> Self {
        self.inert = inert;
        self
    }

    /// With these faulted nodes.
    #[must_use]
    pub fn with_faulted(mut self, faulted: Vec<NodeId>) -> Self {
        self.faulted = faulted;
        self
    }

    /// With this many node errors.
    #[must_use]
    pub fn with_errors(mut self, errors: u64) -> Self {
        self.errors = errors;
        self
    }
}

/// Why and since when a graph is suspended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Suspension {
    /// Why.
    pub reason: SuspendReason,
    /// When, on the runner clock.
    pub at: Seconds,
}

impl Suspension {
    /// Suspended for `reason` at `at`.
    pub fn new(reason: SuspendReason, at: Seconds) -> Self {
        Self { reason, at }
    }
}

/// Why the loop guard suspended a graph (design D8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SuspendReason {
    /// Too many activations or too deep a stack in one step.
    #[non_exhaustive]
    LoopGuard {
        /// The last nodes on the stack (at most [`LOOP_GUARD_LAST_NODES`](crate::limits::LOOP_GUARD_LAST_NODES)), so the
        /// loop is visible.
        last: Vec<NodeId>,
    },
    /// An `event.emit` chain past [`MAX_EMIT_HOPS`](crate::limits::MAX_EMIT_HOPS).
    EmitHops,
}

impl SuspendReason {
    /// The loop guard tripped with `last` on the stack.
    pub fn loop_guard(last: Vec<NodeId>) -> Self {
        SuspendReason::LoopGuard { last }
    }
}

/// An inert node and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct InertNode {
    /// The node.
    pub node: NodeId,
    /// Why it is inert.
    pub why: InertReason,
}

impl InertNode {
    /// `node`, inert for `why`.
    pub fn new(node: NodeId, why: InertReason) -> Self {
        Self { node, why }
    }
}

/// Why a node is inert (design D18).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InertReason {
    /// Its kind isn't registered in this build (a missing plugin).
    UnknownKind,
    /// Its parameters no longer pass the kind's check.
    BadParams,
    /// Its needs aren't approved on this machine.
    #[non_exhaustive]
    NotApproved {
        /// What is missing.
        needs: Vec<Need>,
    },
    /// An `op.run` whose op isn't registered, isn't allowed, or whose arguments don't decode.
    OpNotAllowed,
    /// A variable or group it names isn't declared, or a global's type conflicts.
    Undeclared,
    /// A trigger on an event kind nobody registered.
    UnknownEventKind,
    /// It panicked; inert until the show restarts.
    Faulted,
}

impl InertReason {
    /// Waiting for these approvals.
    pub fn not_approved(needs: Vec<Need>) -> Self {
        InertReason::NotApproved { needs }
    }
}

/// What the engine dropped, by reason. Shown on the Rules panel; none of them stops the show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Dropped {
    /// New tokens over a graph's rate (design D8).
    pub rate: u64,
    /// Droppable events dropped because their queue was full.
    pub inbox: u64,
    /// Other events refused past the inbox's hard cap (`Full`; the producer retries).
    pub inbox_full: u64,
    /// Non-droppable events a producer lost when its retry buffer overflowed, as producers report them through
    /// [`EventSink::producer_dropped`](crate::event::EventSink::producer_dropped) (design D9). Any loss here raises
    /// the "events are being lost" finding.
    pub producer: u64,
    /// Activations that reached an inert node.
    pub inert: u64,
    /// Tokens, wakes and replies of an inactive graph.
    pub inactive: u64,
    /// Parked tokens of instances a hot reload rebuilt.
    pub reload: u64,
    /// Tokens, wakes and replies dropped while paused.
    pub paused: u64,
    /// Platform redeliveries dropped by the inbox: the same (platform, kind, origin id) within ten minutes.
    #[serde(default)]
    pub duplicate: u64,
}

/// What `automation.values` returns: the values monitor (design D10). Refreshed into a one-slot mailbox, so the
/// query never waits on the engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ValuesSnapshot {
    /// When it was taken, on the runner clock.
    pub at: Seconds,
    /// Every variable.
    pub variables: Vec<VarRow>,
    /// Every group's runtime state.
    pub groups: Vec<GroupRow>,
    /// Every graph's state.
    pub graphs: Vec<GraphStatus>,
    /// The last firings, newest last (at most [`MONITOR_FIRINGS`](crate::limits::MONITOR_FIRINGS)).
    pub firings: Vec<Firing>,
    /// The last events, newest last (at most [`MONITOR_EVENTS`](crate::limits::MONITOR_EVENTS)).
    pub events: Vec<EventRow>,
    /// The drop counters.
    pub dropped: Dropped,
    /// The approvals still missing.
    pub missing: Vec<Need>,
}

impl ValuesSnapshot {
    /// An empty snapshot taken at `at`.
    pub fn new(at: Seconds) -> Self {
        Self {
            at,
            variables: Vec::new(),
            groups: Vec::new(),
            graphs: Vec::new(),
            firings: Vec::new(),
            events: Vec::new(),
            dropped: Dropped::default(),
            missing: Vec::new(),
        }
    }
}

/// A node of a graph.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct NodeRef {
    /// The graph.
    pub graph: GraphId,
    /// The node.
    pub node: NodeId,
}

impl NodeRef {
    /// `graph`'s node `node`.
    pub fn new(graph: GraphId, node: NodeId) -> Self {
        Self { graph, node }
    }
}

/// One variable in the values monitor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct VarRow {
    /// The variable.
    pub name: VarName,
    /// Its scope.
    pub scope: VarScope,
    /// Its type.
    pub ty: VarType,
    /// Its value, whole: `automation.set_variable` edits it from here.
    pub value: Value,
    /// When it last changed, on the runner clock.
    pub changed_at: Option<Seconds>,
    /// The node that last changed it; `None` when a person or the start did.
    pub changed_by: Option<NodeRef>,
}

impl VarRow {
    /// `name` (`scope`, `ty`) holding `value`, unchanged this run.
    pub fn new(name: VarName, scope: VarScope, ty: VarType, value: Value) -> Self {
        Self { name, scope, ty, value, changed_at: None, changed_by: None }
    }

    /// Changed at `at` by `by`.
    #[must_use]
    pub fn with_change(mut self, at: Seconds, by: Option<NodeRef>) -> Self {
        self.changed_at = Some(at);
        self.changed_by = by;
        self
    }
}

/// One group in the values monitor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct GroupRow {
    /// The group.
    pub group: GroupId,
    /// Its runtime state.
    pub on: bool,
}

impl GroupRow {
    /// `group`, on or off.
    pub fn new(group: GroupId, on: bool) -> Self {
        Self { group, on }
    }
}

/// One firing in the values monitor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Firing {
    /// When, on the runner clock.
    pub at: Seconds,
    /// The node that fired.
    pub node: NodeRef,
    /// The output it fired.
    pub port: PortName,
    /// The values it had set in the token, each [`shown`] (cut to
    /// [`MONITOR_VALUE_CHARS`](crate::limits::MONITOR_VALUE_CHARS) characters).
    pub values: BTreeMap<PortName, String>,
}

impl Firing {
    /// `node` fired `port` at `at`, with no values.
    pub fn new(at: Seconds, node: NodeRef, port: PortName) -> Self {
        Self { at, node, port, values: BTreeMap::new() }
    }

    /// With this value, shown.
    #[must_use]
    pub fn with_value(mut self, port: PortName, value: &Value) -> Self {
        self.values.insert(port, shown(value));
        self
    }
}

/// One event in the values monitor. Events live in memory only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct EventRow {
    /// Its arrival number.
    pub id: EventId,
    /// Its kind.
    pub kind: EventKind,
    /// Who produced it.
    pub source: Source,
    /// When, on the runner clock.
    pub at: Seconds,
    /// A one-line summary (the user and the text of a chat message, the reward of a redeem).
    pub summary: String,
}

impl EventRow {
    /// The event `id` of `kind` from `source` at `at`, summarised as `summary`.
    pub fn new(id: EventId, kind: EventKind, source: Source, at: Seconds, summary: impl Into<String>) -> Self {
        Self { id, kind, source, at, summary: summary.into() }
    }
}

/// A value as the values monitor shows it: text as it is, anything else as JSON, cut to
/// [`MONITOR_VALUE_CHARS`] characters with a trailing `…` when longer.
pub fn shown(value: &Value) -> String {
    let full = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    match full.char_indices().nth(MONITOR_VALUE_CHARS) {
        Some((cut, _)) => format!("{}…", &full[..cut]),
        None => full,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn shown_values_are_cut_at_the_limit_by_characters() {
        assert_eq!(shown(&json!("Bonk #4")), "Bonk #4");
        assert_eq!(shown(&json!({"a": 1})), "{\"a\":1}");
        let long = "あ".repeat(MONITOR_VALUE_CHARS + 5);
        let cut = shown(&json!(long));
        assert_eq!(cut.chars().count(), MONITOR_VALUE_CHARS + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(shown(&json!("あ".repeat(MONITOR_VALUE_CHARS))).chars().count(), MONITOR_VALUE_CHARS);
    }
}
