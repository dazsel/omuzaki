//! [`Sessions`] for the executor: the registry's controls, ending and undoing sessions, charging them, and deciding
//! approvals (design `agent-sessions.md` §3.4, §4.2, §6, §7, §8.1).
//!
//! Every control runs in one hold of the executor's lock and queues its events there, so a control and a request
//! never interleave: a request admitted a moment before an end, a pause or a budget pause is refused when it reaches
//! the lock, and an approval is taken out of the queue (resolved or withdrawn) exactly once.

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::DocPath;
use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_ops_api::{
    Actor, ApprovalId, ApprovalRef, Budget, CancelReason, Decision, DynOp, EndReason, Ended, Event, Grants, JobId,
    Mode, Need, OpError, OpKind, Outcome, PauseReason, PendingApproval, Plan, Request, Resolution, Resolved,
    SessionChange, SessionId, SessionInfo, SessionSpec, SessionUndo, Sessions, Spend, StopStep, Stopped, TxId,
};
use serde_json::json;

use crate::approvals::{self, Ask, Entry};
use crate::executor::Executor;
use crate::gate::{Allowance, queued};
use crate::state::State;
use crate::sync::lock;
use crate::travel::Abandoned;

impl Sessions for Executor {
    fn open(&self, spec: SessionSpec) -> Result<SessionInfo, OpError> {
        let at = self.env.clock.now();
        self.with_state(|state| {
            let info = state.sessions.open(spec, at)?;
            let snapshot = state.session_info(&info);
            state.queue_event(Event::session_opened(snapshot.clone()));
            Ok(snapshot)
        })
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        let state = lock(&self.env.state);
        state.sessions.all().map(|s| state.session_info(s)).collect()
    }

    fn session(&self, id: SessionId) -> Option<SessionInfo> {
        lock(&self.env.state).session(id)
    }

    fn session_of(&self, actor: &Actor) -> Option<SessionInfo> {
        let state = lock(&self.env.state);
        state.sessions.of_actor(actor).map(|s| state.session_info(s))
    }

    fn pause(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        let at = self.env.clock.now();
        self.change_sessions(id, |state| state.sessions.pause(id, by, at))
    }

    fn resume(&self, id: SessionId, by: &Actor) -> Result<SessionInfo, OpError> {
        let at = self.env.clock.now();
        self.change_sessions(id, |state| state.sessions.resume(id, by, at))
    }

    fn set_grants(&self, id: SessionId, grants: Grants, by: &Actor) -> Result<SessionInfo, OpError> {
        self.change_sessions(id, |state| state.sessions.set_grants(id, grants, by))
    }

    fn set_budget(&self, id: SessionId, budget: Budget, by: &Actor) -> Result<SessionInfo, OpError> {
        let at = self.env.clock.now();
        self.change_sessions(id, |state| state.sessions.set_budget(id, budget, by, at))
    }

    fn charge(&self, id: SessionId, spend: Spend) -> Result<SessionInfo, OpError> {
        let at = self.env.clock.now();
        self.with_state(|state| {
            let index = state.sessions.index(id)?;
            if state.sessions.has_ended(index) {
                return Err(OpError::conflict(format!("session {} has ended", id.0)));
            }
            if spend.seconds != 0 {
                return Err(OpError::invalid("seconds", "a session's active time is counted by the executor")
                    .hint("charge money and tokens only"));
            }
            let crossed = state.sessions.add_spend(index, spend.usd_micros, spend.tokens, at);
            changed(state, id, SessionChange::Spent)?;
            let actor = state.sessions.info(index).actor.clone();
            // Only the charge that pauses asks: one budget approval per pause.
            if crossed && state.sessions.pause_one(index, actor, PauseReason::Budget, at) {
                changed(state, id, SessionChange::Paused)?;
                // With no approver the session stays paused for a person to raise its budget in the panel.
                self.env.ask_budget(state, index, spend, at);
            }
            snapshot(state, id)
        })
    }

