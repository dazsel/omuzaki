//! The job worker: runs `work` off the lock, then `finish` under it, and reports to the manager.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};

use omuzaki_doc_api::{DocRead, Value};
use omuzaki_ops_api::{CancelFlag, JobCtx, JobDoc, JobId, JobSignal, OpError, Outcome};

use super::{Manager, Task};
use crate::executor::{Env, require};
use crate::state::State;
use crate::sync::lock;

/// The thread body for one job: `work`, then (if it returned a result) `finish`.
pub(super) fn run(mgr: &Arc<Manager>, id: JobId) {
    // Best effort: a platform that refuses still runs the job, at normal priority.
    let _ = super::priority::lower();
    let Some(task) = mgr.task(id) else { return };
    let doc = StateDoc(&mgr.env.state);
    let signal = Signal { mgr, id, cancel: task.cancel.clone() };
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
            catch_unwind(AssertUnwindSafe(|| finish(&mgr.env, &task, value)))
                .unwrap_or_else(|_| Err(OpError::internal(format!("{} panicked in finish", task.meta.name))))
        });
        mgr.complete(id, outcome);
    }
    mgr.env.publish();
}

/// Runs the op's `finish` under the lock against the document as it is now, re-checks the op's scope with the grants
/// the job holds, and applies the plan through the path every edit takes: one one-op undo step of the actor's.
fn finish(env: &Env, task: &Task, value: Value) -> Result<Outcome, OpError> {
    let mut state = lock(&env.state);
    require(&task.grants, &task.actor, task.meta.scope, task.meta.name)?;
    let plan = {
        let ctx = env.op_ctx(&state, &task.actor, &task.grants, false);
        task.op.finish(&ctx, value)?
    };
    let record = env.apply(&mut state, &*task.op, (&task.actor, &task.cause, None), &plan)?;
    Ok(Outcome { plan, applied: Some(record) })
}

/// The document as `work` sees it: the lock is held for the closure only.
struct StateDoc<'a>(&'a Mutex<State>);

impl JobDoc for StateDoc<'_> {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(lock(self.0).doc());
    }
}

/// What a running job reports to the manager.
struct Signal<'a> {
    mgr: &'a Arc<Manager>,
    id: JobId,
    cancel: CancelFlag,
}

impl JobSignal for Signal<'_> {
    fn progress(&self, stage: &str, fraction: f32) {
        self.mgr.progress(self.id, stage, fraction);
    }

    fn is_cancelled(&self) -> bool {
        omuzaki_ops_api::Cancel::is_cancelled(&self.cancel)
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.cancel.clone()
    }

    fn spend(&self, usd_micros: u64) -> Result<(), OpError> {
        self.mgr.spend(self.id, usd_micros)
    }
}
