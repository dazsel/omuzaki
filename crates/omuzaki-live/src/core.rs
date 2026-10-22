//! The engine's state machine (design D3, D5, D6, D8): a compiled show, its tokens, wakes and calls in flight, run one
//! item at a time. It has no thread and no clock of its own: the engine thread ([`crate::service`]) feeds it items
//! with their times, and tests drive it directly, so its ordering is exact and reproducible.
//!
//! - **A step** seeds a stack (an event's listening triggers, each in a new token; a wake's or a reply's node, in the
//!   token that asked) and runs it depth-first: the pulses an activation fires go to their destinations in ascending
//!   (node, port), pushed so that the first pulse's whole subtree runs before the second's.
//! - **Tokens** carry each event's values along its path: a node's `set` lands in the token, and a read of an
//!   upstream output looks there first, then at a live output, then at the source's last value, then at the
//!   defaults.
//! - **Faults never stop the show:** a `NodeError` drops that branch and is counted; a panic makes the node faulted
//!   (inert until the show restarts); the loop guard suspends the graph that ran away.
//! - **Groups and rates (design D8, D12):** a graph runs while it is enabled, its group is on and it isn't suspended. A
//!   switch lands when the step that asked closes; switching a graph off drops its wakes and ignores its replies,
//!   switching it on sends its nodes `Start`. Each graph starts at most 500 new tokens a second (a burst of 2,000).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use omuzaki_l10n_api::{MessageTable, Text};
use omuzaki_live_api::event::payloads::{EngineTick, ShowLifecycle, VarChanged};
use omuzaki_live_api::event::{LiveEvent, Source, UtcMillis, check_payload};
use omuzaki_live_api::ids::{EventKind, GraphId, GroupId, NodeId, PortName, VarName};
use omuzaki_live_api::limits::{
    LOOP_GUARD_LAST_NODES, MAX_ACTIVATIONS_PER_STEP, MAX_CALLS_IN_FLIGHT, MAX_EMIT_HOPS, MAX_STACK_DEPTH,
    MAX_WAKES_PER_NODE, MONITOR_EVENTS, MONITOR_FIRINGS, TOKEN_BURST_PER_GRAPH, TOKENS_PER_SECOND_PER_GRAPH,
};
use omuzaki_live_api::node::{CallId, EvalCx, EvalHost, Input, Node, NodeCx, NodeError, NodeHost, WakeId, node_rng};
use omuzaki_live_api::services::{
    Dropped, EventRow, Firing, GraphStatus, GroupRow, InertNode, InertReason, NodeRef, SuspendReason, Suspension,
    ValuesSnapshot, VarRow, shown,
};
use omuzaki_live_api::show::{VarScope, VariableDef};
use omuzaki_live_api::value::{PortType, Val, ValError};
use omuzaki_ops_api::{Actor, OpError};
use serde_json::Value;

use crate::compile::{Compiled, CompiledNode};
use crate::vars::Files;

/// What arrives at the engine from outside a step.
#[derive(Debug)]
pub(crate) enum Item {
    /// An event, from a producer, `automation.fire` or the engine itself.
    Event(LiveEvent),
    /// An op call came back.
    Reply { call: CallId, outcome: Result<Option<Value>, OpError> },
    /// A change from outside the show (`automation.switch_group`, `automation.set_variable`,
    /// `automation.resume_graph`), run as a step of its own so it lands between two steps.
    Control(Control),
}

/// A change to a running show from outside it, checked by the service before it is queued.
#[derive(Debug)]
pub(crate) enum Control {
    /// Switches a group's runtime state.
    SwitchGroup { group: GroupId, on: bool },
    /// Sets a variable (its type was checked).
    SetVariable { name: VarName, value: Val },
    /// Lifts a graph's suspension.
    ResumeGraph { graph: GraphId },
    /// The panic button (design D12): no new tokens start, and wakes and replies are dropped, all counted.
    Pause,
    /// Undoes [`Pause`](Self::Pause); variables and group switches survived it.
    Resume,
}

/// How many rounds of group switches one step's close runs: a switch made by a `Start` that a switch sent lands in
/// the next round, and a show that keeps toggling itself stops here.
const SWITCH_ROUNDS: usize = 8;

/// An op call a step asked for, handed to the dispatcher when the step closes (design D7).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpRequest {
    pub call: CallId,
    pub op: String,
    pub args: Value,
    /// The caller a manual event runs as; `None` runs in the show's name.
    pub acting: Option<Actor>,
    /// The rule and the event, never the event's content (design D7).
    pub cause: String,
}

/// A token: the event it started from, the values set in it, and how many wakes and calls still hold it.
struct Token {
    root: Arc<LiveEvent>,
    values: BTreeMap<(usize, PortName), Val>,
    holds: usize,
}

/// A pending wake.
#[derive(Clone, Copy)]
struct Wake {
    node: usize,
    id: WakeId,
    token: u64,
}

/// What an activation hands the node.
enum Act {
    Start,
    Stop,
    Event(PortName),
    Trigger,
    Wake(WakeId),
    Reply(CallId, Result<Option<Value>, OpError>),
}

/// One activation on the stack.
struct Activation {
    node: usize,
    act: Act,
    token: u64,
}

/// A variable's declaration, value and last change.
struct Variable {
    def: VariableDef,
    value: Val,
    changed: Option<(Seconds, Option<NodeRef>)>,
}