    fn set_fence(&self, id: SessionId, fence: Vec<DocPath>, by: &Actor) -> Result<SessionInfo, OpError> {
        self.change_sessions(id, |state| Ok(vec![(id, state.sessions.set_fence(id, fence, by)?)]))
    }

    fn end(&self, id: SessionId, reason: EndReason) -> Result<Ended, OpError> {
        let at = self.env.clock.now();
        // The jobs first, under the manager's lock, which comes before the document's: a job a session end cancels
        // never applies its finish, since its session has ended by the time the finish is checked (#1218).
        let family = {
            let state = lock(&self.env.state);
            let index = state.sessions.index(id)?;
            if let EndReason::Revoked { by, .. } = &reason {
                state.sessions.check_principal(index, by)?;
            }
            if state.sessions.has_ended(index) { Vec::new() } else { state.sessions.family(index) }
        };
        let cancelled: BTreeMap<SessionId, Vec<JobId>> = family
            .into_iter()
            .map(|member| (member, self.jobs.cancel_session(member, None, CancelReason::SessionEnded)))
            .collect();
        self.with_state(|state| self.end_family(state, id, &reason, at, &cancelled))
    }

    fn request_budget(&self, id: SessionId, needed: Spend) -> Result<(), OpError> {
        let at = self.env.clock.now();
        self.with_state(|state| {
            let index = state.sessions.index(id)?;
            if state.sessions.has_ended(index) {
                return Err(OpError::conflict(format!("session {} has ended", id.0)));
            }
            let Some(left) = state.sessions.short_of(index, needed, at) else { return Ok(()) };
            let actor = state.sessions.info(index).actor.clone();
            if state.sessions.pause_one(index, actor, PauseReason::Budget, at) {
                changed(state, id, SessionChange::Paused)?;
            }
            Err(match self.env.ask_budget(state, index, needed, at) {
                Some(approval) => queued(approval, Need::Budget { needed, left }),
                None => {
                    OpError::denied("the session's budget does not allow what it asks for, and no one can be asked")
                        .hint("raise the session's budget in advance")
                }
            })
        })
    }

    fn request_pairing(&self, id: SessionId) -> Result<ApprovalId, OpError> {
        let at = self.env.clock.now();
        self.with_state(|state| {
            let index = state.sessions.index(id)?;
            if state.sessions.has_ended(index) {
                return Err(OpError::conflict(format!("session {} has ended", id.0)));
            }
            let info = state.sessions.info(index);
            let (principal, actor, client) = (info.on_behalf_of.clone(), info.actor.clone(), info.client.clone());
            if !state.sessions.pause_one(index, principal, PauseReason::Pairing, at) {
                return Err(OpError::conflict(format!("session {} is already paused", id.0))
                    .hint("ask for pairing as the session opens"));
            }
            changed(state, id, SessionChange::Paused)?;
            let ask = Ask {
                session: id,
                actor,
                need: Need::Pairing { client },
                request: None,
                grant: None,
                required: 1,
                carried: Allowance::default(),
                at,
            };
            let pending = state.approvals.push(ask);
            let approval = pending.id;
            state.queue_event(Event::approval_requested(pending));
            Ok(approval)
        })
    }

    fn stop(&self, id: SessionId, by: &Actor) -> Result<Stopped, OpError> {
        let at = self.env.clock.now();
        // a. Pause and cancel, atomically: under the manager's lock, then the document's.
        let (actor, jobs) = self.jobs.stop(id, by, |state| {
            let index = state.sessions.index(id)?;
            state.sessions.check_principal(index, by)?;
            if state.sessions.has_ended(index) {
                return Err(OpError::conflict(format!("session {} has ended", id.0)));
            }
            // A pause already there (a budget pause, say) is kept.
            if state.sessions.pause_one(index, by.clone(), PauseReason::Person, at) {
                changed(state, id, SessionChange::Paused)?;
            }
            state.sessions.set_stopping(index, true);
            Ok(state.sessions.info(index).actor.clone())
        })?;
        // b. Cancelled jobs end by their next checkpoint; a driver joined to an open transaction reverts its own
        // edits from it first, and the job manager commits that transaction once it has (step c, `Pending`).
        let open = lock(&self.env.state).owned_open(&actor);
        let joined: Vec<TxId> = open.iter().copied().filter(|tx| self.jobs.joins(id, *tx)).collect();
        // c. Commit the rest as they stand, in any session state: this is the person-authorised close.
        let steps = self.with_state(|state| {
            open.iter()
                .rev()
                .map(|tx| {
                    if joined.contains(tx) {
                        return Ok(StopStep::Pending);
                    }
                    state.stop_tx(*tx, &actor)
                })
                .collect::<Result<Vec<StopStep>, OpError>>()
        })?;
        Ok(Stopped { step: steps.first().copied().unwrap_or(StopStep::None), jobs })
    }

