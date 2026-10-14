//! Planning an op from inside another op's plan.

use omuzaki_doc_api::{DocRead, Value};

use crate::{Actor, OpCtx, OpError, OpKind, OpMeta, Plan, find, registrations};

impl OpCtx<'_> {
    /// Plans the registered op `op` with `args` against `doc` as one step of a larger plan, and returns the op's
    /// facts with its [`Plan`].
    ///
    /// It exists for composite ops (`library.apply`) and scripts, which plan several ops in order against a
    /// [`Scratch`](omuzaki_doc_api::Scratch): plan a step, apply its patch to the scratch, plan the next against the
    /// result. **The caller owns the combined patch**: nothing here applies anything, logs anything or opens a
    /// transaction, and the caller's own op is the one the runner records.
    ///
    /// The step runs in a context over `doc` that keeps this context's actor, clock, services, grants, project
    /// folder and dry-run flag, so [`fs_access`](Self::fs_access) and [`service`](Self::service) answer as they
    /// would for the caller. The step's op never sees the grants themselves.
    ///
    /// The step's own scope is checked as the runner checks it: it is required unless this is a dry-run (a dry-run
    /// of an edit only shows what would need permission), but a [`Query`](OpKind::Query) step is always gated,
    /// because its plan *is* its answer. Like the runner, this method assumes the request already holds `read`.
    ///
    /// # Errors
    /// - `Invalid` on field `op` when no such op is registered; the hint lists ops in the same area.
    /// - `Invalid` when `args` don't decode into the op's schema (the field is the argument at fault).
    /// - `Denied`, naming the scope, when the grants lack the step's scope.
    /// - Whatever the op's own `plan` returns.
    pub fn plan_step(&self, op: &str, args: Value, doc: &dyn DocRead) -> Result<(OpMeta, Plan), OpError> {
        let registration = find(op).ok_or_else(|| unknown_op(op))?;
        let step = (registration.decode)(args)?;
        let meta = step.meta();
        if (!self.dry_run || meta.kind == OpKind::Query) && !self.grants_allow(meta.scope) {
            return Err(OpError::denied(format!(
                "{} may not run {}: it needs the {} scope",
                who(self.actor),
                meta.name,
                meta.scope
            ))
            .hint(format!("grant the {} scope to run this step", meta.scope)));
        }
        let plan = step.plan(&self.over(doc))?;
        Ok((meta, plan))
    }
}

/// A human-readable name for an actor, worded as the executor's refusals are.
fn who(actor: &Actor) -> String {
    match actor {
        Actor::Person { name } => format!("person {name}"),
        Actor::Agent { id, .. } => format!("agent {id}"),
        Actor::Plugin { id } => format!("plugin {id}"),
        Actor::System => "the system".to_owned(),
    }
}

/// `Invalid` on `op`, with the ops that share its area (the part before the dot) as the hint.
fn unknown_op(name: &str) -> OpError {
    let area = name.split_once('.').map_or(name, |(area, _)| area);
    let mut siblings: Vec<&str> = registrations()
        .map(|r| (r.meta)().name)
        .filter(|candidate| candidate.split_once('.').is_some_and(|(a, _)| a == area))
        .collect();
    siblings.sort_unstable();
    siblings.truncate(5);
    let hint = if siblings.is_empty() {
        "list the available ops first (MCP tools/list, or `omuzaki help`)".to_owned()
    } else {
        format!("ops in the `{area}` area: {}", siblings.join(", "))
    };
    OpError::invalid("op", format!("no op is named {name:?}")).hint(hint)
}