/// A graph's design-time switch and its group.
struct GraphRun {
    enabled: bool,
    group: Option<GroupId>,
}

/// A graph's token bucket (design D8): `level` tokens at `at`, refilled on the engine clock.
#[derive(Clone, Copy)]
struct Bucket {
    level: f64,
    at: Seconds,
}

/// The running state of one show.
pub(crate) struct Core {
    nodes: Vec<CompiledNode>,
    instances: Vec<Option<Box<dyn Node>>>,
    triggers: BTreeMap<EventKind, Vec<usize>>,
    show: omuzaki_live_api::ids::ShowId,
    rngs: Vec<Rng>,
    latched: Vec<BTreeMap<PortName, Val>>,
    tokens: BTreeMap<u64, Token>,
    next_token: u64,
    /// Pending wakes by (due time's bits, order asked): non-negative finite `f64`s order like their bits.
    wakes: BTreeMap<(u64, u64), Wake>,
    next_wake: u64,
    calls: BTreeMap<CallId, (usize, u64)>,
    next_call: u64,
    vars: BTreeMap<VarName, Variable>,
    /// Variables changed in this step: the first old value.
    changed: BTreeMap<VarName, Value>,
    /// Globals whose saved value has another type: another show declared them first, so this run leaves them alone.
    conflicts: BTreeSet<VarName>,
    groups: BTreeMap<GroupId, bool>,
    switches: Vec<(GroupId, bool)>,
    graph_runs: BTreeMap<GraphId, GraphRun>,
    buckets: BTreeMap<GraphId, Bucket>,
    /// Calls of a graph switched off: their replies are dropped and counted.
    ignored: BTreeSet<CallId>,
    /// The values files of the persisted scopes (design D10).
    files: Files,
    paused: bool,
    step_at: Seconds,
    utc: UtcMillis,
    outgoing: Vec<OpRequest>,
    /// The engine's own events, for the next steps (design D9: they never go through `publish`).
    own: VecDeque<LiveEvent>,
    stopping: bool,
    /// Per graph: node errors, faulted nodes, a suspension.
    errors: BTreeMap<GraphId, u64>,
    faulted: BTreeMap<GraphId, Vec<NodeId>>,
    suspended: BTreeMap<GraphId, Suspension>,
    dropped: Dropped,
    firings: VecDeque<Firing>,
    events: VecDeque<EventRow>,
    base_status: Vec<GraphStatus>,
}

/// `text` in English: its declared English with the placeables filled in, which is what a node's `error` output
/// carries (design D4: "`error` holds the English text").
pub(crate) fn english(text: &Text) -> String {
    let declared =
        inventory::iter::<MessageTable>.into_iter().flat_map(|table| table.messages).find(|decl| decl.id == text.id);
    match declared {
        Some(decl) => text.args.iter().fold(decl.english.to_owned(), |english, (name, arg)| {
            english.replace(&format!("{{ ${name} }}"), &arg.to_string())
        }),
        None => text.to_string(),
    }
}

/// A JSON value as the value of its own type: what a `set` constant is before it is coerced to its port.
fn natural(value: &Value) -> Val {
    let ty = match value {
        Value::Bool(_) => PortType::Bool,
        Value::Number(_) => PortType::Number,
        Value::String(_) => PortType::Text,
        Value::Array(_) => PortType::List,
        Value::Null | Value::Object(_) => PortType::Json,
    };
    Val::from_json(value, ty).unwrap_or_else(|_| Val::from(value.clone()))
}

/// `value` checked and coerced to `ty`, with the port named in the error.
fn coerce(port: &str, value: &Val, ty: PortType) -> Result<Val, NodeError> {
    value.check().map_err(|e| NodeError::invalid_value(port, &e))?;
    value.coerce(ty).map_err(|e| match e {
        ValError::Refused { .. } | ValError::Mismatch { .. } => NodeError::type_mismatch(port, ty, value.ty()),
        other => NodeError::invalid_value(port, &other),
    })
}

impl Core {
    /// The running state of `compiled`, with every node's random stream derived from `seed` (design D6).
    pub(crate) fn new(compiled: Compiled, seed: u32, at: Seconds, utc: UtcMillis) -> Self {
        let Compiled { show, def, nodes, instances, triggers, .. } = compiled;
        let base_status = Compiled::graph_status_of(&def, &nodes);
        let rngs = nodes.iter().map(|n| node_rng(seed, n.graph.as_str(), n.id.as_str())).collect();
        let latched = nodes.iter().map(|_| BTreeMap::new()).collect();
        let vars = def
            .variables
            .iter()
            .map(|(name, def)| {
                let value = Val::from_json(&def.initial(), def.ty.port_type())
                    .ok()
                    .or_else(|| def.ty.port_type().default_value())
                    .unwrap_or(Val::Bool(false));
                (name.clone(), Variable { def: def.clone(), value, changed: None })
            })
            .collect();
        let groups = def.groups.iter().map(|(id, g)| (id.clone(), g.enabled)).collect();
        let graph_runs = def
            .graphs
            .iter()
            .map(|(id, g)| (id.clone(), GraphRun { enabled: g.enabled, group: g.group.clone() }))
            .collect();
        Self {
            nodes,
            instances,
            triggers,
            show,
            rngs,
            latched,
            tokens: BTreeMap::new(),
            next_token: 0,
            wakes: BTreeMap::new(),
            next_wake: 0,
            calls: BTreeMap::new(),
            next_call: 0,
            vars,
            changed: BTreeMap::new(),
            conflicts: BTreeSet::new(),
            groups,
            switches: Vec::new(),
            graph_runs,
            buckets: BTreeMap::new(),
            ignored: BTreeSet::new(),
            files: Files::default(),
            paused: false,
            step_at: at,
            utc,
            outgoing: Vec::new(),
            own: VecDeque::new(),
            stopping: false,
            errors: BTreeMap::new(),
            faulted: BTreeMap::new(),
            suspended: BTreeMap::new(),
            dropped: Dropped::default(),
            firings: VecDeque::new(),
            events: VecDeque::new(),
            base_status,
        }
    }

