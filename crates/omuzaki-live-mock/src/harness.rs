//! [`NodeHarness`]: one node instance, scripted inputs at fake times, a [`FakeRunner`], a fake clock and wall, in-memory
//! variables and groups (design D16). A node task's tests are harness tests.
//!
//! The harness orders things exactly as the engine does (design D3, D5, D6):
//! - each action is a **step**, whose time is the later of the item's time and the previous step's, so a node never
//!   sees time go backwards;
//! - [`advance`](NodeHarness::advance) runs due wakes (by due time, then the order they were asked for) and scripted
//!   replies (by arrival time, then request order), and a wake due at `t` runs before a reply arriving at `t`;
//! - every [`send`](NodeHarness::send) and [`trigger`](NodeHarness::trigger) starts a fresh token; wakes and replies
//!   resume the token that asked, so the values a node set before parking are still there;
//! - the values a node sets in a token before it fires are in that firing's [`Fired::values`];
//! - [`set_input`](NodeHarness::set_input) values are what `cx.input` returns, coerced to the port's type;
//! - group switches take effect at the end of the step;
//! - the node's random stream is `Rng::new(seed).fork(rng_stream("test", "node"))`.
//!
//! It sends `Start` before the first action and `Stop` when dropped, and a node that returns `Unhandled` for any input
//! the harness sends fails the test (design D21): a node can't ship a wildcard arm in place of a named one.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_core::rng::Rng;
use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_l10n_api::{MessageTable, Text};
use omuzaki_live_api::event::payloads::EngineTick;
use omuzaki_live_api::event::{LiveEvent, Source, UtcMillis, check_payload};
use omuzaki_live_api::ids::{EventKind, GraphId, GroupId, NodeId, PortName, VarName};
use omuzaki_live_api::limits::{MAX_CALLS_IN_FLIGHT, MAX_EMIT_HOPS, MAX_WAKES_PER_NODE};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::{
    CallId, EvalCx, EvalHost, Input, Node, NodeClass, NodeCx, NodeError, NodeErrorCode, NodeHost, NodeKind, NodeMeta,
    ParamError, ParamErrors, Ports, WakeId, find_node_kind, node_rng, validate_ports,
};
use omuzaki_live_api::services::WallClock;
use omuzaki_live_api::show::VariableDef;
use omuzaki_live_api::value::{PortType, Val, ValError};
use omuzaki_ops_api::OpError;
use serde_json::{Value, json};

use crate::log::canonical;
use crate::runner::{Call, FakeRunner, Reply};
use crate::wall::FakeWall;

/// The graph id the harness runs its node in.
pub const GRAPH: &str = "test";
/// The node id the harness gives its node.
pub const NODE: &str = "node";

/// One firing of an event output.
#[derive(Clone, Debug, PartialEq)]
pub struct Fired {
    /// When, on the runner clock.
    pub at: Seconds,
    /// The output fired.
    pub port: PortName,
    /// The values the node had set in this token when it fired.
    pub values: BTreeMap<PortName, Val>,
}

/// One expected failure the node reported with `cx.fail`.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    /// When.
    pub at: Seconds,
    /// The output it fired (usually `failed`).
    pub port: PortName,
    /// Why, for people.
    pub text: Text,
}

/// A [`NodeError`] the node returned from `handle`: the engine would drop the branch and count it.
#[derive(Clone, Debug, PartialEq)]
pub struct Fault {
    /// When.
    pub at: Seconds,
    /// The input it was handling (`event`, `wake`, ...).
    pub input: &'static str,
    /// The error.
    pub error: NodeError,
}

/// An input the harness owns while it lends it to the node.
enum Item {
    Start,
    Stop,
    Event(String),
    Trigger(LiveEvent),
    Wake(WakeId),
    Reply(CallId, Result<Option<Value>, OpError>),
}

/// A token: the event it started from and the values the node set in it.
struct Token {
    root: LiveEvent,
    values: BTreeMap<PortName, Val>,
}

/// A pending wake.
struct Wake {
    due: Seconds,
    seq: u64,
    id: WakeId,
    token: u64,
}

