//! The audit record, kind 11 (#1058, journal.md 4.4): every [`AuditFact`] encodes and decodes to itself; the frame is
//! written skippable, so a reader without kind 11 reads past it and replays the rest the same; audit records and ops
//! interleave in append order with consecutive LSNs; and a golden segment pins the bytes.
//!
//! The golden is `tests/fixtures/journal/v1/audit/segment-000000000001.tjl`, made by `OMUZAKI_BLESS=1 cargo test -p
//! omuzaki-doc --test journal_audit` the first time. Once merged it never changes.
#![allow(clippy::unwrap_used, clippy::expect_used)] // Helpers outside `#[test]` fns; a failing helper should just panic.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_doc::Project;
use omuzaki_doc::journal::{
    Frame, HEADER_LEN, JournalOptions, ProjectJournal, Reader, Record, SKIPPABLE, SegmentHeader, SessionStart,
    SystemWall, decode, encode, journal_dir, read_frame, reopen, segment_name, write_frame,
};
use omuzaki_doc::snapshot::Snapshotter;
use omuzaki_doc_api::{Change, DocPath, DocVersion, Patch};
use omuzaki_history_api::{AuditFact, HistoryEntry, JournalSink, Lsn};
use omuzaki_ops_api::{Actor, Budget, Grants, OpRecord, Scope, TxId};
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

/// The kind a reader that predates kind 11 would meet in its place: any kind it doesn't know.
const UNKNOWN_KIND: u8 = 201;

fn person() -> Value {
    json!({"kind": "person", "name": "tester"})
}

fn agent() -> Value {
    json!({"kind": "agent", "id": "agent#1", "model": null})
}

/// One of each fact, as the JSON the journal holds.
fn facts() -> Vec<AuditFact> {
    let narrow = Grants::agent_default();
    let wide = Grants::agent_default().with(Scope::Publish);
    let all = [
        json!({"fact": "session_opened", "session": 1, "actor": agent(), "on_behalf_of": person(),
               "transport": {"transport": "stdio"}, "client": "helper", "parent": null, "role": "builder",
               "grants": narrow}),
        json!({"fact": "grants_changed", "session": 1, "by": person(), "grants": wide}),
        json!({"fact": "budget_changed", "session": 1, "by": person(),
               "budget": Budget { usd_micros: 2_000_000, tokens: Some(50_000), ..Budget::default() }}),
        json!({"fact": "fence_changed", "session": 1, "by": person(), "fence": ["characters/mock"]}),
        json!({"fact": "paused", "session": 1, "by": person(), "reason": "person"}),
        json!({"fact": "resumed", "session": 1, "by": person()}),
        json!({"fact": "approval_requested", "approval": 7, "session": 1, "actor": agent(), "op": "project.publish",
               "need": {"need": "scope", "scope": "publish"}}),
        json!({"fact": "approval_resolved", "approval": 7, "by": person(), "decision": "rejected",
               "comment": "not this one"}),
        json!({"fact": "approval_resolved", "approval": 8, "by": null, "decision": "withdrawn", "comment": null}),
        json!({"fact": "session_ended", "session": 1,
               "reason": {"reason": "revoked", "by": person(), "credential": true}}),
    ];
    all.into_iter()
        .map(|value| serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{e}: {value}")))
        .collect()
}

fn audit(n: u32, fact: AuditFact) -> HistoryEntry {
    HistoryEntry::Audit { at: Seconds(f64::from(n) * 0.25), fact: Arc::new(fact) }
}

/// The `seq`th op: the note from `v<seq - 1>` (absent for the first) to `v<seq>`.
fn op(seq: u64) -> HistoryEntry {
    let note = DocPath::parse("project/note").unwrap();
    let before = (seq > 1).then(|| json!(format!("v{}", seq - 1)));
    let to = format!("v{seq}");
    let patch = Patch { changes: vec![Change { path: note, before, after: Some(json!(to)) }] };
    let actor = Actor::Person { name: "tester".into() };
    let record = OpRecord::new(
        seq,
        "project.set_note",
        json!({"note": to}),
        actor,
        TxId(seq),
        Seconds(1.0),
        patch,
        DocVersion(seq),
    );
    HistoryEntry::Op { record: Arc::new(record), commits: true }
}

/// The golden segment's records: the facts, with three ops among them.
fn records() -> Vec<(Lsn, Record)> {
    let mut entries = Vec::new();
    for (n, fact) in (0u32..).zip(facts()) {
        entries.push(audit(n, fact));
        if n % 3 == 1 {
            entries.push(op(u64::from(n / 3) + 1));
        }
    }
    (1u64..).map(Lsn).zip(entries.into_iter().map(Record::Entry)).collect()
}

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal/v1/audit")
}