    // ── Items ──────────────────────────────────────────────────────────────────────────────────────────────────

    /// Sends `Start` to every node of an active graph (each in a token of its own) and queues `show.started`.
    pub(crate) fn start(&mut self, at: Seconds, utc: UtcMillis) {
        self.begin(at, utc);
        let active: BTreeSet<GraphId> = self.graph_runs.keys().filter(|g| self.graph_active(g)).cloned().collect();
        let stack = self.starts(&active);
        self.run(stack);
        let started = LiveEvent::of(&ShowLifecycle::new(self.show.clone()), Source::Engine, self.step_at);
        self.own.extend(started.ok());
        self.close();
    }

    /// Sends `Stop` to every node; what they fire now is ignored. Calls in flight and wakes are dropped.
    pub(crate) fn stop(&mut self, at: Seconds, utc: UtcMillis) {
        self.begin(at, utc);
        self.stopping = true;
        let mut stack = Vec::new();
        for node in (0..self.nodes.len()).rev() {
            if self.instances[node].is_some() {
                let token = self.tick_token(node);
                stack.push(Activation { node, act: Act::Stop, token });
            }
        }
        self.run(stack);
        self.wakes.clear();
        self.calls.clear();
        self.outgoing.clear();
        self.own.clear();
        self.tokens.clear();
    }

    /// Runs one item as a step at `at`.
    pub(crate) fn step(&mut self, at: Seconds, utc: UtcMillis, item: Item) {
        self.begin(at, utc);
        let mut stack = Vec::new();
        match item {
            Item::Event(event) => {
                self.record_event(&event);
                let root = Arc::new(event);
                let listening = self.triggers.get(&root.kind).cloned().unwrap_or_default();
                if self.paused {
                    let would = listening.iter().filter(|&&n| self.runs(n)).count();
                    self.dropped.paused += u64::try_from(would).unwrap_or(u64::MAX);
                    self.close();
                    return;
                }
                // Graph order, so a bucket that runs dry refuses the later listeners of the same event.
                for &node in &listening {
                    if !self.runs(node) {
                        self.dropped.inactive += 1;
                        continue;
                    }
                    let graph = self.nodes[node].graph.clone();
                    if !self.take_token(&graph) {
                        self.dropped.rate += 1;
                        continue;
                    }
                    let token = self.new_token(Arc::clone(&root));
                    stack.push(Activation { node, act: Act::Trigger, token });
                }
                stack.reverse();
            }
            Item::Reply { call, outcome } => {
                if self.ignored.remove(&call) {
                    self.dropped.inactive += 1;
                } else if self.paused {
                    if let Some((_, token)) = self.calls.remove(&call) {
                        self.release(token);
                        self.dropped.paused += 1;
                    }
                } else if let Some((node, token)) = self.calls.remove(&call) {
                    self.release(token);
                    stack.push(Activation { node, act: Act::Reply(call, outcome), token });
                }
            }
            Item::Control(control) => self.control(control),
        }
        self.run(stack);
        self.close();
    }

    /// The time of the earliest pending wake.
    pub(crate) fn next_wake(&self) -> Option<Seconds> {
        self.wakes.keys().next().map(|(bits, _)| Seconds(f64::from_bits(*bits)))
    }

    /// Runs the earliest wake if it is due at `at`, as its own step; `false` when none is due.
    pub(crate) fn wake(&mut self, at: Seconds, utc: UtcMillis) -> bool {
        let Some((&key, &wake)) = self.wakes.iter().next() else { return false };
        let due = Seconds(f64::from_bits(key.0));
        if due.0 > at.0 {
            return false;
        }
        self.wakes.remove(&key);
        self.begin(due, utc);
        self.release(wake.token);
        if self.paused {
            self.dropped.paused += 1;
            self.close();
            return true;
        }
        self.run(vec![Activation { node: wake.node, act: Act::Wake(wake.id), token: wake.token }]);
        self.close();
        true
    }

    /// Takes the stored values of `scope` before the start: each declared variable of that scope with a stored value
    /// of its type starts from it. Returns the names whose stored value had another type (they start from their
    /// defaults, and the file keeps the old value until they change).
    pub(crate) fn load_values(&mut self, scope: VarScope, values: BTreeMap<VarName, Value>) -> Vec<VarName> {
        let mut refused = Vec::new();
        for (name, var) in &mut self.vars {
            if var.def.scope != scope {
                continue;
            }
            let Some(stored) = values.get(name) else { continue };
            let ty = var.def.ty.port_type();
            match Val::from_json(stored, ty) {
                Ok(value) if value.ty() == ty && value.check().is_ok() => var.value = value,
                _ => refused.push(name.clone()),
            }
        }
        if scope == VarScope::Global {
            // A global is one value shared by every show; the saved one settles its type, so the nodes here that
            // expect another are inert and the value is never overwritten (design D10).
            for name in &refused {
                self.conflicts.insert(name.clone());
                for node in &mut self.nodes {
                    if node.inert.is_some() || !node.vars.contains(name) {
                        continue;
                    }
                    node.inert = Some(InertReason::Undeclared);
                    if let Some(status) = self.base_status.iter_mut().find(|s| s.graph == node.graph) {
                        let mut inert = status.inert.clone();
                        inert.push(InertNode::new(node.id.clone(), InertReason::Undeclared));
                        *status = status.clone().with_inert(inert);
                    }
                }
            }
        }
        self.files.loaded(scope, values);
        refused
    }

