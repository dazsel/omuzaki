//! Undo histories (design `agent-sessions.md` §5.4, `journal.md` §9): each actor's undo and redo stacks, the open
//! transactions they come from, and the order across actors that per-principal undo needs.
//!
//! A step is a committed transaction: its combined patch (`Arc`-shared, so a history export copies pointers), the
//! scopes undoing it needs, and the effects its ops had outside the document, in apply order. Every step gets a commit
//! number when it is committed, and an undo number when it is undone, so a person's undo can take the newest step
//! among their own history and their agents' ([`Histories::pick`]).

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use omuzaki_doc_api::Patch;
use omuzaki_history_api::{ActorHistory, OpenTxExport, StepExport, WallTime};
use omuzaki_ops_api::{Actor, Effect, Grants, OpError, Scope, TxId, Undo, checks};

/// The undo depth per actor and the in-memory op log length (#150). Older steps fall off an actor's undo stack; older
/// records leave the in-memory log, and the journal's archive keeps both.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct HistoryLimits {
    /// Steps kept on each actor's undo stack.
    pub undo_depth: usize,
    /// Records kept in the in-memory op log.
    pub log_records: usize,
}

impl HistoryLimits {
    /// Keeps `undo_depth` steps per actor and `log_records` log records.
    pub fn new(undo_depth: usize, log_records: usize) -> Self {
        Self { undo_depth, log_records }
    }
}

impl Default for HistoryLimits {
    /// 1,000 steps per actor and 10,000 records (`journal.md` §9).
    fn default() -> Self {
        Self { undo_depth: 1_000, log_records: 10_000 }
    }
}

/// What a transaction has done so far: the patch, scopes and effects of its ops, in apply order.
#[derive(Clone, Debug, Default)]
pub(crate) struct Draft {
    pub(crate) patch: Patch,
    pub(crate) scopes: BTreeSet<Scope>,
    pub(crate) effects: Vec<Effect>,
}

impl Draft {
    /// Adds one applied op: its patch, the scope [`checks::step_scope`] gives it, and its effect.
    pub(crate) fn absorb(&mut self, scope: Scope, undo: Undo, patch: &Patch, effect: Option<&Effect>) {
        self.scopes.extend(checks::step_scope(scope, undo, patch));
        self.effects.extend(effect.cloned());
        self.patch.extend(patch.clone());
    }

    /// Whether the transaction belongs in history: it changed the document, or something outside it that undo can
    /// reverse ([`checks::in_history`]).
    pub(crate) fn changes_anything(&self) -> bool {
        checks::in_history(&self.patch, !self.effects.is_empty())
    }
}

/// A committed transaction on an undo or redo stack.
#[derive(Clone, Debug)]
pub(crate) struct Step {
    pub(crate) tx: TxId,
    pub(crate) label: String,
    pub(crate) patch: Arc<Patch>,
    pub(crate) scopes: BTreeSet<Scope>,
    pub(crate) effects: Vec<Effect>,
    /// Its commit number: a person's undo takes the newest step across their own and their agents' histories.
    pub(crate) order: u64,
    /// Its undo number, once undone: a person's redo takes the most recently undone.
    pub(crate) undone: u64,
}

impl Step {
    /// The step a committed `draft` becomes.
    pub(crate) fn committed(tx: TxId, label: String, draft: Draft) -> Self {
        Self {
            tx,
            label,
            patch: Arc::new(draft.patch),
            scopes: draft.scopes,
            effects: draft.effects,
            order: 0,
            undone: 0,
        }
    }

    /// Undoing or redoing needs every scope the step recorded.
    pub(crate) fn require(&self, actor: &Actor, grants: &Grants, verb: &str) -> Result<(), OpError> {
        checks::require_step_scopes(self.scopes.iter().copied(), grants, actor, verb, &self.label)
    }

    fn export(&self) -> StepExport {
        StepExport::new(self.tx, self.label.clone(), self.patch.clone(), self.scopes.clone())
            .with_effects(self.effects.clone())
    }

