//! [`ShowHarness`] (design D16): a whole [`ShowDef`] on the real engine core, on fake time, with scripted op replies
//! and scripted events. The engine thread and the harness run the same `Core`, in the same order (the engine's own
//! events first, then due wakes, then items), so a harness test checks the real code path without a thread or a
//! clock.
//!
//! Behind the `testing` feature (tests always see it).

use std::collections::BTreeMap;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::{EventId, LiveEvent, UtcMillis};
use omuzaki_live_api::ids::ShowId;
use omuzaki_live_api::node::CallId;
use omuzaki_live_api::services::{Dropped, GraphStatus, ValuesSnapshot};
use omuzaki_live_api::show::{Finding, ShowDef};
use omuzaki_ops_api::{Actor, OpError};
use serde_json::{Value, json};

use crate::compile::compile;
use crate::core::{Core, Item, OpRequest};

/// An answer script for an op: the reply `latency` after the call.
type Answer = Box<dyn FnMut(&Value) -> Outcome>;

/// An op's outcome.
type Outcome = Result<Option<Value>, OpError>;

/// One op request a step made, as the harness saw it.
#[derive(Clone, Debug, PartialEq)]
pub struct Requested {
    /// When it was made.
    pub at: Seconds,
    /// The call.
    pub call: CallId,
    /// The op.
    pub op: String,
    /// Its arguments.
    pub args: Value,
    /// Who it runs as: `None` is the show's own name.
    pub acting: Option<Actor>,
    /// The rule and the event (design D7).
    pub cause: String,
}

impl Requested {
    fn json(&self) -> Value {
        let acting = self.acting.as_ref().map(|a| serde_json::to_value(a).unwrap_or(Value::Null));
        json!({ "at": self.at.0, "op": self.op, "args": self.args, "cause": self.cause, "as": acting })
    }
}

/// A whole show on the real engine core, driven by a test.
pub struct ShowHarness {
    show: ShowId,
    def: ShowDef,
    seed: u32,
    utc0: i64,
    core: Option<Core>,
    findings: Vec<Finding>,
    now: Seconds,
    answers: BTreeMap<String, (Seconds, Answer)>,
    /// Scheduled replies by (due time's bits, order): non-negative finite `f64`s order like their bits.
    replies: BTreeMap<(u64, u64), (CallId, Outcome)>,
    order: u64,
    next_event: u64,
    requests: Vec<Requested>,
    /// Calls nobody answered yet, for [`reply`](Self::reply).
    open: Vec<CallId>,
}

impl std::fmt::Debug for ShowHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShowHarness").field("show", &self.show).field("now", &self.now).finish_non_exhaustive()
    }
}

impl ShowHarness {
    /// `def` as the show `show`, compiled against the registered node kinds; it starts on the first input.
    pub fn new(show: ShowId, def: ShowDef) -> Self {
        Self {
            show,
            def,
            seed: 0,
            utc0: 1_800_000_000_000,
            core: None,
            findings: Vec::new(),
            now: Seconds(0.0),
            answers: BTreeMap::new(),
            replies: BTreeMap::new(),
            order: 0,
            next_event: 0,
            requests: Vec::new(),
            open: Vec::new(),
        }
    }

    /// With every node's random stream drawn from `seed` (design D6).
    #[must_use]
    pub fn seed(mut self, seed: u32) -> Self {
        self.seed = seed;
        self
    }

    /// With the wall clock at `utc` when the runner clock is 0.
    #[must_use]
    pub fn wall_at(mut self, utc: UtcMillis) -> Self {
        self.utc0 = utc.0;
        self
    }

    /// From now on, answers each call to `op` with `answer(args)`, `latency` after the call. Unanswered calls stay open
    /// for [`reply`](Self::reply).
    pub fn respond(
        &mut self,
        op: &str,
        latency: Seconds,
        answer: impl FnMut(&Value) -> Result<Option<Value>, OpError> + 'static,
    ) {
        self.answers.insert(op.to_owned(), (latency, Box::new(answer)));
    }

    /// The structural findings of the compile.
    pub fn findings(&mut self) -> &[Finding] {
        self.ensure_started();
        &self.findings
    }

    /// The runner clock.
    pub fn now(&self) -> Seconds {
        self.now
    }

    /// Runs `event` as a step: first time moves to its stamp if that is later (running what falls due on the way); an
    /// event stamped earlier than now runs now (time never goes backwards).
    pub fn publish(&mut self, mut event: LiveEvent) {
        self.ensure_started();
        if event.at.0 > self.now.0 {
            self.advance_to(event.at);
        }
        self.next_event += 1;
        event.id = EventId(self.next_event);
        let (now, utc) = (self.now, self.utc());
        if let Some(core) = self.core.as_mut() {
            core.step(now, utc, Item::Event(event));
        }
        self.settle();
    }

    /// Moves time on by `by`, running due wakes and replies in order.
    pub fn advance(&mut self, by: Seconds) {
        self.advance_to(Seconds(self.now.0 + by.0.max(0.0)));
    }

