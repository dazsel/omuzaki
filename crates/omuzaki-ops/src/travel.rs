//! Undo and redo per principal (#245), rollback, abandoning a session's transactions (#149) and undoing a whole
//! session (#357), with each step's effects outside the document (#1091). The [`State`] moves steps and applies
//! patches; this runs the effect calls in between, which need the op registry.
//!
//! A step with effects is tried on a scratch copy first, so the patch is known to apply before any call runs: a refused
//! undo or redo has changed nothing anywhere.

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Actor, Grants, KeptTx, OpError, PauseReason, SessionChange, SessionId, SessionUndo, TxId};

use crate::effects::{EffectFailure, inverted};
use crate::executor::Env;
use crate::history::{Direction, Draft};
use crate::sessions::Entry;
use crate::simulate::Trial;
use crate::state::State;

/// What abandoning an actor's open transactions did.
pub(crate) struct Abandoned {
    pub(crate) abandoned: Vec<TxId>,
    pub(crate) kept: Vec<KeptTx>,
}

impl Env {
    /// Moves the next step of `actor`'s undo (or redo) as `actor`, with `grants` (design §5.4). For a person that is
    /// the newest step among their own history and their agents'; if it belongs to an active session, that session is
    /// paused first (`PauseReason::Undo`), so the agent cannot keep committing on top while the person steps back.
    pub(crate) fn travel(
        &self,
        state: &mut State,
        direction: Direction,
        actor: &Actor,
        grants: &Grants,
    ) -> Result<Option<String>, OpError> {
        state.journal.admit()?;
        let Some(index) = state.pick(direction, actor) else { return Ok(None) };
        let owner = state.owner(index).clone();
        if owner != *actor {
            self.pause_for_undo(state, &owner, actor);
        }
        let Some(step) = state.histories.take(index, direction) else { return Ok(None) };
        let verb = direction.verb();
        if let Err(denied) = step.require(actor, grants, verb) {
            state.histories.put_back(index, direction, step);
            return Err(denied);
        }
        if !step.effects.is_empty() {
            let patch = match direction {
                Direction::Undo => step.patch.inverse(),
                Direction::Redo => omuzaki_doc_api::Patch::clone(&step.patch),
            };
            if let Err(e) = Trial::of(state.doc(), [&patch]).apply(&patch) {
                state.drop_step(index, direction, &step);
                return Err(state.conflict(&format!("{verb} {:?}", step.label), &e, direction.dropped_hint()));
            }
            match self.run_effects(state, &step.effects, direction, actor, grants) {
                Ok(()) => {}
                Err(EffectFailure::Reversed(error)) => {
                    let index = state.histories.find(&owner).unwrap_or(index);
                    state.histories.put_back(index, direction, step);
                    return Err(error);
                }
                Err(EffectFailure::Mixed(error)) => {
                    let index = state.histories.find(&owner).unwrap_or(index);
                    state.drop_step(index, direction, &step);
                    return Err(error);
                }
            }
        }
        let index = state.histories.find(&owner).unwrap_or(index);
        state.move_step(index, direction, step, actor).map(Some)
    }

    /// Pauses `owner`'s session, if it is an active one, because `by` is undoing or redoing its step.
    fn pause_for_undo(&self, state: &mut State, owner: &Actor, by: &Actor) {
        let Some(id) = state.sessions.of_actor(owner).map(|s| s.id) else { return };
        let Ok(index) = state.sessions.index(id) else { return };
        if state.sessions.pause_one(index, by.clone(), PauseReason::Undo, self.clock.now())
            && let Some(snapshot) = state.session(id)
        {
            state.queue_event(omuzaki_ops_api::Event::session_changed(snapshot, SessionChange::Paused));
        }
    }

    /// Rolls back open transaction `tx` of `actor`: its effects' undo calls newest first, run with `grants`, then the
    /// combined inverse patch.
    ///
    /// # Errors
    /// As [`State::rollback`]; a failed effect call (the transaction stays open, its effects reversed back, or mixed as
    /// the hint says).
    pub(crate) fn rollback_tx(
        &self,
        state: &mut State,
        tx: TxId,
        actor: &Actor,
        grants: &Grants,
    ) -> Result<(), OpError> {
        let (effects, revert) = state.open_undo(tx, actor)?;
        if !effects.is_empty() {
            if let Err(e) = Trial::of(state.doc(), [&revert]).apply(&revert) {
                let hint =
                    "the transaction is still open: commit it to keep its changes, or retry once the clash is resolved";
                return Err(state.conflict(&format!("roll back transaction {}", tx.0), &e, hint));
            }
            match self.run_effects(state, &effects, Direction::Undo, actor, grants) {
                Ok(()) => {}
                Err(EffectFailure::Reversed(error) | EffectFailure::Mixed(error)) => return Err(error),
            }
        }
        state.rollback(tx, actor)
    }

