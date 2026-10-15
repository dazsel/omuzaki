//! Property tests. Many generated edits are invalid on purpose (a parent that doesn't exist, removing nothing, a
//! path through a number, publishing without the scope): refusing them must leave no residue. The seed is fixed, so a
//! failure reproduces on every machine.
//!
//! 1. One actor, sequential: undoing everything restores the original document exactly.
//! 2. Two or three actors with different grants, transactions interleaved, random commits, rollbacks (by owners and
//!    by strangers) and undos/redos by random actors. After every action the document equals the replay of the
//!    event stream (every applied op, rollback, undo and redo), undo never reaches across actors, and no sequence
//!    wedges history: unwinding each actor's history always ends, with conflicts as the only errors.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;

use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Event, Grants, Request, Runner, Scope, TxId};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::{Value, json};

use super::{agent, path, person, plugin, request, rig};

#[derive(Debug, Clone)]
enum Edit {
    Set(String, Value),
    Remove(String),
    /// Needs the `publish` scope, which only the person has: everyone else's attempts are refused.
    Publish(String, Value),
}

fn doc_path() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(vec!["a", "b", "c"]), 1..=3).prop_map(|keys| keys.join("/"))
}

fn value() -> impl Strategy<Value = Value> {
    prop_oneof![
        (0..5_i64).prop_map(|n| json!(n)),
        "[a-z]{0,3}".prop_map(|s| json!(s)),
        Just(json!({})),
        (0..3_i64).prop_map(|n| json!({"x": n, "nested": {"y": [n]}})),
    ]
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        4 => (doc_path(), value()).prop_map(|(p, v)| Edit::Set(p, v)),
        2 => doc_path().prop_map(Edit::Remove),
        1 => (doc_path(), value()).prop_map(|(p, v)| Edit::Publish(p, v)),
    ]
}

fn edit_request(edit: &Edit, tx: Option<TxId>) -> Request {
    let req = match edit {
        Edit::Set(p, v) => request("test.set", json!({"path": p, "value": v})),
        Edit::Remove(p) => request("test.remove", json!({"path": p})),
        Edit::Publish(p, v) => request("test.publish", json!({"path": p, "value": v})),
    };
    Request { tx, ..req }
}

fn deterministic_runner() -> TestRunner {
    let config = Config { cases: 256, failure_persistence: None, ..Config::default() };
    TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha))
}

fn original() -> Value {
    json!({"a": {"b": {"c": 1}, "c": 2}, "b": 3})
}

// ---------------------------------------------------------------------------------------------------------------
// 1. One actor, sequential
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum SoloAction {
    Edit(Edit),
    Transaction { edits: Vec<Edit>, commit: bool },
    Undo,
    Redo,
}

fn solo_action() -> impl Strategy<Value = SoloAction> {
    prop_oneof![
        6 => edit().prop_map(SoloAction::Edit),
        2 => (prop::collection::vec(edit(), 0..4), any::<bool>())
            .prop_map(|(edits, commit)| SoloAction::Transaction { edits, commit }),
        1 => Just(SoloAction::Undo),
        1 => Just(SoloAction::Redo),
    ]
}

#[test]
fn one_actor_can_always_undo_everything_back_to_the_original_document() {
    let outcome = deterministic_runner().run(&prop::collection::vec(solo_action(), 0..24), |actions| {
        let rig = rig(original());
        let root = path("");
        let alice = person();
        for action in &actions {
            match action {
                // Refused edits are expected; they must simply change nothing.
                SoloAction::Edit(e) => drop(rig.exec.run(edit_request(e, None))),
                SoloAction::Transaction { edits, commit } => {
                    let before = rig.exec.get(&root, &Grants::person()).unwrap();
                    let tx = rig.exec.begin("generated", &alice);
                    for e in edits {
                        drop(rig.exec.run(edit_request(e, Some(tx))));
                    }
                    if *commit {
                        rig.exec.commit(tx, &alice).unwrap();
                    } else {
                        rig.exec.rollback(tx, &alice).unwrap();
                        prop_assert_eq!(
                            rig.exec.get(&root, &Grants::person()).unwrap(),
                            before,
                            "rollback must restore the document"
                        );
                    }
                }
                SoloAction::Undo => drop(rig.undo()),
                SoloAction::Redo => drop(rig.redo()),
            }
        }

        let finished = rig.exec.get(&root, &Grants::person()).unwrap();
        let mut undone = 0;
        while rig.undo().unwrap().is_some() {
            undone += 1;
        }
        prop_assert_eq!(
            rig.exec.get(&root, &Grants::person()).unwrap(),
            Some(original()),
            "full undo restores the original"
        );
        // Redoing exactly what was just undone lands back on the final document (the redo stack may hold more, from
        // undos earlier in the sequence).
        for _ in 0..undone {
            prop_assert!(rig.redo().unwrap().is_some());
        }
        prop_assert_eq!(
            rig.exec.get(&root, &Grants::person()).unwrap(),
            finished,
            "redoing everything undone restores the final document"
        );
        Ok(())
    });
    outcome.unwrap();
}