    /// Whether the panic button is on.
    pub(crate) fn paused(&self) -> bool {
        self.paused
    }

    /// When the next values file is due to be written.
    pub(crate) fn next_save(&self) -> Option<Seconds> {
        self.files.next_due()
    }

    /// The values files due at `now` (with `flush`, every one the run changed), each with its whole contents.
    pub(crate) fn take_saves(&mut self, now: Seconds, flush: bool) -> Vec<(VarScope, BTreeMap<VarName, Value>)> {
        let (vars, conflicts) = (&self.vars, &self.conflicts);
        self.files.take(now, flush, |scope| {
            vars.iter()
                .filter(|(name, v)| v.def.scope == scope && !conflicts.contains(*name))
                .map(|(name, v)| (name.clone(), v.value.to_json()))
                .collect()
        })
    }

    /// The engine's own next event (`show.started`, `var.changed`, `event.emit`), if any.
    pub(crate) fn pop_own(&mut self) -> Option<LiveEvent> {
        self.own.pop_front()
    }

    /// The op calls the last steps asked for, in order.
    pub(crate) fn take_outgoing(&mut self) -> Vec<OpRequest> {
        std::mem::take(&mut self.outgoing)
    }

    // ── Steps ──────────────────────────────────────────────────────────────────────────────────────────────────

    fn begin(&mut self, at: Seconds, utc: UtcMillis) {
        // A node never sees time go backwards (design D5).
        if at.0 > self.step_at.0 {
            self.step_at = at;
        }
        self.utc = utc;
    }

    /// Whether node `i` may run now: compiled, not inert or faulted, and its graph active.
    fn runs(&self, i: usize) -> bool {
        self.healthy(i) && self.graph_active(&self.nodes[i].graph)
    }

    /// Whether node `i` can run at all: compiled, not inert and not faulted.
    fn healthy(&self, i: usize) -> bool {
        self.nodes[i].inert.is_none() && self.instances[i].is_some()
    }

    /// Whether `graph` runs now (design D12): enabled, its group (if any) on, and not suspended.
    fn graph_active(&self, graph: &GraphId) -> bool {
        let Some(run) = self.graph_runs.get(graph) else { return false };
        run.enabled
            && run.group.as_ref().is_none_or(|g| self.groups.get(g).copied().unwrap_or(true))
            && !self.suspended.contains_key(graph)
    }

    /// One new token from `graph`'s bucket, if it has one (design D8: 500 a second, a burst of 2,000, on the engine
    /// clock).
    fn take_token(&mut self, graph: &GraphId) -> bool {
        let burst = f64::from(TOKEN_BURST_PER_GRAPH);
        let now = self.step_at;
        let bucket = self.buckets.entry(graph.clone()).or_insert(Bucket { level: burst, at: now });
        let refill = (now.0 - bucket.at.0).max(0.0) * f64::from(TOKENS_PER_SECOND_PER_GRAPH);
        bucket.level = (bucket.level + refill).min(burst);
        bucket.at = now;
        if bucket.level >= 1.0 {
            bucket.level -= 1.0;
            true
        } else {
            false
        }
    }

    /// `Start` for every node of `graphs`, in node order.
    fn starts(&mut self, graphs: &BTreeSet<GraphId>) -> Vec<Activation> {
        let mut stack = Vec::new();
        for node in (0..self.nodes.len()).rev() {
            if self.instances[node].is_some() && graphs.contains(&self.nodes[node].graph) {
                let token = self.tick_token(node);
                stack.push(Activation { node, act: Act::Start, token });
            }
        }
        stack
    }

    /// Switching `graph` off (design D12): its pending wakes are dropped and its replies ignored, both counted.
    fn deactivate(&mut self, graph: &GraphId) {
        let wakes: Vec<(u64, u64)> =
            self.wakes.iter().filter(|(_, w)| &self.nodes[w.node].graph == graph).map(|(k, _)| *k).collect();
        for key in wakes {
            if let Some(wake) = self.wakes.remove(&key) {
                self.release(wake.token);
                self.dropped.inactive += 1;
            }
        }
        let calls: Vec<CallId> =
            self.calls.iter().filter(|(_, (n, _))| &self.nodes[*n].graph == graph).map(|(c, _)| *c).collect();
        for call in calls {
            if let Some((_, token)) = self.calls.remove(&call) {
                self.release(token);
                self.ignored.insert(call);
            }
        }
    }

