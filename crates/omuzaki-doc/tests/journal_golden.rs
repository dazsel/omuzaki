//! The golden v1 journal segments: three small files that every later build must still read (journal.md 4.5, "old
//! journals always read").
//!
//! They live in `tests/fixtures/journal/v1/golden/` and are made by this test: `OMUZAKI_BLESS=1 cargo test -p
//! omuzaki-doc --test journal_golden` writes them the first time. Once merged they never change: a change to the format
//! adds a `v2/` folder and keeps `v1/`. The values are synthetic (host `test-host`, pid 1, the mock character).
//!
//! The three segments have consecutive LSNs: 1 to 6, 7 to 11 (a rotation, ending in a skippable record of unknown
//! kind 200) and 12 to 14.
#![allow(clippy::unwrap_used)] // Helpers outside `#[test]` fns; a failing helper should just panic.

use std::path::PathBuf;
use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_doc::journal::{
    End, Reader, Record, SKIPPABLE, SegmentHeader, SessionInfo, encode, segment_name, write_frame,
};
use omuzaki_doc_api::{Change, DocPath, DocVersion, Patch};
use omuzaki_history_api::{HistoryEntry, Lsn, SnapshotId, Stack, WallTime};
use omuzaki_ops_api::{Actor, OpRecord, TxId};
use omuzaki_store_api::ContentHash;
use serde_json::json;

/// The unknown skippable kind the second segment ends with.
const UNKNOWN_KIND: u8 = 200;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/journal/v1/golden")
}

fn actor() -> Actor {
    Actor::Person { name: "tester".into() }
}

fn patch(from: &str, to: &str) -> Patch {
    let path = DocPath::parse("characters/mock/name").unwrap_or_default();
    Patch { changes: vec![Change { path, before: Some(json!(from)), after: Some(json!(to)) }] }
}

fn op(seq: u64, tx: u64, from: &str, to: &str) -> Arc<OpRecord> {
    let args = json!({"name": to});
    let at = Seconds(f64::from(u32::try_from(seq).unwrap_or(0)) * 0.5);
    Arc::new(OpRecord::new(seq, "character.rename", args, actor(), TxId(tx), at, patch(from, to), DocVersion(seq)))
}

fn session(id: &str, base: Option<(SnapshotId, Lsn)>) -> Record {
    let info =
        SessionInfo::new(id.into(), WallTime(1_760_000_000_000), "0.0.0".into(), 1).with_process(1, "test-host".into());
    Record::Session(match base {
        Some((snapshot, lsn)) => info.with_base(snapshot, lsn),
        None => info,
    })
}

fn entry(entry: HistoryEntry) -> Record {
    Record::Entry(entry)
}

fn snapshot(name: &[u8]) -> SnapshotId {
    SnapshotId(ContentHash::of(name))
}

/// The records of the three segments, as LSN ranges 1 to 6, 7 to 10 (the kind 200 record is 11) and 12 to 14.
fn segments() -> [Vec<Record>; 3] {
    [
        vec![
            session("golden-1", None),
            entry(HistoryEntry::Begin { tx: TxId(1), actor: actor(), label: "rename".into(), at: Seconds(0.5) }),
            entry(HistoryEntry::Op { record: op(1, 1, "a", "b"), commits: false }),
            entry(HistoryEntry::Op { record: op(2, 1, "b", "c"), commits: false }),
            entry(HistoryEntry::Commit { tx: TxId(1) }),
            entry(HistoryEntry::Op { record: op(3, 2, "c", "d"), commits: true }),
        ],
        vec![
            entry(HistoryEntry::Undo { actor: actor(), tx: TxId(2), patch: patch("d", "c") }),
            entry(HistoryEntry::Redo { actor: actor(), tx: TxId(2), patch: patch("c", "d") }),
            entry(HistoryEntry::Rollback { tx: TxId(3), patch: patch("e", "d") }),
            entry(HistoryEntry::Dropped { actor: actor(), tx: TxId(1), stack: Stack::Undo }),
        ],
        vec![
            session("golden-2", Some((snapshot(b"golden base"), Lsn(11)))),
            entry(HistoryEntry::Op { record: op(4, 4, "d", "e"), commits: true }),
            Record::Close { snapshot: snapshot(b"golden close") },
        ],
    ]
}

/// `records` numbered from `first`.
fn numbered(first: u64, records: Vec<Record>) -> Vec<(Lsn, Record)> {
    (first..).map(Lsn).zip(records).collect()
}

/// The bytes of the three segments: their files' names and contents.
fn files() -> Vec<(String, Vec<u8>)> {
    let [first, second, third] = segments();
    // LSN 11 is the unknown record, so the third segment starts at 12.
    let first = numbered(1, first);
    let second = numbered(7, second);
    let third = numbered(12, third);

    let mut one = SegmentHeader::new(Lsn(1)).encode().to_vec();
    encode(&first, &mut one).unwrap();
    let mut two = SegmentHeader::new(Lsn(7)).encode().to_vec();
    encode(&second, &mut two).unwrap();
    write_frame(Lsn(11), UNKNOWN_KIND, SKIPPABLE, br#"{"from":"a later build"}"#, &mut two).unwrap();
    let mut three = SegmentHeader::new(Lsn(12)).encode().to_vec();
    encode(&third, &mut three).unwrap();

    [one, two, three]
        .into_iter()
        .zip(1u64..)
        .map(|(bytes, seq)| (segment_name(seq).unwrap_or_default(), bytes))
        .collect()
}

/// The records a scan returns: the three segments' records in LSN order, without the unknown one.
fn expected() -> Vec<(Lsn, Record)> {
    let [first, second, third] = segments();
    let lsns = (1u64..=10).chain(12..=14).map(Lsn);
    lsns.zip(first.into_iter().chain(second).chain(third)).collect()
}

#[test]
fn this_build_writes_the_v1_bytes() {
    let files = files();
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        std::fs::create_dir_all(dir()).unwrap();
        for (name, bytes) in &files {
            std::fs::write(dir().join(name), bytes).unwrap();
        }
    }
    for (name, bytes) in &files {
        assert!(bytes.len() < 4096, "{name} is {} bytes; a golden segment stays under 4 KiB", bytes.len());
        let golden = std::fs::read(dir().join(name))
            .unwrap_or_else(|e| panic!("{name}: {e} (OMUZAKI_BLESS=1 writes the golden files)"));
        assert!(golden == *bytes, "{name} differs from this build's bytes; the v1 files never change");
    }
}

#[test]
fn the_v1_segments_read_back() {
    let scan = Reader::scan(&dir(), Lsn(0)).unwrap();
    assert_eq!(scan.records, expected());
    assert_eq!(scan.end, End::Clean);
}
