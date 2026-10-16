//! The job worker: runs `work` off the lock, then `finish` under it, and reports to the manager.
//!
//! A driver job's `work` edits through [`JobCtx::call`]: each inner call takes the lock for that call only, as the
//! job's actor with its grants, inside the job's transaction (ADR 0036 decision 10). After the finish, the driver's
//! own transaction is committed (one undo step); on an error, a cancel or a cap it is rolled back, and a joined
//! caller's transaction loses only the job's own edits.

use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use omuzaki_doc_api::{DocRead, Value};
use omuzaki_ops_api::{
    CancelFlag, CancelReason, ErrorCode, ErrorDetail, JobCalls, JobCtx, JobDoc, JobId, JobSignal, JobState, Mode,
    OpError, OpKind, Outcome, Request,
};

use super::{DriverTx, Manager, Task, cancelled, journal_end};
use crate::executor::{Env, require};
use crate::gate::Allowance;
use crate::state::State;
use crate::sync::lock;

/// The thread body for one job: `work`, then (if it returned a result) `finish`.
pub(super) fn run(mgr: &Arc<Manager>, id: JobId) {
    // Best effort: a platform that refuses still runs the job, at normal priority.
    let _ = super::priority::lower();
    let Some(task) = mgr.task(id) else { return };
    let reading = Reading::default();
    let doc = StateDoc { mgr, id, reading: &reading };
    let signal = Signal { mgr, id, cancel: task.cancel.clone(), reading: &reading };
    let calls = Calls { mgr, task: &task };
    let result = {
        let mut ctx = JobCtx::new(id, &task.actor, &doc, &signal)
            .with_services(&mgr.env.services)
            .with_grants(&task.grants)
            .with_reserved_usd_micros(task.reserved);
        if let Some(dir) = &mgr.env.project_dir {
            ctx = ctx.with_project_dir(dir);
        }
        if let Some(output) = &task.plan_output {
            ctx = ctx.with_plan_output(output);
        }
        if task.driver.is_some() {
            ctx = ctx.with_calls(&calls);
        }
        // A panic in an op must end the job, not strand it: the manager waits for this thread to report.
        catch_unwind(AssertUnwindSafe(|| task.op.work(&ctx)))
    };
    let panicked = result.is_err();
    let work = result.unwrap_or_else(|_| Err(OpError::internal(format!("{} panicked in work", task.meta.name))));
    let proceed = mgr.work_returned(id, panicked);
    if proceed {
        let finished = work.and_then(|value| {
            catch_unwind(AssertUnwindSafe(|| finish(mgr, id, &task, value)))
                .unwrap_or_else(|_| Err(OpError::internal(format!("{} panicked in finish", task.meta.name))))
        });
        let (outcome, journalled) = match finished {
            Ok(outcome) => (Ok(outcome), true),
            Err(error) => {
                let error = match task.driver {
                    Some(driver) => undo_driver(&mgr.env, &task.actor, id, driver, error),
                    None => error,
                };
                (Err(error), false)
            }
        };
        mgr.complete(id, outcome, journalled);
    }
    mgr.env.publish();
}

/// Runs the op's `finish` under the lock against the document as it is now, re-checks the op's scope with the grants
/// the job holds, and applies the plan through the path every edit takes: a plain job's one-op undo step, or into a
/// driver's transaction, which is then committed (when the driver began it). The job's end is journalled in the same
/// lock hold, right after the record it names (#1602).
///
/// A finish whose session is paused is held (`JobState::Held`) until the session resumes, then applies against the
/// document and the grants of that moment; a stop or an end discards it.
fn finish(mgr: &Manager, id: JobId, task: &Task, value: Value) -> Result<Outcome, OpError> {
    let env: &Env = &mgr.env;
    loop {
        // A stop or a session end discards the finish (`ops-api.md` §8.3). Asked with no lock held: the manager's lock
        // is never taken under the document's.
        if let Some(reason) = mgr.cancelled_for(id).filter(|r| discards(*r)) {
            return Err(cancelled(id, reason));
        }
        let mut state = lock(&env.state);
        // A shutdown that began while `work` ran must not see the document change after it: the result is discarded.
        // This first look only saves running `finish` for nothing; the check that counts is the one before the apply.
        if *lock(&mgr.shut) {
            return Err(cancelled(id, CancelReason::Shutdown));
        }
        match state.sessions.admit(&task.actor, &task.grants, env.clock.now()) {
            Err(error) if error.code == ErrorCode::Waiting => {
                drop(state);
                mgr.hold(id);
                mgr.wait_tick();
            }
            // The session may have been narrowed while `work` ran: what it holds now decides, under the lock that
            // applies the result.
            other => {
                let grants = other?;
                return apply_finish(mgr, &mut state, id, task, value, &grants);
            }
        }
    }
}

