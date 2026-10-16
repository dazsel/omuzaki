//! Playing a macro: its calls as one transaction, undone with one step, rolled back whole on the first failure.

use omuzaki_ops_api::{Actor, Cause, CauseKind, Grants, Mode, OpError, Request, Runner};

use omuzaki_core::time::{Clock, SystemClock};

use super::Macro;
use crate::bindings::refusal_in;
use crate::error::limit_error;
use crate::{LimitKind, Limits};

/// What a macro that played did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayReport {
    /// The calls that ran.
    pub steps: usize,
}

/// Why a macro stopped, and where: the 1-based step, its op and the op's own error. The transaction was rolled back.
/// A `Waiting` or `Denied` answer is an error like any other; a `Denied` error keeps the scope to grant.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("step {step} (`{op}`) failed: {error}")]
pub struct PlayError {
    /// The 1-based step that failed. When the commit itself failed, the last step.
    pub step: usize,
    /// The op of that step, or `commit` when the commit failed.
    pub op: String,
    /// What the runner answered.
    pub error: OpError,
}

/// Plays `m` as `actor`, within `grants`, as one transaction labelled with the macro's name, held to the console's
/// limits ([`play_within`]). Every call is sent with the cause `Script` named after the macro: a replay is a scripted
/// batch, so the executor never queues it for approval.
///
/// # Errors
/// As [`play_within`].
pub fn play(runner: &dyn Runner, m: &Macro, actor: &Actor, grants: &Grants) -> Result<PlayReport, PlayError> {
    play_within(runner, m, actor, grants, &Limits::console(), &SystemClock::new())
}

/// [`play`] held to `limits`, with the time read from `clock` (a test passes a fake one). A macro is held to them as a
/// script is: a step a script's calls are refused (a job, an edit with effects outside the document, which the rollback
/// can't undo, and `script.run`, which would recurse) before anything starts, more than [`Limits::calls`] steps are
/// refused before anything starts, and the transaction is rolled back when the clock passes [`Limits::seconds`], which
/// is checked after every call, the last one included.
///
/// # Errors
/// A [`PlayError`] naming the first step that failed; everything the macro did is rolled back first. If the rollback
/// itself fails, its error is added to the message and the transaction stays open on the runner. A refused step or a
/// macro over the call limit fails the same way before the transaction begins, so there is nothing to roll back.
pub fn play_within(
    runner: &dyn Runner,
    m: &Macro,
    actor: &Actor,
    grants: &Grants,
    limits: &Limits,
    clock: &dyn Clock,
) -> Result<PlayReport, PlayError> {
    if m.steps.is_empty() {
        return Ok(PlayReport { steps: 0 });
    }
    if m.steps.len() > limits.calls as usize {
        let step = limits.calls as usize + 1;
        return Err(PlayError {
            step,
            op: m.steps[step - 1].op.clone(),
            error: limit_error(
                LimitKind::Calls,
                format!("the macro has {} steps, over the {}-call limit", m.steps.len(), limits.calls),
            ),
        });
    }
    let ops = runner.ops();
    let refused = m.steps.iter().enumerate().find_map(|(index, step)| {
        refusal_in(&ops, &step.op).map(|error| PlayError { step: index + 1, op: step.op.clone(), error })
    });
    if let Some(refused) = refused {
        return Err(refused);
    }
    let tx = runner.begin(&m.name, actor);
    let cause = Cause { kind: CauseKind::Script, text: Some(m.name.clone()) };
    let abort = |step: usize, op: &str, mut error: OpError| {
        if let Err(rollback) = runner.rollback(tx, actor) {
            error.message = format!("{}; rollback failed: {}", error.message, rollback.message);
        }
        PlayError { step, op: op.to_owned(), error }
    };
    let started = clock.now().0;
    for (index, step) in m.steps.iter().enumerate() {
        let request = Request {
            op: step.op.clone(),
            args: step.args.clone(),
            mode: Mode::Apply,
            actor: actor.clone(),
            grants: grants.clone(),
            cause: Some(cause.clone()),
            tx: Some(tx),
            base: None,
        };
        if let Err(error) = runner.run(request) {
            return Err(abort(index + 1, &step.op, error));
        }
        if clock.now().0 - started > f64::from(limits.seconds) {
            let error = limit_error(LimitKind::Time, format!("the macro ran over its {}-second limit", limits.seconds));
            return Err(abort(index + 1, &step.op, error));
        }
    }
    runner.commit(tx, actor).map_err(|error| abort(m.steps.len(), "commit", error))?;
    Ok(PlayReport { steps: m.steps.len() })
}
