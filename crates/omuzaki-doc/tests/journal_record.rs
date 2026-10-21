//! The journal's records (journal.md 4.4 and 4.5): every kind round-trips through `encode`, `read_frame` and `decode`,
//! and what can't be written or read is refused with the documented error. Values are synthetic.
#![allow(clippy::unwrap_used)] // Helpers outside `#[test]` fns; a failing helper should just panic.

use std::collections::BTreeSet;
use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_doc::journal::{
    JournalError, MAX_DEPTH, Record, SKIPPABLE, SessionInfo, decode, encode, kind, read_frame, write_frame,
};
use omuzaki_doc_api::{Change, DocPath, DocVersion, Patch};
use omuzaki_history_api::{HistoryEntry, HistoryExport, Lsn, OpenTxExport, SnapshotId, Stack, WallTime};
use omuzaki_ops_api::{Actor, ErrorDetail, JobId, JobState, OpError, OpRecord, TxId};
use omuzaki_store_api::ContentHash;
use serde_json::json;

fn actor() -> Actor {
    Actor::Person { name: "tester".into() }
}

fn patch() -> Patch {
    let path = DocPath::parse("characters/mock/name").unwrap_or_default();
    Patch { changes: vec![Change { path, before: Some(json!("a")), after: Some(json!("b")) }] }
}

fn snapshot() -> SnapshotId {
    SnapshotId(ContentHash::of(b"synthetic snapshot"))
}

fn op_record(at: Seconds) -> OpRecord {
    OpRecord::new(1, "set", json!({"name": "b"}), actor(), TxId(1), at, patch(), DocVersion(2))
}

fn session() -> SessionInfo {
    SessionInfo::new("s1".into(), WallTime(1_760_000_000_000), "0.0.0".into(), 3)
        .with_base(snapshot(), Lsn(7))
        .with_process(1, "test-host".into())
}

/// One record of each kind this module writes.
fn every_kind() -> Vec<Record> {
    let remaining = OpenTxExport::new(actor(), TxId(1), "edit", patch(), BTreeSet::new());
    vec![
        Record::Session(session()),
        Record::Entry(HistoryEntry::Begin { tx: TxId(1), actor: actor(), label: "edit".into(), at: Seconds(1.5) }),
        Record::Entry(HistoryEntry::Op { record: Arc::new(op_record(Seconds(2.0))), commits: false }),
        Record::Entry(HistoryEntry::Commit { tx: TxId(1) }),
        Record::Entry(HistoryEntry::Rollback { tx: TxId(2), patch: patch() }),
        Record::Entry(HistoryEntry::Undo { actor: actor(), tx: TxId(1), patch: patch() }),
        Record::Entry(HistoryEntry::Redo { actor: actor(), tx: TxId(1), patch: patch() }),
        Record::Entry(HistoryEntry::Dropped { actor: actor(), tx: TxId(1), stack: Stack::Redo }),
        Record::Close { snapshot: snapshot() },
        Record::Entry(HistoryEntry::JobStarted {
            job: JobId(4),
            op: "render".into(),
            args: json!({"frames": 2}),
            actor: actor(),
            on_behalf_of: Some(Actor::System),
            at: Seconds(3.0),
        }),
        Record::Entry(HistoryEntry::JobEnded { job: JobId(4), state: JobState::done(TxId(3), Some(5)) }),
        Record::Entry(HistoryEntry::TxRevert { tx: TxId(1), seqs: vec![1], patch: patch(), remaining }),
    ]
}

fn numbered(records: Vec<Record>) -> Vec<(Lsn, Record)> {
    records.into_iter().zip(1u64..).map(|(r, n)| (Lsn(n), r)).collect()
}

/// The kind of the frame `record` encodes to.
fn kind_of(record: &Record) -> u8 {
    let mut out = Vec::new();
    encode(&[(Lsn(1), record.clone())], &mut out).unwrap();
    read_frame(&out).unwrap().kind
}

#[test]
fn every_record_round_trips() {
    let records = numbered(every_kind());
    let mut out = Vec::new();
    encode(&records, &mut out).unwrap();

    let mut rest = out.as_slice();
    let mut back = Vec::new();
    while !rest.is_empty() {
        let frame = read_frame(rest).unwrap();
        assert_eq!(frame.flags, 0, "no kind written here is skippable");
        back.push((frame.lsn, decode(&frame).unwrap().unwrap()));
        rest = &rest[frame.len()..];
    }
    assert_eq!(back, records);
}

