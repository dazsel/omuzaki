//! Effects outside the document (#1091, ADR 0036 decision 8): an `Undo::Explicit` op records what it did as a pair of
//! inverse calls, and undo, redo, rollback, session end and undo-session run them through the one function here.
//!
//! Every call is an ordinary registered op, run under the lock as the undoing actor with its grants: schema, scope and
//! session checks apply. It may not plan a document change, and it never queues for approval (it would run later, out
//! of the undo it belongs to). If call *k* fails, the calls already run are reversed with their opposite, newest
//! first; if a reversing call fails too, the effects are mixed and the caller drops the step.

use omuzaki_ops_api::{Actor, Effect, Grants, Mode, OpCall, OpError, Request};

use crate::executor::Env;
use crate::gate::Allowance;
use crate::history::Direction;
use crate::state::State;

/// Why running a step's effects failed.
pub(crate) enum EffectFailure {
    /// A call failed and every call already run was reversed: nothing outside the document changed. The step stays.
    Reversed(OpError),
    /// A call failed and so did reversing one already run: some effects are reversed, some still in place. The error's
    /// hint names which; the caller drops the step so history cannot wedge on it.
    Mixed(OpError),
}

impl Env {
    /// Runs `effects` for a step moving in `direction`: their `undo` calls newest first for an undo (or a rollback),
    /// their `redo` calls oldest first for a redo. On a failure, the calls already run are reversed newest first.
    pub(crate) fn run_effects(
        &self,
        state: &mut State,
        effects: &[Effect],
        direction: Direction,
        actor: &Actor,
        grants: &Grants,
    ) -> Result<(), EffectFailure> {
        let order: Vec<&Effect> = match direction {
            Direction::Undo => effects.iter().rev().collect(),
            Direction::Redo => effects.iter().collect(),
        };
        let forward = |effect: &Effect| match direction {
            Direction::Undo => effect.undo.clone(),
            Direction::Redo => effect.redo.clone(),
        };
        let back = |effect: &Effect| match direction {
            Direction::Undo => effect.redo.clone(),
            Direction::Redo => effect.undo.clone(),
        };
        for (done, effect) in order.iter().enumerate() {
            let Err(error) = self.call(state, &forward(effect), actor, grants) else { continue };
            let ran = &order[..done];
            let mut reversed = Vec::new();
            for (k, earlier) in ran.iter().enumerate().rev() {
                if let Err(reason) = self.call(state, &back(earlier), actor, grants) {
                    let still: Vec<&str> = ran[..=k].iter().map(|e| e.summary.as_str()).collect();
                    let hint = format!(
                        "the effects outside the document are mixed, so the step is dropped: reversed again: [{}]; \
                         still {} (putting one back failed: {}): [{}]",
                        reversed.join(", "),
                        direction.verb_past(),
                        reason.message,
                        still.join(", "),
                    );
                    return Err(EffectFailure::Mixed(error.hint(hint)));
                }
                reversed.push(earlier.summary.as_str());
            }
            return Err(EffectFailure::Reversed(error));
        }
        Ok(())
    }

    /// Runs one effect call: the op it names, applied as `actor` with `grants`, in no transaction.
    pub(crate) fn call(&self, state: &mut State, call: &OpCall, actor: &Actor, grants: &Grants) -> Result<(), OpError> {
        let registration = self.ops.find(&call.op)?;
        let op = (registration.decode)(call.args.clone())?;
        let req = Request {
            op: call.op.clone(),
            args: call.args.clone(),
            mode: Mode::Apply,
            actor: actor.clone(),
            grants: grants.clone(),
            cause: None,
            tx: None,
            base: None,
        };
        self.run_locked(state, &*op, req, &Allowance { effect_call: true, ..Allowance::default() }).map(drop)
    }
}

impl Direction {
    /// "undone" or "redone", for messages about effects.
    fn verb_past(self) -> &'static str {
        match self {
            Direction::Undo => "undone",
            Direction::Redo => "redone",
        }
    }
}

/// The effect that reverses `effect`: its calls swapped. An undo-session's step holds these, so undoing it puts the
/// agent's effects back.
pub(crate) fn inverted(effect: &Effect) -> Effect {
    Effect { summary: format!("undo of {}", effect.summary), undo: effect.redo.clone(), redo: effect.undo.clone() }
}
