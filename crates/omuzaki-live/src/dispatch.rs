//! Dispatch (design D7, D8, D12; #1176): where the engine's op calls meet the executor.
//!
//! - **The show's session.** A start on behalf of a person opens one automation session for the run
//!   (`Sessions::open`, `Transport::Automation`, client `automation:<show>`, the base grants, D7's limits); every call
//!   the show makes runs as that session's actor, pause and resume map to the session's, and the stop ends it. A start
//!   with nobody behind it (a test, a start before the executor has sessions) runs in Omuzaki's own name. The session
//!   is opened, paused and resumed on the ordered worker, never on the caller's thread: `automation.start` and
//!   `automation.pause` run inside the executor, which can't be re-entered from there.
//! - **Who acts.** A call in a token rooted at `automation.fire` runs as the caller, with the caller's own grants, never
//!   the show's (D7). The cause names the show, graph, node and event, never the event's content.
//! - **Lanes.** One ordered worker runs edits, queries and live actions in the order the steps asked for them; four job
//!   workers run `OpKind::Job` ops, so a long job never holds the ordered lane. Network calls (`net.*`) and programs
//!   (`exec.run`) wait on the outside world, so each runs on a thread of its own, bounded by its limit.
//! - **Limits (D8).** At most 8 `net.*` calls in flight, and 4 `exec.run` running and 30 started a minute (runner
//!   clock); over a limit the call fails `limited` at once. Nothing is retried: `limited`, `waiting` and `denied`
//!   come back as the node's failure. (The 64 calls in flight are the engine's own limit.)
//! - **Stop.** Calls not yet started are dropped; the session ends once the calls in flight are back, or after
//!   2 seconds, on a thread of its own so a stop from inside an op never waits on the executor.

use std::collections::{BTreeMap, VecDeque};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_live_api::ids::ShowId;
use omuzaki_live_api::limits::{
    MAX_EXEC_PER_MINUTE, MAX_EXEC_RUNNING, MAX_NET_IN_FLIGHT, SESSION_EDITS_BURST, SESSION_EDITS_PER_MINUTE,
    SESSION_MAX_PENDING_APPROVALS, SESSION_RUNS_BURST, SESSION_RUNS_PER_MINUTE, STOP_GRACE_SECONDS,
};
use omuzaki_ops_api::{
    Actor, Budget, Cause, CauseKind, EndReason, ErrorCode, Grants, Limits, Mode, OpError, OpKind, Request, Runner,
    Scope, SessionId, SessionInfo, SessionSpec, Sessions, Transport,
};
use serde_json::Value;

use crate::core::{Item, OpRequest};
use crate::inbox::Inbox;

/// How many workers run `OpKind::Job` calls.
pub(crate) const JOB_WORKERS: usize = 4;

/// The grants a show's calls run with until a person approves more (design D11): reading and performing.
pub(crate) fn base_grants() -> Grants {
    Grants::of([Scope::Read, Scope::Perform])
}