#[test]
fn every_kind_has_its_number() {
    let kinds: Vec<u8> = every_kind().iter().map(kind_of).collect();
    assert_eq!(
        kinds,
        [
            kind::SESSION,
            kind::BEGIN,
            kind::OP,
            kind::COMMIT,
            kind::ROLLBACK,
            kind::UNDO,
            kind::REDO,
            kind::DROPPED,
            kind::CLOSE,
            kind::JOB_STARTED,
            kind::JOB_ENDED,
            kind::TX_REVERT,
        ]
    );
    assert_eq!(
        [kind::SESSION, kind::BEGIN, kind::OP, kind::COMMIT, kind::ROLLBACK, kind::UNDO, kind::REDO, kind::DROPPED],
        [1, 2, 3, 4, 5, 6, 7, 8]
    );
    assert_eq!([kind::CLOSE, kind::RESERVED_BRANCH, kind::AUDIT], [9, 10, 11]);
    assert_eq!([kind::JOB_STARTED, kind::JOB_ENDED, kind::TX_REVERT], [12, 13, 14]);
}

#[test]
fn a_session_without_base_or_process_round_trips() {
    let record = Record::Session(SessionInfo::new("s".into(), WallTime(5), "1.0".into(), 1));
    let mut out = Vec::new();
    encode(&[(Lsn(1), record.clone())], &mut out).unwrap();
    assert_eq!(decode(&read_frame(&out).unwrap()).unwrap(), Some(record));
}

#[test]
fn mark_and_session_entries_are_refused() {
    let session = HistoryEntry::Session { session: "s".into(), started: WallTime(1) };
    let mark = HistoryEntry::Mark { history: Arc::new(HistoryExport::empty()) };
    for entry in [session, mark] {
        let mut out = vec![9, 9];
        let err = encode(&[(Lsn(4), Record::Entry(entry))], &mut out).unwrap_err();
        assert!(matches!(err, JournalError::Unencodable { lsn: 4, .. }), "{err:?}");
        assert_eq!(out, [9, 9]);
    }
}

#[test]
fn a_non_finite_time_is_refused_and_out_is_unchanged() {
    let begin = |at| HistoryEntry::Begin { tx: TxId(1), actor: actor(), label: String::new(), at: Seconds(at) };
    let op = |at| HistoryEntry::Op { record: Arc::new(op_record(Seconds(at))), commits: true };
    let job = |at| HistoryEntry::JobStarted {
        job: JobId(1),
        op: "x".into(),
        args: json!({}),
        actor: actor(),
        on_behalf_of: None,
        at: Seconds(at),
    };
    let bad = [begin(f64::NAN), op(f64::INFINITY), job(f64::NEG_INFINITY)];
    for entry in bad {
        let good = (Lsn(1), Record::Entry(HistoryEntry::Commit { tx: TxId(1) }));
        let records = [good, (Lsn(2), Record::Entry(entry))];
        let mut out = b"before".to_vec();
        let err = encode(&records, &mut out).unwrap_err();
        assert!(matches!(err, JournalError::Unencodable { lsn: 2, .. }), "{err:?}");
        assert_eq!(out, b"before", "the good first record is rolled back too");
    }
}

#[test]
fn an_unknown_skippable_kind_is_skipped() {
    let mut out = Vec::new();
    write_frame(Lsn(1), 200, SKIPPABLE, b"{}", &mut out).unwrap();
    assert_eq!(decode(&read_frame(&out).unwrap()).unwrap(), None);
}

#[test]
fn the_audit_kind_is_skipped_until_it_is_decoded() {
    let mut out = Vec::new();
    write_frame(Lsn(1), kind::AUDIT, SKIPPABLE, b"{}", &mut out).unwrap();
    assert_eq!(decode(&read_frame(&out).unwrap()).unwrap(), None);
}

#[test]
fn an_unknown_required_kind_is_too_new() {
    for unknown in [200, kind::RESERVED_BRANCH, 15] {
        let mut out = Vec::new();
        write_frame(Lsn(1), unknown, 0, b"{}", &mut out).unwrap();
        let err = decode(&read_frame(&out).unwrap()).unwrap_err();
        assert!(matches!(err, JournalError::TooNew { kind: Some(k), .. } if k == unknown), "{err:?}");
    }
}

