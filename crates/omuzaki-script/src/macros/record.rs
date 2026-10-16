//! Recording: the person's own committed edits, read from the runner's events.
//!
//! What a macro keeps is what the person kept: an edit enters when its transaction commits, a rolled-back transaction
//! never does, and an undo takes an edit out again (a redo puts it back). A slider drag is one step, not ninety, for ops
//! that say they are absolute setters (`docs/design/scripting.md` section 7).

use std::collections::{BTreeSet, HashMap};
use std::sync::mpsc::Receiver;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{Patch, Value};
use omuzaki_ops_api::{Actor, CauseKind, Event, Grants, OpError, OpKind, OpRecord, Runner, TxId, find};

use super::{Macro, RecordedCall};
use crate::bindings::refusal_for;

/// Two calls in different transactions merge only when they are this close on the runner's clock.
const COALESCE_WINDOW: Seconds = Seconds(1.0);

/// Most edits a recorder holds, open and kept together, before it stops taking more. A macro can't replay more than
/// [`Limits::calls`](crate::Limits::calls) steps anyway, and a recorder left running (a drag is many edits) must not
/// grow without bound; [`MacroRecorder::overflowed`] says when it stopped.
const MAX_ENTRIES: usize = 100_000;

/// A recording that held 100,000 edits and left later ones out.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("the recording held more than {} edits and left the later ones out, so it is not complete", MAX_ENTRIES)]
pub struct Overflowed {
    /// The macro of the edits that were kept: the first ones, not the whole session.
    pub partial: Macro,
}

/// One recorded edit: the call, and what is needed to decide whether it merges with its neighbour.
struct Entry {
    tx: TxId,
    call: RecordedCall,
    /// The document paths the edit replaced, when it only replaced values (no path added or removed).
    replaced: Option<BTreeSet<String>>,
    at: Seconds,
    /// Whether the op declared itself an absolute setter ([`Op::coalesces`](omuzaki_ops_api::Op::coalesces)).
    coalesces: bool,
    /// False while its transaction is undone.
    live: bool,
}

/// What the recorder knows about an op: whether it is an edit, whether it coalesces, and whether a replay may call
/// it (a macro plays as a script does: no jobs, no edits with outside effects, no `script.run`).
#[derive(Copy, Clone)]
struct Facts {
    kind: OpKind,
    coalesces: bool,
    replayable: bool,
}

/// Records a person's edits from the runner's events. Create it with [`start`](Self::start), call
/// [`poll`](Self::poll) now and then (it never blocks), and end with [`finish`](Self::finish).
pub struct MacroRecorder {
    events: Receiver<Event>,
    person: Actor,
    ops: HashMap<String, Facts>,
    /// Applied but not yet committed, by transaction.
    open: HashMap<TxId, Vec<Entry>>,
    /// Committed, in commit order.
    kept: Vec<Entry>,
    /// Entries held, open and kept together: what [`MAX_ENTRIES`] limits.
    held: usize,
    /// Set when an edit was dropped because the recorder was full.
    overflowed: bool,
}

impl MacroRecorder {
    /// Subscribes to the runner's events; needs `read`. Only `person`'s own work is recorded.
    ///
    /// # Errors
    /// `Denied` if `grants` lack `read`.
    pub fn start(runner: &dyn Runner, person: &Actor, grants: &Grants) -> Result<Self, OpError> {
        let events = runner.subscribe(grants)?;
        let ops = runner
            .ops()
            .into_iter()
            .map(|info| {
                (
                    info.name.to_owned(),
                    Facts {
                        kind: info.kind,
                        coalesces: info.coalesces,
                        replayable: refusal_for(info.name, info.kind, info.undo).is_none(),
                    },
                )
            })
            .collect();
        Ok(Self {
            events,
            person: person.clone(),
            ops,
            open: HashMap::new(),
            kept: Vec::new(),
            held: 0,
            overflowed: false,
        })
    }

