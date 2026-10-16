//! The hand-off to the journal (#251, design `journal.md` §5.1): every change goes to the sink as a physical entry, in
//! history order, so the entries' patches applied in order to the starting document give the executor's document;
//! `admit` gates applied edits, undo and redo but never commit or rollback; a requested mark is appended once.

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocWrite, Patch, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_history_api::HistoryEntry;
use omuzaki_history_mock::RecordingSink;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, ErrorCode, Grants, OpError, Request, Runner, TxId};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::json;

use super::{person, request};

pub(crate) fn bob() -> Actor {
    Actor::Person { name: "bob".into() }
}

/// An executor over `doc` that journals to a fresh sink.
pub(crate) fn journalled(doc: Value) -> (Executor, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::new());
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(doc)), clock).unwrap().with_journal(sink.clone());
    (exec, sink)
}

/// The patch an entry applied, if it changed the document.
pub(crate) fn applied(entry: &HistoryEntry) -> Option<&Patch> {
    match entry {
        HistoryEntry::Op { record, .. } => Some(&record.patch),
        HistoryEntry::Rollback { patch, .. }
        | HistoryEntry::Undo { patch, .. }
        | HistoryEntry::Redo { patch, .. }
        | HistoryEntry::TxRevert { patch, .. } => Some(patch),
        _ => None,
    }
}