/// Whether a cancel for `reason` discards a job's result rather than letting it apply what it finished.
pub(super) fn discards(reason: CancelReason) -> bool {
    matches!(reason, CancelReason::Stop | CancelReason::SessionEnded | CancelReason::Shutdown)
}

fn apply_finish(
    mgr: &Manager,
    state: &mut State,
    id: JobId,
    task: &Task,
    value: Value,
    grants: &omuzaki_ops_api::Grants,
) -> Result<Outcome, OpError> {
    let env = &mgr.env;
    require(grants, &task.actor, task.meta.scope, task.meta.name)?;
    let plan = {
        let ctx = env.op_ctx(state, &task.actor, grants, false);
        task.op.finish(&ctx, value)?
    };
    // Shutdown may have begun while `finish` ran. The flag stays locked through the apply, so a shutdown either came
    // first (nothing applies) or waits until the edit is in.
    let shut = lock(&mgr.shut);
    if *shut {
        return Err(cancelled(id, CancelReason::Shutdown));
    }
    let tx = task.driver.map(|d| d.tx);
    let applied = if plan.patch.is_empty() && task.driver.is_some() {
        // A driver's finish that changes nothing adds no record to its transaction.
        None
    } else {
        Some(env.apply(state, &*task.op, (&task.actor, &task.cause, tx), &plan)?)
    };
    if let Some(DriverTx { tx, joined: false }) = task.driver {
        state.commit(tx, &task.actor)?;
    }
    drop(shut);
    let done_tx = tx
        .or_else(|| applied.as_ref().map(|r| r.tx))
        .ok_or_else(|| OpError::internal("a job's finish applied nothing"))?;
    let mut info = task.info.clone();
    info.state = JobState::Done { tx: done_tx, seq: applied.as_ref().map(|r| r.seq) };
    journal_end(env, state, &info);
    let outcome = Outcome::new(plan);
    Ok(match applied {
        Some(record) => outcome.with_applied(record),
        None => outcome,
    })
}

/// Undoes what driver job `job` of `actor` did after its error, cancel or cap, and returns the error the job ends
/// with: its own transaction is rolled back (or, if that conflicts, kept as "<label> (unfinished)", as a session end
/// does); a joined caller's transaction loses only the job's own edits, all or nothing, and when that cannot be done
/// (an `Unreverted` conflict, a failed effect call) that error replaces the job's, since the edits stayed.
pub(super) fn undo_driver(
    env: &Env,
    actor: &omuzaki_ops_api::Actor,
    job: JobId,
    driver: DriverTx,
    error: OpError,
) -> OpError {
    let mut state = lock(&env.state);
    let at = env.clock.now();
    let grants = Env::own_grants(&mut state, actor, at);
    let result = if driver.joined {
        env.revert_driver(&mut state, driver.tx, job, actor, &grants)
    } else {
        match env.rollback_tx(&mut state, driver.tx, actor, &grants) {
            Ok(()) => Ok(()),
            // Kept, not lost: nobody could ever close it otherwise.
            Err(_) => state.commit_as_is(driver.tx, actor, " (unfinished)").map(drop),
        }
    };
    drop(state);
    env.publish();
    match (driver.joined, result) {
        (true, Err(retained)) => retained,
        _ => error,
    }
}

/// A driver job's inner calls (ADR 0036 decision 10).
struct Calls<'a> {
    mgr: &'a Arc<Manager>,
    task: &'a Task,
}

impl JobCalls for Calls<'_> {
    fn call(&self, op: &str, args: Value, mode: Mode) -> Result<Outcome, OpError> {
        let Some(driver) = self.task.driver else {
            return Err(OpError::unsupported("only a driver job makes inner calls"));
        };
        let env = &self.mgr.env;
        let id = self.task.info.id;
        let registration = env.ops.find(op)?;
        if (registration.meta)().kind == OpKind::Job {
            return Err(OpError::invalid("op", format!("a driver job cannot start the job {op}"))
                .hint("call edits and queries only: a job inside a job could wait on its own lane"));
        }
        let inner_op = (registration.decode)(args.clone())?;
        loop {
            if omuzaki_ops_api::Cancel::is_cancelled(&self.task.cancel) {
                let reason = self.mgr.cancelled_for(id).unwrap_or(CancelReason::Requested);
                return Err(cancelled(id, reason));
            }
            let req = Request {
                op: op.to_owned(),
                args: args.clone(),
                mode,
                actor: self.task.actor.clone(),
                grants: self.task.grants.clone(),
                cause: self.task.cause.clone(),
                tx: Some(driver.tx),
                base: None,
            };
            let allow = Allowance { driver: Some(id), ..Allowance::default() };
            let result = env.run_locked(&mut lock(&env.state), &*inner_op, req, &allow);
            env.publish();
            match result {
                // A paused session's inner call waits, with the transaction open (decisions 3 to 7).
                Err(error) if matches!(error.detail.as_deref(), Some(ErrorDetail::Paused { .. })) => {
                    self.mgr.wait_tick();
                }
                other => return other,
            }
        }
    }
}

