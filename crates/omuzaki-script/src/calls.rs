//! Where a run's inner requests go, and who owns its transaction (`docs/design/scripting.md` section 4).

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Cause, CauseKind, Grants, Mode, OpError, OpKind, Outcome, Plan, Request, Runner, TxId};
use serde::Serialize;

/// Polled by the host in `on_progress` (every Rhai operation) and before every inner call.
pub trait Cancel: Send + Sync {
    /// Whether the run should stop and roll back.
    fn is_cancelled(&self) -> bool;
}

/// One call a dry run planned instead of applying.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PlannedCall {
    /// The op's registered name.
    pub op: String,
    /// The arguments the script gave it.
    pub args: Value,
    /// What the op planned.
    pub plan: Plan,
}

/// Where a run's inner requests go. The host builds each [`Request`] (op, arguments, mode, actor, grants, cause and the
/// run's `tx`); the implementation decides how it reaches the document and who owns the transaction.
pub trait Calls: Send + Sync {
    /// Runs one inner request.
    ///
    /// # Errors
    /// Whatever the op or the runner answers.
    fn call(&self, request: Request) -> Result<Outcome, OpError>;

    /// Reads the document.
    ///
    /// # Errors
    /// `Denied` without the read scope.
    fn get(&self, path: &DocPath) -> Result<Option<Value>, OpError>;

    /// Called once when the script returned and its result is accepted. Returns the transaction it committed, if the
    /// implementation owns one. A caller that owns the transaction (a driver job) keeps the default.
    ///
    /// # Errors
    /// The commit failed; the host then rolls back.
    fn commit(&self) -> Result<Option<TxId>, OpError> {
        Ok(None)
    }

    /// Called once when the run failed, was cancelled or hit a limit. Undoes what this implementation owns.
    ///
    /// # Errors
    /// The rollback failed; the run's error is still reported, with this appended to its message.
    fn rollback(&self) -> Result<(), OpError> {
        Ok(())
    }
}

/// [`Calls`] over a [`Runner`], for the console and the CLI: one transaction per run, opened before the first edit.
pub struct RunnerCalls {
    runner: Arc<dyn Runner>,
    actor: Actor,
    grants: Grants,
    name: String,
    /// The names of the registry's `Edit` ops, read once so a call costs no `Runner::ops` listing.
    edits: HashSet<String>,
    tx: Mutex<Option<TxId>>,
}

impl RunnerCalls {
    /// Calls for one run of the script `name`, made as `actor` within `grants`.
    pub fn new(runner: Arc<dyn Runner>, name: impl Into<String>, actor: Actor, grants: Grants) -> Self {
        let edits = runner.ops().into_iter().filter(|i| i.kind == OpKind::Edit).map(|i| i.name.to_owned()).collect();
        Self { runner, actor, grants, name: name.into(), edits, tx: Mutex::new(None) }
    }

    /// The run's transaction while it is open: from the first edit until a commit or rollback succeeds. After a failed
    /// rollback (the runner leaves the transaction open, for example on a conflict) it is still here, so the caller can
    /// retry [`Calls::rollback`] or settle it on the runner.
    pub fn open_tx(&self) -> Option<TxId> {
        *self.tx.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The open transaction, begun now if this is the run's first edit.
    fn tx_for_edit(&self) -> TxId {
        let mut open = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        *open.get_or_insert_with(|| self.runner.begin(&self.name, &self.actor))
    }
}

impl Calls for RunnerCalls {
    fn call(&self, mut request: Request) -> Result<Outcome, OpError> {
        if request.tx.is_some() {
            return Err(OpError::invalid("tx", "a script run owns its transaction and cannot join another")
                .hint("joining a caller's transaction is for driver jobs, which pass the job's handle as `Calls`"));
        }
        let is_edit = request.mode == Mode::Apply && self.edits.contains(&request.op);
        request.tx = if is_edit { Some(self.tx_for_edit()) } else { self.open_tx() };
        request.actor = self.actor.clone();
        // The host narrows a run's grants to the scopes the script names; the caller's own grants are the ceiling.
        request.grants = self.grants.intersect(&request.grants);
        request.cause = Some(Cause { kind: CauseKind::Script, text: Some(self.name.clone()) });
        request.base = None;
        self.runner.run(request)
    }

    fn get(&self, path: &DocPath) -> Result<Option<Value>, OpError> {
        self.runner.get(path, &self.grants)
    }

    // Commit and rollback forget the transaction only when the runner settled it: after a failed commit the host
    // rolls back, and after a failed rollback the runner keeps it open for a retry, so the handle must survive both.
    fn commit(&self) -> Result<Option<TxId>, OpError> {
        let mut open = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tx) = *open else { return Ok(None) };
        self.runner.commit(tx, &self.actor)?;
        *open = None;
        Ok(Some(tx))
    }

    fn rollback(&self) -> Result<(), OpError> {
        let mut open = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tx) = *open else { return Ok(()) };
        self.runner.rollback(tx, &self.actor)?;
        *open = None;
        Ok(())
    }
}