/// One generated action of the seeded session.
#[derive(Debug, Clone)]
pub(crate) enum Act {
    Set { who: bool, key: &'static str, value: i64, join: bool },
    Begin { who: bool },
    Commit { who: bool },
    Rollback { who: bool },
    Undo { who: bool },
    Redo { who: bool },
}

pub(crate) fn act() -> impl Strategy<Value = Act> {
    let key = prop::sample::select(vec!["a", "b", "c"]);
    prop_oneof![
        5 => (any::<bool>(), key, 0..4_i64, any::<bool>()).prop_map(|(who, key, value, join)| Act::Set { who, key, value, join }),
        1 => any::<bool>().prop_map(|who| Act::Begin { who }),
        1 => any::<bool>().prop_map(|who| Act::Commit { who }),
        1 => any::<bool>().prop_map(|who| Act::Rollback { who }),
        2 => any::<bool>().prop_map(|who| Act::Undo { who }),
        1 => any::<bool>().prop_map(|who| Act::Redo { who }),
    ]
}

/// Plays `acts` on `exec` with two persons, each with at most one open transaction. Refusals (conflicts, nothing to
/// undo) are part of the session: they journal nothing, or a drop.
pub(crate) fn play(exec: &Executor, acts: &[Act], mut each: impl FnMut(usize)) {
    let mut open: [Option<TxId>; 2] = [None, None];
    for (n, act) in acts.iter().enumerate() {
        let who = |w: bool| if w { bob() } else { person() };
        let slot = |w: bool| usize::from(w);
        // A refusal is part of the session too: it journals nothing, or a drop.
        let outcome: Result<(), OpError> = match act {
            Act::Set { who: w, key, value, join } => {
                let tx = if *join { open[slot(*w)] } else { None };
                let req = Request { actor: who(*w), tx, ..request("test.set", json!({"path": key, "value": value})) };
                exec.run(req).map(drop)
            }
            Act::Begin { who: w } => {
                if open[slot(*w)].is_none() {
                    open[slot(*w)] = Some(exec.begin("generated", &who(*w)));
                }
                Ok(())
            }
            Act::Commit { who: w } => open[slot(*w)].take().map_or(Ok(()), |tx| exec.commit(tx, &who(*w))),
            Act::Rollback { who: w } => match open[slot(*w)] {
                Some(tx) => exec.rollback(tx, &who(*w)).map(|()| open[slot(*w)] = None),
                None => Ok(()),
            },
            Act::Undo { who: w } => exec.undo(&who(*w), &Grants::person()).map(drop),
            Act::Redo { who: w } => exec.redo(&who(*w), &Grants::person()).map(drop),
        };
        drop(outcome);
        each(n);
    }
}

pub(crate) fn document(exec: &Executor) -> Value {
    exec.get(&DocPath::root(), &Grants::person()).unwrap().unwrap()
}

#[test]
fn the_entries_patches_rebuild_the_document() {
    let config = Config { cases: 96, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    runner
        .run(&prop::collection::vec(act(), 1..40), |acts| {
            let start = json!({"a": 0, "b": 0, "c": 0});
            let (exec, sink) = journalled(start.clone());
            play(&exec, &acts, |_| {});
            let mut rebuilt = MemDoc::from_value(start);
            for entry in sink.entries() {
                if let Some(patch) = applied(&entry) {
                    rebuilt.apply(patch).map_err(|e| TestCaseError::fail(format!("{entry:?}: {e}")))?;
                }
            }
            prop_assert_eq!(rebuilt.root(), &document(&exec));
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_refusing_journal_refuses_edits_undo_and_redo_but_never_commit_or_rollback() {
    let (exec, sink) = journalled(json!({"a": 0}));
    exec.run(request("test.set", json!({"path": "a", "value": 1}))).unwrap();
    let open = exec.begin("closing stays possible", &person());
    exec.run(Request { tx: Some(open), ..request("test.set", json!({"path": "a", "value": 2})) }).unwrap();
    let rolled = exec.begin("so does this", &person());

    sink.script_admit(Err(OpError::unavailable("JournalSink").hint("saving is failing")));
    for refused in [
        exec.run(request("test.set", json!({"path": "a", "value": 3}))).map(drop),
        exec.undo(&person(), &Grants::person()).map(drop),
        exec.redo(&person(), &Grants::person()).map(drop),
    ] {
        assert_eq!(refused.unwrap_err().code, ErrorCode::Unavailable);
    }
    exec.run(request("test.get", json!({"path": "a"}))).unwrap();
    exec.commit(open, &person()).unwrap();
    exec.rollback(rolled, &person()).unwrap();
}

#[test]
fn a_requested_mark_is_appended_once_with_the_history() {
    let (exec, sink) = journalled(json!({"a": 0}));
    exec.run(request("test.set", json!({"path": "a", "value": 1}))).unwrap();
    sink.request_mark();
    exec.run(request("test.set", json!({"path": "a", "value": 2}))).unwrap();
    exec.run(request("test.set", json!({"path": "a", "value": 3}))).unwrap();
    let marks: Vec<_> = sink
        .entries()
        .into_iter()
        .filter_map(|e| match e {
            HistoryEntry::Mark { history } => Some(history),
            _ => None,
        })
        .collect();
    assert_eq!(marks.len(), 1);
    assert_eq!(marks[0].actors[0].undo.len(), 2, "the mark came right after the change that followed the request");
    exec.flush_history();
    assert_eq!(sink.entries().iter().filter(|e| matches!(e, HistoryEntry::Mark { .. })).count(), 2, "flush appends one");
}

#[test]
fn every_kind_of_change_is_journalled_in_order() {
    let (exec, sink) = journalled(json!({"a": 0, "b": 0}));
    let tx = exec.begin("t", &person());
    exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 1})) }).unwrap();
    exec.commit(tx, &person()).unwrap();
    exec.run(request("test.set", json!({"path": "b", "value": 1}))).unwrap();
    exec.undo(&person(), &Grants::person()).unwrap();
    exec.redo(&person(), &Grants::person()).unwrap();
    let rolled = exec.begin("r", &person());
    exec.run(Request { tx: Some(rolled), ..request("test.set", json!({"path": "a", "value": 2})) }).unwrap();
    exec.rollback(rolled, &person()).unwrap();
    let kinds: Vec<&str> = sink
        .entries()
        .iter()
        .map(|e| match e {
            HistoryEntry::Begin { .. } => "begin",
            HistoryEntry::Op { commits: false, .. } => "op",
            HistoryEntry::Op { commits: true, .. } => "op+commit",
            HistoryEntry::Commit { .. } => "commit",
            HistoryEntry::Undo { .. } => "undo",
            HistoryEntry::Redo { .. } => "redo",
            HistoryEntry::Rollback { .. } => "rollback",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["begin", "op", "commit", "op+commit", "undo", "redo", "begin", "op", "rollback"]);
}