// ---------------------------------------------------------------------------------------------------------------
// 2. Several actors, interleaved
// ---------------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Action {
    /// `by` opens a transaction (it joins the open list).
    Begin {
        by: usize,
    },
    /// `by` applies an edit, inside the open transaction picked by `join` (maybe someone else's: then refused), or on
    /// its own.
    Edit {
        by: usize,
        edit: Edit,
        join: Option<usize>,
    },
    /// `by` tries to commit the open transaction picked by `slot` (maybe someone else's: then refused).
    Commit {
        slot: usize,
        by: usize,
    },
    /// `by` tries to roll back the open transaction picked by `slot`.
    Rollback {
        slot: usize,
        by: usize,
    },
    Undo {
        by: usize,
    },
    Redo {
        by: usize,
    },
}

fn action() -> impl Strategy<Value = Action> {
    let actor = 0..3_usize;
    prop_oneof![
        2 => actor.clone().prop_map(|by| Action::Begin { by }),
        6 => (actor.clone(), edit(), prop::option::weighted(0.6, 0..4_usize))
            .prop_map(|(by, edit, join)| Action::Edit { by, edit, join }),
        2 => (0..4_usize, actor.clone()).prop_map(|(slot, by)| Action::Commit { slot, by }),
        1 => (0..4_usize, actor.clone()).prop_map(|(slot, by)| Action::Rollback { slot, by }),
        3 => actor.clone().prop_map(|by| Action::Undo { by }),
        1 => actor.prop_map(|by| Action::Redo { by }),
    ]
}

/// A model of the document built only from the event stream: every applied op's patch, and the combined patch of a
/// transaction (inverted for rollback and undo) when it is rolled back, undone or redone. If the executor ever
/// changed the document in a way its log and events don't describe, the two diverge.
struct Replay {
    doc: MemDoc,
    events: Receiver<Event>,
    patches: HashMap<TxId, Patch>,
    owners: HashMap<TxId, Actor>,
    commits: usize,
}

impl Replay {
    fn new(events: Receiver<Event>) -> Self {
        Self {
            doc: MemDoc::from_value(original()),
            events,
            patches: HashMap::new(),
            owners: HashMap::new(),
            commits: 0,
        }
    }

    /// Consumes the events produced by one action of `acting`, applying them to the model.
    fn catch_up(&mut self, acting: &Actor) -> Result<(), TestCaseError> {
        let events: Vec<Event> = self.events.try_iter().collect();
        for event in events {
            match event {
                Event::OpApplied { record, .. } => {
                    self.apply(&record.patch, &format!("logged op {} ({})", record.seq, record.op))?;
                    self.owners.insert(record.tx, record.actor.clone());
                    self.patches.entry(record.tx).or_default().extend(record.patch);
                }
                Event::TxCommitted { .. } => self.commits += 1,
                Event::TxRolledBack { tx, .. } => self.replay_tx(tx, true, None)?,
                Event::Undone { tx, .. } => self.replay_tx(tx, true, Some(acting))?,
                Event::Redone { tx, .. } => self.replay_tx(tx, false, Some(acting))?,
                // This executor has no sessions, approvals or jobs yet, so these never arrive, and
                // none of them changes the document (a job's edits arrive as `OpApplied`).
                Event::SessionOpened { .. }
                | Event::SessionChanged { .. }
                | Event::SessionEnded { .. }
                | Event::SessionUndone { .. }
                | Event::ApprovalRequested { .. }
                | Event::ApprovalResolved { .. }
                | Event::JobChanged { .. }
                | Event::JobProgress { .. }
                | Event::Clash { .. } => {}
            }
        }
        Ok(())
    }

    fn replay_tx(&mut self, tx: TxId, inverse: bool, must_be_owned_by: Option<&Actor>) -> Result<(), TestCaseError> {
        if let Some(actor) = must_be_owned_by {
            prop_assert_eq!(self.owners.get(&tx), Some(actor), "undo and redo never reach another actor's step");
        }
        let patch = self.patches.get(&tx).cloned().unwrap_or_default();
        let patch = if inverse { patch.inverse() } else { patch };
        self.apply(&patch, &format!("transaction {}", tx.0))
    }

    fn apply(&mut self, patch: &Patch, what: &str) -> Result<(), TestCaseError> {
        self.doc
            .apply(patch)
            .map(|_| ())
            .map_err(|e| TestCaseError::fail(format!("{what} doesn't apply to the replay: {e}")))
    }
}

struct World {
    rig: super::Rig,
    cast: Vec<(Actor, Grants)>,
    open: Vec<(TxId, usize)>,
    replay: Replay,
}

impl World {
    fn new(actors: usize) -> Self {
        let rig = rig(original());
        let replay = Replay::new(rig.subscribe());
        let everyone = [
            (person(), Grants::person()),
            (agent(), Grants::agent_default()),
            (plugin(), Grants::of([Scope::Read, Scope::EditCharacter])),
        ];
        Self { rig, cast: everyone.into_iter().take(actors).collect(), open: Vec::new(), replay }
    }