/// Everything the node can reach: the harness's side of [`NodeHost`].
struct Host {
    ports: Ports,
    clock: Arc<FakeClock>,
    wall: FakeWall,
    step_at: Seconds,
    last_step: Seconds,
    rng: Rng,
    inputs: BTreeMap<String, Val>,
    tokens: BTreeMap<u64, Token>,
    current: u64,
    wakes: Vec<Wake>,
    next_wake: u64,
    runner: FakeRunner,
    in_flight: BTreeMap<CallId, u64>,
    replies: Vec<Reply>,
    vars: BTreeMap<VarName, (VariableDef, Val)>,
    groups: BTreeMap<GroupId, bool>,
    switches: Vec<(GroupId, bool)>,
    fired: Vec<Fired>,
    failures: Vec<Failure>,
    emitted: Vec<LiveEvent>,
    faults: Vec<Fault>,
    stopping: bool,
}

/// `text` in English: its message's declared English with the placeables filled in, as `cx.fail` sets `error`. No
/// package ships its Fluent files yet, so the declaration is the one English source every binary has; a message the
/// binary doesn't declare falls back to the text's last-resort form.
fn english(text: &Text) -> String {
    let declared =
        inventory::iter::<MessageTable>.into_iter().flat_map(|table| table.messages).find(|decl| decl.id == text.id);
    match declared {
        Some(decl) => text.args.iter().fold(decl.english.to_owned(), |english, (name, arg)| {
            english.replace(&format!("{{ ${name} }}"), &arg.to_string())
        }),
        None => text.to_string(),
    }
}

fn ids() -> (GraphId, NodeId) {
    match (GraphId::new(GRAPH), NodeId::new(NODE)) {
        (Ok(graph), Ok(node)) => (graph, node),
        _ => panic!("the harness's graph and node ids are slugs"),
    }
}

impl Host {
    fn new(ports: Ports) -> Self {
        let clock = Arc::new(FakeClock::new(Seconds(0.0)));
        Self {
            ports,
            wall: FakeWall::new(UtcMillis(0), Arc::clone(&clock)),
            clock,
            step_at: Seconds(0.0),
            last_step: Seconds(0.0),
            rng: node_rng(0, GRAPH, NODE),
            inputs: BTreeMap::new(),
            tokens: BTreeMap::new(),
            current: 0,
            wakes: Vec::new(),
            next_wake: 0,
            runner: FakeRunner::new(),
            in_flight: BTreeMap::new(),
            replies: Vec::new(),
            vars: BTreeMap::new(),
            groups: BTreeMap::new(),
            switches: Vec::new(),
            fired: Vec::new(),
            failures: Vec::new(),
            emitted: Vec::new(),
            faults: Vec::new(),
            stopping: false,
        }
    }

    /// A new token rooted at `root`.
    fn token(&mut self, root: LiveEvent) -> u64 {
        let id = u64::try_from(self.tokens.len()).unwrap_or(u64::MAX);
        self.tokens.insert(id, Token { root, values: BTreeMap::new() });
        id
    }

    /// The synthetic root of a token the node starts outside any event (design D5).
    fn tick(&self) -> LiveEvent {
        let (graph, node) = ids();
        match LiveEvent::of(&EngineTick::new(graph, node), Source::Test, self.clock.now()) {
            Ok(event) => event,
            Err(error) => panic!("an engine tick is always a valid event: {error}"),
        }
    }

    fn current_token(&mut self) -> &mut Token {
        let current = self.current;
        match self.tokens.get_mut(&current) {
            Some(token) => token,
            None => panic!("a step always runs in a token the harness made"),
        }
    }

    fn value_output(&self, port: &str) -> Result<(PortName, PortType), NodeError> {
        match self.ports.output(port) {
            Some(decl) if !decl.ty.is_event() && !decl.live => Ok((decl.name.clone(), decl.ty)),
            _ => Err(NodeError::unknown_port(port)),
        }
    }

    fn check_value(port: &str, value: &Val, ty: PortType) -> Result<Val, NodeError> {
        value.check().map_err(|e| NodeError::invalid_value(port, &e))?;
        value.coerce(ty).map_err(|e| match e {
            ValError::Refused { .. } | ValError::Mismatch { .. } => NodeError::type_mismatch(port, ty, value.ty()),
            other => NodeError::invalid_value(port, &other),
        })
    }
}