    fn import(step: &StepExport) -> Self {
        Self {
            tx: step.tx,
            label: step.label.clone(),
            patch: step.patch.clone(),
            scopes: step.scopes.clone(),
            effects: step.effects.clone(),
            order: 0,
            undone: 0,
        }
    }
}

/// A transaction that has begun and not yet committed or rolled back.
#[derive(Clone, Debug)]
pub(crate) struct OpenTx {
    pub(crate) owner: Actor,
    pub(crate) label: String,
    pub(crate) draft: Draft,
    /// The `OpApplied` events of driver jobs' inner edits, held until this transaction commits (ADR 0036 decision 10),
    /// so a run that is rolled back is never seen.
    pub(crate) held: Vec<omuzaki_ops_api::Event>,
    /// Which of the draft's changes and effects each driver job's inner edit added, so a joined driver that fails can
    /// revert only its own.
    pub(crate) driven: Vec<Driven>,
}

/// One inner edit of a driver job inside a transaction: its record and where its changes and effects sit in the
/// transaction's draft.
#[derive(Clone, Debug)]
pub(crate) struct Driven {
    pub(crate) job: omuzaki_ops_api::JobId,
    pub(crate) seq: u64,
    pub(crate) changes: std::ops::Range<usize>,
    pub(crate) effects: std::ops::Range<usize>,
}

impl OpenTx {
    pub(crate) fn export(&self, tx: TxId) -> OpenTxExport {
        OpenTxExport::new(
            self.owner.clone(),
            tx,
            self.label.clone(),
            self.draft.patch.clone(),
            self.draft.scopes.clone(),
        )
        .with_effects(self.draft.effects.clone())
    }

    pub(crate) fn import(open: &OpenTxExport) -> Self {
        let draft = Draft { patch: open.patch.clone(), scopes: open.scopes.clone(), effects: open.effects.clone() };
        Self::new(open.owner.clone(), open.label.clone(), draft)
    }

    pub(crate) fn new(owner: Actor, label: String, draft: Draft) -> Self {
        Self { owner, label, draft, held: Vec::new(), driven: Vec::new() }
    }
}

/// Which way a step moves.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Undo,
    Redo,
}

impl Direction {
    pub(crate) fn verb(self) -> &'static str {
        match self {
            Direction::Undo => "undo",
            Direction::Redo => "redo",
        }
    }

    /// What a conflicting step's actor should do next. The step is dropped from the history so that it can never
    /// wedge it: the next call reaches the step before.
    pub(crate) fn dropped_hint(self) -> &'static str {
        match self {
            Direction::Undo => {
                "that step is dropped from the undo history, so the next undo reaches the step before it; \
                 run an op to make the change again if you still want it"
            }
            Direction::Redo => {
                "that step is dropped from the redo history, so the next redo reaches the one before it; \
                 run an op to make the change again if you still want it"
            }
        }
    }
}

/// One actor's undo and redo stacks, oldest first.
pub(crate) struct History {
    pub(crate) actor: Actor,
    pub(crate) undo: VecDeque<Step>,
    pub(crate) redo: Vec<Step>,
    /// When the actor last acted, in wall time (`WallTime(0)`: not known).
    pub(crate) last_seen: WallTime,
}

impl History {
    fn top(&self, direction: Direction) -> Option<&Step> {
        match direction {
            Direction::Undo => self.undo.back(),
            Direction::Redo => self.redo.last(),
        }
    }
}

/// Every actor's history, with the commit and undo counters across them and the undo depth.
pub(crate) struct Histories {
    list: Vec<History>,
    commits: u64,
    undos: u64,
    depth: usize,
}

impl Histories {
    pub(crate) fn new(depth: usize) -> Self {
        Self { list: Vec::new(), commits: 0, undos: 0, depth }
    }

    pub(crate) fn set_depth(&mut self, depth: usize) {
        self.depth = depth;
    }

