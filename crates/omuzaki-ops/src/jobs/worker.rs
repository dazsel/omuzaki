//! The job worker: runs `work` off the lock, then `finish` under it, and reports to the manager.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

use omuzaki_doc_api::{DocRead, Value};
use omuzaki_ops_api::{CancelFlag, CancelReason, JobCtx, JobDoc, JobId, JobSignal, OpError, Outcome};

use super::{Manager, Task, cancelled};
use crate::executor::{Env, require};
use crate::sync::lock;

/// The thread body for one job: `work`, then (if it returned a result) `finish`.
pub(super) fn run(mgr: &Arc<Manager>, id: JobId) {
    // Best effort: a platform that refuses still runs the job, at normal priority.
    let _ = super::priority::lower();
    let Some(task) = mgr.task(id) else { return };
    let reading = Reading::default();
    let doc = StateDoc { mgr, id, reading: &reading };
    let signal = Signal { mgr, id, cancel: task.cancel.clone(), reading: &reading };
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
        // A panic in an op must end the job, not strand it: the manager waits for this thread to report.
        catch_unwind(AssertUnwindSafe(|| task.op.work(&ctx)))
    };
    let panicked = result.is_err();
    let work = result.unwrap_or_else(|_| Err(OpError::internal(format!("{} panicked in work", task.meta.name))));
    let proceed = mgr.work_returned(id, panicked);
    if proceed {
        let outcome = work.and_then(|value| {
            catch_unwind(AssertUnwindSafe(|| finish(mgr, id, &task, value)))
                .unwrap_or_else(|_| Err(OpError::internal(format!("{} panicked in finish", task.meta.name))))
        });
        mgr.complete(id, outcome);
    }
    mgr.env.publish();
}

/// Runs the op's `finish` under the lock against the document as it is now, re-checks the op's scope with the grants
/// the job holds, and applies the plan through the path every edit takes: one one-op undo step of the actor's.
fn finish(mgr: &Manager, id: JobId, task: &Task, value: Value) -> Result<Outcome, OpError> {
    let env: &Env = &mgr.env;
    let mut state = lock(&env.state);
    // A shutdown that began while `work` ran must not see the document change after it: the result is discarded.
    // This first look only saves running `finish` for nothing; the check that counts is the one before the apply.
    if *lock(&mgr.shut) {
        return Err(cancelled(id, CancelReason::Shutdown));
    }
    require(&task.grants, &task.actor, task.meta.scope, task.meta.name)?;
    let plan = {
        let ctx = env.op_ctx(&state, &task.actor, &task.grants, false);
        task.op.finish(&ctx, value)?
    };
    // Shutdown may have begun while `finish` ran. The flag stays locked through the apply, so a shutdown either came
    // first (nothing applies) or waits until the edit is in.
    let shut = lock(&mgr.shut);
    if *shut {
        return Err(cancelled(id, CancelReason::Shutdown));
    }
    let record = env.apply(&mut state, &*task.op, (&task.actor, &task.cause, None), &plan)?;
    drop(shut);
    Ok(Outcome { plan, applied: Some(record) })
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

/// The document as `work` sees it: the lock is held for the closure only. The manager's lock is taken after the
/// document's nowhere, so the closure's signals are deferred (progress) or refused (spend) while it runs.
struct StateDoc<'a> {
    mgr: &'a Arc<Manager>,
    id: JobId,
    reading: &'a Reading,
}

impl JobDoc for StateDoc<'_> {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        {
            let state = lock(&self.mgr.env.state);
            *lock(&self.reading.thread) = Some(std::thread::current().id());
            f(state.doc());
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