    fn undo_session(&self, id: SessionId, by: &Actor) -> Result<SessionUndo, OpError> {
        let at = self.env.clock.now();
        self.with_state(|state| {
            let index = state.sessions.index(id)?;
            state.sessions.check_principal(index, by)?;
            // Paused first, so that nothing the agent sends lands on top while its work is reverted.
            if state.sessions.pause_one(index, by.clone(), PauseReason::Undo, at) {
                changed(state, id, SessionChange::Paused)?;
            }
            self.env.undo_session_locked(state, id, by, at)
        })
    }

    fn approvals(&self) -> Vec<PendingApproval> {
        lock(&self.env.state).approvals.all()
    }

    fn resolve(&self, id: ApprovalId, decision: Decision, by: &Actor) -> Result<Resolution, OpError> {
        let at = self.env.clock.now();
        match self.with_state(|state| self.resolve_locked(state, id, decision, by, at))? {
            Decided::Done(resolution) => Ok(resolution),
            // An approved job request starts the job now, after the lock: the job manager's lock comes before the
            // document's. The approval was taken out under the lock, so it still resolves exactly once.
            Decided::StartJob { op, request, allow } => {
                let result = match self.start_job(op, request, false, &allow) {
                    Ok(job) => Resolved::Applied { outcome: Outcome::new(Plan::answer(json!({ "job": job.id.0 }))) },
                    Err(error) => Resolved::Failed { error },
                };
                let resolution = Resolution { id, by: Some(by.clone()), result, at };
                self.with_state(|state| state.queue_event(Event::approval_resolved(resolution.clone())));
                Ok(resolution)
            }
        }
    }
}

impl Executor {
    /// Runs a session control under the lock and emits one `SessionChanged` per session it changed. Returns session
    /// `id` as it stands afterwards.
    fn change_sessions(
        &self,
        id: SessionId,
        change: impl FnOnce(&mut State) -> Result<Vec<(SessionId, SessionChange)>, OpError>,
    ) -> Result<SessionInfo, OpError> {
        self.with_state(|state| {
            for (changed_id, what) in change(state)? {
                changed(state, changed_id, what)?;
            }
            snapshot(state, id)
        })
    }