    /// The index of `actor`'s history, if it has one.
    pub(crate) fn find(&self, actor: &Actor) -> Option<usize> {
        self.list.iter().position(|h| h.actor == *actor)
    }

    /// `actor`'s history, created on first use.
    pub(crate) fn of(&mut self, actor: &Actor) -> &mut History {
        let index = self.find(actor).unwrap_or_else(|| {
            self.list.push(History {
                actor: actor.clone(),
                undo: VecDeque::new(),
                redo: Vec::new(),
                last_seen: WallTime(0),
            });
            self.list.len() - 1
        });
        &mut self.list[index]
    }

    pub(crate) fn get(&self, index: usize) -> &History {
        &self.list[index]
    }

    pub(crate) fn get_mut(&mut self, index: usize) -> &mut History {
        &mut self.list[index]
    }

    /// Pushes a committed `step` onto `owner`'s undo stack, numbered after every step so far, and clears `owner`'s
    /// redo. Returns the steps that fell off the bottom (past the depth) and the redo steps cleared, oldest first, for
    /// the journal.
    pub(crate) fn commit(&mut self, owner: &Actor, mut step: Step) -> (Vec<Step>, Vec<Step>) {
        self.commits += 1;
        step.order = self.commits;
        let depth = self.depth;
        let history = self.of(owner);
        let cleared = std::mem::take(&mut history.redo);
        history.undo.push_back(step);
        let over = history.undo.len().saturating_sub(depth);
        (history.undo.drain(..over).collect(), cleared)
    }

    /// Takes every redo step of `actor` off its stack, oldest first.
    pub(crate) fn clear_redo(&mut self, actor: &Actor) -> Vec<Step> {
        self.find(actor).map(|index| std::mem::take(&mut self.list[index].redo)).unwrap_or_default()
    }

    /// The history among `candidates` whose next step in `direction` is the newest: the newest commit for undo, the
    /// most recently undone for redo.
    pub(crate) fn pick(&self, direction: Direction, candidates: &[Actor]) -> Option<usize> {
        candidates
            .iter()
            .filter_map(|actor| self.find(actor))
            .filter_map(|index| self.list[index].top(direction).map(|step| (index, step)))
            .max_by_key(|(_, step)| match direction {
                Direction::Undo => step.order,
                Direction::Redo => step.undone,
            })
            .map(|(index, _)| index)
    }

    /// Takes the next step in `direction` off history `index`.
    pub(crate) fn take(&mut self, index: usize, direction: Direction) -> Option<Step> {
        let history = &mut self.list[index];
        match direction {
            Direction::Undo => history.undo.pop_back(),
            Direction::Redo => history.redo.pop(),
        }
    }

    /// Puts a step taken with [`take`](Self::take) back where it was: it was refused and stays.
    pub(crate) fn put_back(&mut self, index: usize, direction: Direction, step: Step) {
        let history = &mut self.list[index];
        match direction {
            Direction::Undo => history.undo.push_back(step),
            Direction::Redo => history.redo.push(step),
        }
    }

    /// Lands a step moved in `direction` on the other stack of history `index`: an undone step goes on the redo
    /// stack with the next undo number; a redone one goes back on the undo stack.
    pub(crate) fn land(&mut self, index: usize, direction: Direction, mut step: Step) {
        match direction {
            Direction::Undo => {
                self.undos += 1;
                step.undone = self.undos;
                self.list[index].redo.push(step);
            }
            Direction::Redo => self.list[index].undo.push_back(step),
        }
    }

    /// Removes step `tx` from `actor`'s `stack`, wherever it is in it (replay of a dropped step). Returns whether it
    /// was there.
    pub(crate) fn remove(&mut self, actor: &Actor, direction: Direction, tx: TxId) -> bool {
        let Some(index) = self.find(actor) else { return false };
        let history = &mut self.list[index];
        match direction {
            Direction::Undo => history.undo.iter().position(|s| s.tx == tx).and_then(|at| history.undo.remove(at)),
            Direction::Redo => history.redo.iter().position(|s| s.tx == tx).map(|at| history.redo.remove(at)),
        }
        .is_some()
    }

