//! Driver jobs (#1092, ADR 0036 decision 10): a job whose `work` edits through inner calls is one transaction, one
//! undo entry, its events held for the commit; cancel, error or a cap rolls back everything it did; joined to a
//! caller's transaction it reverts only its own edits, and never overwrites another actor.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, LazyLock, Mutex};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_history_api::HistoryEntry;
use omuzaki_history_mock::RecordingSink;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    CauseKind, ErrorCode, ErrorDetail, Event, Grants, JobCtx, JobShape, JobState, Jobs, Mode, Op, OpCtx, OpError,
    OpKind, OpMeta, Plan, Request, Runner, Scope, SessionSpec, Sessions, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Rig, drain, open_agents, person, request, rig, session_spec};

/// The test's side of a pause in a driver's work: `entered` hears the work reach it, `release` lets it go on.
struct Pause {
    entered: Receiver<()>,
    release: Sender<()>,
}

/// The job's side of one pause: it says it arrived, then waits to be let go.
type JobSide = (Sender<()>, Arc<Mutex<Receiver<()>>>);

/// The job's side, by key.
static PAUSES: LazyLock<Mutex<HashMap<String, JobSide>>> = LazyLock::new(Mutex::default);

impl Pause {
    fn new(key: &str) -> Self {
        let (entered_tx, entered) = channel();
        let (release, release_rx) = channel();
        PAUSES.lock().unwrap().insert(key.to_owned(), (entered_tx, Arc::new(Mutex::new(release_rx))));
        Self { entered, release }
    }
}

/// Edits through inner calls: `test.set` of each path in turn (or `call`, to try an op it may not run). It waits at
/// `pause_after` (if given) and fails at `fail_after`.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Drive {
    key: String,
    sets: Vec<(String, Value)>,
    #[serde(default)]
    pause_after: Option<usize>,
    #[serde(default)]
    fail_after: Option<usize>,
    #[serde(default)]
    call: Option<String>,
}

impl Op for Drive {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.drive",
            summary: "Drive the test's edits.",
            kind: OpKind::Job,
            scope: Scope::Script,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        for (n, (path, value)) in self.sets.iter().enumerate() {
            if self.pause_after == Some(n) {
                let (entered, release) = PAUSES.lock().unwrap().get(&self.key).cloned().unwrap();
                entered.send(()).unwrap();
                let _ = release.lock().unwrap().recv();
                job.checkpoint("paused", 0.5)?;
            }
            if self.fail_after == Some(n) {
                return Err(OpError::invalid("sets", format!("the script failed at step {n}")));
            }
            let op = self.call.as_deref().unwrap_or("test.set");
            job.call(op, json!({"path": path, "value": value}), Mode::Apply)?;
        }
        Ok(json!({"steps": self.sets.len()}))
    }

    fn job_shape() -> JobShape {
        JobShape::Driver
    }
}
register_op!(Drive);

fn drive(key: &str, sets: &[(&str, i64)], extra: Value) -> Request {
    let sets: Vec<(String, Value)> = sets.iter().map(|(p, v)| ((*p).to_owned(), json!(v))).collect();
    let mut args = json!({"key": key, "sets": sets});
    if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        args.extend(extra.clone());
    }
    request("test.drive", args)
}

fn doc() -> Value {
    json!({"a": 0, "b": 0, "c": 0, "x": 1, "y": 0})
}

fn applied(events: &[Event]) -> usize {
    events.iter().filter(|e| matches!(e, Event::OpApplied { record, .. } if record.op == "test.set")).count()
}

#[test]
fn three_inner_edits_are_one_undo_entry_and_their_events_arrive_at_the_commit() {
    let rig = rig(doc());
    let rx = rig.subscribe();
    let pause = Pause::new("one");
    let info = rig.exec.start(drive("one", &[("a", 1), ("b", 1), ("c", 1)], json!({"pause_after": 2}))).unwrap();
    pause.entered.recv().unwrap();
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(1)), Some(json!(1))), "inner edits apply at once");
    assert_eq!(applied(&drain(&rx)), 0, "but their events wait for the commit");
    pause.release.send(()).unwrap();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert!(matches!(ended.state, JobState::Done { .. }), "{:?}", ended.state);
    let events = drain(&rx);
    assert_eq!(applied(&events), 3);
    let committed =
        events.iter().position(|e| matches!(e, Event::TxCommitted { label, .. } if label == "Drive the test's edits."));
    let last_applied = events.iter().rposition(|e| matches!(e, Event::OpApplied { .. }));
    assert!(last_applied < committed, "held events go out before the commit: {events:?}");
    // Each inner record is a script's step, named by the job, in the job's transaction.
    let inner: Vec<_> = rig.log().into_iter().filter(|r| r.op == "test.set").collect();
    assert_eq!(inner.len(), 3);
    for record in &inner {
        let cause = record.cause.as_ref().expect("an inner call has a cause");
        assert_eq!((cause.kind, cause.text.as_deref()), (CauseKind::Script, Some("Drive the test's edits.")));
        assert_eq!(record.tx, inner[0].tx);
    }
    assert_eq!(rig.undo().unwrap().as_deref(), Some("Drive the test's edits."), "one undo entry");
    assert_eq!((rig.at("a"), rig.at("b"), rig.at("c")), (Some(json!(0)), Some(json!(0)), Some(json!(0))));
}