fn segment_bytes(records: &[(Lsn, Record)]) -> Vec<u8> {
    let mut bytes = SegmentHeader::new(Lsn(1)).encode().to_vec();
    encode(records, &mut bytes).unwrap();
    bytes
}

/// The frames of a segment's bytes, after the header.
fn frames(bytes: &[u8]) -> Vec<Frame<'_>> {
    let mut at = HEADER_LEN;
    let mut out = Vec::new();
    while at < bytes.len() {
        let frame = read_frame(&bytes[at..]).unwrap_or_else(|end| panic!("not a whole frame at {at}: {end:?}"));
        at += frame.len();
        out.push(frame);
    }
    out
}

#[test]
fn every_fact_round_trips_through_a_skippable_kind_11_frame() {
    let records = records();
    let bytes = segment_bytes(&records);
    let frames = frames(&bytes);
    assert_eq!(frames.len(), records.len());
    for (frame, (lsn, record)) in frames.iter().zip(&records) {
        assert_eq!(frame.lsn, *lsn);
        let is_audit = matches!(record, Record::Entry(HistoryEntry::Audit { .. }));
        assert_eq!((frame.kind == 11, frame.skippable()), (is_audit, is_audit), "LSN {}", lsn.0);
        assert_eq!(decode(frame).unwrap().as_ref(), Some(record));
        assert!(frame.payload.len() <= 2048, "an audit payload is at most 2 KiB");
    }
}

#[test]
fn the_golden_audit_segment_decodes_to_the_facts() {
    let bytes = segment_bytes(&records());
    let path = golden_dir().join(segment_name(1).unwrap());
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&path, &bytes).unwrap();
    }
    let golden = std::fs::read(&path).unwrap_or_else(|e| panic!("{e} (OMUZAKI_BLESS=1 writes the golden file)"));
    assert!(golden == bytes, "the golden audit segment differs from this build's bytes; the v1 files never change");
    let scan = Reader::scan(&golden_dir(), Lsn(0)).unwrap();
    assert_eq!(scan.records, records());
}

#[test]
fn a_reader_without_kind_11_reads_past_it_and_replays_the_rest_the_same() {
    let records = records();
    let bytes = segment_bytes(&records);
    // The same segment as a build that predates kind 11 sees it: each audit frame an unknown skippable kind.
    let mut older = SegmentHeader::new(Lsn(1)).encode().to_vec();
    for frame in frames(&bytes) {
        let kind = if frame.kind == 11 { UNKNOWN_KIND } else { frame.kind };
        write_frame(frame.lsn, kind, frame.flags, frame.payload, &mut older).unwrap();
    }
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(segment_name(1).unwrap()), &older).unwrap();
    let scan = Reader::scan(tmp.path(), Lsn(0)).unwrap();
    let without_audit: Vec<_> = records
        .into_iter()
        .filter(|(_, record)| !matches!(record, Record::Entry(HistoryEntry::Audit { .. })))
        .collect();
    assert_eq!(scan.records, without_audit, "the ops, in order, and nothing else");
    assert_eq!(frames(&older).iter().filter(|f| f.flags & SKIPPABLE != 0).count(), facts().len());
}

fn snapshots(dir: &Path) -> Arc<Snapshotter> {
    Arc::new(Snapshotter::new(dir, Arc::new(MemStore::default())))
}

#[test]
fn ops_and_audit_facts_written_by_the_journal_read_back_in_append_order() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let reopened = reopen(tmp.path(), &snapshots(tmp.path())).unwrap();
    let start = SessionStart::new(
        "test",
        "0.0.0",
        reopened.last_lsn,
        reopened.history,
        Arc::new(SystemWall),
        snapshots(tmp.path()),
    );
    let journal = ProjectJournal::start(reopened.project, start, JournalOptions::default()).unwrap();
    let facts = facts();
    let appended = vec![op(1), audit(0, facts[0].clone()), op(2), audit(1, facts[4].clone()), op(3)];
    for entry in &appended {
        journal.append(entry.clone());
    }
    journal.sync(Duration::from_secs(10)).unwrap();
    drop(journal);

    let scan = Reader::scan(&journal_dir(tmp.path()), Lsn(0)).unwrap();
    let entries: Vec<(Lsn, HistoryEntry)> = scan
        .records
        .into_iter()
        .filter_map(|(lsn, record)| match record {
            Record::Entry(entry) => Some((lsn, entry)),
            _ => None,
        })
        .collect();
    assert_eq!(entries.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(), appended);
    let lsns: Vec<u64> = entries.iter().map(|(lsn, _)| lsn.0).collect();
    assert!(lsns.windows(2).all(|w| w[1] == w[0] + 1), "consecutive LSNs: {lsns:?}");
}