    /// Notes that `actor` acted at `at`.
    pub(crate) fn seen(&mut self, actor: &Actor, at: WallTime) {
        if let Some(index) = self.find(actor) {
            self.list[index].last_seen = at;
        }
    }

    /// Every history as data, with the actors not seen since `before` left out.
    pub(crate) fn export(&self, before: Option<WallTime>) -> Vec<ActorHistory> {
        self.list
            .iter()
            .filter(|h| !stale(h.last_seen, before))
            .map(|h| {
                let mut actor = ActorHistory::new(h.actor.clone(), h.last_seen);
                actor.undo = h.undo.iter().map(Step::export).collect();
                // In the order they will be redone: the stack's last first.
                actor.redo = h.redo.iter().rev().map(Step::export).collect();
                actor
            })
            .collect()
    }

    /// Replaces every history with `actors`, leaving out those not seen since `before`. Steps are numbered in the
    /// order they are read: each actor's undo stack oldest first, then the redo stacks, so a person's undo after a
    /// restore reaches the most recently listed actor's newest step first.
    pub(crate) fn import(&mut self, actors: &[ActorHistory], before: Option<WallTime>) {
        self.list.clear();
        for actor in actors.iter().filter(|a| !stale(a.last_seen, before)) {
            let undo = actor
                .undo
                .iter()
                .map(|s| {
                    self.commits += 1;
                    Step { order: self.commits, ..Step::import(s) }
                })
                .collect();
            // The export lists redo steps next first; the stack keeps the next one last.
            let redo = actor
                .redo
                .iter()
                .rev()
                .map(|s| {
                    self.undos += 1;
                    Step { undone: self.undos, ..Step::import(s) }
                })
                .collect();
            self.list.push(History { actor: actor.actor.clone(), undo, redo, last_seen: actor.last_seen });
        }
    }

    /// Every history, for tests and the per-step walk of undo-session.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &History> {
        self.list.iter()
    }
}

/// Whether an actor last seen at `seen` is older than `before`. An unknown time (`WallTime(0)`) is never stale: an
/// executor without a wall clock cannot say.
fn stale(seen: WallTime, before: Option<WallTime>) -> bool {
    before.is_some_and(|before| seen != WallTime(0) && seen < before)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn step(tx: u64) -> Step {
        let patch = Patch::new().with(omuzaki_doc_api::DocPath::parse("a").unwrap(), None, Some(json!(tx)));
        Step::committed(TxId(tx), format!("s{tx}"), Draft { patch, ..Draft::default() })
    }

    #[test]
    fn the_newest_step_across_actors_is_picked_and_old_ones_fall_off() {
        let (a, b) = (Actor::Person { name: "a".into() }, Actor::Agent { id: "b#1".into(), model: None });
        let mut histories = Histories::new(2);
        for tx in [1, 3] {
            histories.commit(&a, step(tx));
        }
        histories.commit(&b, step(2));
        let (fell, _) = histories.commit(&a, step(4));
        assert_eq!(fell.iter().map(|s| s.tx).collect::<Vec<_>>(), vec![TxId(1)], "depth 2");
        let both = [a.clone(), b.clone()];
        let first = histories.pick(Direction::Undo, &both).unwrap();
        assert_eq!(histories.get(first).actor, a);
        for _ in 0..2 {
            let undone = histories.take(first, Direction::Undo).unwrap();
            histories.land(first, Direction::Undo, undone);
        }
        assert_eq!(histories.get(histories.pick(Direction::Undo, &both).unwrap()).actor, b, "s2 is next");
        let redo = histories.pick(Direction::Redo, &both).unwrap();
        assert_eq!(histories.get(redo).redo.last().unwrap().tx, TxId(3), "the last undone redoes first");
    }
}
