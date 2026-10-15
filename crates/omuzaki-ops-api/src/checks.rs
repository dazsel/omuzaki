//! The checks every [`Runner`](crate::Runner) makes, in one copy: the executor (`omuzaki-ops`) and the test runner
//! (`omuzaki-ops-mock`) both call these, so the oracle cannot drift from the product on who may do what, which plans
//! are refused, what the log records and which steps undo reaches.
//!
//! A run checks in this order, and [`plan`] keeps the middle of it in one place:
//! 1. `read`, before the op is looked up ([`require_scope`] with [`Scope::Read`]): plans and conflict messages carry
//!    document values, so nothing runs without it, not even a dry-run.
//! 2. The transaction, which only the runner can check.
//! 3. The op's own scope ([`authorize`]), before the op is asked for a plan.
//! 4. The plan, in a context that carries the request's grants and mode, then the plan itself ([`check_plan`]).
//! 5. A dry-run or a query ([`answers_only`]) ends there. Only an apply serialises the op's arguments for the log
//!    ([`record_args`]), so an op whose `Serialize` fails still answers dry-runs and queries, and is refused
//!    `Denied` rather than `Internal` when the caller lacks its scope.
//!
//! History: an applied op adds [`step_scope`] to its undo step, the step belongs in history only if
//! [`in_history`] says so, undo and redo need every recorded scope ([`require_step_scopes`]), and a step holding an
//! [`Undo::Explicit`] op is refused with [`explicit_refused`].

use omuzaki_doc_api::{Patch, Value};

use crate::{
    Actor, DynOp, Grants, Mode, OpCtx, OpError, OpInfo, OpKind, OpMeta, OpRegistration, Plan, Request, Scope, Undo,
};

/// A human-readable name for an actor, for refusals.
fn who(actor: &Actor) -> String {
    match actor {
        Actor::Person { name } => format!("person {name}"),
        Actor::Agent { id, .. } => format!("agent {id}"),
        Actor::Plugin { id } => format!("plugin {id}"),
        Actor::System => "the system".to_owned(),
    }
}

/// `Denied`: `actor` lacks `scope` for `doing` (for example "run face.set_profile"). The hint names the scope to grant.
pub fn denied(actor: &Actor, doing: &str, scope: Scope) -> OpError {
    OpError::denied(format!("{} may not {doing}: it needs the `{scope}` scope", who(actor)))
        .hint(format!("grant the `{scope}` scope for this session, then retry"))
}

/// `actor` needs `scope` to do `doing`.
///
/// # Errors
/// [`denied`] when `grants` lack `scope`.
pub fn require_scope(grants: &Grants, actor: &Actor, scope: Scope, doing: &str) -> Result<(), OpError> {
    if grants.allows(scope) { Ok(()) } else { Err(denied(actor, doing, scope)) }
}

/// A read that carries no actor (the document, the log, events, jobs) needs `read`; the grants alone say who asks.
///
/// # Errors
/// `Denied` naming `read` when `grants` lack it.
pub fn require_read(grants: &Grants, doing: &str) -> Result<(), OpError> {
    if grants.allows(Scope::Read) {
        Ok(())
    } else {
        Err(OpError::denied(format!("the caller may not {doing}: it needs the `{}` scope", Scope::Read))
            .hint(format!("grant the `{}` scope for this session, then retry", Scope::Read)))
    }
}

/// Checks the op's own scope for a request in `mode`. Applying needs it. A dry-run of an edit or a job needs only
/// `read` (it is how an actor shows what it would ask permission for), but a query's plan *is* its answer, so a
/// dry-run of one is gated like the real thing.
///
/// # Errors
/// [`denied`] when the op's scope is needed and `grants` lack it.
pub fn authorize(grants: &Grants, actor: &Actor, meta: OpMeta, mode: Mode) -> Result<(), OpError> {
    if mode == Mode::Apply || meta.kind == OpKind::Query {
        require_scope(grants, actor, meta.scope, &format!("run {}", meta.name))
    } else {
        Ok(())
    }
}