    /// Reverts only driver job `job`'s own inner edits from the caller's open transaction `tx`, all or nothing (ADR 0036
    /// decision 10): its effects' undo calls, then one revert patch, tried first like an undo. If another actor (or
    /// the caller) changed a path since, nothing is reverted and the job's edits stay in the transaction.
    ///
    /// # Errors
    /// `Conflict` with [`ErrorDetail::Unreverted`](omuzaki_ops_api::ErrorDetail::Unreverted), listing every conflict
    /// and every retained path; a failed effect call.
    pub(crate) fn revert_driver(
        &self,
        state: &mut State,
        tx: TxId,
        job: omuzaki_ops_api::JobId,
        actor: &Actor,
        grants: &Grants,
    ) -> Result<(), OpError> {
        let Some((edits, effects)) = state.driven_by(tx, job) else { return Ok(()) };
        if edits.is_empty() && effects.is_empty() {
            return Ok(());
        }
        let revert = edits.inverse();
        if Trial::of(state.doc(), [&revert]).apply(&revert).is_err() {
            let (conflicts, retained) = state.unreverted(&edits);
            let paths: Vec<String> = conflicts.iter().map(|c| c.path.to_string()).collect();
            return Err(OpError::conflict(format!(
                "job {} was stopped, but its edits were changed since at {}: nothing was reverted",
                job.0,
                paths.join(", ")
            ))
            .hint("the job's edits stay in your transaction: commit it as it is, fix it up, or roll it back")
            .with_detail(omuzaki_ops_api::ErrorDetail::Unreverted { conflicts, retained }));
        }
        if let Err(EffectFailure::Reversed(error) | EffectFailure::Mixed(error)) =
            self.run_effects(state, &effects, Direction::Undo, actor, grants)
        {
            return Err(error);
        }
        state.revert_driven(tx, job, &revert, actor)
    }

    /// What `actor` may do when the executor itself acts for it (an end, a rollback without grants): its session's
    /// effective grants, or a person's own.
    pub(crate) fn own_grants(state: &mut State, actor: &Actor, at: Seconds) -> Grants {
        state.sessions.admit_run(actor, &Grants::person(), at, Entry::Inner).map(|a| a.grants).unwrap_or_default()
    }

    /// Abandons every open transaction `actor` owns, newest first (design §4.2): each is rolled back with its effects,
    /// or, when that fails, committed as it is with " (unfinished)" added to its label, so nobody is left with a
    /// transaction no one can close. A failure never stops the rest; other actors' transactions are untouched.
    pub(crate) fn abandon(&self, state: &mut State, actor: &Actor, at: Seconds) -> Result<Abandoned, OpError> {
        let grants = Self::own_grants(state, actor, at);
        let (mut abandoned, mut kept) = (Vec::new(), Vec::new());
        for tx in state.owned_open(actor) {
            match self.rollback_tx(state, tx, actor, &grants) {
                Ok(()) => abandoned.push(tx),
                Err(mut error) => {
                    error.hint = Some(
                        "it is kept as an unfinished step in the agent's history; undoing the session tries again"
                            .into(),
                    );
                    let label = state.commit_as_is(tx, actor, " (unfinished)")?;
                    kept.push(KeptTx { tx, label, error });
                }
            }
        }
        Ok(Abandoned { abandoned, kept })
    }

    /// Undoes session `id` as one step in `by`'s history (design §6). The caller checked `by` and paused the session.
    ///
    /// The agent's open transactions are abandoned first. Then its steps are reverted newest first, each tried on a
    /// scratch copy on top of the ones before: a step whose revert conflicts, whose scopes `by` lacks, or whose effects
    /// cannot be undone is skipped and reported, and stays applied. The reverts (and the undone effects) become one
    /// step labelled "Undo <client>'s session (N steps)", applied as one change, so `by`'s undo brings the work back.
    /// The agent's undo and redo stacks are cleared.
    pub(crate) fn undo_session_locked(
        &self,
        state: &mut State,
        id: SessionId,
        by: &Actor,
        at: Seconds,
    ) -> Result<SessionUndo, OpError> {
        let info = state.sessions.info(state.sessions.index(id)?).clone();
        // A transaction that cannot be reverted is kept as an unfinished step, which the walk below tries again.
        let Abandoned { abandoned, .. } = self.abandon(state, &info.actor, at)?;
        let grants = state.sessions.principal_grants(by);
        let steps = state.take_history(&info.actor);
        let mut trial = Trial::of(state.doc(), steps.iter().map(|s| &*s.patch));
        let (mut reverted, mut skipped, mut outside) = (Vec::new(), Vec::new(), Vec::new());
        let mut draft = Draft::default();
        for step in steps {
            if let Err(error) = step.require(by, &grants, "undo") {
                skipped.push(KeptTx { tx: step.tx, label: step.label, error });
                continue;
            }
            let inverse = step.patch.inverse();
            if let Err(e) = trial.apply(&inverse) {
                let hint = "that step stays applied; the steps before it may depend on it";
                let error = state.conflict(&format!("undo {:?}", step.label), &e, hint);
                skipped.push(KeptTx { tx: step.tx, label: step.label, error });
                continue;
            }
            if !step.effects.is_empty() {
                if let Err(EffectFailure::Reversed(error) | EffectFailure::Mixed(error)) =
                    self.run_effects(state, &step.effects, Direction::Undo, by, &grants)
                {
                    // Put the scratch back as it was before this step's revert: the step stays applied.
                    trial
                        .apply(&step.patch)
                        .map_err(|e| OpError::internal(format!("a tried revert did not reverse: {e}")))?;
                    outside.push(step.tx);
                    skipped.push(KeptTx { tx: step.tx, label: step.label, error });
                    continue;
                }
                draft.effects.extend(step.effects.iter().rev().map(inverted));
            }
            reverted.push(step.tx);
            draft.patch.extend(inverse);
            draft.scopes.extend(step.scopes.iter().copied());
        }
        let tx = (!reverted.is_empty()).then(|| state.reserve_tx());
        let result = SessionUndo { step: tx, reverted, skipped, abandoned, outside };
        state.queue_event(omuzaki_ops_api::Event::session_undone(id, by.clone(), result.clone()));
        if let Some(tx) = tx {
            let noun = if result.reverted.len() == 1 { "step" } else { "steps" };
            let label = format!("Undo {}'s session ({} {noun})", info.client, result.reverted.len());
            state.commit_combined(id, tx, by, label, draft, at)?;
        }
        Ok(result)
    }
}