impl EvalHost for Host {
    fn now(&self) -> Seconds {
        self.step_at
    }

    fn utc(&self) -> UtcMillis {
        self.wall.now()
    }

    fn input(&mut self, port: &str) -> Result<Val, NodeError> {
        let decl = match self.ports.input(port) {
            Some(decl) if !decl.ty.is_event() => decl,
            _ => return Err(NodeError::unknown_port(port)),
        };
        let raw = match self.inputs.get(port).or(decl.default.as_ref()) {
            Some(value) => value.clone(),
            None => decl.ty.default_value().ok_or_else(|| NodeError::unknown_port(port))?,
        };
        Self::check_value(port, &raw, decl.ty)
    }

    fn var(&self, name: &VarName) -> Option<Val> {
        self.vars.get(name).map(|(_, value)| value.clone())
    }

    fn group_enabled(&self, group: &GroupId) -> Option<bool> {
        self.groups.get(group).copied()
    }
}

impl NodeHost for Host {
    fn rng(&mut self) -> &mut Rng {
        &mut self.rng
    }

    fn set(&mut self, port: &str, value: Val) -> Result<(), NodeError> {
        let (name, ty) = self.value_output(port)?;
        let value = Self::check_value(port, &value, ty)?;
        self.current_token().values.insert(name, value);
        Ok(())
    }

    fn fire(&mut self, port: &str) -> Result<(), NodeError> {
        let name = match self.ports.output(port) {
            Some(decl) if decl.ty.is_event() => decl.name.clone(),
            _ => return Err(NodeError::unknown_port(port)),
        };
        if !self.stopping {
            let (at, values) = (self.step_at, self.current_token().values.clone());
            self.fired.push(Fired { at, port: name, values });
        }
        Ok(())
    }

    fn fail(&mut self, port: &str, why: Text) -> Result<(), NodeError> {
        if self.value_output("error").is_ok_and(|(_, ty)| ty == PortType::Text) {
            self.set("error", Val::text(english(&why)))?;
        }
        let name = PortName::new(port).map_err(|_| NodeError::unknown_port(port))?;
        if !self.stopping {
            self.failures.push(Failure { at: self.step_at, port: name, text: why });
        }
        self.fire(port)
    }

    fn wake_at(&mut self, at: Seconds) -> Result<WakeId, NodeError> {
        if self.wakes.len() >= MAX_WAKES_PER_NODE {
            return Err(NodeError::limit("MAX_WAKES_PER_NODE", MAX_WAKES_PER_NODE));
        }
        self.next_wake += 1;
        let id = WakeId(self.next_wake);
        let due = if at.0 > self.step_at.0 { at } else { self.step_at };
        self.wakes.push(Wake { due, seq: self.next_wake, id, token: self.current });
        Ok(id)
    }

    fn cancel_wake(&mut self, id: WakeId) {
        self.wakes.retain(|wake| wake.id != id);
    }

    fn run_op(&mut self, op: &str, args: Value) -> Result<CallId, NodeError> {
        if self.in_flight.len() >= MAX_CALLS_IN_FLIGHT {
            return Err(NodeError::limit("MAX_CALLS_IN_FLIGHT", MAX_CALLS_IN_FLIGHT));
        }
        let (call, reply) = self.runner.request(self.step_at, op, args);
        self.in_flight.insert(call, self.current);
        self.replies.extend(reply);
        Ok(call)
    }

    fn set_var(&mut self, name: &VarName, value: Val) -> Result<(), NodeError> {
        let Some((def, slot)) = self.vars.get_mut(name) else {
            return Err(NodeError::undeclared(name.as_str()));
        };
        let ty = def.ty.port_type();
        value.check().map_err(|e| NodeError::invalid_value(name.as_str(), &e))?;
        if value.ty() != ty {
            return Err(NodeError::type_mismatch(name.as_str(), ty, value.ty()));
        }
        *slot = value;
        Ok(())
    }