/// Refuses a plan its op may not return. A query that planned a patch would change the document through an
/// interface everyone assumes is read-only. A job changes the document once, at finish: a plan that already carries a
/// patch would apply twice or not at all, so it is refused in the dry-run too.
///
/// # Errors
/// `Internal`, naming the op, for either.
pub fn check_plan(meta: OpMeta, plan: &Plan) -> Result<(), OpError> {
    if plan.patch.changes.is_empty() {
        return Ok(());
    }
    match meta.kind {
        OpKind::Query => {
            Err(OpError::internal(format!("{} is a query but planned a change to the document", meta.name))
                .hint("this is a bug in the op: queries return an answer and no patch"))
        }
        OpKind::Job => Err(OpError::internal(format!("{} is a job but planned a change to the document", meta.name))
            .hint("this is a bug in the op: a job's plan returns no patch; its `finish` returns the change")),
        OpKind::Edit => Ok(()),
    }
}

/// Authorizes `req` for `op`, plans it and checks the plan: steps 3 and 4 of the [module](self) order. `ctx` is the
/// runner's context over the document; this sets the request's grants (a composite op's
/// [`plan_step`](OpCtx::plan_step) checks them) and its mode on it. The caller has checked `read` and the
/// transaction.
///
/// # Errors
/// As [`authorize`], then whatever the op's `plan` returns, then as [`check_plan`].
pub fn plan<'a>(op: &dyn DynOp, req: &'a Request, ctx: OpCtx<'a>) -> Result<Plan, OpError> {
    let meta = op.meta();
    authorize(&req.grants, &req.actor, meta, req.mode)?;
    let plan = op.plan(&ctx.with_grants(&req.grants).with_dry_run(req.mode == Mode::DryRun))?;
    check_plan(meta, &plan)?;
    Ok(plan)
}

/// Whether the outcome is the plan alone, with nothing applied or logged: a dry-run, or any query.
pub fn answers_only(meta: OpMeta, mode: Mode) -> bool {
    mode == Mode::DryRun || meta.kind == OpKind::Query
}

/// The arguments the op log records: the decoded op serialised back (defaults filled in, aliases resolved), never
/// the request's raw `args`. Call it only on the apply path, after [`plan`] (step 5).
///
/// # Errors
/// `Internal` if the op's own `Serialize` fails.
pub fn record_args(op: &dyn DynOp) -> Result<Value, OpError> {
    op.args()
}

/// What a surface lists for a registered op, `coalesces` included.
pub fn op_info(registration: &OpRegistration) -> OpInfo {
    OpInfo::new((registration.meta)(), (registration.schema)()).with_coalesces((registration.coalesces)())
}

/// The scope an applied op adds to its undo step, which undo and redo then need. An op that changed nothing (a query
/// inside a transaction, a no-op edit) adds none. An [`Undo::Explicit`] op adds its scope even with an empty patch:
/// its effect outside the document still needs reversing, and its scope must still gate the refusal.
pub fn step_scope(meta: OpMeta, patch: &Patch) -> Option<Scope> {
    (!patch.is_empty() || meta.undo == Undo::Explicit).then_some(meta.scope)
}

/// Whether a finished step belongs in its owner's history (and so clears their redo). `patch` is the step's whole
/// patch and `explicit` whether it holds an [`Undo::Explicit`] op. A no-op step stays out, so running a query or an
/// idempotent edit never throws redo away; an explicit step stays in even with an empty patch, so that undo refuses
/// it instead of silently skipping it.
pub fn in_history(patch: &Patch, explicit: bool) -> bool {
    !patch.is_empty() || explicit
}

/// Undo and redo need every scope the step recorded (see [`step_scope`]). `verb` is `undo` or `redo`, `label` the
/// step's.
///
/// # Errors
/// [`denied`] naming the first scope `grants` lack. The runner leaves the step where it was: the actor may retry
/// with more.
pub fn require_step_scopes(
    scopes: impl IntoIterator<Item = Scope>,
    grants: &Grants,
    actor: &Actor,
    verb: &str,
    label: &str,
) -> Result<(), OpError> {
    match scopes.into_iter().find(|scope| !grants.allows(*scope)) {
        Some(scope) => Err(denied(actor, &format!("{verb} {label:?}"), scope)),
        None => Ok(()),
    }
}

/// `Unsupported`: a step holds `op`, which has effects outside the document. Undo and redo can't revert those yet, so
/// they refuse the whole step rather than revert only its document part (ADR 0036 decision 9), and drop it: the
/// document is untouched, and the next call reaches the step before.
pub fn explicit_refused(verb: &str, label: &str, op: &str) -> OpError {
    OpError::unsupported(format!(
        "can't {verb} {label:?}: {op} has effects outside the document, and that effect was not reverted"
    ))
    .hint(format!(
        "that step is dropped from your {verb} history, so the next {verb} reaches the step before it; \
         reverse the effect of {op} by hand"
    ))
}