#[test]
fn an_error_or_a_cancel_rolls_back_everything_and_emits_no_document_event() {
    let rig = rig(doc());
    let rx = rig.subscribe();
    let failed = rig.exec.run(drive("f", &[("a", 1), ("b", 1), ("c", 1)], json!({"fail_after": 2}))).unwrap_err();
    assert!(failed.message.contains("step 2"), "{failed}");
    assert_eq!((rig.at("a"), rig.at("b")), (Some(json!(0)), Some(json!(0))));
    assert_eq!(applied(&drain(&rx)), 0, "a rolled-back run emits no OpApplied");

    let failing_call = rig.exec.run(drive("g", &[("a", 1), ("missing/z", 1)], json!({}))).unwrap_err();
    assert_eq!(failing_call.code, ErrorCode::NotFound, "{failing_call:?}");
    assert_eq!(rig.at("a"), Some(json!(0)), "an inner call's error rolls back too");

    let pause = Pause::new("c");
    let info = rig.exec.start(drive("c", &[("a", 1), ("b", 1)], json!({"pause_after": 1}))).unwrap();
    pause.entered.recv().unwrap();
    rig.exec.cancel(info.id, &person()).unwrap();
    pause.release.send(()).unwrap();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert!(matches!(ended.state, JobState::Cancelled { .. }), "{:?}", ended.state);
    assert_eq!(rig.at("a"), Some(json!(0)), "cancelling midway rolled the edit back");
    assert_eq!(applied(&drain(&rx)), 0);
    assert_eq!(rig.undo().unwrap(), None, "nothing to undo");
}

#[test]
fn a_joined_driver_never_begins_commits_nor_rolls_back_the_callers_transaction() {
    let rig = rig(doc());
    let rx = rig.subscribe();
    let tx = rig.exec.begin("caller", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 5})) }).unwrap();
    drain(&rx);
    let info = rig.exec.start(Request { tx: Some(tx), ..drive("j", &[("b", 1), ("c", 1)], json!({})) }).unwrap();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert!(matches!(ended.state, JobState::Done { tx: t, .. } if t == tx), "{:?}", ended.state);
    assert_eq!(applied(&drain(&rx)), 0, "a joined driver's events wait for the caller's commit");
    rig.exec.commit(tx, &person()).unwrap();
    assert_eq!(applied(&drain(&rx)), 2);
    assert_eq!(rig.undo().unwrap().as_deref(), Some("caller"), "one step: the caller's");
    assert_eq!((rig.at("a"), rig.at("b"), rig.at("c")), (Some(json!(0)), Some(json!(0)), Some(json!(0))));

    let tx = rig.exec.begin("caller again", &person());
    let info = rig.exec.start(Request { tx: Some(tx), ..drive("k", &[("b", 2)], json!({})) }).unwrap();
    rig.exec.wait(info.id, None).unwrap();
    rig.exec.rollback(tx, &person()).unwrap();
    assert_eq!(applied(&drain(&rx)), 0, "none if the caller then rolls back");
}

#[test]
fn a_joined_driver_that_fails_reverts_only_its_own_edits_and_journals_what_remains() {
    let sink = Arc::new(RecordingSink::new());
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(doc())), clock.clone()).unwrap().with_journal(sink.clone());
    open_agents(&exec);
    let rig = Rig { exec, clock };
    let tx = rig.exec.begin("caller", &person());
    rig.exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 5})) }).unwrap();
    let joined = Request { tx: Some(tx), ..drive("r", &[("b", 1), ("c", 1), ("y", 1)], json!({"fail_after": 2})) };
    rig.exec.run(joined).unwrap_err();
    assert_eq!((rig.at("a"), rig.at("b"), rig.at("c")), (Some(json!(5)), Some(json!(0)), Some(json!(0))));
    let revert = sink.entries().into_iter().find_map(|e| match e {
        HistoryEntry::TxRevert { tx: t, seqs, remaining, .. } => Some((t, seqs.len(), remaining)),
        _ => None,
    });
    let (t, seqs, remaining) = revert.expect("one TxRevert");
    assert_eq!((t, seqs, remaining.patch.changes.len()), (tx, 2, 1), "only the caller's edit remains");
    rig.exec.commit(tx, &person()).unwrap();
    assert_eq!(rig.undo().unwrap().as_deref(), Some("caller"));
    assert_eq!(rig.at("a"), Some(json!(0)));
}