    fn switch_group(&mut self, group: &GroupId, on: bool) -> Result<(), NodeError> {
        if !self.groups.contains_key(group) {
            return Err(NodeError::undeclared(group.as_str()));
        }
        self.switches.push((group.clone(), on));
        Ok(())
    }

    fn emit(&mut self, kind: EventKind, payload: Value) -> Result<(), NodeError> {
        let root = &self.current_token().root;
        let (hops, acting) = (root.hops.saturating_add(1), root.acting.clone());
        if hops > MAX_EMIT_HOPS {
            return Err(NodeError::limit("MAX_EMIT_HOPS", usize::from(MAX_EMIT_HOPS)));
        }
        check_payload(&kind, &payload).map_err(|e| NodeError::invalid_payload(kind.as_str(), e.to_string()))?;
        let (graph, node) = ids();
        let mut event = LiveEvent::new(kind, Source::graph(graph, node), self.step_at, payload).with_hops(hops);
        if let Some(actor) = acting {
            event = event.with_acting(actor);
        }
        self.emitted.push(event);
        Ok(())
    }

    fn event(&self) -> &LiveEvent {
        match self.tokens.get(&self.current) {
            Some(token) => &token.root,
            None => panic!("a step always runs in a token the harness made"),
        }
    }
}

/// One node instance under test. See the module documentation for the ordering it keeps.
pub struct NodeHarness {
    node: Box<dyn Node>,
    meta: NodeMeta,
    listens: Vec<EventKind>,
    needs: Vec<Need>,
    host: Host,
    started: bool,
    stopped: bool,
}

impl NodeHarness {
    /// An instance of `K` with `params`.
    ///
    /// # Panics
    /// If `K`'s check refuses `params` or its ports break a rule ([`validate_ports`]): a test of a kind starts from
    /// parameters the kind accepts. Test a refusal through `NodeRegistration::of::<K>().check` instead.
    pub fn of<K: NodeKind>(params: K::Params) -> Self {
        if let Err(errors) = K::check(&params) {
            panic!("{} refuses these parameters: {errors}", K::meta().id);
        }
        let ports = K::ports(&params);
        if let Err(errors) = validate_ports(&ports, K::meta().class) {
            panic!("{} declares invalid ports: {errors}", K::meta().id);
        }
        let (listens, needs) = (K::listens(&params), K::needs(&params));
        Self::build(Box::new(K::new(params)), K::meta(), ports, listens, needs)
    }

    /// An instance of the registered kind `kind` with `params` as JSON, through the registry, as the engine makes
    /// one.
    ///
    /// # Errors
    /// The [`ParamErrors`] the kind reports, or one at `kind` if no such kind is registered.
    // The signature design D16 fixes: a test builds the JSON in the call.
    #[allow(clippy::needless_pass_by_value)]
    pub fn by_id(kind: &str, params: Value) -> Result<Self, ParamErrors> {
        let registration = find_node_kind(kind).ok_or_else(|| {
            ParamErrors(vec![ParamError::new("kind", omuzaki_live_api::l10n::unknown_node_kind(kind))])
        })?;
        let ports = (registration.ports)(&params)?;
        let (listens, needs) = ((registration.listens)(&params)?, (registration.needs)(&params)?);
        let node = (registration.new)(&params)?;
        Ok(Self::build(node, (registration.meta)(), ports, listens, needs))
    }

    fn build(node: Box<dyn Node>, meta: NodeMeta, ports: Ports, listens: Vec<EventKind>, needs: Vec<Need>) -> Self {
        Self { node, meta, listens, needs, host: Host::new(ports), started: false, stopped: false }
    }

    /// With this show seed: the node's stream becomes `Rng::new(seed).fork(rng_stream("test", "node"))`.
    #[must_use]
    pub fn seed(mut self, seed: u32) -> Self {
        self.host.rng = node_rng(seed, GRAPH, NODE);
        self
    }

    /// Starting at `at` on the runner clock and `utc` on the wall clock.
    #[must_use]
    pub fn starting_at(mut self, at: Seconds, utc: UtcMillis) -> Self {
        self.host.clock.set(at);
        self.host.wall = FakeWall::new(utc, Arc::clone(&self.host.clock));
        (self.host.step_at, self.host.last_step) = (at, at);
        self
    }