#[test]
fn a_tag_that_disagrees_with_its_kind_is_a_bad_payload() {
    let commit = Record::Entry(HistoryEntry::Commit { tx: TxId(1) });
    let mut out = Vec::new();
    encode(&[(Lsn(1), commit)], &mut out).unwrap();
    let payload = read_frame(&out).unwrap().payload.to_vec();

    let mut wrong = Vec::new();
    write_frame(Lsn(1), kind::BEGIN, 0, &payload, &mut wrong).unwrap();
    let err = decode(&read_frame(&wrong).unwrap()).unwrap_err();
    assert!(matches!(err, JournalError::BadPayload { lsn: 1, kind: 2, .. }), "{err:?}");
}

#[test]
fn a_session_or_mark_tag_in_an_entry_kind_is_a_bad_payload() {
    let tagged = json!({"entry": "session", "session": "s", "started": 1}).to_string();
    let mut out = Vec::new();
    write_frame(Lsn(1), kind::COMMIT, 0, tagged.as_bytes(), &mut out).unwrap();
    let err = decode(&read_frame(&out).unwrap()).unwrap_err();
    assert!(matches!(err, JournalError::BadPayload { .. }), "{err:?}");
}

#[test]
fn a_payload_that_is_not_json_for_its_kind_is_a_bad_payload() {
    for (k, payload) in [(kind::SESSION, &b"nope"[..]), (kind::CLOSE, b"{}"), (kind::OP, b"{\"entry\":\"op\"}")] {
        let mut out = Vec::new();
        write_frame(Lsn(3), k, 0, payload, &mut out).unwrap();
        let err = decode(&read_frame(&out).unwrap()).unwrap_err();
        assert!(matches!(err, JournalError::BadPayload { lsn: 3, .. }), "{err:?}");
    }
}

#[test]
fn encoding_is_deterministic() {
    let records = numbered(every_kind());
    let (mut a, mut b) = (Vec::new(), Vec::new());
    encode(&records, &mut a).unwrap();
    encode(&records, &mut b).unwrap();
    assert_eq!(a, b);
}

/// An `op` record whose args are `levels` arrays nested in each other.
fn nested(levels: usize) -> (Lsn, Record) {
    let mut args = json!(1);
    for _ in 0..levels {
        args = json!([args]);
    }
    let record = OpRecord::new(1, "set", args, actor(), TxId(1), Seconds(1.0), patch(), DocVersion(2));
    (Lsn(1), Record::Entry(HistoryEntry::Op { record: Arc::new(record), commits: true }))
}

#[test]
fn every_record_encode_accepts_decodes_including_deep_ones() {
    let mut accepted = 0;
    for levels in 0..(MAX_DEPTH + 10) {
        let record = nested(levels);
        let mut out = Vec::new();
        match encode(std::slice::from_ref(&record), &mut out) {
            Ok(()) => {
                accepted = levels;
                let frame = read_frame(&out).unwrap();
                assert_eq!(decode(&frame).unwrap(), Some(record.1), "{levels} levels");
            }
            Err(err) => {
                assert!(matches!(err, JournalError::Unencodable { lsn: 1, .. }), "{levels} levels: {err:?}");
                assert!(out.is_empty(), "{levels} levels: out is unchanged");
                assert!(levels > accepted, "{levels} levels is refused after {accepted} were accepted");
            }
        }
    }
    // The cap bites: the args sit a few levels inside the payload, so fewer than MAX_DEPTH levels fit, and one more is
    // refused with a message naming the cap.
    assert!(accepted > 100 && accepted < MAX_DEPTH, "{accepted}");
    let JournalError::Unencodable { reason, .. } = encode(&[nested(accepted + 1)], &mut Vec::new()).unwrap_err() else {
        panic!("not unencodable");
    };
    assert!(reason.contains("level"), "{reason}");
}

#[test]
fn a_value_json_cannot_hold_exactly_is_refused() {
    // An f32 progress or cost that is not finite would be written as null and read back as something else.
    let job = |seconds: f32| {
        Record::Entry(HistoryEntry::JobEnded {
            job: JobId(1),
            state: JobState::Failed {
                error: OpError::internal("limit").with_detail(ErrorDetail::RetryAfter { seconds, limit: "x=1".into() }),
            },
        })
    };
    let mut out = Vec::new();
    for seconds in [f32::NAN, f32::INFINITY] {
        assert!(encode(&[(Lsn(1), job(seconds))], &mut out).is_err(), "{seconds}");
        assert_eq!(out, Vec::<u8>::new());
    }
    encode(&[(Lsn(1), job(1.5))], &mut out).unwrap();
}