    /// [`Sessions::resolve`] under the lock. The person and the decision are checked first, while the approval is
    /// still queued, so a refused decision leaves it there; then it is taken out, once, and acted on.
    fn resolve_locked(
        &self,
        state: &mut State,
        id: ApprovalId,
        decision: Decision,
        by: &Actor,
        at: Seconds,
    ) -> Result<Decided, OpError> {
        let entry = state.approvals.get(id).ok_or_else(|| gone(id))?;
        let (session, need) = (entry.pending.session, entry.pending.need.clone());
        let index = state.sessions.index(session)?;
        self.check_resolver(state, index, entry, &decision, by)?;
        let transport = &state.sessions.info(index).transport;
        approvals::check_decision(&need, &decision, transport, entry.request.is_some())?;
        let has_request = entry.request.is_some();
        // The two decisions that are recorded and leave the approval waiting.
        match (&need, &decision) {
            (Need::Review { .. }, Decision::Approve) => {
                let entry = state.approvals.get_mut(id).ok_or_else(|| gone(id))?;
                if !entry.reviewers.contains(by) {
                    entry.reviewers.push(by.clone());
                }
                if entry.reviewers.len() < usize::from(entry.required) {
                    return Err(queued(id, need));
                }
            }
            (Need::Budget { .. }, Decision::RaiseBudget { budget }) if has_request => {
                // Raising a cap is not approving the request: the budget is set and the session resumed, and the
                // request still waits for Approve or Reject.
                for (changed_id, what) in state.sessions.replace_budget(index, *budget, by, at) {
                    changed(state, changed_id, what)?;
                }
                return Err(queued(id, need));
            }
            _ => {}
        }
        let entry = state.approvals.take(id).ok_or_else(|| gone(id))?;
        let mut revoke = false;
        let result = match decision {
            Decision::Reject { comment } => {
                // Refusing a pairing refuses the client: the session ends revoked, credential included.
                revoke = matches!(need, Need::Pairing { .. });
                Resolved::Rejected { comment }
            }
            Decision::RaiseBudget { budget } => {
                for (changed_id, what) in state.sessions.replace_budget(index, budget, by, at) {
                    changed(state, changed_id, what)?;
                }
                Resolved::Applied { outcome: Outcome::new(Plan::default()) }
            }
            Decision::Approve | Decision::ApproveForSession | Decision::ApproveForProject => {
                if let (Need::Scope { scope }, Decision::ApproveForSession | Decision::ApproveForProject) =
                    (&need, &decision)
                {
                    // "For this project" is the credential holder's to remember (the transport that asked); here
                    // both widen the session's ceiling.
                    for (changed_id, what) in state.sessions.widen(index, *scope, by) {
                        changed(state, changed_id, what)?;
                    }
                }
                if matches!(need, Need::Pairing { .. }) {
                    if state.sessions.lift(index, PauseReason::Pairing, at) {
                        changed(state, session, SessionChange::Resumed { by: by.clone() })?;
                    }
                    Resolved::Applied { outcome: Outcome::new(Plan::default()) }
                } else {
                    match self.run_approved(state, entry, by) {
                        Approved::Ran(result) => result,
                        Approved::Job { op, request, allow } => return Ok(Decided::StartJob { op, request, allow }),
                    }
                }
            }
        };
        let resolution = Resolution { id, by: Some(by.clone()), result, at };
        state.queue_event(Event::approval_resolved(resolution.clone()));
        if revoke {
            // A session waiting to be paired has run nothing, so it has no jobs to cancel.
            let none = BTreeMap::new();
            self.end_family(state, session, &EndReason::Revoked { by: by.clone(), credential: true }, at, &none)?;
        }
        Ok(Decided::Done(resolution))
    }

    /// Checks that `by` may decide `entry` (design §7.2): a person; the session's principal, or for a review a
    /// reviewer the policy allows (never the principal, who may still reject); holding the scope it would grant.
    fn check_resolver(
        &self,
        state: &State,
        index: usize,
        entry: &Entry,
        decision: &Decision,
        by: &Actor,
    ) -> Result<(), OpError> {
        if !matches!(by, Actor::Person { .. }) {
            return Err(OpError::person_only("only a person can decide an approval, in Omuzaki's window"));
        }
        let principal = &state.sessions.info(index).on_behalf_of;
        match &entry.pending.need {
            Need::Review { .. } if matches!(decision, Decision::Reject { .. }) && by == principal => {}
            Need::Review { policy } => {
                if by == principal {
                    return Err(OpError::denied("a request's own person cannot review it")
                        .hint("ask another reviewer of the team"));
                }
                let allowed = self.env.review.as_ref().is_some_and(|p| p.may_approve(by, principal, policy));
                if !allowed {
                    return Err(OpError::denied(format!("this person may not review under the policy {policy}")));
                }
            }
            _ => state.sessions.check_principal(index, by)?,
        }
        if let Need::Scope { scope } = &entry.pending.need
            && !state.sessions.principal_grants(by).allows(*scope)
        {
            return Err(OpError::denied(format!("only a person who holds `{scope}` can grant it")));
        }
        Ok(())
    }