    /// Whether an edit was left out because the recorder held 100,000 edits already, so a surface can stop the
    /// recording and say so. [`finish`](Self::finish) fails in that case.
    pub fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Takes in the events that are waiting, without blocking.
    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.take(event);
        }
    }

    /// Ends the recording and returns the macro named `name`. A transaction still open is not part of it.
    ///
    /// # Errors
    /// [`Overflowed`] if the recorder was full at any point, because the macro would be missing the later edits and
    /// must not be saved as if it were complete. The error holds what was recorded, for a surface that wants to offer it
    /// as a partial macro.
    pub fn finish(mut self, name: &str) -> Result<Macro, Overflowed> {
        self.poll();
        let recorded = Macro { name: name.to_owned(), steps: merged(&self.kept) };
        if self.overflowed { Err(Overflowed { partial: recorded }) } else { Ok(recorded) }
    }

    fn take(&mut self, event: Event) {
        match event {
            Event::OpApplied { record, .. } => self.applied(&record),
            Event::TxCommitted { tx, actor, .. } if actor == self.person => {
                self.kept.extend(self.open.remove(&tx).unwrap_or_default());
            }
            Event::TxRolledBack { tx, .. } => {
                let dropped = self.open.remove(&tx).map_or(0, |entries| entries.len());
                self.held = self.held.saturating_sub(dropped);
            }
            Event::Undone { tx, owner, .. } if owner == self.person => self.set_live(tx, false),
            Event::Redone { tx, owner, .. } if owner == self.person => self.set_live(tx, true),
            _ => {}
        }
    }

    fn applied(&mut self, record: &OpRecord) {
        let direct = record.cause.as_ref().is_none_or(|cause| cause.kind == CauseKind::Direct);
        if record.actor != self.person || !direct {
            return;
        }
        let Some(facts) = self.facts(&record.op) else { return };
        if facts.kind != OpKind::Edit || !facts.replayable {
            return;
        }
        if self.held >= MAX_ENTRIES {
            self.overflowed = true;
            return;
        }
        self.held += 1;
        self.open.entry(record.tx).or_default().push(Entry {
            tx: record.tx,
            call: RecordedCall { op: record.op.clone(), args: record.args.clone() },
            replaced: replaced_paths(&record.patch),
            at: record.at,
            coalesces: facts.coalesces,
            live: true,
        });
    }

    /// What the runner said about `op` when recording started, or the registry's word for an op registered since.
    fn facts(&self, op: &str) -> Option<Facts> {
        self.ops.get(op).copied().or_else(|| {
            let meta = (find(op)?.meta)();
            Some(Facts {
                kind: meta.kind,
                coalesces: false,
                replayable: refusal_for(meta.name, meta.kind, meta.undo).is_none(),
            })
        })
    }

    fn set_live(&mut self, tx: TxId, live: bool) {
        for entry in self.kept.iter_mut().filter(|entry| entry.tx == tx) {
            entry.live = live;
        }
    }
}

/// The steps of the live entries, with a slider drag folded into its last call.
fn merged(kept: &[Entry]) -> Vec<RecordedCall> {
    let mut steps: Vec<&Entry> = Vec::new();
    for entry in kept.iter().filter(|entry| entry.live) {
        match steps.last_mut() {
            Some(previous) if merges(previous, entry) => *previous = entry,
            _ => steps.push(entry),
        }
    }
    steps.into_iter().map(|entry| entry.call.clone()).collect()
}

/// Whether `next` replaces `previous`: the same op, which declared itself an absolute setter; arguments that differ
/// only in numbers; and two edits that both only replaced values at the same paths (a second guard against a wrong
/// declaration). Across transactions they must also be close in time.
fn merges(previous: &Entry, next: &Entry) -> bool {
    next.coalesces
        && previous.call.op == next.call.op
        && previous.replaced.is_some()
        && previous.replaced == next.replaced
        && differ_only_in_numbers(&previous.call.args, &next.call.args)
        && (previous.tx == next.tx || next.at.0 - previous.at.0 <= COALESCE_WINDOW.0)
}

/// The paths a patch replaced, if it did nothing else: no value added or removed.
fn replaced_paths(patch: &Patch) -> Option<BTreeSet<String>> {
    let only_replaces =
        !patch.changes.is_empty() && patch.changes.iter().all(|c| c.before.is_some() && c.after.is_some());
    only_replaces.then(|| patch.changes.iter().map(|change| change.path.to_string()).collect())
}

/// Whether two values are the same except for the numbers in them. Iterative, so a deep value can't overflow the stack.
fn differ_only_in_numbers(a: &Value, b: &Value) -> bool {
    let mut pending = vec![(a, b)];
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (Value::Number(_), Value::Number(_)) => {}
            (Value::Object(x), Value::Object(y)) if x.len() == y.len() => {
                for (key, left) in x {
                    match y.get(key) {
                        Some(right) => pending.push((left, right)),
                        None => return false,
                    }
                }
            }
            (Value::Array(x), Value::Array(y)) if x.len() == y.len() => pending.extend(x.iter().zip(y)),
            (a, b) if a == b => {}
            _ => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn numbers_may_differ_and_nothing_else() {
        assert!(differ_only_in_numbers(&json!({"path": "a/b", "value": 1}), &json!({"path": "a/b", "value": 2.5})));
        assert!(differ_only_in_numbers(&json!([1, {"k": [2]}]), &json!([3, {"k": [4]}])));
        assert!(!differ_only_in_numbers(&json!({"path": "a/b", "value": 1}), &json!({"path": "a/c", "value": 1})));
        assert!(!differ_only_in_numbers(&json!({"value": 1}), &json!({"value": "1"})));
        assert!(!differ_only_in_numbers(&json!({"a": 1}), &json!({"b": 1})));
        assert!(!differ_only_in_numbers(&json!([1, 2]), &json!([1])));
        assert!(!differ_only_in_numbers(&json!({"a": 1}), &json!({"a": 1, "b": 2})));
    }
}
