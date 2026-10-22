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

use std::collections::{BTreeMap, VecDeque};
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
    MAX_WAKES_PER_NODE, MONITOR_EVENTS, MONITOR_FIRINGS,
};
use omuzaki_live_api::node::{CallId, EvalCx, EvalHost, Input, Node, NodeCx, NodeError, NodeHost, WakeId, node_rng};
use omuzaki_live_api::services::{
    Dropped, EventRow, Firing, GraphStatus, GroupRow, InertNode, InertReason, NodeRef, SuspendReason, Suspension,
    ValuesSnapshot, VarRow,
};
use omuzaki_live_api::show::VariableDef;
use omuzaki_live_api::value::{PortType, Val, ValError};
use omuzaki_ops_api::{Actor, OpError};
use serde_json::Value;

use crate::compile::{Compiled, CompiledNode};

/// What arrives at the engine from outside a step.
#[derive(Debug)]
pub(crate) enum Item {
    /// An event, from a producer, `automation.fire` or the engine itself.
    Event(LiveEvent),
    /// An op call came back.
    Reply { call: CallId, outcome: Result<Option<Value>, OpError> },
}

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
    groups: BTreeMap<GroupId, bool>,
    switches: Vec<(GroupId, bool)>,
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
            groups,
            switches: Vec::new(),
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

    /// Sends `Start` to every node (each in a token of its own) and queues `show.started`.
    pub(crate) fn start(&mut self, at: Seconds, utc: UtcMillis) {
        self.begin(at, utc);
        let mut stack = Vec::new();
        for node in (0..self.nodes.len()).rev() {
            if self.instances[node].is_some() {
                let token = self.tick_token(node);
                stack.push(Activation { node, act: Act::Start, token });
            }
        }
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
                for node in listening.into_iter().rev() {
                    if !self.runs(node) {
                        self.dropped.inactive += 1;
                        continue;
                    }
                    let token = self.new_token(Arc::clone(&root));
                    stack.push(Activation { node, act: Act::Trigger, token });
                }
            }
            Item::Reply { call, outcome } => {
                if let Some((node, token)) = self.calls.remove(&call) {
                    self.release(token);
                    stack.push(Activation { node, act: Act::Reply(call, outcome), token });
                }
            }
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
        self.run(vec![Activation { node: wake.node, act: Act::Wake(wake.id), token: wake.token }]);
        self.close();
        true
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

    /// Whether node `i` may run now: compiled, not inert or faulted, its graph active and not suspended.
    fn runs(&self, i: usize) -> bool {
        let node = &self.nodes[i];
        node.inert.is_none() && node.active && !self.suspended.contains_key(&node.graph) && self.instances[i].is_some()
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
                self.dropped.inert += 1;
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
        for (group, on) in std::mem::take(&mut self.switches) {
            self.groups.insert(group, on);
        }
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
        let row = EventRow::new(event.id, event.kind.clone(), event.source.clone(), event.at, event.kind.to_string());
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
                    status = status.with_suspension(suspension.clone()).with_active(false);
                }
                status
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