    /// With the variable `name` declared as `def`, holding `value`.
    ///
    /// # Panics
    /// If `name` isn't a valid variable name or `value` doesn't have the declared type.
    #[must_use]
    pub fn with_var(mut self, name: &str, def: VariableDef, value: Val) -> Self {
        let Ok(var) = VarName::new(name) else { panic!("{name:?} isn't a valid variable name") };
        assert_eq!(value.ty(), def.ty.port_type(), "{name} is declared {}", def.ty.as_str());
        self.host.vars.insert(var, (def, value));
        self
    }

    /// With the group `group` declared, on or off.
    ///
    /// # Panics
    /// If `group` isn't a valid group id.
    #[must_use]
    pub fn with_group(mut self, group: &str, on: bool) -> Self {
        let Ok(id) = GroupId::new(group) else { panic!("{group:?} isn't a valid group id") };
        self.host.groups.insert(id, on);
        self
    }

    /// The kind's identity.
    pub fn meta(&self) -> &NodeMeta {
        &self.meta
    }

    /// The instance's ports.
    pub fn ports(&self) -> &Ports {
        &self.host.ports
    }

    /// The event kinds it listens to.
    pub fn listens(&self) -> &[EventKind] {
        &self.listens
    }

    /// What it needs.
    pub fn needs(&self) -> &[Need] {
        &self.needs
    }

    /// The fake wall clock, to [`jump`](FakeWall::jump) it.
    pub fn wall(&self) -> &FakeWall {
        &self.host.wall
    }

    /// The runner clock's time.
    pub fn now(&self) -> Seconds {
        self.host.clock.now()
    }

    /// Sends `Start` (the harness does it before the first action if the test doesn't).
    ///
    /// # Panics
    /// If the node was already started.
    pub fn start(&mut self) {
        assert!(!self.started, "the node was already started");
        self.started = true;
        let token = self.host.token(self.host.tick());
        self.step(self.now(), token, &Item::Start);
    }

    fn ensure_started(&mut self) {
        assert!(!self.stopped, "the node was stopped");
        if !self.started {
            self.start();
        }
    }

    /// What the value input `port` reads from now on (coerced to its type when read).
    ///
    /// # Panics
    /// If `port` isn't a value input.
    pub fn set_input(&mut self, port: &str, value: impl Into<Val>) {
        assert!(
            self.host.ports.input(port).is_some_and(|p| !p.ty.is_event()),
            "{} has no value input {port}",
            self.meta.id
        );
        self.host.inputs.insert(port.to_owned(), value.into());
    }

    /// A pulse on the event input `port` at the current time, in a fresh token, after any wake already due (a
    /// wake due at `t` runs before an item arriving at `t`, design D5).
    ///
    /// # Panics
    /// If `port` isn't an event input.
    pub fn send(&mut self, port: &str) {
        assert!(
            self.host.ports.input(port).is_some_and(|p| p.ty.is_event()),
            "{} has no event input {port}",
            self.meta.id
        );
        self.ensure_started();
        self.advance_to(self.now());
        let token = self.host.token(self.host.tick());
        self.step(self.now(), token, &Item::Event(port.to_owned()));
    }

    /// `event` to a trigger at its timestamp, in a fresh token rooted at it, as the engine delivers an inbox item
    /// (design D5): a later stamp first moves the clock to it, running the wakes and replies due by then (a wake
    /// due at the same time runs first); an earlier one runs now, since time never goes backwards.
    ///
    /// # Panics
    /// If the node isn't a trigger or doesn't listen to the event's kind: the engine never delivers it.
    pub fn trigger(&mut self, event: LiveEvent) {
        assert_eq!(self.meta.class, NodeClass::Trigger, "{} isn't a trigger", self.meta.id);
        assert!(self.listens.contains(&event.kind), "{} doesn't listen to {}", self.meta.id, event.kind);
        self.ensure_started();
        let at = event.at;
        self.advance_to(at);
        let token = self.host.token(event.clone());
        self.step(at, token, &Item::Trigger(event));
    }