/// D7's session limits for an automation.
fn automation_limits() -> Limits {
    Limits {
        edits_per_minute: SESSION_EDITS_PER_MINUTE,
        edit_burst: SESSION_EDITS_BURST,
        runs_per_minute: SESSION_RUNS_PER_MINUTE,
        run_burst: SESSION_RUNS_BURST,
        max_pending: u32::try_from(SESSION_MAX_PENDING_APPROVALS).unwrap_or(u32::MAX),
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A call refused by one of D8's limits.
fn limited(what: String) -> OpError {
    OpError { code: ErrorCode::Limited, ..OpError::conflict(what) }
}

/// What the limits count.
#[derive(Default)]
struct Counters {
    in_flight: usize,
    net: usize,
    exec_running: usize,
    /// When each `exec.run` of the last minute started, on the runner clock.
    exec_started: VecDeque<Seconds>,
}

/// Which limit a call holds while it runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Held {
    Net,
    Exec,
    Neither,
}

/// What the ordered worker takes, in order.
pub(crate) enum Work {
    /// An op call a step asked for.
    Call(OpRequest),
    /// Pause (`true`) or resume the show's session.
    Pause(bool),
    /// The show's approvals changed: its calls and its session hold these grants from now on (design D11).
    Ceiling(Grants),
}

/// One run's dispatch.
pub(crate) struct Dispatch {
    runner: Option<Weak<dyn Runner>>,
    sessions: Option<Weak<dyn Sessions>>,
    /// The person the session is opened for, if any.
    principal: Option<Actor>,
    show: ShowId,
    /// The show's session, once the ordered worker opened it.
    session: OnceLock<SessionInfo>,
    /// What the show's calls may do: the base plus what the person approved ([`crate::approvals::ceiling`]).
    ceiling: Mutex<Grants>,
    inbox: Arc<Inbox>,
    clock: Arc<dyn Clock>,
    counters: Mutex<Counters>,
    returned: Condvar,
    stopping: AtomicBool,
}

impl Dispatch {
    /// Dispatch for show `show`, with a session opened on behalf of `principal` (when there is one and the executor
    /// has sessions) as the ordered worker starts.
    pub(crate) fn new(
        runner: Option<Weak<dyn Runner>>,
        sessions: Option<Weak<dyn Sessions>>,
        show: &ShowId,
        principal: Option<Actor>,
        ceiling: Grants,
        inbox: Arc<Inbox>,
        clock: Arc<dyn Clock>,
    ) -> Arc<Dispatch> {
        Arc::new(Dispatch {
            runner,
            sessions,
            principal,
            show: show.clone(),
            session: OnceLock::new(),
            ceiling: Mutex::new(ceiling),
            inbox,
            clock,
            counters: Mutex::new(Counters::default()),
            returned: Condvar::new(),
            stopping: AtomicBool::new(false),
        })
    }

    /// The show's session, `SessionId(0)` while it runs in Omuzaki's own name.
    pub(crate) fn session_id(&self) -> SessionId {
        self.session.get().map_or(SessionId(0), |s| s.id)
    }

    /// Opens the show's session. A refusal leaves the run in Omuzaki's own name with the show's base grants, which is
    /// what it holds before approvals anyway.
    fn open_session(&self) {
        let (Some(sessions), Some(person)) = (self.sessions.as_ref().and_then(Weak::upgrade), &self.principal) else {
            return;
        };
        let spec = SessionSpec {
            transport: Transport::Automation,
            client: format!("automation:{}", self.show),
            model: None,
            on_behalf_of: person.clone(),
            grants: lock(&self.ceiling).clone(),
            budget: Budget::default(),
            limits: automation_limits(),
            parent: None,
            role: None,
            fence: Vec::new(),
        };
        if let Ok(session) = sessions.open(spec) {
            let _ = self.session.set(session);
        }
    }

    /// Starts the ordered worker on `work` and the job workers.
    ///
    /// # Errors
    /// A thread that didn't start.
    pub(crate) fn spawn(self: &Arc<Self>, work: Receiver<Work>) -> Result<(), OpError> {
        let show = self.show.clone();
        let spawn_failed =
            |e: std::io::Error| OpError::internal(format!("the automation dispatcher didn't start: {e}"));
        let (jobs, queued) = channel::<OpRequest>();
        let queued = Arc::new(Mutex::new(queued));
        for i in 0..JOB_WORKERS {
            let (me, queued) = (Arc::clone(self), Arc::clone(&queued));
            std::thread::Builder::new()
                .name(format!("automation-jobs-{show}-{i}"))
                .spawn(move || {
                    loop {
                        // One worker waits on the queue at a time; the lock is released before the call runs.
                        let next = lock(&queued).recv();
                        let Ok(call) = next else { break };
                        me.run(&call);
                    }
                })
                .map_err(spawn_failed)?;
        }
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name(format!("automation-calls-{show}"))
            .spawn(move || {
                me.open_session();
                let mut kinds: BTreeMap<String, bool> = BTreeMap::new();
                for item in work {
                    let call = match item {
                        Work::Call(call) => call,
                        Work::Pause(on) => {
                            me.pause(on);
                            continue;
                        }
                        Work::Ceiling(grants) => {
                            me.set_ceiling(grants);
                            continue;
                        }
                    };
                    let job = *kinds.entry(call.op.clone()).or_insert_with(|| {
                        omuzaki_ops_api::find(&call.op).is_some_and(|r| (r.meta)().kind == OpKind::Job)
                    });
                    if Self::beside(&call.op) {
                        me.run_beside(call);
                    } else if job {
                        // The job workers only end when this sender goes.
                        let _ = jobs.send(call);
                    } else {
                        me.run(&call);
                    }
                }
            })
            .map_err(spawn_failed)?;
        Ok(())
    }

    /// Whether `op` runs beside the lanes, bounded by its own limit: network calls and programs, which wait on the
    /// outside world, so a slow host or program never holds the ordered lane or a job worker.
    fn beside(op: &str) -> bool {
        op.starts_with("net.") || op == "exec.run"
    }

    /// Admits a network call or a program at once (or refuses it with `limited`), then runs it on a thread of its own.
    fn run_beside(self: &Arc<Self>, call: OpRequest) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let held = match self.admit(&call.op) {
            Ok(held) => held,
            Err(refused) => {
                self.inbox.push_reply(Item::Reply { call: call.call, outcome: Err(refused) });
                return;
            }
        };
        let me = Arc::clone(self);
        let started = std::thread::Builder::new().name(format!("automation-{}", call.op)).spawn(move || {
            let outcome = me.call(&call);
            me.release(held);
            me.inbox.push_reply(Item::Reply { call: call.call, outcome });
        });
        if started.is_err() {
            // The call went with the closure; its node hears nothing and the show runs on.
            self.release(held);
        }
    }

    /// Runs one call and posts its reply; refused at once when a limit is reached, dropped when stopping.
    fn run(&self, call: &OpRequest) {
        if self.stopping.load(Ordering::SeqCst) {
            return;
        }
        let outcome = match self.admit(&call.op) {
            Ok(held) => {
                let outcome = self.call(call);
                self.release(held);
                outcome
            }
            Err(refused) => Err(refused),
        };
        self.inbox.push_reply(Item::Reply { call: call.call, outcome });
    }

    /// Counts the call in, or refuses it with `limited` (design D8).
    fn admit(&self, op: &str) -> Result<Held, OpError> {
        let now = self.clock.now();
        let mut counters = lock(&self.counters);
        let held = if op.starts_with("net.") {
            if counters.net >= MAX_NET_IN_FLIGHT {
                return Err(limited(format!("{MAX_NET_IN_FLIGHT} network calls are already running for this show")));
            }
            counters.net += 1;
            Held::Net
        } else if op == "exec.run" {
            while counters.exec_started.front().is_some_and(|t| now.0 - t.0 >= 60.0) {
                counters.exec_started.pop_front();
            }
            if counters.exec_running >= MAX_EXEC_RUNNING {
                return Err(limited(format!("{MAX_EXEC_RUNNING} programs are already running for this show")));
            }
            if counters.exec_started.len() >= usize::try_from(MAX_EXEC_PER_MINUTE).unwrap_or(usize::MAX) {
                return Err(limited(format!("this show started {MAX_EXEC_PER_MINUTE} programs in the last minute")));
            }
            counters.exec_running += 1;
            counters.exec_started.push_back(now);
            Held::Exec
        } else {
            Held::Neither
        };
        counters.in_flight += 1;
        Ok(held)
    }

    fn release(&self, held: Held) {
        let mut counters = lock(&self.counters);
        match held {
            Held::Net => counters.net = counters.net.saturating_sub(1),
            Held::Exec => counters.exec_running = counters.exec_running.saturating_sub(1),
            Held::Neither => {}
        }
        counters.in_flight = counters.in_flight.saturating_sub(1);
        drop(counters);
        self.returned.notify_all();
    }

    /// Who a call acts as and with what grants (design D7): the caller of a manual event with its own grants, else
    /// the show's session, else Omuzaki itself with the show's grants.
    fn acting(&self, call: &OpRequest) -> (Actor, Grants) {
        if let Some(actor) = &call.acting {
            let grants = match actor {
                Actor::Person { .. } => Grants::person(),
                _ => self
                    .sessions
                    .as_ref()
                    .and_then(Weak::upgrade)
                    .and_then(|s| s.session_of(actor))
                    .map_or_else(|| Grants::of([Scope::Read]), |s| s.grants),
            };
            return (actor.clone(), grants);
        }
        let ceiling = lock(&self.ceiling).clone();
        match self.session.get() {
            Some(session) => (session.actor.clone(), ceiling),
            None => (Actor::System, ceiling),
        }
    }

    fn call(&self, call: &OpRequest) -> Result<Option<Value>, OpError> {
        let runner = self.runner.as_ref().and_then(Weak::upgrade).ok_or_else(|| {
            OpError::unsupported("the automation engine has no op runner")
                .hint("the app attaches its runner at startup")
        })?;
        let (actor, grants) = self.acting(call);
        let request = Request {
            op: call.op.clone(),
            args: call.args.clone(),
            mode: Mode::Apply,
            actor,
            grants,
            cause: Some(Cause { kind: CauseKind::Rule, text: Some(call.cause.clone()) }),
            tx: None,
            base: None,
        };
        runner.run(request).map(|outcome| outcome.plan.output)
    }

    /// Pauses (`true`) or resumes the show's session, as its person. A session already in that state (or ended)
    /// refuses, which changes nothing.
    /// New grants for the show's calls, and for its session when it has one.
    fn set_ceiling(&self, grants: Grants) {
        *lock(&self.ceiling) = grants.clone();
        let (Some(session), Some(sessions)) = (self.session.get(), self.sessions.as_ref().and_then(Weak::upgrade))
        else {
            return;
        };
        // A refusal keeps the session's old grants; the calls still carry only the new ceiling.
        let _ = sessions.set_grants(session.id, grants, &session.on_behalf_of);
    }

    fn pause(&self, on: bool) {
        let (Some(session), Some(sessions)) = (self.session.get(), self.sessions.as_ref().and_then(Weak::upgrade))
        else {
            return;
        };
        let by = &session.on_behalf_of;
        let _ = if on { sessions.pause(session.id, by) } else { sessions.resume(session.id, by) };
    }

    /// Stops: calls not yet started are dropped, and the session ends once the calls in flight are back or after
    /// [`STOP_GRACE_SECONDS`], on a thread of its own. Returns that thread, for tests that wait for the end.
    pub(crate) fn finish(self: &Arc<Self>) -> Option<JoinHandle<()>> {
        self.stopping.store(true, Ordering::SeqCst);
        let me = Arc::clone(self);
        std::thread::Builder::new()
            .name("automation-session-end".to_owned())
            .spawn(move || {
                let grace = Duration::from_secs_f64(STOP_GRACE_SECONDS);
                let counters = lock(&me.counters);
                let (counters, _) = me
                    .returned
                    .wait_timeout_while(counters, grace, |c| c.in_flight > 0)
                    .unwrap_or_else(PoisonError::into_inner);
                drop(counters);
                if let (Some(session), Some(sessions)) =
                    (me.session.get(), me.sessions.as_ref().and_then(Weak::upgrade))
                {
                    // An ended session can't be ended twice; nothing else can fail here that a stop could act on.
                    let _ = sessions.end(session.id, EndReason::Closed);
                }
            })
            .ok()
    }
}
