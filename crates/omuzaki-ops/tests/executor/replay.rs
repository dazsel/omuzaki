//! Restore and replay (#155, `journal.md` §8.3, §9): executor B, restored from a mark's history and the document at
//! that mark and then fed the rest of A's journal, ends where A is: the same document, the same labels on every
//! actor's stacks, the same next undo and redo, ids that continue. A tampered entry stops replay at its LSN.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::Patch;
use omuzaki_doc_mock::MemDoc;
use omuzaki_history_api::{HistoryEntry, HistoryExport, Lsn, WallTime};
use omuzaki_history_mock::FakeWallClock;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Grants, OpRecord, Runner};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestRng, TestRunner};
use serde_json::{Value, json};

use super::journal::{act, applied, bob, document, journalled, play};
use super::person;

fn restored(doc: Value, history: &HistoryExport) -> Executor {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let wall = Arc::new(FakeWallClock::new(WallTime(1)));
    Executor::restore(Box::new(MemDoc::from_value(doc)), clock, wall, history).unwrap()
}

/// Every actor's undo and redo labels, by actor.
fn labels(exec: &Executor) -> BTreeMap<String, (Vec<String>, Vec<String>)> {
    exec.history_export()
        .actors
        .into_iter()
        .map(|a| {
            let names = |steps: &[omuzaki_history_api::StepExport]| steps.iter().map(|s| s.label.clone()).collect();
            (format!("{:?}", a.actor), (names(&a.undo), names(&a.redo)))
        })
        .collect()
}

#[test]
fn a_restored_executor_replaying_the_rest_ends_where_the_first_did() {
    let config = Config { cases: 96, failure_persistence: None, ..Config::default() };
    let mut runner = TestRunner::new_with_rng(config, TestRng::deterministic_rng(RngAlgorithm::ChaCha));
    runner
        .run(&(prop::collection::vec(act(), 2..40), any::<prop::sample::Index>()), |(acts, at)| {
            let (a, sink) = journalled(json!({"a": 0, "b": 0, "c": 0}));
            let mark_at = at.index(acts.len());
            let mut base = None;
            play(&a, &acts, |n| {
                if n == mark_at {
                    sink.request_mark();
                }
                // The mark follows the next change; the document then is the snapshot's.
                if base.is_none() && sink.entries().iter().any(|e| matches!(e, HistoryEntry::Mark { .. })) {
                    base = Some(document(&a));
                }
            });
            // A mark asked for after the last change comes with the clean close's flush.
            let base = base.or_else(|| Some(document(&a)));
            a.flush_history();
            let entries = sink.numbered();
            let Some(mark) = entries.iter().position(|(_, e)| matches!(e, HistoryEntry::Mark { .. })) else {
                return Ok(());
            };
            let HistoryEntry::Mark { history } = &entries[mark].1 else { unreachable!() };
            let base = base.ok_or_else(|| TestCaseError::fail("a mark without its document"))?;
            let b = restored(base, history);
            b.replay(&entries[mark + 1..]).map_err(|e| TestCaseError::fail(e.to_string()))?;

            prop_assert_eq!(document(&b), document(&a));
            prop_assert_eq!(labels(&b), labels(&a));
            let (ha, hb) = (a.history_export(), b.history_export());
            prop_assert_eq!((hb.next_tx, hb.next_seq), (ha.next_tx, ha.next_seq), "ids continue");
            for actor in [person(), bob()] {
                let (ua, ub) = (a.undo(&actor, &Grants::person()), b.undo(&actor, &Grants::person()));
                prop_assert_eq!(ua.map_err(|e| e.code), ub.map_err(|e| e.code));
                prop_assert_eq!(document(&b), document(&a));
                let (ra, rb) = (a.redo(&actor, &Grants::person()), b.redo(&actor, &Grants::person()));
                prop_assert_eq!(ra.map_err(|e| e.code), rb.map_err(|e| e.code));
                prop_assert_eq!(document(&b), document(&a));
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_tampered_entry_stops_replay_at_its_lsn() {
    let (a, sink) = journalled(json!({"a": 0}));
    for value in 1..=3 {
        a.run(super::request("test.set", json!({"path": "a", "value": value}))).unwrap();
    }
    let mut entries = sink.numbered();
    let HistoryEntry::Op { record, commits } = &entries[1].1 else { panic!("{:?}", entries[1]) };
    let mut tampered = OpRecord::clone(record);
    tampered.patch = Patch::new().with(omuzaki_doc_api::DocPath::parse("a").unwrap(), Some(json!(99)), Some(json!(2)));
    entries[1].1 = HistoryEntry::Op { record: Arc::new(tampered), commits: *commits };
    let b = restored(json!({"a": 0}), &HistoryExport::empty());
    let stopped = b.replay(&entries).unwrap_err();
    assert_eq!(stopped.lsn, entries[1].0);
    assert_eq!(document(&b), json!({"a": 1}), "the entries before it stay replayed");
    assert!(stopped.lsn > Lsn(0) && applied(&entries[1].1).is_some());
}