    /// Moves the clock on by `by`, running due wakes and scripted replies in the engine's order.
    pub fn advance(&mut self, by: Seconds) {
        self.advance_to(Seconds(self.now().0 + by.0.max(0.0)));
    }

    /// Moves the clock to `at` (never back), running due wakes and scripted replies in the engine's order.
    pub fn advance_to(&mut self, at: Seconds) {
        self.ensure_started();
        loop {
            let wake = (0..self.host.wakes.len()).filter(|&i| self.host.wakes[i].due.0 <= at.0).min_by(|&a, &b| {
                let (a, b) = (&self.host.wakes[a], &self.host.wakes[b]);
                a.due.0.total_cmp(&b.due.0).then(a.seq.cmp(&b.seq))
            });
            let reply = (0..self.host.replies.len()).filter(|&i| self.host.replies[i].at.0 <= at.0).min_by(|&a, &b| {
                let (a, b) = (&self.host.replies[a], &self.host.replies[b]);
                a.at.0.total_cmp(&b.at.0).then(a.call.cmp(&b.call))
            });
            match (wake, reply) {
                (Some(w), Some(r)) if self.host.wakes[w].due.0 <= self.host.replies[r].at.0 => self.run_wake(w),
                (Some(w), None) => self.run_wake(w),
                (_, Some(r)) => self.run_reply(r),
                (None, None) => break,
            }
        }
        let to =
            [at, self.host.last_step, self.host.clock.now()].into_iter().fold(at, |a, b| if b.0 > a.0 { b } else { a });
        self.host.clock.set(to);
    }

    fn run_wake(&mut self, index: usize) {
        let wake = self.host.wakes.remove(index);
        self.step(wake.due, wake.token, &Item::Wake(wake.id));
    }

    fn run_reply(&mut self, index: usize) {
        let reply = self.host.replies.remove(index);
        let Some(token) = self.host.in_flight.remove(&reply.call) else {
            panic!("a scheduled reply's call is in flight until it is delivered");
        };
        self.step(reply.at, token, &Item::Reply(reply.call, reply.outcome));
    }

    /// From now on, answers each call to `op` with `answer(args)`, `latency` after the call.
    pub fn respond(
        &mut self,
        op: &str,
        latency: Seconds,
        answer: impl FnMut(&Value) -> Result<Option<Value>, OpError> + 'static,
    ) {
        self.host.runner.respond(op, latency, answer);
    }

    /// Delivers a reply to `call` now, in the token that made it (for calls no script answers), after any wake
    /// already due (a wake due at `t` runs before a reply arriving at `t`, design D5).
    ///
    /// # Panics
    /// If `call` isn't waiting for a reply.
    pub fn reply(&mut self, call: CallId, outcome: Result<Option<Value>, OpError>) {
        self.ensure_started();
        assert!(self.host.in_flight.contains_key(&call), "call {call:?} isn't waiting for a reply");
        self.advance_to(self.now());
        let Some(token) = self.host.in_flight.remove(&call) else { panic!("call {call:?} isn't waiting for a reply") };
        self.host.replies.retain(|reply| reply.call != call);
        self.step(self.now(), token, &Item::Reply(call, outcome));
    }

    /// The live output `port`, computed now and checked against its declaration as the engine checks it.
    ///
    /// # Errors
    /// [`NodeError::not_live`] if `port` isn't a live output, what the node's `eval` returns, or
    /// [`NodeError::invalid_value`] and [`NodeError::type_mismatch`] for a value its port can't carry.
    pub fn read(&mut self, port: &str) -> Result<Val, NodeError> {
        self.ensure_started();
        let ty = match self.host.ports.output(port) {
            Some(decl) if decl.live => decl.ty,
            _ => return Err(NodeError::not_live(port)),
        };
        self.host.step_at = self.now();
        let value = self.node.eval(port, &mut EvalCx::new(&mut self.host))?;
        Host::check_value(port, &value, ty)
    }

    /// Sends `Stop` (the harness does it when dropped if the test doesn't). Outputs fired while stopping are ignored.
    ///
    /// # Panics
    /// If the node was already stopped.
    pub fn stop(&mut self) {
        assert!(!self.stopped, "the node was already stopped");
        if !self.started {
            self.start();
        }
        self.host.stopping = true;
        let token = self.host.token(self.host.tick());
        self.step(self.now(), token, &Item::Stop);
        self.stopped = true;
    }