    /// A change from outside the show.
    fn control(&mut self, control: Control) {
        match control {
            Control::SwitchGroup { group, on } => {
                if self.groups.contains_key(&group) {
                    self.switches.push((group, on));
                }
            }
            Control::SetVariable { name, value } => {
                let at = self.step_at;
                if let Some(var) = self.vars.get_mut(&name)
                    && value.ty() == var.def.ty.port_type()
                    && !self.conflicts.contains(&name)
                {
                    let old = std::mem::replace(&mut var.value, value).to_json();
                    var.changed = Some((at, None));
                    self.files.changed(var.def.scope, at);
                    self.changed.entry(name).or_insert(old);
                }
            }
            Control::Pause => self.paused = true,
            Control::Resume => self.paused = false,
            Control::ResumeGraph { graph } => {
                let was = self.graph_active(&graph);
                if self.suspended.remove(&graph).is_some() && !was && self.graph_active(&graph) {
                    let stack = self.starts(&BTreeSet::from([graph]));
                    self.run(stack);
                }
            }
        }
    }

    fn new_token(&mut self, root: Arc<LiveEvent>) -> u64 {
        self.next_token += 1;
        self.tokens.insert(self.next_token, Token { root, values: BTreeMap::new(), holds: 0 });
        self.next_token
    }

    /// A token rooted at a synthetic `engine.tick` naming the node (design D5).
    fn tick_token(&mut self, node: usize) -> u64 {
        let (graph, id) = (self.nodes[node].graph.clone(), self.nodes[node].id.clone());
        let root = LiveEvent::of(&EngineTick::new(graph, id), Source::Engine, self.step_at).unwrap_or_else(|_| {
            LiveEvent::new(EventKind::from_static("engine.tick"), Source::Engine, self.step_at, Value::Null)
        });
        self.new_token(Arc::new(root))
    }

    fn hold(&mut self, token: u64) {
        if let Some(t) = self.tokens.get_mut(&token) {
            t.holds += 1;
        }
    }

    fn release(&mut self, token: u64) {
        if let Some(t) = self.tokens.get_mut(&token) {
            t.holds = t.holds.saturating_sub(1);
        }
    }

    /// Runs `stack` depth-first under the loop guard (design D5, D8).
    fn run(&mut self, mut stack: Vec<Activation>) {
        let mut activations = 0usize;
        let mut recent: VecDeque<usize> = VecDeque::new();
        while let Some(Activation { node, act, token }) = stack.pop() {
            activations += 1;
            recent.push_back(node);
            if recent.len() > LOOP_GUARD_LAST_NODES {
                recent.pop_front();
            }
            if activations > MAX_ACTIVATIONS_PER_STEP || stack.len() > MAX_STACK_DEPTH {
                self.trip_loop_guard(&recent);
                return;
            }
            let lifecycle = matches!(act, Act::Start | Act::Stop);
            if !lifecycle && !self.runs(node) {
                if self.healthy(node) {
                    self.dropped.inactive += 1;
                } else {
                    self.dropped.inert += 1;
                }
                continue;
            }
            let Some(mut instance) = self.instances[node].take() else { continue };
            let root = self.tokens.get(&token).map(|t| Arc::clone(&t.root));
            let mut host = Host { core: self, node, token, fired: Vec::new() };
            let result = catch_unwind(AssertUnwindSafe(|| {
                let mut cx = NodeCx::new(&mut host);
                match (act, &root) {
                    (Act::Start, _) => instance.handle(Input::Start, &mut cx),
                    (Act::Stop, _) => instance.handle(Input::Stop, &mut cx),
                    (Act::Event(port), _) => instance.handle(Input::event(port.as_str()), &mut cx),
                    (Act::Trigger, Some(event)) => instance.handle(Input::trigger(event), &mut cx),
                    (Act::Trigger, None) => Ok(()),
                    (Act::Wake(id), _) => instance.handle(Input::wake(id), &mut cx),
                    (Act::Reply(call, outcome), _) => instance.handle(Input::reply(call, outcome), &mut cx),
                }
            }));
            let fired = std::mem::take(&mut host.fired);
            let graph = self.nodes[node].graph.clone();
            match result {
                Ok(Ok(())) => self.instances[node] = Some(instance),
                Ok(Err(_)) => {
                    // The branch is dropped; the node keeps running (design D5).
                    self.instances[node] = Some(instance);
                    *self.errors.entry(graph).or_default() += 1;
                    continue;
                }
                Err(_) => {
                    self.faulted.entry(graph).or_default().push(self.nodes[node].id.clone());
                    continue;
                }
            }
            if self.stopping {
                continue;
            }
            let mut next = Vec::new();
            for port in fired {
                self.record_firing(node, token, &port);
                if let Some(dests) = self.nodes[node].outs.get(&port) {
                    next.extend(dests.iter().map(|(dest, input)| (*dest, input.clone())));
                }
            }
            for (dest, input) in next.into_iter().rev() {
                stack.push(Activation { node: dest, act: Act::Event(input), token });
            }
        }
    }

    /// Suspends the graph that ran most among the last nodes on the stack, naming them so the loop is visible.
    fn trip_loop_guard(&mut self, recent: &VecDeque<usize>) {
        let mut counts: BTreeMap<&GraphId, usize> = BTreeMap::new();
        for &node in recent {
            *counts.entry(&self.nodes[node].graph).or_default() += 1;
        }
        let Some(graph) = counts.into_iter().max_by_key(|(_, n)| *n).map(|(g, _)| g.clone()) else { return };
        let last = recent.iter().map(|&n| self.nodes[n].id.clone()).collect();
        self.suspended.insert(graph, Suspension::new(SuspendReason::loop_guard(last), self.step_at));
    }

