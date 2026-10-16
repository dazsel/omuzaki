//! The job manager (ADR 0036): plain jobs run plan, work and finish; lanes, caps, cancel and the money reservation.
//!
//! One state machine under one mutex, `Inner`: the jobs, the lanes with their queues, and the ledger. The lock order
//! is manager, then document state (to queue an event); the document lock is never held while the manager's is
//! taken, and neither is held across `work`, an op callback (but the start journal, which must not block) or an event
//! send (events are queued under the lock and delivered after it is released). Time reads the executor's clock;
//! [`Manager::poll`] enforces the deadlines.

mod lanes;
mod priority;
mod reservation;
mod worker;

use std::collections::{BTreeMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::Value;
use omuzaki_history_api::HistoryEntry;
use omuzaki_ops_api::{
    Actor, CancelFlag, CancelReason, Cause, CostHint, DynOp, ErrorCode, ErrorDetail, Event, Grants, JobId, JobInfo,
    JobShape, JobState, Lane, OpError, OpMeta, Outcome, Plan, Request, SessionId, TxId,
};

use self::lanes::{Lanes, Refusal};
use self::reservation::Ledger;
use crate::executor::Env;
use crate::sync::lock;

/// How long a cancelled job may keep running before it is ended anyway and its thread detached.
const GRACE_SECS: f64 = 10.0;
/// Progress events per second per job, at most.
const PROGRESS_INTERVAL: f64 = 0.1;
/// Ended jobs `jobs()` still lists.
const KEEP_ENDED: usize = 100;
/// Jobs one agent session may run at once, and have waiting (ADR 0036 decision 6).
const SESSION_RUNNING: usize = 4;
const SESSION_QUEUED: usize = 16;
/// How often the supervisor thread checks deadlines against the clock, and a timed `wait` re-reads it, in wall time.
const SUPERVISOR_TICK: Duration = Duration::from_millis(50);

#[derive(Copy, Clone, PartialEq, Eq)]
enum Phase {
    Queued,
    /// A thread is running `work`.
    Working,
    /// `work` returned; its thread is running `finish`.
    Finishing,
    Ended,
}

#[derive(Clone)]
struct Cancelling {
    by: Option<Actor>,
    reason: CancelReason,
    /// When the job is ended anyway, on the executor's clock.
    grace_until: f64,
}

struct Job {
    info: JobInfo,
    phase: Phase,
    op: Arc<dyn DynOp>,
    meta: OpMeta,
    grants: Grants,
    cause: Option<Cause>,
    plan_output: Option<Value>,
    /// The request's arguments, for the start journal.
    args: Value,
    cancel: CancelFlag,
    cancelling: Option<Cancelling>,
    /// Micro-dollars held for this job, and how much `work` has reported spending.
    reserved: u64,
    spent: u64,
    settled: bool,
    /// Seconds `work` may run, and the clock time it must end by.
    cap_secs: f64,
    deadline: f64,
    last_progress: Option<f64>,
    /// A thread is still inside `work` (it may outlive the job's state).
    thread_live: bool,
    /// Set once the thread is given up: whether the lane lent a replacement (freed the slot) for it.
    detached: Option<bool>,
    outcome: Option<Result<Outcome, OpError>>,
    /// Callers blocked on this job (or, for `Runner::run`, registered at start): the entry outlives the bounded
    /// history of ended jobs until the last of them has read it.
    waiters: usize,
    /// A driver job's transaction (#1092).
    driver: Option<DriverTx>,
    /// Its end was journalled with its finish, under the document lock, so [`Manager::end`] does not again.
    end_journalled: bool,
}

/// A driver job's transaction: its own, begun at start and committed after its finish, or the caller's, which it
/// joined and never begins, commits nor rolls back (ADR 0036 decision 10).
#[derive(Copy, Clone, Debug)]
pub(crate) struct DriverTx {
    pub(crate) tx: TxId,
    pub(crate) joined: bool,
}

/// What a worker thread needs to run a job, copied out so no lock is held while it runs.
pub(super) struct Task {
    pub(super) info: JobInfo,
    pub(super) driver: Option<DriverTx>,
    pub(super) op: Arc<dyn DynOp>,
    pub(super) meta: OpMeta,
    pub(super) actor: Actor,
    pub(super) grants: Grants,
    pub(super) cause: Option<Cause>,
    pub(super) plan_output: Option<Value>,
    pub(super) reserved: u64,
    pub(super) cancel: CancelFlag,
}

struct Inner {
    jobs: BTreeMap<JobId, Job>,
    next_id: u64,
    lanes: Lanes,
    ledger: Ledger,
    ended: VecDeque<JobId>,
    closed: bool,
}

/// Starts, runs, cancels and settles jobs for one executor.
pub(crate) struct Manager {
    env: Env,
    inner: Mutex<Inner>,
    /// Signalled on every job end and every returned thread.
    changed: Condvar,
    supervisor: AtomicBool,
    /// Set when shutdown begins. A `finish` holds it from its last check through its apply, so a shutdown lands
    /// before the check (the result is discarded) or after the apply, never between. A leaf: nothing is locked under it.
    shut: Mutex<bool>,
}

impl Manager {
    pub(crate) fn new(env: Env, budget_usd_micros: u64) -> Arc<Self> {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let inner = Inner {
            jobs: BTreeMap::new(),
            // Ids continue after the highest one the journal recorded (#1602): `JobId` is never reused.
            next_id: env.last_job_id,
            lanes: Lanes::new(cores),
            ledger: Ledger::new(budget_usd_micros),
            ended: VecDeque::new(),
            closed: false,
        };
        Arc::new(Self {
            env,
            inner: Mutex::new(inner),
            changed: Condvar::new(),
            supervisor: AtomicBool::new(false),
            shut: Mutex::new(false),
        })
    }

    fn now(&self) -> f64 {
        self.env.clock.now().0
    }

    /// Whether this manager must not be replaced: once a job was admitted it owns jobs (or their ids and money
    /// history), and once it is shut down a fresh manager would quietly take new jobs again.
    pub(crate) fn committed(&self) -> bool {
        let inner = lock(&self.inner);
        inner.next_id > self.env.last_job_id || inner.closed
    }

    /// Queues `JobChanged` for `job`'s current snapshot and wakes waiters. Called with the manager lock held.
    fn announce(&self, job: &Job) {
        lock(&self.env.state).queue_event(Event::job_changed(job.info.clone()));
        self.changed.notify_all();
    }

    /// Admits a planned job: checks its session's job caps and budget, reserves its money, opens a driver job's
    /// transaction (or joins the caller's), queues it and starts it if its lane has room. An `awaited` job is
    /// registered for [`outcome`](Self::outcome) from the start, so its result is kept however many jobs end first.
    pub(crate) fn start(
        self: &Arc<Self>,
        op: Arc<dyn DynOp>,
        req: Request,
        plan: Plan,
        awaited: bool,
        over_budget: bool,
    ) -> Result<JobInfo, OpError> {
        let meta = op.meta();
        let driving = op.job_shape() == JobShape::Driver;
        // Driver jobs edit through inner calls on the CPU lane (ADR 0036 decision 10).
        let lane = if driving { Lane::Cpu } else { lanes::lane_of(&meta, plan.cost.as_ref()) };
        let reserved = reserve_micros(plan.cost.as_ref());
        let started = {
            let mut inner = lock(&self.inner);
            if inner.closed {
                return Err(unavailable("the job manager is shut down", "start the job after the project reopens"));
            }
            // Whose job this is, and whether that session may still start one, under both locks (manager, then
            // document, the order `announce` takes), so two starts cannot both pass a session check only one fits.
            let mut state = lock(&self.env.state);
            state.sessions.check_active(&req.actor)?;
            let owner = state.sessions.session_ref(&req.actor);
            let (session, on_behalf_of) = owner.map_or((None, None), |(id, person)| (Some(id), Some(person)));
            if let Some(session) = session {
                self.session_caps(&inner, session)?;
                let held = inner
                    .jobs
                    .values()
                    .filter(|j| j.info.session == Some(session) && !j.settled)
                    .map(|j| j.reserved)
                    .sum();
                let index = state.sessions.index(session)?;
                let refused =
                    (!over_budget).then(|| self.env.job_budget(&mut state, index, &*op, &req, (reserved, held)));
                if let Some(refusal) = refused.flatten() {
                    drop(state);
                    drop(inner);
                    self.env.publish();
                    return Err(refusal);
                }
            }
            let id = JobId(inner.next_id + 1);
            match inner.lanes.admit(lane, id) {
                Ok(()) => {}
                Err(Refusal::QueueFull) => {
                    return Err(limited(
                        "256 jobs are already queued",
                        "wait for a job to start, then retry",
                        "queued_jobs=256",
                    ));
                }
                Err(Refusal::LaneExhausted) => {
                    return Err(unavailable(
                        "the job lane has no thread to spare: cancelled jobs are still running",
                        "retry once a cancelled job has returned",
                    ));
                }
            }
            if !inner.ledger.reserve(reserved) {
                inner.lanes.unqueue(lane, id);
                return Err(limited(
                    "the job's estimated cost does not fit the money left",
                    "wait for running jobs to settle, or run a cheaper job",
                    "job_budget",
                ));
            }
            inner.next_id += 1;
            let now = self.now();
            let label = if driving { meta.summary.to_owned() } else { meta.name.to_owned() };
            let driver = driving.then(|| match req.tx {
                Some(tx) => DriverTx { tx, joined: true },
                None => DriverTx { tx: state.begin(&label, &req.actor, Seconds(now)), joined: false },
            });
            drop(state);
            let job = Job {
                info: JobInfo {
                    id,
                    op: meta.name.to_owned(),
                    label,
                    actor: req.actor.clone(),
                    on_behalf_of,
                    session,
                    cause: req.cause.clone(),
                    lane,
                    state: JobState::Queued,
                    stage: None,
                    fraction: 0.0,
                    estimate: plan.cost.clone(),
                    spent_usd_micros: 0,
                    queued_at: Seconds(now),
                    started_at: None,
                    ended_at: None,
                },
                phase: Phase::Queued,
                op,
                meta,
                grants: req.grants,
                cause: req.cause,
                plan_output: plan.output,
                args: req.args,
                cancel: CancelFlag::new(),
                cancelling: None,
                reserved,
                spent: 0,
                settled: false,
                cap_secs: time_cap(plan.cost.as_ref()),
                deadline: f64::INFINITY,
                last_progress: None,
                thread_live: false,
                detached: None,
                outcome: None,
                waiters: usize::from(awaited),
                driver,
                end_journalled: false,
            };
            // Admission is recorded before anyone sees the job, so an acknowledged start, queued or running, is
            // durable. A refused record fails the job before it can run.
            let recorded = self.record_start(&job);
            inner.jobs.insert(id, job);
            match recorded {
                Ok(()) => {
                    if let Some(job) = inner.jobs.get(&id) {
                        self.announce(job);
                    }
                    self.dispatch(&mut inner);
                }
                Err(error) => self.fail_start(&mut inner, id, error),
            }
            inner.jobs.get(&id).map(|job| job.info.clone())
        };
        self.env.publish();
        self.ensure_supervisor();
        started.ok_or_else(|| OpError::internal("a started job vanished"))
    }

    /// Refuses a job of `session` past the session's caps: 4 running at once, 16 waiting (ADR 0036 decision 6).
    fn session_caps(&self, inner: &Inner, session: SessionId) -> Result<(), OpError> {
        let mine = || inner.jobs.values().filter(|j| j.info.session == Some(session));
        let running = mine().filter(|j| matches!(j.phase, Phase::Working | Phase::Finishing)).count();
        let queued = mine().filter(|j| j.phase == Phase::Queued).count();
        if running >= SESSION_RUNNING {
            return Err(limited(
                &format!("this session already runs {SESSION_RUNNING} jobs"),
                "wait for one to end, then retry",
                &format!("session_running_jobs = {SESSION_RUNNING}"),
            ));
        }
        if queued >= SESSION_QUEUED {
            return Err(limited(
                &format!("this session already has {SESSION_QUEUED} jobs waiting"),
                "wait for one to start, then retry",
                &format!("session_queued_jobs = {SESSION_QUEUED}"),
            ));
        }
        Ok(())
    }

    /// Starts every queued job whose lane has a free slot. A job whose session is paused does not start, nor one whose
    /// session already runs [`SESSION_RUNNING`] jobs: it stays at the head of its queue until that changes.
    fn dispatch(self: &Arc<Self>, inner: &mut Inner) {
        let mut waiting = Vec::new();
        while let Some((lane, id)) = inner.lanes.next_ready() {
            let now = self.now();
            let Some(session) = inner.jobs.get(&id).map(|job| job.info.session) else {
                inner.lanes.finished(lane);
                continue;
            };
            let full = session.is_some_and(|s| {
                inner.jobs.values().filter(|j| j.info.session == Some(s) && j.phase == Phase::Working).count()
                    >= SESSION_RUNNING
            });
            // The session is re-admitted and the job marked running under one hold of the state lock, so a pause lands
            // either before (the job waits and `work` has no effect) or after (a running job, whose finish is held).
            // The grants are narrowed again: roots revoked while the job waited are not readable by `work`.
            let started = {
                let mut state = lock(&self.env.state);
                let Some(job) = inner.jobs.get_mut(&id) else {
                    inner.lanes.finished(lane);
                    continue;
                };
                match state.sessions.admit(&job.info.actor, &job.grants, Seconds(now)) {
                    Err(error) if error.code == ErrorCode::Waiting => None,
                    Ok(_) if full => None,
                    // The lane slot `next_ready` handed out is given back by `fail_start`, as for a failed spawn.
                    Err(error) => Some(Err(error)),
                    Ok(grants) => {
                        job.phase = Phase::Working;
                        job.grants = grants;
                        job.thread_live = true;
                        job.info.state = JobState::Running;
                        job.info.started_at = Some(Seconds(now));
                        job.deadline = now + job.cap_secs;
                        Some(Ok(()))
                    }
                }
            };
            match started {
                None => {
                    waiting.push((lane, id));
                    continue;
                }
                Some(Err(error)) => {
                    if let Some(job) = inner.jobs.get_mut(&id) {
                        job.phase = Phase::Working;
                    }
                    self.fail_start(inner, id, error);
                    continue;
                }
                Some(Ok(())) => {}
            }
            let Some(job) = inner.jobs.get(&id) else { continue };
            self.announce(job);
            let mgr = Arc::clone(self);
            let spawned =
                std::thread::Builder::new().name(format!("omuzaki-job-{}", id.0)).spawn(move || worker::run(&mgr, id));
            if let Err(e) = spawned {
                self.fail_start(inner, id, OpError::internal(format!("could not start a job thread: {e}")));
            }
        }
        // Back at the head of their queues, in their order.
        for (lane, id) in waiting.into_iter().rev() {
            inner.lanes.hold_back(lane, id);
        }
    }

    /// Records that `job` started (ADR 0036 decision 7): the host's start journal if it set one, else a `JobStarted`
    /// entry for the journal sink. A panic in the hook is a refusal like an error: it must not strand the job.
    fn record_start(&self, job: &Job) -> Result<(), OpError> {
        if let Some(record) = &self.env.job_started {
            return catch_unwind(AssertUnwindSafe(|| record(&job.info, &job.args)))
                .unwrap_or_else(|_| Err(OpError::internal("the job journal panicked")));
        }
        let state = lock(&self.env.state);
        state.journal.append(|| HistoryEntry::JobStarted {
            job: job.info.id,
            op: job.info.op.clone(),
            args: job.args.clone(),
            actor: job.info.actor.clone(),
            on_behalf_of: job.info.on_behalf_of.clone(),
            at: job.info.queued_at,
        });
        Ok(())
    }

    /// Records that a job ended (#1602), with the state it ended in, once.
    fn record_end(&self, info: &JobInfo) {
        let state = lock(&self.env.state);
        journal_end(&self.env, &state, info);
    }

    /// Fails a job whose `work` never ran: its queue place or lane slot and its whole reservation are given back.
    fn fail_start(&self, inner: &mut Inner, id: JobId, error: OpError) {
        let Some(job) = inner.jobs.get_mut(&id) else { return };
        let lane = job.info.lane;
        if job.phase == Phase::Queued {
            inner.lanes.unqueue(lane, id);
        } else {
            inner.lanes.finished(lane);
        }
        job.thread_live = false;
        settle(&mut inner.ledger, job, 0);
        self.end(inner, id, JobState::Failed { error: error.clone() }, Err(error));
    }

    /// Moves a job to its final state, keeping the outcome for whoever waits on it, and journals the end unless its
    /// finish already did, under the lock that applied it.
    fn end(&self, inner: &mut Inner, id: JobId, state: JobState, outcome: Result<Outcome, OpError>) {
        let now = self.now();
        let Some(job) = inner.jobs.get_mut(&id) else { return };
        job.phase = Phase::Ended;
        job.info.state = state;
        job.info.ended_at = Some(Seconds(now));
        job.outcome = Some(outcome);
        if !job.end_journalled {
            job.end_journalled = true;
            self.record_end(&job.info);
        }
        self.announce(job);
        self.finish_stop(inner, id);
        inner.ended.push_back(id);
        trim(inner);
    }

    /// Step c of a person's Stop (agent-runtime D4): once the last driver job joined to a stopped session's open
    /// transaction has ended (and reverted what it could), that transaction is committed as it stands, labelled
    /// "(stopped)", in any session state.
    fn finish_stop(&self, inner: &Inner, id: JobId) {
        let Some(job) = inner.jobs.get(&id) else { return };
        let (Some(session), Some(DriverTx { tx, joined: true })) = (job.info.session, job.driver) else { return };
        let others = inner.jobs.values().any(|other| {
            other.info.id != id && other.phase != Phase::Ended && other.driver.is_some_and(|d| d.joined && d.tx == tx)
        });
        if others {
            return;
        }
        let mut state = lock(&self.env.state);
        let stopping = state.sessions.index(session).is_ok_and(|index| state.sessions.is_stopping(index));
        if stopping {
            // The transaction may be gone already (its owner closed it): nothing is left to close then.
            drop(state.stop_tx(tx, &job.info.actor));
        }
    }

    /// Asks a job to stop. A queued job ends at once; a running one is told through its flag and ends when `work`
    /// returns or the grace period passes. Idempotent.
    fn cancel_job(&self, inner: &mut Inner, id: JobId, by: Option<Actor>, reason: CancelReason) {
        let now = self.now();
        let Some(job) = inner.jobs.get_mut(&id) else { return };
        match job.phase {
            Phase::Queued => {
                inner.lanes.unqueue(job.info.lane, id);
                settle(&mut inner.ledger, job, 0);
                let error = cancelled(id, reason);
                self.end(inner, id, JobState::Cancelled { by, reason }, Err(error));
            }
            Phase::Working if job.cancelling.is_none() => {
                job.cancel.cancel();
                job.cancelling = Some(Cancelling { by, reason, grace_until: now + GRACE_SECS });
            }
            // Shutdown reaches a job already in `finish` too, so its failure to apply ends it `Cancelled`. And it
            // overrides an earlier reason (`Requested`, `Timeout`, `Budget`): shutdown promises every live job ends
            // cancelled for shutdown, and the snapshot must agree with the error the run returns. The earlier grace
            // deadline stands, so an upgrade never buys the work more time.
            // Shutdown, a stop and a session end reach a job already in `finish` too (one held for a paused session,
            // say), so its result is discarded and it ends `Cancelled`.
            Phase::Working | Phase::Finishing
                if matches!(reason, CancelReason::Shutdown | CancelReason::Stop | CancelReason::SessionEnded) =>
            {
                job.cancel.cancel();
                let grace_until = job.cancelling.as_ref().map_or(now + GRACE_SECS, |c| c.grace_until);
                job.cancelling = Some(Cancelling { by, reason, grace_until });
            }
            // The flag is the one handle work cloned out: flipping it still stops what outlived `work` (a stream), and
            // changes no state.
            Phase::Working | Phase::Finishing | Phase::Ended => job.cancel.cancel(),
        }
    }

    /// Enforces deadlines against the clock: a job past its time cap is cancelled; one still running after its grace
    /// period is ended `Cancelled` and its thread detached.
    pub(crate) fn poll(self: &Arc<Self>) {
        let now = self.now();
        {
            let mut inner = lock(&self.inner);
            let working: Vec<JobId> =
                inner.jobs.iter().filter(|(_, job)| job.phase == Phase::Working).map(|(id, _)| *id).collect();
            for id in working {
                let Some(job) = inner.jobs.get(&id) else { continue };
                match job.cancelling.clone() {
                    Some(c) if now >= c.grace_until => self.detach(&mut inner, id, c),
                    None if now >= job.deadline => self.cancel_job(&mut inner, id, None, CancelReason::Timeout),
                    Some(_) | None => {}
                }
            }
            // A queued job whose session resumed (or ran fewer jobs) since the last look starts now.
            self.dispatch(&mut inner);
            // A timed `wait` measures the executor's clock, which may just have moved, and a held finish rechecks
            // its session.
            self.changed.notify_all();
        }
        self.env.publish();
    }

    /// Ends a job whose work ignored its cancellation, and gives up its thread. The reservation stays held until the
    /// thread returns. A driver's edits are undone now: the thread's later inner calls are refused.
    fn detach(self: &Arc<Self>, inner: &mut Inner, id: JobId, c: Cancelling) {
        let Some(job) = inner.jobs.get_mut(&id) else { return };
        let lent = inner.lanes.detach(job.info.lane);
        job.detached = Some(lent);
        if let Some(driver) = job.driver {
            // The job ends cancelled whatever the undo says; an `Unreverted` conflict is in the caller's transaction for
            // them to see.
            let undone = worker::undo_driver(&self.env, &job.info.actor, id, driver, cancelled(id, c.reason));
            drop(undone);
        }
        self.end(inner, id, JobState::Cancelled { by: c.by, reason: c.reason }, Err(cancelled(id, c.reason)));
        if lent {
            self.dispatch(inner);
        }
    }

    pub(super) fn task(&self, id: JobId) -> Option<Task> {
        let inner = lock(&self.inner);
        let job = inner.jobs.get(&id)?;
        Some(Task {
            info: job.info.clone(),
            driver: job.driver,
            op: Arc::clone(&job.op),
            meta: job.meta,
            actor: job.info.actor.clone(),
            grants: job.grants.clone(),
            cause: job.cause.clone(),
            plan_output: job.plan_output.clone(),
            reserved: job.reserved,
            cancel: job.cancel.clone(),
        })
    }

    /// `work` returned: settle the money. Returns whether the thread should go on to `finish`; it should not when
    /// the job already ended (a detached thread), whose result is discarded.
    pub(super) fn work_returned(self: &Arc<Self>, id: JobId, panicked: bool) -> bool {
        let mut guard = lock(&self.inner);
        let inner = &mut *guard;
        let Some(job) = inner.jobs.get_mut(&id) else { return false };
        // A panic says nothing about what the paid call did, so the whole reservation is charged.
        let spent = if panicked { job.reserved } else { job.spent };
        settle(&mut inner.ledger, job, spent);
        job.thread_live = false;
        let Some(lent) = job.detached else {
            job.phase = Phase::Finishing;
            return true;
        };
        let lane = job.info.lane;
        inner.lanes.returned(lane, lent);
        // The returned thread no longer pins its ended job in the history.
        trim(inner);
        self.dispatch(inner);
        self.changed.notify_all();
        false
    }

    /// `finish` ended: the job's final state follows from its result. `journalled`: the finish already journalled
    /// the end, in the lock hold that applied it.
    pub(super) fn complete(self: &Arc<Self>, id: JobId, result: Result<Outcome, OpError>, journalled: bool) {
        let mut inner = lock(&self.inner);
        if let Some(job) = inner.jobs.get_mut(&id) {
            job.end_journalled |= journalled;
        }
        let Some(job) = inner.jobs.get(&id) else { return };
        let (lane, cancelling, driver) = (job.info.lane, job.cancelling.clone(), job.driver);
        inner.lanes.finished(lane);
        // A driver's run is its transaction, whether or not its finish added a record to it.
        let done = result.as_ref().ok().and_then(|outcome| match outcome.applied.as_ref() {
            Some(record) => Some(JobState::Done { tx: record.tx, seq: Some(record.seq) }),
            None => driver.map(|d| JobState::Done { tx: d.tx, seq: None }),
        });
        let (state, outcome) = match (done, result, cancelling) {
            // The edit is in. Under shutdown too: `finish` holds the shutdown flag through its apply, so the apply
            // came before the shutdown began, and the job's state must not deny an edit the log holds.
            (Some(state), result, _) => (state, result),
            // Shutdown promises every job ends cancelled with its result discarded: whatever `work` or `finish`
            // returned instead, an error or a panic, gives way to it.
            (None, _, Some(c)) if c.reason == CancelReason::Shutdown => {
                (JobState::Cancelled { by: c.by, reason: c.reason }, Err(cancelled(id, c.reason)))
            }
            (None, Ok(_), _) => {
                let error = OpError::internal("a job's finish applied nothing");
                (JobState::Failed { error: error.clone() }, Err(error))
            }
            // A cancelled job whose work stopped with `Cancelled`, or on the money cap with `Limited`, is cancelled,
            // not failed; the run still returns the work's own error.
            (None, Err(error), Some(c)) if matches!(error.code, ErrorCode::Cancelled | ErrorCode::Limited) => {
                let error = if error.code == ErrorCode::Cancelled && error.detail.is_none() {
                    error.with_detail(ErrorDetail::Cancelled { job: id, reason: c.reason })
                } else {
                    error
                };
                (JobState::Cancelled { by: c.by, reason: c.reason }, Err(error))
            }
            (None, Err(error), _) => (JobState::Failed { error: error.clone() }, Err(error)),
        };
        self.end(&mut inner, id, state, outcome);
        self.dispatch(&mut inner);
    }

    pub(super) fn progress(&self, id: JobId, stage: &str, fraction: f32) {
        let now = self.now();
        {
            let mut inner = lock(&self.inner);
            let Some(job) = inner.jobs.get_mut(&id) else { return };
            if job.phase != Phase::Working {
                return;
            }
            let fraction = if fraction.is_nan() { 0.0 } else { fraction.clamp(0.0, 1.0) };
            job.info.stage = Some(stage.to_owned());
            job.info.fraction = fraction;
            // Coalesced: the snapshot always holds the latest, the event stream at most 10 a second.
            if job.last_progress.is_none_or(|at| now - at >= PROGRESS_INTERVAL - 1e-9) {
                job.last_progress = Some(now);
                lock(&self.env.state).queue_event(Event::job_progress(id, stage, fraction));
            }
        }
        self.env.publish();
    }

    /// Records money spent. A late report from a detached thread is charged like any other.
    pub(super) fn spend(&self, id: JobId, usd_micros: u64) -> Result<(), OpError> {
        let result = {
            let mut inner = lock(&self.inner);
            let Some(job) = inner.jobs.get_mut(&id) else { return Err(OpError::not_found("the job is gone")) };
            // An overflowing total is past any cap.
            if let Some(total) = job.spent.checked_add(usd_micros).filter(|total| *total <= job.reserved) {
                job.spent = total;
                job.info.spent_usd_micros = total;
                // A session's job spends the session's money too, and it stays spent whatever becomes of the job.
                if let Some(session) = job.info.session {
                    let at = Seconds(self.now());
                    let mut state = lock(&self.env.state);
                    if let Ok(index) = state.sessions.index(session) {
                        state.sessions.add_spend(index, usd_micros, 0, at);
                        if let Some(snapshot) = state.session(session) {
                            state.queue_event(Event::session_changed(snapshot, omuzaki_ops_api::SessionChange::Spent));
                        }
                    }
                }
                Ok(())
            } else {
                let (spent, cap) = (job.spent, job.reserved);
                self.cancel_job(&mut inner, id, None, CancelReason::Budget);
                Err(limited(
                    &format!("spending {usd_micros} micro-dollars would pass the job's cap of {cap} ({spent} spent)"),
                    "the job's estimate is its money cap: plan a larger estimate to allow more",
                    "job_cost",
                ))
            }
        };
        self.env.publish();
        result
    }

    pub(crate) fn job(&self, id: JobId) -> Option<JobInfo> {
        lock(&self.inner).jobs.get(&id).map(|job| job.info.clone())
    }

    /// The live jobs, then the last [`KEEP_ENDED`] ended ones (#1603). Ended jobs a waiter still pins stay kept for
    /// that waiter, but are not listed past the limit.
    pub(crate) fn list(&self) -> Vec<JobInfo> {
        let inner = lock(&self.inner);
        let recent: std::collections::BTreeSet<JobId> = inner.ended.iter().rev().take(KEEP_ENDED).copied().collect();
        let live = inner.jobs.values().filter(|job| job.phase != Phase::Ended);
        let ended = inner.jobs.values().filter(|job| job.phase == Phase::Ended && recent.contains(&job.info.id));
        live.chain(ended).map(|job| job.info.clone()).collect()
    }

    /// Cancels every live job of `session` for `reason`, returning their ids: the one path a stop, an end, a revoke and
    /// a person's or agent's `cancel_session` take.
    pub(crate) fn cancel_session(&self, session: SessionId, by: Option<&Actor>, reason: CancelReason) -> Vec<JobId> {
        let ids = {
            let mut inner = lock(&self.inner);
            Self::cancel_of(self, &mut inner, session, by, reason)
        };
        self.env.publish();
        ids
    }

    fn cancel_of(&self, inner: &mut Inner, session: SessionId, by: Option<&Actor>, reason: CancelReason) -> Vec<JobId> {
        let live: Vec<JobId> = inner
            .jobs
            .values()
            .filter(|job| job.info.session == Some(session) && job.phase != Phase::Ended)
            .map(|job| job.info.id)
            .collect();
        for id in &live {
            self.cancel_job(inner, *id, by.cloned(), reason);
        }
        live
    }

    /// A person's Stop, step a (agent-runtime D4): under the manager's lock and then the document's, `pause` runs and
    /// every live job of `session` is cancelled for `Stop`, atomically.
    pub(crate) fn stop<T>(
        &self,
        session: SessionId,
        by: &Actor,
        pause: impl FnOnce(&mut crate::state::State) -> Result<T, OpError>,
    ) -> Result<(T, Vec<JobId>), OpError> {
        let done = {
            let mut inner = lock(&self.inner);
            let paused = pause(&mut lock(&self.env.state))?;
            let cancelled = self.cancel_of(&mut inner, session, Some(by), CancelReason::Stop);
            (paused, cancelled)
        };
        self.env.publish();
        Ok(done)
    }

    /// Whether a live driver job of `session` still runs in transaction `tx` (one joined to it): a stop commits the
    /// transaction only once none does.
    pub(crate) fn joins(&self, session: SessionId, tx: TxId) -> bool {
        lock(&self.inner).jobs.values().any(|job| {
            job.info.session == Some(session)
                && job.phase != Phase::Ended
                && job.driver.is_some_and(|d| d.joined && d.tx == tx)
        })
    }

    /// Marks a job whose finish waits for its paused session (`Held`).
    pub(super) fn hold(&self, id: JobId) {
        {
            let mut inner = lock(&self.inner);
            if let Some(job) = inner.jobs.get_mut(&id)
                && job.info.state != JobState::Held
            {
                job.info.state = JobState::Held;
                self.announce(job);
            }
        }
        self.env.publish();
    }

    /// Waits one supervisor tick (or until something changes): how a held finish and a paused driver's inner call
    /// wait for their session, without holding any lock.
    pub(super) fn wait_tick(&self) {
        let inner = lock(&self.inner);
        drop(self.changed.wait_timeout(inner, SUPERVISOR_TICK).unwrap_or_else(PoisonError::into_inner));
    }

    /// The reason a cancelled job was cancelled for, if it was.
    pub(super) fn cancelled_for(&self, id: JobId) -> Option<CancelReason> {
        lock(&self.inner).jobs.get(&id).and_then(|job| job.cancelling.as_ref().map(|c| c.reason))
    }

    /// Cancels `id` for `by`: its own actor or a person (or the system) may.
    pub(crate) fn cancel(&self, id: JobId, by: &Actor) -> Result<JobInfo, OpError> {
        let info = {
            let mut inner = lock(&self.inner);
            let Some(job) = inner.jobs.get(&id) else {
                return Err(OpError::not_found(format!("job {} is unknown", id.0)));
            };
            let may = job.info.actor == *by || matches!(by, Actor::Person { .. } | Actor::System);
            if !may {
                return Err(OpError::denied(format!("job {} belongs to another actor", id.0))
                    .hint("only the job's own actor, or a person, can cancel it"));
            }
            self.cancel_job(&mut inner, id, Some(by.clone()), CancelReason::Requested);
            inner.jobs.get(&id).map(|job| job.info.clone())
        };
        self.env.publish();
        info.ok_or_else(|| OpError::internal("a cancelled job vanished"))
    }

    /// Blocks until the job ends or `timeout` passes on the executor's clock.
    pub(crate) fn wait(&self, id: JobId, timeout: Option<Duration>) -> Result<JobInfo, OpError> {
        let mut inner = lock(&self.inner);
        let Some(job) = inner.jobs.get_mut(&id) else {
            return Err(OpError::not_found(format!("job {} is unknown", id.0)));
        };
        job.waiters += 1;
        let (inner, info) = self.wait_registered(inner, id, timeout);
        release(inner, id);
        self.after_wait(id, info)
    }

    /// What `Runner::run` returns for a job started `awaited`: waits for it, then hands over its outcome.
    pub(crate) fn outcome(&self, id: JobId) -> Result<Outcome, OpError> {
        let inner = lock(&self.inner);
        let (mut inner, info) = self.wait_registered(inner, id, None);
        let taken = inner.jobs.get_mut(&id).and_then(|job| job.outcome.take());
        release(inner, id);
        self.after_wait(id, info)?;
        taken.unwrap_or_else(|| Err(OpError::internal("the job's outcome was already taken")))
    }

    /// Waits on a job this caller is already counted among the waiters of, so it cannot be dropped from the history
    /// meanwhile. Returns the lock and the job's last snapshot (`None` only for a job never known).
    fn wait_registered<'a>(
        &self,
        mut inner: MutexGuard<'a, Inner>,
        id: JobId,
        timeout: Option<Duration>,
    ) -> (MutexGuard<'a, Inner>, Option<JobInfo>) {
        let until = timeout.map(|t| self.now() + t.as_secs_f64());
        loop {
            let Some(job) = inner.jobs.get(&id) else { return (inner, None) };
            if job.phase == Phase::Ended {
                let info = job.info.clone();
                return (inner, Some(info));
            }
            inner = match until {
                None => self.changed.wait(inner).unwrap_or_else(PoisonError::into_inner),
                // Fake time moves without a wall-clock wake-up, so the clock is re-read every tick as well as on
                // every change (`poll` signals one).
                Some(until) if self.now() < until => {
                    self.changed.wait_timeout(inner, SUPERVISOR_TICK).unwrap_or_else(PoisonError::into_inner).0
                }
                Some(_) => {
                    let info = job.info.clone();
                    return (inner, Some(info));
                }
            };
        }
    }

    /// The end of a wait, after the lock is released.
    fn after_wait(&self, id: JobId, info: Option<JobInfo>) -> Result<JobInfo, OpError> {
        // Whoever sees the job ended sees its events too: the worker may not have delivered them yet.
        self.env.publish();
        info.ok_or_else(|| OpError::not_found(format!("job {} is unknown", id.0)))
    }

    /// Blocks until at most `at_most` detached threads are still inside `work`.
    pub(crate) fn wait_detached(&self, at_most: usize) {
        let mut inner = lock(&self.inner);
        while inner.lanes.detached() > at_most {
            inner = self.changed.wait(inner).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Cancels everything, refuses new jobs, and charges every reservation still held in full.
    pub(crate) fn shutdown(&self) -> u64 {
        let charged = {
            let mut guard = lock(&self.inner);
            let inner = &mut *guard;
            inner.closed = true;
            // Under the manager's lock too, so a finish that sees the flag completes into `cancelling` set below.
            *lock(&self.shut) = true;
            let ids: Vec<JobId> = inner.jobs.keys().copied().collect();
            for id in ids {
                self.cancel_job(inner, id, None, CancelReason::Shutdown);
                if let Some(job) = inner.jobs.get_mut(&id)
                    && job.thread_live
                {
                    let reserved = job.reserved;
                    settle(&mut inner.ledger, job, reserved);
                }
            }
            self.changed.notify_all();
            inner.ledger.charged()
        };
        self.env.publish();
        charged
    }

    /// Starts the thread that calls [`poll`](Self::poll) while the manager lives, and after a shutdown until no job is
    /// still running, so every cancelled job reaches its end by its deadline. It holds only a weak handle, so it never
    /// keeps an executor's jobs alive.
    fn ensure_supervisor(self: &Arc<Self>) {
        if self.supervisor.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(self);
        // If the thread cannot start, deadlines are enforced only by explicit `poll_jobs` calls.
        let _ = std::thread::Builder::new().name("omuzaki-job-clock".into()).spawn(move || {
            loop {
                std::thread::sleep(SUPERVISOR_TICK);
                match weak.upgrade() {
                    Some(mgr) if !mgr.idle_after_shutdown() => mgr.poll(),
                    _ => break,
                }
            }
        });
    }
}

impl Manager {
    /// Shut down, with no job left whose deadline the supervisor still has to enforce.
    fn idle_after_shutdown(&self) -> bool {
        let inner = lock(&self.inner);
        inner.closed && inner.jobs.values().all(|job| job.phase != Phase::Working)
    }
}

/// Journals that the job `info` describes ended, under the document lock the caller holds: the host's end journal if
/// it set one, else a `JobEnded` entry for the journal sink. A failing or panicking hook is logged and never strands
/// the job: the job still ends and its money is still settled.
fn journal_end(env: &Env, state: &crate::state::State, info: &JobInfo) {
    match &env.job_ended {
        Some(record) => {
            // The job has ended either way: a journal that cannot take the entry reports it through its own health
            // (`JournalStatus::Failing`), and recovery gives a job with no end entry an `Interrupted` one.
            let recorded = catch_unwind(AssertUnwindSafe(|| record(info)));
            drop(recorded);
        }
        None => state.journal.append(|| HistoryEntry::JobEnded { job: info.id, state: info.state.clone() }),
    }
}

/// Drops one waiter from `id` and trims the history it may have held open.
fn release(mut inner: MutexGuard<'_, Inner>, id: JobId) {
    if let Some(job) = inner.jobs.get_mut(&id) {
        job.waiters = job.waiters.saturating_sub(1);
    }
    trim(&mut inner);
}

/// Keeps the last `KEEP_ENDED` ended jobs listed. A job whose thread is still inside `work` stays (its late `spend`
/// needs the entry), and so does one with a waiter (its outcome is not read yet).
fn trim(inner: &mut Inner) {
    while inner.ended.len() > KEEP_ENDED {
        let Some(at) = inner
            .ended
            .iter()
            .position(|old| inner.jobs.get(old).is_none_or(|job| !job.thread_live && job.waiters == 0))
        else {
            break;
        };
        if let Some(old) = inner.ended.remove(at) {
            inner.jobs.remove(&old);
        }
    }
}

/// Ends a job's reservation once: `spent` is charged and the rest released. A second call does nothing, so a
/// shutdown that already charged in full cannot be charged again.
fn settle(ledger: &mut Ledger, job: &mut Job, spent: u64) {
    if !job.settled {
        job.settled = true;
        ledger.settle(job.reserved, spent);
    }
}

/// Micro-dollars to reserve for `cost.usd`, rounded up; nothing without an estimate.
fn reserve_micros(cost: Option<&CostHint>) -> u64 {
    // Through the shortest decimal form, so the 0.1 a plan wrote is 100,000 micro-dollars and not 100,001.
    let usd = cost.and_then(|c| c.usd).and_then(|usd| usd.to_string().parse::<f64>().ok()).unwrap_or(0.0);
    if usd.is_finite() && usd > 0.0 {
        // `usd` is positive and finite; a huge one saturates, which is the right cap for "more than the budget".
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let micros = (usd * 1e6).ceil() as u64;
        micros
    } else {
        0
    }
}

/// How long `work` may run: four times the estimate, at least 60 s, 30 minutes without one, never over 24 hours.
fn time_cap(cost: Option<&CostHint>) -> f64 {
    let secs = match cost.and_then(|c| c.seconds).map(f64::from) {
        Some(s) if s.is_finite() => (s * 4.0).max(60.0),
        _ => 30.0 * 60.0,
    };
    secs.min(24.0 * 3600.0)
}

fn cancelled(id: JobId, reason: CancelReason) -> OpError {
    OpError::cancelled(id).with_detail(ErrorDetail::Cancelled { job: id, reason })
}

fn limited(message: &str, hint: &str, limit: &str) -> OpError {
    OpError {
        code: ErrorCode::Limited,
        message: message.to_owned(),
        field: None,
        hint: Some(hint.to_owned()),
        detail: Some(Box::new(ErrorDetail::RetryAfter { seconds: 1.0, limit: limit.to_owned() })),
    }
}

fn unavailable(message: &str, hint: &str) -> OpError {
    OpError {
        code: ErrorCode::Unavailable,
        message: message.to_owned(),
        field: None,
        hint: Some(hint.to_owned()),
        detail: None,
    }
}
