//! [`ShowHarness`] (design D16): a whole [`ShowDef`] on the real engine core, on fake time, with scripted op replies
//! and scripted events. The engine thread and the harness run the same `Core`, in the same order (the engine's own
//! events first, then due wakes, then items), so a harness test checks the real code path without a thread or a
//! clock.
//!
//! Behind the `testing` feature (tests always see it).

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::{EventId, LiveEvent, UtcMillis};
use omuzaki_live_api::ids::{GraphId, GroupId, ShowId, VarName};
use omuzaki_live_api::node::CallId;
use omuzaki_live_api::services::{Dropped, GraphStatus, PersistScope, ShowApprovals, ValuesSnapshot, VariableStore};
use omuzaki_live_api::show::{Finding, ShowDef, VarScope};
use omuzaki_live_api::value::Val;
use omuzaki_ops_api::{Actor, OpError};
use serde_json::{Value, json};

use crate::compile::{Arming, FileHasher, ProgramHasher, compile_with};
use crate::core::{Control, Core, Item, OpRequest};
use crate::vars;

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
    /// Steps run after the start: events, wakes, replies and changes from outside.
    steps: u64,
    /// Where variable values persist, and the project folder a show's own values live under.
    store: Option<(Arc<dyn VariableStore>, Option<String>)>,
    /// The file each persisted scope writes to.
    files: Vec<(VarScope, PersistScope)>,
    /// What this machine approved for the show (design D11).
    approvals: ShowApprovals,
    /// How program files are hashed for the program rule.
    hasher: Arc<dyn ProgramHasher>,
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
            steps: 0,
            store: None,
            files: Vec::new(),
            approvals: ShowApprovals::default(),
            hasher: Arc::new(FileHasher),
        }
    }

    /// With these approvals for the show (design D11); nothing is approved otherwise.
    #[must_use]
    pub fn approvals(mut self, approvals: ShowApprovals) -> Self {
        self.approvals = approvals;
        self
    }

    /// With program files hashed by `hasher` (the real files otherwise).
    #[must_use]
    pub fn hasher(mut self, hasher: Arc<dyn ProgramHasher>) -> Self {
        self.hasher = hasher;
        self
    }

    /// Swaps in `def` (and `approvals`, when given) between two steps, as an edit to the running show does (design
    /// D5's hot reload): unchanged nodes keep their state, changed ones start again.
    pub fn reload(&mut self, def: ShowDef, approvals: Option<ShowApprovals>) {
        self.ensure_started();
        if let Some(approvals) = approvals {
            self.approvals = approvals;
        }
        self.def = def;
        let (now, utc) = (self.now, self.utc());
        let arming = Arming { approvals: &self.approvals, hasher: self.hasher.as_ref() };
        if let Some(core) = self.core.as_mut() {
            let compiled = compile_with(&self.show, &self.def, arming, Some(core.compiled_nodes()));
            self.findings.clone_from(&compiled.findings);
            core.reload(compiled, now, utc);
        }
        self.settle();
    }

    /// With every node's random stream drawn from `seed` (design D6).
    #[must_use]
    pub fn seed(mut self, seed: u32) -> Self {
        self.seed = seed;
        self
    }

    /// With variable values loaded from and saved to `store` (a show's own under `project`), as the service does:
    /// each file written one second after its first change, and every changed file again at [`stop`](Self::stop).
    #[must_use]
    pub fn variables(mut self, store: Arc<dyn VariableStore>, project: Option<&str>) -> Self {
        self.store = Some((store, project.map(str::to_owned)));
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
        self.steps += 1;
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
            // A values file due at `t` is written after the steps at `t`, as the engine thread looks at saves last.
            let save = self.core.as_ref().and_then(Core::next_save).filter(|due| due.0 <= at.0);
            if let Some(s) = save
                && wake.is_none_or(|w| s.0 < w.0)
                && reply.is_none_or(|r| s.0 < r.0)
            {
                self.now = Seconds(self.now.0.max(s.0));
                self.save(false);
                continue;
            }
            match (wake, reply) {
                (Some(w), r) if r.is_none_or(|r| w.0 <= r.0) => {
                    self.now = Seconds(self.now.0.max(w.0));
                    self.steps += 1;
                    let utc = self.utc();
                    if let Some(core) = self.core.as_mut() {
                        core.wake(w, utc);
                    }
                }
                (_, Some(r)) => {
                    self.now = Seconds(self.now.0.max(r.0));
                    let Some((_, (call, outcome))) = self.replies.pop_first() else { break };
                    self.steps += 1;
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
        self.steps += 1;
        let (now, utc) = (self.now, self.utc());
        if let Some(core) = self.core.as_mut() {
            core.step(now, utc, Item::Reply { call, outcome });
        }
        self.settle();
    }

    /// Switches `group` from outside the show (`automation.switch_group`), as a step of its own.
    pub fn switch_group(&mut self, group: GroupId, on: bool) {
        self.control(Control::SwitchGroup { group, on });
    }

    /// Sets `name` from outside the show (`automation.set_variable`), as a step of its own. The service checks the
    /// type first; the engine ignores a value of the wrong type.
    pub fn set_variable(&mut self, name: VarName, value: Val) {
        self.control(Control::SetVariable { name, value });
    }

    /// Lifts `graph`'s suspension (`automation.resume_graph`), as a step of its own.
    pub fn resume_graph(&mut self, graph: GraphId) {
        self.control(Control::ResumeGraph { graph });
    }

    /// The panic button (`automation.pause`): no new tokens start, and wakes and replies are dropped.
    pub fn pause(&mut self) {
        self.control(Control::Pause);
    }

    /// Lets the show run again (`automation.resume`).
    pub fn resume(&mut self) {
        self.control(Control::Resume);
    }

    /// How many steps ran after the start: an idle show runs none.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    fn control(&mut self, control: Control) {
        self.ensure_started();
        self.steps += 1;
        let (now, utc) = (self.now, self.utc());
        if let Some(core) = self.core.as_mut() {
            core.step(now, utc, Item::Control(control));
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

    /// Stops the show: every node gets `Stop`, and every values file the run changed is written.
    pub fn stop(&mut self) {
        let (now, utc) = (self.now, self.utc());
        if let Some(core) = self.core.as_mut() {
            core.stop(now, utc);
        }
        self.save(true);
    }

    /// Writes the values files due now (with `flush`, every one the run changed).
    fn save(&mut self, flush: bool) {
        let (Some(core), Some((store, _))) = (self.core.as_mut(), self.store.as_ref()) else { return };
        for (scope, values) in core.take_saves(self.now, flush) {
            if let Some((_, persist)) = self.files.iter().find(|(s, _)| *s == scope) {
                // A test that wants to see a failed write reads its store.
                let _ = store.save(persist, &values);
            }
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
        let arming = Arming { approvals: &self.approvals, hasher: self.hasher.as_ref() };
        let compiled = compile_with(&self.show, &self.def, arming, None);
        self.findings.clone_from(&compiled.findings);
        let mut core = Core::new(compiled, self.seed, self.now, self.utc());
        if let Some((store, project)) = &self.store {
            let (findings, files) = vars::load(&mut core, store.as_ref(), project.as_deref(), &self.show);
            self.findings.extend(findings);
            self.files = files;
        }
        core.start(self.now, self.utc());
        self.core = Some(core);
        self.settle();
        // The start and what it queued (`show.started`) aren't counted.
        self.steps = 0;
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
            self.steps += 1;
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