    /// Closes a step: group switches, coalesced variable changes, tokens nothing holds any more.
    fn close(&mut self) {
        for _ in 0..SWITCH_ROUNDS {
            if self.switches.is_empty() {
                break;
            }
            self.apply_switches();
        }
        self.switches.clear();
        for (name, old) in std::mem::take(&mut self.changed) {
            let Some(var) = self.vars.get(&name) else { continue };
            let new = var.value.to_json();
            if new == old {
                continue;
            }
            let payload = VarChanged::new(name, var.def.scope, old, new);
            self.own.extend(LiveEvent::of(&payload, Source::Engine, self.step_at).ok());
        }
        self.tokens.retain(|_, t| t.holds > 0);
    }

    /// Applies the queued group switches (design D12): graphs that went off are deactivated, graphs that came on get
    /// `Start`.
    fn apply_switches(&mut self) {
        let before: BTreeSet<GraphId> = self.graph_runs.keys().filter(|g| self.graph_active(g)).cloned().collect();
        for (group, on) in std::mem::take(&mut self.switches) {
            self.groups.insert(group, on);
        }
        let after: BTreeSet<GraphId> = self.graph_runs.keys().filter(|g| self.graph_active(g)).cloned().collect();
        for graph in before.difference(&after) {
            self.deactivate(graph);
        }
        let on: BTreeSet<GraphId> = after.difference(&before).cloned().collect();
        if !on.is_empty() && !self.stopping {
            let stack = self.starts(&on);
            self.run(stack);
        }
    }

    // ── Values (design D3) ─────────────────────────────────────────────────────────────────────────────────────

    /// The value input `port` of `node`, resolved in `token` and coerced to the port's type.
    fn input(&mut self, node: usize, port: &str, token: u64) -> Result<Val, NodeError> {
        let decl = match self.nodes[node].ports.input(port) {
            Some(decl) if !decl.ty.is_event() => decl,
            _ => return Err(NodeError::unknown_port(port)),
        };
        let (ty, default) = (decl.ty, decl.default.clone().or_else(|| decl.ty.default_value()));
        let wire = self.nodes[node].value_wires.get(port).cloned();
        let raw = match wire {
            Some((src, out)) => self.upstream(src, &out, token)?.or(default),
            None => self.nodes[node].set.get(port).map(natural).or(default),
        };
        let raw = raw.ok_or_else(|| NodeError::unknown_port(port))?;
        coerce(port, &raw, ty)
    }

    /// The value of `src`'s output `out` as seen from `token`: the token's value, else a live output, else the
    /// source's last value.
    fn upstream(&mut self, src: usize, out: &PortName, token: u64) -> Result<Option<Val>, NodeError> {
        if let Some(value) = self.tokens.get(&token).and_then(|t| t.values.get(&(src, out.clone()))) {
            return Ok(Some(value.clone()));
        }
        let live = self.nodes[src].ports.output(out.as_str()).is_some_and(|d| d.live);
        if live && self.runs(src) {
            // Taken while it evaluates, so a value cycle (refused when wiring) falls back to the last value here.
            if let Some(instance) = self.instances[src].take() {
                let mut host = Host { core: self, node: src, token, fired: Vec::new() };
                let result = instance.eval(out.as_str(), &mut EvalCx::new(&mut host));
                self.instances[src] = Some(instance);
                return result.map(Some);
            }
        }
        Ok(self.latched[src].get(out).cloned())
    }

    // ── The monitor ────────────────────────────────────────────────────────────────────────────────────────────

    fn record_firing(&mut self, node: usize, token: u64, port: &PortName) {
        let reference = NodeRef::new(self.nodes[node].graph.clone(), self.nodes[node].id.clone());
        let mut firing = Firing::new(self.step_at, reference, port.clone());
        if let Some(t) = self.tokens.get(&token) {
            for ((_, out), value) in t.values.iter().filter(|((n, _), _)| *n == node) {
                firing = firing.with_value(out.clone(), &value.to_json());
            }
        }
        self.firings.push_back(firing);
        if self.firings.len() > MONITOR_FIRINGS {
            self.firings.pop_front();
        }
    }

    fn record_event(&mut self, event: &LiveEvent) {
        let row = EventRow::new(event.id, event.kind.clone(), event.source.clone(), event.at, summary(event));
        self.events.push_back(row);
        if self.events.len() > MONITOR_EVENTS {
            self.events.pop_front();
        }
    }

    /// Each graph's state now.
    pub(crate) fn graphs(&self) -> Vec<GraphStatus> {
        self.base_status
            .iter()
            .map(|base| {
                let graph = &base.graph;
                let mut status = base.clone().with_errors(self.errors.get(graph).copied().unwrap_or(0));
                if let Some(faulted) = self.faulted.get(graph) {
                    status = status.with_faulted(faulted.clone());
                    let mut inert = status.inert.clone();
                    inert.extend(faulted.iter().map(|n| InertNode::new(n.clone(), InertReason::Faulted)));
                    status = status.with_inert(inert);
                }
                if let Some(suspension) = self.suspended.get(graph) {
                    status = status.with_suspension(suspension.clone());
                }
                status.with_active(self.graph_active(graph))
            })
            .collect()
    }

    /// What was dropped so far.
    pub(crate) fn dropped(&self) -> Dropped {
        self.dropped.clone()
    }