    fn actor(&self, index: usize) -> &(Actor, Grants) {
        &self.cast[index % self.cast.len()]
    }

    /// The open transaction picked by `slot`, as (id, owner index).
    fn pick(&self, slot: usize) -> Option<(TxId, usize)> {
        (!self.open.is_empty()).then(|| self.open[slot % self.open.len()])
    }

    fn step(&mut self, action: &Action) -> Result<Actor, TestCaseError> {
        match *action {
            Action::Begin { by } => {
                let (actor, _) = self.actor(by).clone();
                let tx = self.rig.exec.begin("generated", &actor);
                self.open.push((tx, by % self.cast.len()));
                Ok(actor)
            }
            Action::Edit { by, ref edit, join } => {
                let (actor, grants) = self.actor(by).clone();
                let tx = join.and_then(|slot| self.pick(slot)).map(|(tx, _)| tx);
                // Refusals (a stranger's transaction, a missing scope, a bad path) are expected: they change nothing.
                drop(self.rig.exec.run(Request { actor: actor.clone(), grants, ..edit_request(edit, tx) }));
                Ok(actor)
            }
            Action::Commit { slot, by } => self.close(slot, by, false),
            Action::Rollback { slot, by } => self.close(slot, by, true),
            Action::Undo { by } => {
                let (actor, grants) = self.actor(by).clone();
                if let Err(e) = self.rig.exec.undo(&actor, &grants) {
                    prop_assert_eq!(e.code, ErrorCode::Conflict, "undo only ever fails by conflict here: {}", e);
                }
                Ok(actor)
            }
            Action::Redo { by } => {
                let (actor, grants) = self.actor(by).clone();
                if let Err(e) = self.rig.exec.redo(&actor, &grants) {
                    prop_assert_eq!(e.code, ErrorCode::Conflict, "redo only ever fails by conflict here: {}", e);
                }
                Ok(actor)
            }
        }
    }

    /// `by` tries to commit or roll back an open transaction: the owner succeeds (a rollback may conflict and then
    /// leaves it open), anyone else is refused and changes nothing.
    fn close(&mut self, slot: usize, by: usize, rollback: bool) -> Result<Actor, TestCaseError> {
        let (actor, _) = self.actor(by).clone();
        let Some((tx, owner)) = self.pick(slot) else { return Ok(actor) };
        let result = if rollback { self.rig.exec.rollback(tx, &actor) } else { self.rig.exec.commit(tx, &actor) };
        let is_owner = owner == by % self.cast.len();
        match result {
            Ok(()) => {
                prop_assert!(is_owner, "only the owner may close a transaction");
                self.open.retain(|(open, _)| *open != tx);
            }
            Err(e) if is_owner => {
                prop_assert!(rollback && e.code == ErrorCode::Conflict, "an owner's commit never fails: {}", e);
            }
            Err(e) => prop_assert_eq!(e.code, ErrorCode::Denied),
        }
        Ok(actor)
    }

    fn check(&mut self, acting: &Actor) -> Result<(), TestCaseError> {
        self.replay.catch_up(acting)?;
        prop_assert_eq!(
            self.rig.exec.get(&path(""), &Grants::person()).unwrap(),
            self.replay.doc.get(&path("")).cloned(),
            "the document equals the replay of the event stream"
        );
        Ok(())
    }

    /// Unwinds `by`'s whole history with `step` (undo or redo). Every call consumes one step, by success or by
    /// dropping a conflicting one, so it must report `None` within (total steps + 1) calls.
    fn unwind(&mut self, by: usize, redo: bool) -> Result<(), TestCaseError> {
        let (actor, grants) = self.actor(by).clone();
        let bound = self.replay.commits + 1;
        for _ in 0..=bound {
            let result = if redo { self.rig.exec.redo(&actor, &grants) } else { self.rig.exec.undo(&actor, &grants) };
            self.check(&actor)?;
            match result {
                Ok(None) => return Ok(()),
                Ok(Some(_)) => {}
                Err(e) => prop_assert_eq!(e.code, ErrorCode::Conflict, "{}", e),
            }
        }
        Err(TestCaseError::fail(format!("{actor:?}'s history is wedged: still not empty after {bound} calls")))
    }
}

#[test]
fn interleaved_actors_keep_the_document_replayable_and_history_unwedgeable() {
    let strategy = (2..=3_usize, prop::collection::vec(action(), 0..40));
    let outcome = deterministic_runner().run(&strategy, |(actors, actions)| {
        let mut world = World::new(actors);
        for action in &actions {
            let acting = world.step(action)?;
            world.check(&acting)?;
        }

        // Owners close what they left open: committing is always possible.
        for (tx, owner) in std::mem::take(&mut world.open) {
            let (actor, _) = world.actor(owner).clone();
            world.rig.exec.commit(tx, &actor).unwrap();
            world.check(&actor)?;
        }
        for by in 0..actors {
            world.unwind(by, false)?;
        }
        for by in 0..actors {
            world.unwind(by, true)?;
        }
        Ok(())
    });
    outcome.unwrap();
}