    /// Runs one step: the node handles `item` in `token` at `at`, or at the clock's time if that is later (the
    /// previous step's, or where an idle advance moved it): time never goes backwards.
    fn step(&mut self, at: Seconds, token: u64, item: &Item) {
        let floor = self.host.clock.now();
        let floor = if floor.0 > self.host.last_step.0 { floor } else { self.host.last_step };
        let at = if at.0 > floor.0 { at } else { floor };
        self.host.clock.set(at);
        (self.host.step_at, self.host.current) = (at, token);
        let input = match item {
            Item::Start => Input::Start,
            Item::Stop => Input::Stop,
            Item::Event(port) => Input::event(port),
            Item::Trigger(event) => Input::trigger(event),
            Item::Wake(id) => Input::wake(*id),
            Item::Reply(call, outcome) => Input::reply(*call, outcome.clone()),
        };
        let name = input.name();
        if let Err(error) = self.node.handle(input, &mut NodeCx::new(&mut self.host)) {
            assert!(
                error.code != NodeErrorCode::Unhandled,
                "{} doesn't handle the `{name}` input: give it a named arm (design D21)",
                self.meta.id
            );
            self.host.faults.push(Fault { at, input: name, error });
        }
        for (group, on) in std::mem::take(&mut self.host.switches) {
            self.host.groups.insert(group, on);
        }
        self.host.last_step = at;
    }

    /// Every firing, in order.
    pub fn fired(&self) -> &[Fired] {
        &self.host.fired
    }

    /// Every op call, in order.
    pub fn calls(&self) -> &[Call] {
        self.host.runner.calls()
    }

    /// Every failure reported with `cx.fail`, in order.
    pub fn failures(&self) -> &[Failure] {
        &self.host.failures
    }

    /// Every error the node returned from `handle`, in order.
    pub fn faults(&self) -> &[Fault] {
        &self.host.faults
    }

    /// Every event the node emitted, in order.
    pub fn emitted(&self) -> &[LiveEvent] {
        &self.host.emitted
    }

    /// A variable's value.
    pub fn var(&self, name: &str) -> Option<Val> {
        let name = VarName::new(name).ok()?;
        self.host.vars.get(&name).map(|(_, value)| value.clone())
    }

    /// A group's runtime state.
    pub fn group(&self, group: &str) -> Option<bool> {
        let group = GroupId::new(group).ok()?;
        self.host.groups.get(&group).copied()
    }

    /// Everything above as canonical JSON (sorted keys, numbers in round-trip form): run a script twice and compare
    /// the two byte for byte (design D6).
    pub fn log_json(&self) -> String {
        let values = |values: &BTreeMap<PortName, Val>| -> Value {
            values.iter().map(|(port, value)| (port.to_string(), value.to_json())).collect()
        };
        let log = json!({
            "fired": self.host.fired.iter().map(|f| json!({"at": f.at, "port": f.port, "values": values(&f.values)})).collect::<Vec<_>>(),
            "calls": self.host.runner.calls(),
            "failures": self.host.failures.iter().map(|f| json!({"at": f.at, "port": f.port, "text": f.text})).collect::<Vec<_>>(),
            "faults": self.host.faults.iter().map(|f| json!({"at": f.at, "input": f.input, "error": f.error})).collect::<Vec<_>>(),
            "emitted": self.host.emitted,
            "variables": self.host.vars.iter().map(|(name, (_, value))| (name.to_string(), value.to_json())).collect::<serde_json::Map<_, _>>(),
            "groups": self.host.groups.iter().map(|(group, on)| (group.to_string(), Value::Bool(*on))).collect::<serde_json::Map<_, _>>(),
        });
        canonical(log).to_string()
    }
}

impl Drop for NodeHarness {
    fn drop(&mut self) {
        // Every test sends Stop, so a node's Stop arm is always exercised; never while a test is already failing.
        if self.started && !self.stopped && !std::thread::panicking() {
            self.stop();
        }
    }
}
