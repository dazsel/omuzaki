//! The undo depth and the in-memory op log are capped (#150; the journal's archive keeps the rest): history and the
//! log stay the same size however long a session runs, sequence numbers keep counting, and a conflict whose cause is
//! older than what the executor knows says so.

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_history_api::{ActorHistory, HistoryExport, StepExport, WallTime};
use omuzaki_history_mock::FakeWallClock;
use omuzaki_ops::{Executor, HistoryLimits};
use omuzaki_ops_api::{ErrorCode, Grants, Runner, TxId};
use serde_json::json;

use super::{person, request};

fn capped(depth: usize, records: usize) -> Executor {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    Executor::new(Box::new(MemDoc::from_value(json!({"a": 0}))), clock)
        .unwrap()
        .with_history_limits(HistoryLimits::new(depth, records))
}

fn set(exec: &Executor, value: u64) {
    exec.run(request("test.set", json!({"path": "a", "value": value}))).unwrap();
}

#[test]
fn the_oldest_steps_fall_off_past_the_depth() {
    let exec = capped(3, 100);
    for value in 1..=5 {
        set(&exec, value);
    }
    for expect in [4, 3, 2] {
        exec.undo(&person(), &Grants::person()).unwrap().unwrap();
        assert_eq!(exec.get(&DocPath::parse("a").unwrap(), &Grants::person()).unwrap(), Some(json!(expect)));
    }
    assert_eq!(exec.undo(&person(), &Grants::person()).unwrap(), None, "steps 1 and 2 fell off");
}

#[test]
fn the_log_keeps_the_newest_records_and_seq_keeps_counting() {
    let exec = capped(1_000, 10);
    for value in 1..=25 {
        set(&exec, value);
    }
    let kept = exec.log(0, usize::MAX, &Grants::person()).unwrap();
    assert_eq!(kept.iter().map(|r| r.seq).collect::<Vec<_>>(), (16..=25).collect::<Vec<_>>());
    let after = exec.log(20, 2, &Grants::person()).unwrap();
    assert_eq!(after.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![21, 22]);
}

#[test]
fn memory_stays_flat_over_a_hundred_thousand_ops() {
    let exec = capped(10, 100);
    for value in 0..100_000 {
        set(&exec, value);
    }
    assert_eq!(exec.log(0, usize::MAX, &Grants::person()).unwrap().len(), 100);
    let history = exec.history_export();
    assert_eq!(history.actors.iter().map(|a| a.undo.len()).sum::<usize>(), 10);
    assert_eq!(history.next_seq, 100_001);
}

#[test]
fn a_conflict_older_than_the_session_says_so() {
    // A restored step whose patch the document no longer matches: the change came before this executor.
    let patch = Patch::new().with(DocPath::parse("a").unwrap(), Some(json!(1)), Some(json!(2)));
    let mut actor = ActorHistory::new(person(), WallTime(0));
    actor.undo.push(StepExport::new(TxId(1), "old", Arc::new(patch), Default::default()));
    let mut history = HistoryExport::empty();
    history.actors.push(actor);
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let wall = Arc::new(FakeWallClock::new(WallTime(1)));
    let exec = Executor::restore(Box::new(MemDoc::from_value(json!({"a": 7}))), clock, wall, &history).unwrap();
    let e = exec.undo(&person(), &Grants::person()).unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(e.message.contains("before this session"), "{e}");
}