    /// The values monitor's snapshot (design D10).
    pub(crate) fn values(&self) -> ValuesSnapshot {
        let mut snapshot = ValuesSnapshot::new(self.step_at);
        snapshot.variables = self
            .vars
            .iter()
            .map(|(name, var)| {
                let row = VarRow::new(name.clone(), var.def.scope, var.def.ty, var.value.to_json());
                match &var.changed {
                    Some((at, by)) => row.with_change(*at, by.clone()),
                    None => row,
                }
            })
            .collect();
        snapshot.groups = self.groups.iter().map(|(g, on)| GroupRow::new(g.clone(), *on)).collect();
        snapshot.graphs = self.graphs();
        snapshot.firings = self.firings.iter().cloned().collect();
        snapshot.events = self.events.iter().cloned().collect();
        snapshot.dropped = self.dropped();
        snapshot
    }
}

impl Compiled {
    /// Each graph's compiled state, before anything ran.
    fn graph_status_of(def: &omuzaki_live_api::show::ShowDef, nodes: &[CompiledNode]) -> Vec<GraphStatus> {
        let mut graphs: BTreeMap<&GraphId, GraphStatus> = BTreeMap::new();
        for (id, graph) in &def.graphs {
            let active = nodes.iter().find(|n| &n.graph == id).map_or(graph.enabled, |n| n.active);
            graphs.insert(id, GraphStatus::new(id.clone()).with_active(active));
        }
        for node in nodes {
            if let (Some(why), Some(status)) = (&node.inert, graphs.get_mut(&node.graph)) {
                let mut inert = status.inert.clone();
                inert.push(InertNode::new(node.id.clone(), why.clone()));
                *status = status.clone().with_inert(inert);
            }
        }
        graphs.into_values().collect()
    }
}

/// One activation's view of the engine: the node, its token and the pulses it fires.
struct Host<'a> {
    core: &'a mut Core,
    node: usize,
    token: u64,
    fired: Vec<PortName>,
}

impl Host<'_> {
    fn compiled(&self) -> &CompiledNode {
        &self.core.nodes[self.node]
    }

    fn reference(&self) -> NodeRef {
        NodeRef::new(self.compiled().graph.clone(), self.compiled().id.clone())
    }
}

impl EvalHost for Host<'_> {
    fn now(&self) -> Seconds {
        self.core.step_at
    }

    fn utc(&self) -> UtcMillis {
        self.core.utc
    }

    fn input(&mut self, port: &str) -> Result<Val, NodeError> {
        self.core.input(self.node, port, self.token)
    }

    fn var(&self, name: &VarName) -> Option<Val> {
        self.core.vars.get(name).map(|v| v.value.clone())
    }

    fn group_enabled(&self, group: &GroupId) -> Option<bool> {
        self.core.groups.get(group).copied()
    }
}