#[test]
fn a_joined_driver_whose_edit_someone_changed_reverts_nothing() {
    let rig = rig(doc());
    let other = rig.bobs_agent();
    let tx = rig.exec.begin("caller", &person());
    let pause = Pause::new("u");
    let joined = Request { tx: Some(tx), ..drive("u", &[("x", 2), ("y", 1), ("b", 9)], json!({"pause_after": 2})) };
    let info = rig.exec.start(joined).unwrap();
    pause.entered.recv().unwrap();
    rig.set_as(&other, "x", json!(3)); // another actor changes x: 2 -> 3 between two inner calls
    rig.exec.cancel(info.id, &person()).unwrap();
    pause.release.send(()).unwrap();
    let ended = rig.exec.wait(info.id, None).unwrap();
    let JobState::Failed { error } = &ended.state else { panic!("{:?}", ended.state) };
    assert_eq!(error.code, ErrorCode::Conflict);
    let Some(ErrorDetail::Unreverted { conflicts, retained }) = error.detail.as_deref() else { panic!("{error:?}") };
    assert_eq!(conflicts.len(), 1);
    assert_eq!(
        (conflicts[0].path.to_string(), &conflicts[0].job_value, &conflicts[0].current),
        ("x".to_owned(), &json!(2), &Some(json!(3)))
    );
    assert_eq!(conflicts[0].actor, other);
    assert_eq!(retained.iter().map(ToString::to_string).collect::<Vec<_>>(), vec!["y".to_owned()]);
    assert_eq!((rig.at("x"), rig.at("y")), (Some(json!(3)), Some(json!(1))), "nothing reverted, x stays 3");

    // The caller's whole rollback follows the usual rule: a conflict on x while the other change stands.
    assert_eq!(rig.exec.rollback(tx, &person()).unwrap_err().code, ErrorCode::Conflict);
    rig.exec.undo(&other, &Grants::agent_default()).unwrap();
    rig.exec.rollback(tx, &person()).unwrap();
    assert_eq!(rig.at("x"), Some(json!(1)));
}

#[test]
fn inner_calls_are_checked_like_any_request() {
    let rig = rig(doc());
    // A session that may run scripts but not publish.
    let spec = SessionSpec { grants: Grants::agent_default().with(Scope::Script), ..session_spec("scripter") };
    let agent = rig.exec.open(spec).unwrap().actor;
    let as_agent = |req: Request| Request { actor: agent.clone(), grants: Grants::person(), ..req };
    let denied = rig.exec.run(as_agent(drive("p", &[("a", 1)], json!({"call": "test.publish"})))).unwrap_err();
    assert_eq!(denied.code, ErrorCode::Denied, "Script does not reach past the job's grants: {denied:?}");
    assert_eq!(rig.exec.approvals().len(), 0, "an inner call never queues");
    let nested = rig.exec.run(drive("n", &[("a", 1)], json!({"call": "test.job"}))).unwrap_err();
    assert_eq!(nested.code, ErrorCode::Invalid, "{nested:?}");
    let plain = Request { tx: Some(rig.exec.begin("t", &person())), ..request("test.job", json!({"key": "pj"})) };
    assert_eq!(rig.exec.run(plain).unwrap_err().code, ErrorCode::Invalid, "a plain job with tx is still invalid");
}

#[test]
fn the_lock_is_not_held_across_a_run() {
    let rig = rig(doc());
    let other = rig.bobs_agent();
    let pause = Pause::new("l");
    let info = rig.exec.start(drive("l", &[("a", 1), ("b", 1)], json!({"pause_after": 1}))).unwrap();
    pause.entered.recv().unwrap();
    // The driver holds its pause until the edit is in, so an edit that waited on the driver would never return; a
    // wall-clock bound here only measured the machine's load.
    rig.set_as(&other, "c", json!(7));
    assert_eq!(rig.at("c"), Some(json!(7)), "another actor's edit landed while the driver paused");
    pause.release.send(()).unwrap();
    rig.exec.wait(info.id, None).unwrap();
    assert_eq!((rig.at("a"), rig.at("b"), rig.at("c")), (Some(json!(1)), Some(json!(1)), Some(json!(7))));
}

/// Where a restart's snapshot is taken in [`a_joined_driver_s_failed_edits_stay_gone_after_a_restart`].
#[derive(Clone, Copy, Debug)]
enum SnapshotAt {
    /// Before the caller began: the whole session is replayed.
    Start,
    /// After the driver's records, before its revert: replay starts at the `TxRevert`.
    MidDriver,
}