/// Which thread is inside [`JobCtx::read`] (the document lock held), and the progress it reported meanwhile.
#[derive(Default)]
struct Reading {
    thread: Mutex<Option<ThreadId>>,
    progress: Mutex<Option<(String, f32)>>,
}

impl Reading {
    /// Whether the calling thread holds the document lock for a read.
    fn here(&self) -> bool {
        *lock(&self.thread) == Some(std::thread::current().id())
    }
}

thread_local! {
    /// The document an outer [`JobCtx::read`] on this thread holds, as a pointer to the reference its closure got, and
    /// the reader it belongs to: a nested read of the same job is served from it instead of locking again (#1593).
    static HELD: Cell<(*const (), *const ())> = const { Cell::new((std::ptr::null(), std::ptr::null())) };
}

/// Clears [`HELD`] when the outer read ends, by return or by panic, so a later read never sees a stale pointer.
struct Release;

impl Drop for Release {
    fn drop(&mut self) {
        HELD.with(|held| held.set((std::ptr::null(), std::ptr::null())));
    }
}

/// The document as `work` sees it: the lock is held for the closure only. The manager's lock is taken after the
/// document's nowhere, so the closure's signals are deferred (progress) or refused (spend) while it runs.
struct StateDoc<'a> {
    mgr: &'a Arc<Manager>,
    id: JobId,
    reading: &'a Reading,
}

impl JobDoc for StateDoc<'_> {
    #[allow(unsafe_code)] // a nested read reuses the outer read's reference; see the SAFETY note
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        let me: *const () = std::ptr::from_ref(self).cast();
        let (held, owner) = HELD.with(Cell::get);
        if !held.is_null() && owner == me && self.reading.here() {
            // SAFETY: `held` was set below by the outer `read` of this same reader on this thread, to the address of
            // the `&dyn DocRead` local it passes to its closure; that frame is still running (this call is inside its
            // closure) and holds the document lock, and `Release` clears `held` before that local goes away.
            let doc: &&dyn DocRead = unsafe { &*held.cast::<&dyn DocRead>() };
            f(*doc);
            return;
        }
        {
            let state = lock(&self.mgr.env.state);
            *lock(&self.reading.thread) = Some(std::thread::current().id());
            let doc: &dyn DocRead = state.doc();
            HELD.with(|cell| cell.set((std::ptr::from_ref(&doc).cast(), me)));
            let release = Release;
            f(doc);
            drop(release);
            *lock(&self.reading.thread) = None;
        }
        // The lock is released: the latest progress the closure reported goes out now.
        let pending = lock(&self.reading.progress).take();
        if let Some((stage, fraction)) = pending {
            self.mgr.progress(self.id, &stage, fraction);
        }
    }
}

/// What a running job reports to the manager.
struct Signal<'a> {
    mgr: &'a Arc<Manager>,
    id: JobId,
    cancel: CancelFlag,
    reading: &'a Reading,
}

impl JobSignal for Signal<'_> {
    fn progress(&self, stage: &str, fraction: f32) {
        if self.reading.here() {
            // The document lock is held, and the manager's lock must never be taken under it.
            *lock(&self.reading.progress) = Some((stage.to_owned(), fraction));
        } else {
            self.mgr.progress(self.id, stage, fraction);
        }
    }

    fn is_cancelled(&self) -> bool {
        omuzaki_ops_api::Cancel::is_cancelled(&self.cancel)
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.cancel.clone()
    }

    fn spend(&self, usd_micros: u64) -> Result<(), OpError> {
        if self.reading.here() {
            return Err(OpError::invalid("spend", "money cannot be reported inside JobCtx::read")
                .hint("copy what you need out of the document, then call spend"));
        }
        self.mgr.spend(self.id, usd_micros)
    }
}