impl NodeHost for Host<'_> {
    fn rng(&mut self) -> &mut Rng {
        &mut self.core.rngs[self.node]
    }

    fn set(&mut self, port: &str, value: Val) -> Result<(), NodeError> {
        let (name, ty) = match self.compiled().ports.output(port) {
            Some(decl) if !decl.ty.is_event() && !decl.live => (decl.name.clone(), decl.ty),
            _ => return Err(NodeError::unknown_port(port)),
        };
        let value = coerce(port, &value, ty)?;
        let (node, token) = (self.node, self.token);
        if let Some(t) = self.core.tokens.get_mut(&token) {
            t.values.insert((node, name.clone()), value.clone());
        }
        self.core.latched[node].insert(name, value);
        Ok(())
    }

    fn fire(&mut self, port: &str) -> Result<(), NodeError> {
        match self.compiled().ports.output(port) {
            Some(decl) if decl.ty.is_event() => {
                let name = decl.name.clone();
                self.fired.push(name);
                Ok(())
            }
            _ => Err(NodeError::unknown_port(port)),
        }
    }

    fn fail(&mut self, port: &str, why: Text) -> Result<(), NodeError> {
        if self.compiled().ports.output("error").is_some_and(|d| d.ty == PortType::Text && !d.live) {
            self.set("error", Val::text(english(&why)))?;
        }
        self.fire(port)
    }

    fn wake_at(&mut self, at: Seconds) -> Result<WakeId, NodeError> {
        let node = self.node;
        if self.core.wakes.values().filter(|w| w.node == node).count() >= MAX_WAKES_PER_NODE {
            return Err(NodeError::limit("MAX_WAKES_PER_NODE", MAX_WAKES_PER_NODE));
        }
        let due = if at.0 > self.core.step_at.0 { at.0 } else { self.core.step_at.0 };
        self.core.next_wake += 1;
        let id = WakeId(self.core.next_wake);
        self.core.wakes.insert((due.max(0.0).to_bits(), self.core.next_wake), Wake { node, id, token: self.token });
        self.core.hold(self.token);
        Ok(id)
    }

    fn cancel_wake(&mut self, id: WakeId) {
        let node = self.node;
        let key = self.core.wakes.iter().find(|(_, w)| w.node == node && w.id == id).map(|(k, w)| (*k, w.token));
        if let Some((key, token)) = key {
            self.core.wakes.remove(&key);
            self.core.release(token);
        }
    }

    fn run_op(&mut self, op: &str, args: Value) -> Result<CallId, NodeError> {
        if self.core.calls.len() >= MAX_CALLS_IN_FLIGHT {
            return Err(NodeError::limit("MAX_CALLS_IN_FLIGHT", MAX_CALLS_IN_FLIGHT));
        }
        self.core.next_call += 1;
        let call = CallId(self.core.next_call);
        let (acting, cause) = match self.core.tokens.get(&self.token) {
            Some(t) => {
                let node = self.compiled();
                let cause =
                    format!("{}/{}/{} on {} #{}", self.core.show, node.graph, node.id, t.root.kind, t.root.id.0);
                (t.root.acting.clone(), cause)
            }
            None => (None, format!("{}", self.core.show)),
        };
        self.core.calls.insert(call, (self.node, self.token));
        self.core.hold(self.token);
        self.core.outgoing.push(OpRequest { call, op: op.to_owned(), args, acting, cause });
        Ok(call)
    }

    fn set_var(&mut self, name: &VarName, value: Val) -> Result<(), NodeError> {
        let at = self.core.step_at;
        let by = self.reference();
        let Some(var) = self.core.vars.get_mut(name) else {
            return Err(NodeError::undeclared(name.as_str()));
        };
        let ty = var.def.ty.port_type();
        value.check().map_err(|e| NodeError::invalid_value(name.as_str(), &e))?;
        if value.ty() != ty {
            return Err(NodeError::type_mismatch(name.as_str(), ty, value.ty()));
        }
        let old = std::mem::replace(&mut var.value, value).to_json();
        var.changed = Some((at, Some(by)));
        let scope = var.def.scope;
        self.core.files.changed(scope, at);
        self.core.changed.entry(name.clone()).or_insert(old);
        Ok(())
    }

    fn switch_group(&mut self, group: &GroupId, on: bool) -> Result<(), NodeError> {
        if !self.core.groups.contains_key(group) {
            return Err(NodeError::undeclared(group.as_str()));
        }
        self.core.switches.push((group.clone(), on));
        Ok(())
    }

    fn emit(&mut self, kind: EventKind, payload: Value) -> Result<(), NodeError> {
        let Some(root) = self.core.tokens.get(&self.token).map(|t| Arc::clone(&t.root)) else {
            return Err(NodeError::internal("a node ran outside any token"));
        };
        let hops = root.hops.saturating_add(1);
        if hops > MAX_EMIT_HOPS {
            let graph = self.compiled().graph.clone();
            self.core.suspended.insert(graph, Suspension::new(SuspendReason::EmitHops, self.core.step_at));
            return Err(NodeError::limit("MAX_EMIT_HOPS", usize::from(MAX_EMIT_HOPS)));
        }
        check_payload(&kind, &payload).map_err(|e| NodeError::invalid_payload(kind.as_str(), e.to_string()))?;
        let source = Source::graph(self.compiled().graph.clone(), self.compiled().id.clone());
        let mut event = LiveEvent::new(kind, source, self.core.step_at, payload).with_hops(hops);
        if let Some(actor) = root.acting.clone() {
            event = event.with_acting(actor);
        }
        self.core.own.push_back(event);
        Ok(())
    }

    fn event(&self) -> &LiveEvent {
        match self.core.tokens.get(&self.token) {
            Some(t) => &t.root,
            // Every activation runs in a token the step made, so this is unreachable; a static tick keeps it total.
            None => fallback_event(),
        }
    }
}

/// One line for the values monitor: who (the user, the raider, the gifter) and what (the text, the reward, the bits,
/// the viewers), cut to the monitor's width. Only the fields the normalised payloads share; other kinds show their kind.
pub(crate) fn summary(event: &LiveEvent) -> String {
    let p = &event.payload;
    let text = |pointer: &str| p.pointer(pointer).and_then(Value::as_str).filter(|s| !s.is_empty());
    let number = |key: &str| p.get(key).and_then(Value::as_u64);
    let who = text("/user/name").or_else(|| text("/from/name")).or_else(|| text("/gifter/name"));
    let what = text("/text")
        .map(str::to_owned)
        .or_else(|| text("/reward/title").map(str::to_owned))
        .or_else(|| number("bits").map(|b| format!("{b} bits")))
        .or_else(|| number("viewers").map(|v| format!("{v} viewers")))
        .or_else(|| text("/message").map(str::to_owned));
    let line = match (who, what) {
        (Some(who), Some(what)) => format!("{who}: {what}"),
        (Some(who), None) => who.to_owned(),
        (None, Some(what)) => what,
        (None, None) => event.kind.to_string(),
    };
    // One line: a newline in chat text would break the monitor's rows.
    shown(&Value::String(line.replace(['\n', '\r'], " ")))
}

/// A placeholder root for the unreachable case of an activation without a token.
fn fallback_event() -> &'static LiveEvent {
    static FALLBACK: std::sync::OnceLock<LiveEvent> = std::sync::OnceLock::new();
    FALLBACK.get_or_init(|| {
        LiveEvent::new(EventKind::from_static("engine.tick"), Source::Engine, Seconds(0.0), Value::Null)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_keys_order_like_their_times() {
        let times = [0.0, 0.25, 1.0, 1.5, 60.0, 3600.5];
        let bits: Vec<u64> = times.iter().map(|t: &f64| t.to_bits()).collect();
        assert!(bits.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn english_fills_the_placeables() {
        let text = omuzaki_live_api::l10n::unknown_port("scene");
        assert_eq!(english(&text), "This node has no port scene.");
    }
}