    /// Moves time on to `at`, running due wakes and replies in order: a wake due at `t` runs before a reply arriving at
    /// `t`, as on the engine thread.
    pub fn advance_to(&mut self, at: Seconds) {
        self.ensure_started();
        loop {
            let wake = self.core.as_ref().and_then(Core::next_wake).filter(|due| due.0 <= at.0);
            let reply =
                self.replies.keys().next().map(|(bits, _)| Seconds(f64::from_bits(*bits))).filter(|due| due.0 <= at.0);
            match (wake, reply) {
                (Some(w), r) if r.is_none_or(|r| w.0 <= r.0) => {
                    self.now = Seconds(self.now.0.max(w.0));
                    let utc = self.utc();
                    if let Some(core) = self.core.as_mut() {
                        core.wake(w, utc);
                    }
                }
                (_, Some(r)) => {
                    self.now = Seconds(self.now.0.max(r.0));
                    let Some((_, (call, outcome))) = self.replies.pop_first() else { break };
                    let (now, utc) = (self.now, self.utc());
                    if let Some(core) = self.core.as_mut() {
                        core.step(now, utc, Item::Reply { call, outcome });
                    }
                }
                _ => break,
            }
            self.settle();
        }
        self.now = Seconds(self.now.0.max(at.0));
    }

    /// Delivers a reply to an open call now.
    pub fn reply(&mut self, call: CallId, outcome: Result<Option<Value>, OpError>) {
        self.ensure_started();
        self.open.retain(|c| *c != call);
        let (now, utc) = (self.now, self.utc());
        if let Some(core) = self.core.as_mut() {
            core.step(now, utc, Item::Reply { call, outcome });
        }
        self.settle();
    }

    /// Calls nobody answered yet.
    pub fn open_calls(&self) -> &[CallId] {
        &self.open
    }

    /// Every op request so far, in order.
    pub fn requests(&self) -> &[Requested] {
        &self.requests
    }

    /// The requests as JSON (`at`, `op`, `args`, `cause`, `as`), for golden comparisons.
    pub fn requests_json(&self) -> Value {
        Value::Array(self.requests.iter().map(Requested::json).collect())
    }

    /// The values monitor's snapshot.
    pub fn values(&mut self) -> ValuesSnapshot {
        self.ensure_started();
        self.core.as_ref().map_or_else(|| ValuesSnapshot::new(self.now), Core::values)
    }

    /// Each graph's state.
    pub fn graphs(&mut self) -> Vec<GraphStatus> {
        self.ensure_started();
        self.core.as_ref().map(Core::graphs).unwrap_or_default()
    }

    /// What was dropped so far.
    pub fn dropped(&mut self) -> Dropped {
        self.ensure_started();
        self.core.as_ref().map(Core::dropped).unwrap_or_default()
    }

    /// The requests, the firings and the variables as one canonical JSON text: run a script twice and compare the
    /// two byte for byte (design D6).
    pub fn log_json(&mut self) -> String {
        let values = self.values();
        let log = json!({
            "requests": self.requests_json(),
            "firings": serde_json::to_value(&values.firings).unwrap_or(Value::Null),
            "variables": serde_json::to_value(&values.variables).unwrap_or(Value::Null),
            "groups": serde_json::to_value(&values.groups).unwrap_or(Value::Null),
        });
        log.to_string()
    }

    /// Stops the show: every node gets `Stop`.
    pub fn stop(&mut self) {
        let (now, utc) = (self.now, self.utc());
        if let Some(core) = self.core.as_mut() {
            core.stop(now, utc);
        }
    }

    fn utc(&self) -> UtcMillis {
        // Fake runs never come near i64's range.
        #[allow(clippy::cast_possible_truncation)]
        let ms = (self.now.0 * 1_000.0).round() as i64;
        UtcMillis(self.utc0 + ms)
    }

    fn ensure_started(&mut self) {
        if self.core.is_some() {
            return;
        }
        let compiled = compile(&self.show, &self.def);
        self.findings.clone_from(&compiled.findings);
        let mut core = Core::new(compiled, self.seed, self.now, self.utc());
        core.start(self.now, self.utc());
        self.core = Some(core);
        self.settle();
    }

    /// After a step: hands its requests to the answer scripts and runs the engine's own events, as the engine thread
    /// does before it looks at wakes or the inbox.
    fn settle(&mut self) {
        loop {
            let Some(core) = self.core.as_mut() else { return };
            let outgoing = core.take_outgoing();
            for request in outgoing {
                self.record(request);
            }
            let Some(core) = self.core.as_mut() else { return };
            let Some(mut event) = core.pop_own() else { return };
            self.next_event += 1;
            event.id = EventId(self.next_event);
            let (now, utc) = (self.now, self.utc());
            if let Some(core) = self.core.as_mut() {
                core.step(now, utc, Item::Event(event));
            }
        }
    }

    fn record(&mut self, request: OpRequest) {
        let OpRequest { call, op, args, acting, cause } = request;
        if let Some((latency, answer)) = self.answers.get_mut(&op) {
            let outcome = answer(&args);
            let due = Seconds(self.now.0 + latency.0.max(0.0));
            self.order += 1;
            self.replies.insert((due.0.to_bits(), self.order), (call, outcome));
        } else {
            self.open.push(call);
        }
        self.requests.push(Requested { at: self.now, call, op, args, acting, cause });
    }
}