    /// Runs an approved request against the current document as the agent, as a transaction of its own, with the
    /// need met for this run only (design §4.4, §7.1). A second need it hits is queued as a new approval, named in
    /// the failure.
    fn run_approved(&self, state: &mut State, entry: Entry, by: &Actor) -> Approved {
        let Entry { pending, request, grant, reviewers, carried, .. } = entry;
        let Some(request) = request else {
            return Approved::Ran(Resolved::Applied { outcome: Outcome::new(Plan::default()) });
        };
        let also: Vec<Actor> = reviewers.into_iter().filter(|reviewer| reviewer != by).collect();
        let need = &pending.need;
        // This approval's need, on top of what earlier approvals of the same request met.
        let mut scopes = carried.scopes;
        scopes.extend(grant.into_iter().chain(match need {
            Need::Scope { scope } => Some(*scope),
            _ => None,
        }));
        let allow = Allowance {
            scopes,
            confirmed_by: if matches!(need, Need::Person { .. }) { Some(by.clone()) } else { carried.confirmed_by },
            over_budget: carried.over_budget || matches!(need, Need::Budget { .. }),
            reviewed: carried.reviewed || matches!(need, Need::Review { .. }),
            approval: Some(ApprovalRef::new(pending.id, by.clone()).with_also(also)),
            effect_call: false,
            driver: None,
        };
        let op =
            match self.env.ops.find(&request.op).and_then(|registration| (registration.decode)(request.args.clone())) {
                Ok(op) => op,
                Err(error) => return Approved::Ran(Resolved::Failed { error }),
            };
        if op.meta().kind == OpKind::Job && request.mode == Mode::Apply {
            return Approved::Job { op: Arc::from(op), request, allow };
        }
        Approved::Ran(match self.env.run_locked(state, &*op, request, &allow) {
            Ok(outcome) => Resolved::Applied { outcome },
            Err(error) => Resolved::Failed { error },
        })
    }
}

impl Executor {
    /// Ends session `id` and its sub-agents, the most recently opened first, each with `reason` (design §3.4, §4.2).
    /// Ending an ended session changes nothing. Returns what ending `id` itself did.
    fn end_family(
        &self,
        state: &mut State,
        id: SessionId,
        reason: &EndReason,
        at: Seconds,
        cancelled: &BTreeMap<SessionId, Vec<JobId>>,
    ) -> Result<Ended, OpError> {
        let index = state.sessions.index(id)?;
        if let EndReason::Revoked { by, .. } = reason {
            state.sessions.check_principal(index, by)?;
        }
        let mut own = Ended { abandoned: Vec::new(), kept: Vec::new(), withdrawn: Vec::new(), cancelled: Vec::new() };
        for member in state.sessions.family(index).into_iter().rev() {
            let member_index = state.sessions.index(member)?;
            if state.sessions.has_ended(member_index) {
                continue;
            }
            let jobs = cancelled.get(&member).cloned().unwrap_or_default();
            let ended = self.end_one(state, member_index, reason.clone(), at, jobs)?;
            if member == id {
                own = ended;
            }
        }
        Ok(own)
    }

    /// Ends one session, atomically: abandons its open transactions, withdraws its approvals, marks it ended (a revoke
    /// also drops its ceiling) and queues `SessionEnded` after the transaction and approval events.
    fn end_one(
        &self,
        state: &mut State,
        index: usize,
        reason: EndReason,
        at: Seconds,
        cancelled: Vec<JobId>,
    ) -> Result<Ended, OpError> {
        let (id, actor) = {
            let info = state.sessions.info(index);
            (info.id, info.actor.clone())
        };
        let Abandoned { abandoned, kept } = self.env.abandon(state, &actor, at)?;
        let mut withdrawn = Vec::new();
        for entry in state.approvals.withdraw(id) {
            let approval = entry.pending.id;
            let result = Resolved::Withdrawn { reason: reason.clone() };
            state.queue_event(Event::approval_resolved(Resolution { id: approval, by: None, result, at }));
            withdrawn.push(approval);
        }
        state.sessions.end(index, reason, at);
        let ended = Ended { abandoned, kept, withdrawn, cancelled };
        let info = snapshot(state, id)?;
        state.queue_event(Event::session_ended(info, ended.clone()));
        Ok(ended)
    }
}