/// The same executor started again from a snapshot (`doc` and `history`) and the journal after it.
fn restarted(
    doc: Value,
    history: &omuzaki_history_api::HistoryExport,
    tail: &[(omuzaki_history_api::Lsn, HistoryEntry)],
) -> Executor {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let wall = Arc::new(omuzaki_history_mock::FakeWallClock::new(omuzaki_history_api::WallTime(1)));
    let exec = Executor::restore(Box::new(MemDoc::from_value(doc)), clock, wall, history).unwrap();
    exec.replay(tail).unwrap();
    exec
}

fn whole(exec: &Executor) -> Value {
    exec.get(&omuzaki_doc_api::DocPath::root(), &Grants::person()).unwrap().unwrap()
}

/// A joined driver fails after two edits (pausing in between, where `at` may take the snapshot), and the caller
/// commits its own edit. Returns the snapshot and the journal after it.
fn joined_driver_fails(
    key: &str,
    at: SnapshotAt,
    commit: bool,
) -> (Value, omuzaki_history_api::HistoryExport, Vec<(omuzaki_history_api::Lsn, HistoryEntry)>) {
    let sink = Arc::new(RecordingSink::new());
    let exec = Executor::new(Box::new(MemDoc::from_value(doc())), Arc::new(FakeClock::new(Seconds(0.0))))
        .unwrap()
        .with_journal(sink.clone());
    let mut snapshot = (whole(&exec), exec.history_export(), 0);
    let tx = exec.begin("caller", &person());
    exec.run(Request { tx: Some(tx), ..request("test.set", json!({"path": "a", "value": 5})) }).unwrap();
    let pause = Pause::new(key);
    let joined = Request {
        tx: Some(tx),
        ..drive(key, &[("b", 1), ("c", 1), ("y", 1)], json!({"pause_after": 2, "fail_after": 2}))
    };
    let info = exec.start(joined).unwrap();
    pause.entered.recv().unwrap();
    if matches!(at, SnapshotAt::MidDriver) {
        snapshot = (whole(&exec), exec.history_export(), sink.numbered().len());
        assert_eq!(snapshot.0["b"], json!(1), "the snapshot holds the driver's edits");
    }
    pause.release.send(()).unwrap();
    assert!(matches!(exec.wait(info.id, None).unwrap().state, JobState::Failed { .. }));
    if commit {
        exec.commit(tx, &person()).unwrap();
    }
    let entries = sink.numbered();
    assert!(entries[snapshot.2..].iter().any(|(_, e)| matches!(e, HistoryEntry::TxRevert { .. })));
    (snapshot.0, snapshot.1, entries[snapshot.2..].to_vec())
}

#[test]
fn a_joined_driver_s_failed_edits_stay_gone_after_a_restart() {
    for (n, at) in [SnapshotAt::Start, SnapshotAt::MidDriver].into_iter().enumerate() {
        let (doc0, history, tail) = joined_driver_fails(&format!("restart{n}"), at, true);
        let exec = restarted(doc0, &history, &tail);
        let only_caller = json!({"a": 5, "b": 0, "c": 0, "x": 1, "y": 0});
        assert_eq!(whole(&exec), only_caller, "{at:?}: the driver's edits are absent");
        assert_eq!(exec.undo(&person(), &Grants::person()).unwrap().as_deref(), Some("caller"), "{at:?}");
        assert_eq!(whole(&exec), doc(), "{at:?}");
        assert_eq!(exec.redo(&person(), &Grants::person()).unwrap().as_deref(), Some("caller"), "{at:?}");
        assert_eq!(whole(&exec), only_caller, "{at:?}: redo never brings the driver's edits back");
    }
}

#[test]
fn a_crash_between_the_revert_and_the_callers_commit_rolls_the_transaction_back_whole() {
    for (n, at) in [SnapshotAt::Start, SnapshotAt::MidDriver].into_iter().enumerate() {
        let (doc0, history, tail) = joined_driver_fails(&format!("crash{n}"), at, false);
        let exec = restarted(doc0, &history, &tail);
        // What recovery does with a transaction left open: roll it back with its remaining patch's inverse.
        let open = exec.history_export().open;
        assert_eq!(open.len(), 1, "{at:?}");
        assert_eq!(open[0].patch.changes.len(), 1, "{at:?}: only the caller's edit remains in it");
        let lsn = tail.last().map(|(lsn, _)| *lsn).unwrap();
        exec.replay(&[(lsn, HistoryEntry::Rollback { tx: open[0].tx, patch: open[0].patch.inverse() })]).unwrap();
        assert_eq!(whole(&exec), doc(), "{at:?}: nothing half-applied remains");
        assert_eq!(exec.undo(&person(), &Grants::person()).unwrap(), None, "{at:?}");
    }
}