/// What deciding an approval came to under the lock.
#[allow(clippy::large_enum_variant)] // one per decision, matched at once: boxing would only add an allocation
enum Decided {
    /// Resolved: the resolution is final and announced.
    Done(Resolution),
    /// An approved job request: the job starts once the lock is released, and then the resolution is announced.
    StartJob { op: Arc<dyn DynOp>, request: Request, allow: Allowance },
}

/// What running an approved request came to.
#[allow(clippy::large_enum_variant)] // as `Decided`
enum Approved {
    Ran(Resolved),
    Job { op: Arc<dyn DynOp>, request: Request, allow: Allowance },
}

/// Session `id` as it stands, for a result.
fn snapshot(state: &State, id: SessionId) -> Result<SessionInfo, OpError> {
    state.session(id).ok_or_else(|| OpError::internal(format!("session {} vanished from the registry", id.0)))
}

/// Queues `SessionChanged` for session `id` with its snapshot after `what`.
fn changed(state: &mut State, id: SessionId, what: SessionChange) -> Result<(), OpError> {
    let info = snapshot(state, id)?;
    state.queue_event(Event::session_changed(info, what));
    Ok(())
}

/// The approval is not queued: it was resolved or withdrawn, or never existed.
fn gone(id: ApprovalId) -> OpError {
    OpError::not_found(format!("approval {} is gone", id.0)).hint("it was decided or withdrawn already")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use omuzaki_core::time::FakeClock;
    use omuzaki_doc_mock::MemDoc;
    use omuzaki_ops_api::{ErrorCode, Limits, SessionState, Transport};

    use super::*;

    /// An executor with one session paused for pairing and its `Pairing` approval queued.
    fn paired() -> (Executor, SessionId, ApprovalId) {
        let clock = Arc::new(FakeClock::new(Seconds(0.0)));
        let exec = Executor::new(Box::new(MemDoc::new()), clock).unwrap();
        let alice = Actor::Person { name: "alice".into() };
        let spec = SessionSpec {
            transport: Transport::Stdio,
            client: "bridge-tool".into(),
            model: None,
            on_behalf_of: alice.clone(),
            grants: Grants::agent_default(),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        };
        let info = exec.open(spec).unwrap();
        let id = exec.request_pairing(info.id).unwrap();
        let state = exec.session(info.id).unwrap().state;
        assert_eq!(state, SessionState::Paused { by: alice, reason: PauseReason::Pairing });
        let pending = exec.approvals();
        assert!(matches!(&pending[..], [a] if a.id == id && a.need == Need::Pairing { client: "bridge-tool".into() }));
        assert_eq!(exec.request_pairing(info.id).unwrap_err().code, ErrorCode::Conflict, "one card at a time");
        (exec, info.id, id)
    }

    #[test]
    fn pairing_resumes_the_session_and_refusing_it_revokes_the_session() {
        let alice = Actor::Person { name: "alice".into() };
        let (exec, session, id) = paired();
        assert_eq!(exec.resume(session, &alice).unwrap_err().code, ErrorCode::Denied, "only the card pairs");
        let raise = Decision::RaiseBudget { budget: Budget::default() };
        assert_eq!(exec.resolve(id, raise, &alice).unwrap_err().code, ErrorCode::Invalid);
        let resolution = exec.resolve(id, Decision::Approve, &alice).unwrap();
        assert!(matches!(resolution.result, Resolved::Applied { .. }));
        assert_eq!(exec.session(session).unwrap().state, SessionState::Active);

        let (exec, session, id) = paired();
        exec.resolve(id, Decision::Reject { comment: None }, &alice).unwrap();
        let state = exec.session(session).unwrap().state;
        let revoked = EndReason::Revoked { by: alice, credential: true };
        assert!(matches!(state, SessionState::Ended { reason, .. } if reason == revoked));
    }
}
