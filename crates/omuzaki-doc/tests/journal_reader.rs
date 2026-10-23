//! `Reader::scan` (journal.md 8.2): rotation, the torn-tail and damage rules, and the errors that stop a scan.
//!
//! Segments are written into a temporary folder with the format module's `SegmentHeader` and `write_frame` and the
//! record module's `encode`. Records are one-field commits, so frames are small and their offsets easy to compute.
#![allow(clippy::unwrap_used)] // Helpers outside `#[test]` fns; a failing helper should just panic.

use std::fs;
use std::path::Path;

use omuzaki_doc::journal::{
    End, FRAME_HEADER_LEN, FrameEnd, HEADER_LEN, HeaderFault, JournalError, MAX_PAYLOAD, Position, Reader, Record,
    SKIPPABLE, SegmentHeader, SessionInfo, decode, encode, kind, read_frame, segment_name, write_frame,
};
use omuzaki_history_api::{HistoryEntry, Lsn, WallTime};
use omuzaki_ops_api::TxId;

/// A commit record: the smallest real record, whose tx is its LSN.
fn commit(lsn: u64) -> (Lsn, Record) {
    (Lsn(lsn), Record::Entry(HistoryEntry::commit(TxId(lsn))))
}

fn session(lsn: u64) -> (Lsn, Record) {
    let info = SessionInfo::new(format!("s{lsn}"), WallTime(1), "0.0.0".into(), 1);
    (Lsn(lsn), Record::Session(info))
}

fn frame(record: &(Lsn, Record)) -> Vec<u8> {
    let mut out = Vec::new();
    encode(std::slice::from_ref(record), &mut out).unwrap();
    out
}

/// A segment file: the header for `first`, then the frames of `records`.
fn segment(first: u64, records: &[(Lsn, Record)]) -> Vec<u8> {
    let mut bytes = SegmentHeader::new(Lsn(first)).encode().to_vec();
    encode(records, &mut bytes).unwrap();
    bytes
}

/// The commits `first..=last`.
fn commits(first: u64, last: u64) -> Vec<(Lsn, Record)> {
    (first..=last).map(commit).collect()
}

/// The byte offset in a segment of the frame at `index` of `records`.
fn offset_of(records: &[(Lsn, Record)], index: usize) -> usize {
    HEADER_LEN + records[..index].iter().map(|r| frame(r).len()).sum::<usize>()
}

fn put(dir: &Path, seq: u64, bytes: &[u8]) {
    fs::write(dir.join(segment_name(seq).unwrap()), bytes).unwrap();
}

fn lsns(scan: &omuzaki_doc::journal::Scan) -> Vec<u64> {
    scan.records.iter().map(|(lsn, _)| lsn.0).collect()
}

fn at(segment: u64, offset: usize, after: u64) -> Position {
    Position { segment, offset: u64::try_from(offset).unwrap(), after: Lsn(after) }
}

/// Two segments, 1 to 3 and 4 to 5.
fn two_segments(dir: &Path) {
    put(dir, 1, &segment(1, &commits(1, 3)));
    put(dir, 2, &segment(4, &commits(4, 5)));
}

#[test]
fn a_folder_without_segments_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("notes.txt"), b"not a segment").unwrap();
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!((scan.records.len(), scan.end), (0, End::Clean));
}

#[test]
fn a_missing_folder_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = Reader::scan(&dir.path().join("absent"), Lsn(0)).unwrap_err();
    assert!(matches!(err, JournalError::Io { .. }), "{err:?}");
}

#[test]
fn a_clean_journal_reads_whole() {
    let dir = tempfile::tempdir().unwrap();
    two_segments(dir.path());
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2, 3, 4, 5]);
    assert_eq!(scan.records, (1..=5).map(commit).collect::<Vec<_>>());
    assert_eq!(scan.end, End::Clean);
}

#[test]
fn from_skips_earlier_records_and_segments() {
    let dir = tempfile::tempdir().unwrap();
    two_segments(dir.path());
    for (from, want) in [(1, vec![2, 3, 4, 5]), (3, vec![4, 5]), (4, vec![5]), (5, vec![]), (9, vec![])] {
        let scan = Reader::scan(dir.path(), Lsn(from)).unwrap();
        assert_eq!((lsns(&scan), scan.end), (want, End::Clean), "from {from}");
    }

    // Segment 1 deleted: old damage or absence can't block a scan that starts after it.
    fs::remove_file(dir.path().join(segment_name(1).unwrap())).unwrap();
    let scan = Reader::scan(dir.path(), Lsn(3)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![4, 5], End::Clean));
}

#[test]
fn old_damage_before_the_start_segment_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = segment(1, &commits(1, 3));
    first[HEADER_LEN + 6] ^= 0xff;
    put(dir.path(), 1, &first);
    put(dir.path(), 2, &segment(4, &commits(4, 5)));
    let scan = Reader::scan(dir.path(), Lsn(3)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![4, 5], End::Clean));
}

#[test]
fn a_zero_tail_is_clean() {
    for zeros in [1, 19, 20, 4096] {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = segment(1, &commits(1, 3));
        bytes.extend(std::iter::repeat_n(0u8, zeros));
        put(dir.path(), 1, &bytes);
        let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
        assert_eq!((lsns(&scan), scan.end), (vec![1, 2, 3], End::Clean), "{zeros} zeros");
    }
}

#[test]
fn a_prepared_next_segment_is_clean() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    let mut prepared = SegmentHeader::prepared().encode().to_vec();
    prepared.extend([0u8; 64]);
    put(dir.path(), 2, &prepared);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![1, 2, 3], End::Clean));
}

#[test]
fn a_torn_last_frame_is_a_torn_tail() {
    let records = commits(1, 3);
    let full = segment(1, &records);
    let third = offset_of(&records, 2);
    // Any cut inside the third frame, even one byte short, leaves a torn tail after record 2.
    for cut in [full.len() - 1, full.len() - 10, third + 19, third + 1] {
        let dir = tempfile::tempdir().unwrap();
        put(dir.path(), 1, &full[..cut]);
        let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
        assert_eq!(lsns(&scan), [1, 2], "cut at {cut}");
        assert_eq!(scan.end, End::TornTail { at: at(1, third, 2) }, "cut at {cut}");
    }
}

#[test]
fn a_bad_frame_with_valid_frames_after_is_damage() {
    let records = commits(1, 5);
    let mut bytes = segment(1, &records);
    let second = offset_of(&records, 1);
    bytes[second + 20] ^= 0x01; // a payload byte of frame 2
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1]);
    assert_eq!(scan.end, End::Damaged { at: at(1, second, 1), later_valid: 3, search_complete: true });
}

#[test]
fn damage_counts_the_frames_of_later_segments() {
    let dir = tempfile::tempdir().unwrap();
    let records = commits(1, 3);
    let mut bytes = segment(1, &records);
    bytes[offset_of(&records, 2) + 20] ^= 0x01;
    put(dir.path(), 1, &bytes);
    put(dir.path(), 2, &segment(4, &commits(4, 5)));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2]);
    assert_eq!(scan.end, End::Damaged { at: at(1, offset_of(&records, 2), 2), later_valid: 2, search_complete: true });
}

#[test]
fn an_lsn_gap_ends_the_log() {
    let records = [commit(1), commit(2), commit(4), commit(5)];
    let third = offset_of(&records, 2);
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &records));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2], "nothing after the gap is returned");
    assert_eq!(scan.end, End::Damaged { at: at(1, third, 2), later_valid: 1, search_complete: true });

    // With nothing after the out-of-sequence frame, it is the torn end of a batch.
    put(dir.path(), 1, &segment(1, &records[..3]));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![1, 2], End::TornTail { at: at(1, third, 2) }));
}

#[test]
fn a_first_frame_that_does_not_match_its_header_ends_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = SegmentHeader::new(Lsn(5)).encode().to_vec();
    encode(&commits(6, 6), &mut bytes).unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(4)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![], End::TornTail { at: at(1, HEADER_LEN, 4) }));
}

#[test]
fn a_bad_header_ends_the_log() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    let mut next = segment(4, &commits(4, 5));
    next[0] ^= 0xff;
    put(dir.path(), 2, &next);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![1, 2, 3], End::TornTail { at: at(2, 0, 3) }));

    // A file too short for a header ends it the same way.
    put(dir.path(), 2, &next[..10]);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![1, 2, 3], End::TornTail { at: at(2, 0, 3) }));
}

#[test]
fn a_next_segment_that_does_not_follow_is_damage() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    put(dir.path(), 2, &segment(9, &commits(9, 11)));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2, 3]);
    assert_eq!(scan.end, End::Damaged { at: at(2, 0, 3), later_valid: 3, search_complete: true });
}

#[test]
fn the_post_recovery_session_continues_past_a_torn_tail() {
    let records = commits(1, 4);
    let full = segment(1, &records);
    let dir = tempfile::tempdir().unwrap();
    // Record 4 was torn by the crash; recovery started a segment at LSN 4 with a session record.
    put(dir.path(), 1, &full[..full.len() - 3]);
    let mut recovered = SegmentHeader::new(Lsn(4)).encode().to_vec();
    encode(&[session(4), commit(5)], &mut recovered).unwrap();
    put(dir.path(), 2, &recovered);

    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2, 3, 4, 5]);
    assert!(matches!(scan.records[3].1, Record::Session(_)));
    assert_eq!(scan.end, End::Clean);
}

#[test]
fn a_next_segment_without_a_session_is_not_the_exception() {
    let records = commits(1, 4);
    let full = segment(1, &records);
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &full[..full.len() - 3]);
    put(dir.path(), 2, &segment(4, &commits(4, 5)));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2, 3]);
    assert_eq!(scan.end, End::Damaged { at: at(1, offset_of(&records, 3), 3), later_valid: 2, search_complete: true });
}

#[test]
fn a_session_segment_that_starts_elsewhere_is_not_the_exception() {
    let records = commits(1, 4);
    let full = segment(1, &records);
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &full[..full.len() - 3]);
    let mut other = SegmentHeader::new(Lsn(6)).encode().to_vec();
    encode(&[session(6)], &mut other).unwrap();
    put(dir.path(), 2, &other);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2, 3]);
    assert!(matches!(scan.end, End::Damaged { later_valid: 1, .. }), "{:?}", scan.end);
}

#[test]
fn a_newer_header_is_too_new() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    let mut newer = segment(4, &commits(4, 5));
    newer[8..10].copy_from_slice(&2u16.to_le_bytes());
    let crc = crc32fast::hash(&newer[..28]);
    newer[28..32].copy_from_slice(&crc.to_le_bytes());
    put(dir.path(), 2, &newer);
    let err = Reader::scan(dir.path(), Lsn(0)).unwrap_err();
    assert!(matches!(err, JournalError::TooNew { version: 2, kind: None }), "{err:?}");
}

#[test]
fn an_unknown_required_kind_is_too_new() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = segment(1, &commits(1, 2));
    write_frame(Lsn(3), 200, 0, b"{}", &mut bytes).unwrap();
    put(dir.path(), 1, &bytes);
    let err = Reader::scan(dir.path(), Lsn(0)).unwrap_err();
    assert!(matches!(err, JournalError::TooNew { kind: Some(200), .. }), "{err:?}");

    // Below `from` it is not read, so a base snapshot past it still opens.
    let scan = Reader::scan(dir.path(), Lsn(3)).unwrap();
    assert_eq!((scan.records.len(), scan.end), (0, End::Clean));
}

#[test]
fn an_unknown_skippable_kind_is_skipped_but_counts_in_the_sequence() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = segment(1, &commits(1, 1));
    write_frame(Lsn(2), 200, SKIPPABLE, b"{}", &mut bytes).unwrap();
    encode(&commits(3, 3), &mut bytes).unwrap();
    // An audit record this build can't read, as a fact a later build adds would be: skipped too.
    write_frame(Lsn(4), kind::AUDIT, SKIPPABLE, br#"{"entry":"audit","at":1.0,"fact":{"fact":"later"}}"#, &mut bytes)
        .unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![1, 3], End::Clean));
}

#[test]
fn a_bad_payload_with_a_good_crc_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = segment(1, &commits(1, 1));
    write_frame(Lsn(2), kind::SESSION, 0, b"nope", &mut bytes).unwrap();
    put(dir.path(), 1, &bytes);
    let err = Reader::scan(dir.path(), Lsn(0)).unwrap_err();
    assert!(matches!(err, JournalError::BadPayload { lsn: 2, .. }), "{err:?}");
}

#[test]
fn a_scan_from_zero_starts_at_the_first_segment_and_reports_a_restart() {
    // Segment 2 restarts at LSN 1: with `from` 0 the scan starts at segment 1 and sees the discontinuity.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    put(dir.path(), 2, &segment(1, &commits(1, 2)));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2, 3]);
    assert_eq!(scan.end, End::Damaged { at: at(2, 0, 3), later_valid: 2, search_complete: true });
}

#[test]
fn a_zero_tail_ends_the_log_unless_a_recovery_session_follows() {
    for zeros in [1, 19, 20, 4096] {
        // A later segment that continues the LSNs is not a rotation: the zero tail ended the log.
        let dir = tempfile::tempdir().unwrap();
        let mut first = segment(1, &commits(1, 3));
        first.extend(std::iter::repeat_n(0u8, zeros));
        put(dir.path(), 1, &first);
        put(dir.path(), 2, &segment(4, &commits(4, 5)));
        let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
        assert_eq!((lsns(&scan), scan.end), (vec![1, 2, 3], End::Clean), "{zeros} zeros");

        // A segment an earlier recovery started (a session record one past the last good record) is read.
        put(dir.path(), 2, &segment(4, &[session(4), commit(5)]));
        let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
        assert_eq!(lsns(&scan), [1, 2, 3, 4, 5], "{zeros} zeros, recovery session");
        assert_eq!(scan.end, End::Clean);
    }
}

#[test]
fn lsn_u64_max_is_the_end_of_the_sequence() {
    let top = u64::MAX;
    // A second frame with the same LSN is a bad frame, not a duplicate to accept.
    let dir = tempfile::tempdir().unwrap();
    let records = [commit(top), commit(top)];
    put(dir.path(), 1, &segment(top, &records));
    let scan = Reader::scan(dir.path(), Lsn(top - 1)).unwrap();
    assert_eq!(lsns(&scan), [top]);
    assert_eq!(scan.end, End::TornTail { at: at(1, offset_of(&records, 1), top) });

    // So is a following segment that starts at u64::MAX again.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(top, &[commit(top)]));
    put(dir.path(), 2, &segment(top, &[commit(top)]));
    let scan = Reader::scan(dir.path(), Lsn(top - 1)).unwrap();
    assert_eq!(lsns(&scan), [top]);
    assert_eq!(scan.end, End::Damaged { at: at(2, 0, top), later_valid: 1, search_complete: true });
}

#[test]
fn a_large_noisy_tail_is_scanned_in_bounded_work() {
    // 16 MiB of deterministic noise after a bad frame. Looking past the bad frame must not hash a payload at every
    // offset (hundreds of GB for this input): only offsets whose length fits and whose payload looks like a JSON
    // object are hashed, so this finishes at once, and the noise holds no valid frame.
    let records = commits(1, 3);
    let mut bytes = segment(1, &records);
    let second = offset_of(&records, 1);
    bytes[second + 20] ^= 0x01;
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    bytes.extend((0..16 * 1024 * 1024).map(|_| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state.to_le_bytes()[3]
    }));
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1]);
    // Frame 3 is intact after the bad frame 2, so this is damage with one later valid frame... unless the noise
    // overwrote it: the noise is appended, so frame 3 is still there.
    assert_eq!(scan.end, End::Damaged { at: at(1, second, 1), later_valid: 1, search_complete: true });
}

/// Frame 2 of `commits(1, 3)` with a payload byte flipped, so its CRC fails.
fn with_bad_second(records: &[(Lsn, Record)]) -> (Vec<u8>, usize) {
    let mut bytes = segment(1, records);
    let second = offset_of(records, 1);
    bytes[second + 20] ^= 0x01;
    (bytes, second)
}

/// A commit frame whose JSON payload is wrapped in `pad` on both sides.
fn padded_commit(lsn: u64, pad: &[u8]) -> Vec<u8> {
    let bytes = frame(&commit(lsn));
    let inner = read_frame(&bytes).unwrap().payload.to_vec();
    let payload = [pad, &inner, pad].concat();
    let mut out = Vec::new();
    write_frame(Lsn(lsn), kind::COMMIT, 0, &payload, &mut out).unwrap();
    out
}

#[test]
fn a_whitespace_padded_frame_after_a_bad_one_is_counted() {
    let records = commits(1, 2);
    let (mut bytes, second) = with_bad_second(&records);
    bytes.extend(padded_commit(3, b" \t\r\n "));
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1]);
    assert_eq!(scan.end, End::Damaged { at: at(1, second, 1), later_valid: 1, search_complete: true });
}

#[test]
fn a_huge_whitespace_payload_is_cheap_and_not_a_frame() {
    let records = commits(1, 2);
    let (mut bytes, second) = with_bad_second(&records);
    // A header claiming 8 MiB of payload, then 8 MiB of spaces: skimmed once, then skipped.
    let len = 8u32 * 1024 * 1024;
    bytes.extend(len.to_le_bytes());
    bytes.extend([0u8; 16]);
    bytes.extend(std::iter::repeat_n(b' ', usize::try_from(len).unwrap()));
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(scan.end, End::TornTail { at: at(1, second, 1) });
}

#[test]
fn an_exhausted_resync_budget_reports_damage_with_at_least_one_frame() {
    // Frames 2 and 3 are both bad, so nothing valid follows the first bad frame.
    let records = commits(1, 3);
    let (mut bytes, second) = with_bad_second(&records);
    bytes[offset_of(&records, 2) + 24] ^= 0x01;
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(scan.end, End::TornTail { at: at(1, second, 1) });
    // With no budget the scan can't look: it reports damage, with nothing verified and the search incomplete.
    let scan = Reader::scan_with_budget(dir.path(), Lsn(0), 0).unwrap();
    assert_eq!(lsns(&scan), [1]);
    assert_eq!(scan.end, End::Damaged { at: at(1, second, 1), later_valid: 0, search_complete: false });
}

/// `bytes` with its header rewritten to format version 2.
fn newer(mut bytes: Vec<u8>) -> Vec<u8> {
    bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
    let crc = crc32fast::hash(&bytes[..28]);
    bytes[28..32].copy_from_slice(&crc.to_le_bytes());
    bytes
}

#[test]
fn a_newer_segment_past_the_end_of_the_log_is_not_reached() {
    // A zero tail ends the log, so the newer segment after it is never read.
    let dir = tempfile::tempdir().unwrap();
    let mut first = segment(1, &commits(1, 3));
    first.extend([0u8; 40]);
    put(dir.path(), 1, &first);
    put(dir.path(), 2, &newer(segment(4, &commits(4, 5))));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![1, 2, 3], End::Clean));

    // A prepared segment ends the log too.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    put(dir.path(), 2, &SegmentHeader::prepared().encode());
    put(dir.path(), 3, &newer(segment(4, &commits(4, 5))));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![1, 2, 3], End::Clean));
}

#[test]
fn a_newer_first_segment_is_too_new() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &newer(segment(1, &commits(1, 3))));
    let err = Reader::scan(dir.path(), Lsn(0)).unwrap_err();
    assert!(matches!(err, JournalError::TooNew { version: 2, kind: None }), "{err:?}");
}

#[test]
fn a_segment_that_restarts_at_an_earlier_lsn_is_never_a_start() {
    // Segment 2 restarts at LSN 1: every scan starts in segment 1 and reaches the discontinuity from there.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    put(dir.path(), 2, &segment(1, &commits(1, 2)));
    for (from, want) in [(0, vec![1, 2, 3]), (1, vec![2, 3]), (2, vec![3]), (3, vec![])] {
        let scan = Reader::scan(dir.path(), Lsn(from)).unwrap();
        assert_eq!(lsns(&scan), want, "from {from}");
        assert_eq!(scan.end, End::Damaged { at: at(2, 0, 3), later_valid: 2, search_complete: true }, "from {from}");
    }
}

#[test]
fn start_selection_stops_at_a_prepared_segment() {
    // Records 1-3, a prepared segment, then a stale segment at 4: the prepared segment is the end of the log.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    put(dir.path(), 2, &SegmentHeader::prepared().encode());
    put(dir.path(), 3, &segment(4, &commits(4, 5)));
    for from in [0, 1, 2, 3] {
        let scan = Reader::scan(dir.path(), Lsn(from)).unwrap();
        assert_eq!(lsns(&scan), (from + 1..=3).collect::<Vec<_>>(), "from {from}");
        assert_eq!(scan.end, End::Clean, "from {from}");
    }
}

#[cfg(unix)]
#[test]
fn files_after_the_end_of_the_log_are_never_opened() {
    // A zero tail ends the log; a broken symlink after it, or an unreadable header, must not fail the scan.
    let dir = tempfile::tempdir().unwrap();
    let mut first = segment(1, &commits(1, 3));
    first.extend([0u8; 40]);
    put(dir.path(), 1, &first);
    std::os::unix::fs::symlink(dir.path().join("missing"), dir.path().join(segment_name(2).unwrap())).unwrap();
    for from in [0, 2] {
        let scan = Reader::scan(dir.path(), Lsn(from)).unwrap();
        assert_eq!((lsns(&scan), scan.end), ((from + 1..=3).collect::<Vec<_>>(), End::Clean), "from {from}");
    }
    // A file the scan does reach is an error.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    std::os::unix::fs::symlink(dir.path().join("missing"), dir.path().join(segment_name(2).unwrap())).unwrap();
    assert!(matches!(Reader::scan(dir.path(), Lsn(0)).unwrap_err(), JournalError::Io { .. }));
}

#[test]
fn damage_before_the_base_does_not_block_the_scan() {
    // Frame 2 of 1-10 is bad; the snapshot covers 1-5.
    let dir = tempfile::tempdir().unwrap();
    let records = commits(1, 10);
    let mut bytes = segment(1, &records);
    bytes[offset_of(&records, 1) + 8] ^= 0xff;
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(5)).unwrap();
    assert_eq!((lsns(&scan), scan.end), ((6..=10).collect::<Vec<_>>(), End::Clean));
    // Past the base the same damage is the end of the log.
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1]);
    assert!(matches!(scan.end, End::Damaged { .. }));
    // With nothing after the damage that reaches the base, it is the end of the log as before.
    let scan = Reader::scan(dir.path(), Lsn(20)).unwrap();
    assert_eq!(lsns(&scan), Vec::<u64>::new());
    assert!(matches!(scan.end, End::Damaged { .. } | End::TornTail { .. }));
}

#[test]
fn a_discontinuous_segment_without_a_valid_frame_is_a_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    put(dir.path(), 2, &segment(9, &[]));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2, 3]);
    assert_eq!(scan.end, End::TornTail { at: at(2, 0, 3) });
}

#[test]
fn a_stale_segment_after_a_zero_tail_is_never_a_start() {
    // Segment 1 holds 1-3 and a zero tail; segment 2 advertises first LSN 4 but is not a recovery session.
    let dir = tempfile::tempdir().unwrap();
    let mut first = segment(1, &commits(1, 3));
    first.extend([0u8; 40]);
    put(dir.path(), 1, &first);
    put(dir.path(), 2, &segment(4, &commits(4, 5)));
    for from in 0..=3 {
        let scan = Reader::scan(dir.path(), Lsn(from)).unwrap();
        assert_eq!((lsns(&scan), scan.end), ((from + 1..=3).collect::<Vec<_>>(), End::Clean), "from {from}");
    }
}

#[test]
fn damage_is_counted_only_up_to_a_prepared_or_bad_header_segment() {
    let records = commits(1, 3);
    for ender in [SegmentHeader::prepared().encode().to_vec(), vec![0xAA; HEADER_LEN]] {
        let dir = tempfile::tempdir().unwrap();
        let mut first = segment(1, &records);
        let torn = first.len() - 3;
        first.truncate(torn);
        put(dir.path(), 1, &first);
        put(dir.path(), 2, &ender);
        put(dir.path(), 3, &segment(3, &commits(3, 6)));
        let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
        assert_eq!(lsns(&scan), [1, 2]);
        assert_eq!(scan.end, End::TornTail { at: at(1, offset_of(&records, 2), 2) });
    }
}

#[test]
fn a_session_record_that_does_not_decode_is_not_the_recovery_exception() {
    let records = commits(1, 4);
    let full = segment(1, &records);
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &full[..full.len() - 3]);
    let mut bogus = SegmentHeader::new(Lsn(4)).encode().to_vec();
    write_frame(Lsn(4), kind::SESSION, 0, b"{}", &mut bogus).unwrap();
    put(dir.path(), 2, &bogus);
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2, 3]);
    assert!(matches!(scan.end, End::Damaged { later_valid: 1, .. }), "{:?}", scan.end);
}

/// How a generated segment ends.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tail {
    Eof,
    /// 40 zero bytes.
    Zeros,
    /// A zero length followed by header bytes that are not zero.
    ZeroLen,
}

/// What a generated segment is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    /// A real header at `first`, then `n` records (the first a `session` when `session` is set). `damage` is the index
    /// of a record whose payload is flipped (or, with `zeroed`, wiped to zeros), and `ghost` the number of valid frames written after a zero tail.
    Real {
        first: u64,
        n: u64,
        session: bool,
        tail: Tail,
        damage: Option<u64>,
        zeroed: bool,
        ghost: u64,
    },
    Prepared,
    BadHeader,
    Newer,
}

fn build(shape: Shape) -> Vec<u8> {
    match shape {
        Shape::Real { first, n, session: sess, tail, damage, zeroed, ghost } => {
            let mut records = commits(first, first + n);
            records.truncate(usize::try_from(n).unwrap());
            if sess && n > 0 {
                records[0] = session(first);
            }
            let mut bytes = segment(first, &records);
            if let Some(index) = damage {
                let index = usize::try_from(index).unwrap();
                let start = offset_of(&records, index);
                let end = start + frame(&records[index]).len();
                if zeroed {
                    bytes[start + FRAME_HEADER_LEN..end].fill(0);
                } else {
                    bytes[start + 25] ^= 0x04;
                }
            }
            if tail != Tail::Eof {
                if tail == Tail::Zeros {
                    bytes.extend([0u8; 40]);
                } else {
                    bytes.extend([0u8; 4]);
                    bytes.extend([0xAB; 40]);
                }
                encode(&commits(first + n + 3, first + n + 2 + ghost), &mut bytes).unwrap();
            }
            bytes
        }
        Shape::Prepared => SegmentHeader::prepared().encode().to_vec(),
        Shape::BadHeader => vec![0xAA; HEADER_LEN],
        Shape::Newer => newer(segment(1, &commits(1, 1))),
    }
}

/// What the reference scanner says a scan returns: the LSNs, and how it ended as `(kind, segment index)`.
type Model = Result<(Vec<u64>, char, usize), ()>;

enum Head {
    Real(u64),
    Prepared,
    TooNew,
    Bad,
}

fn head(file: &[u8]) -> Head {
    match SegmentHeader::decode(file) {
        Ok(header) if header.first.0 == 0 => Head::Prepared,
        Ok(header) => Head::Real(header.first.0),
        Err(HeaderFault::TooNew { .. }) => Head::TooNew,
        Err(_) => Head::Bad,
    }
}

/// A frame boundary that is known to be one is a zero tail when the length is 0 or a header's worth of zeros follows;
/// the end of the file is not one.
fn zero_tail(rest: &[u8]) -> bool {
    !rest.is_empty()
        && (matches!(read_frame(rest), Err(FrameEnd::Zero)) || rest.iter().take(FRAME_HEADER_LEN).all(|&b| b == 0))
}

/// What looking for the frame after the base in old damage finds.
enum Found {
    At(usize),
    Tail,
    Nothing,
}

/// A plain, slow scanner over the segment files as byte vectors: journal.md 8.2 read literally, with brute force
/// wherever the reader is clever. It defines the rules the reader is checked against.
struct Reference<'a> {
    files: &'a [Vec<u8>],
    from: u64,
}

impl Reference<'_> {
    /// How many valid frames a look past a bad frame finds in `file` from `offset`, trying every offset.
    fn count_valid(file: &[u8], offset: usize) -> usize {
        let (mut count, mut at) = (0, offset);
        while at < file.len() {
            match read_frame(&file[at..]) {
                Ok(frame) => {
                    count += 1;
                    at += frame.len();
                }
                Err(_) => at += 1,
            }
        }
        count
    }

    /// The valid frames of the files from `first` on, up to one whose header ends the log.
    fn later(&self, first: usize) -> usize {
        self.files
            .iter()
            .skip(first)
            .take_while(|file| matches!(head(file), Head::Real(_)))
            .map(|file| Self::count_valid(file, HEADER_LEN))
            .sum()
    }

    /// Whether file `i + 1` is a recovery session segment that continues the log at `expected`.
    fn recovery(&self, i: usize, expected: u64) -> bool {
        let Some(file) = self.files.get(i + 1) else { return false };
        let Head::Real(first) = head(file) else { return false };
        first == expected
            && read_frame(&file[HEADER_LEN..]).is_ok_and(|frame| {
                frame.lsn.0 == first
                    && frame.kind == kind::SESSION
                    && matches!(decode(&frame), Ok(Some(Record::Session(_))))
            })
    }

    /// The offset of the frame `from + 1` at or after `offset`, in damage the snapshot covers.
    fn find_base(&self, file: &[u8], offset: usize) -> Found {
        let mut at = offset;
        while at < file.len() {
            if let Ok(frame) = read_frame(&file[at..]) {
                if frame.lsn.0 == self.from + 1 {
                    return Found::At(at);
                }
                at += frame.len();
                if zero_tail(&file[at..]) {
                    return Found::Tail;
                }
            } else {
                // The bad frame's own length, when it fits, says where the next one starts.
                let len = file.get(at..at + 4).map(|b| usize::try_from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])));
                if at == offset
                    && let Some(Ok(len)) = len
                    && len != 0
                    && len <= MAX_PAYLOAD
                    && at + FRAME_HEADER_LEN + len <= file.len()
                    && zero_tail(&file[at + FRAME_HEADER_LEN + len..])
                {
                    return Found::Tail;
                }
                at += 1;
            }
        }
        Found::Nothing
    }

    fn scan(&self) -> Model {
        let (files, from) = (self.files, self.from);
        let mut out = Vec::new();
        let damage = |count: usize| if count > 0 { 'D' } else { 'T' };
        let Some(first_file) = files.first() else { return Ok((out, 'C', 0)) };
        match head(first_file) {
            Head::Prepared => return Ok((out, 'C', 0)),
            Head::Bad => return Ok((out, 'T', 0)),
            Head::TooNew => return Err(()),
            // The oldest segment starts after the base: records are missing.
            Head::Real(first) if first > from + 1 => return Ok((out, damage(self.later(0)), 0)),
            Head::Real(_) => {}
        }
        let mut i = 0;
        loop {
            let file = &files[i];
            let Head::Real(first) = head(file) else { unreachable!("only real segments are walked") };
            let (mut expected, mut offset) = (first, HEADER_LEN);
            let (stop, bad_at) = loop {
                match read_frame(&file[offset..]) {
                    Ok(frame) if frame.lsn.0 == expected => {
                        if expected > from {
                            out.push(expected);
                        }
                        expected += 1;
                        offset += frame.len();
                    }
                    Err(FrameEnd::Short) if offset == file.len() => break ('E', offset),
                    Err(FrameEnd::Zero) => break ('Z', offset),
                    Err(FrameEnd::Short) if file[offset..].iter().all(|&b| b == 0) => break ('Z', offset),
                    _ => {
                        // Old damage: the snapshot covers it, so look for the record after the base, unless a recovery
                        // session took over at the last good record.
                        if expected <= from && !self.recovery(i, expected) {
                            match self.find_base(file, offset) {
                                Found::At(found) => {
                                    expected = from + 1;
                                    offset = found;
                                    continue;
                                }
                                // A genuine tail: the log ends there.
                                Found::Tail => break ('Z', offset),
                                Found::Nothing => {}
                            }
                        }
                        break ('B', offset);
                    }
                }
            };
            let next = files.get(i + 1).map(|f| head(f));
            let covered = |f: u64| expected <= from && expected < f && f <= from + 1;
            let continues = match (stop, next) {
                ('E', None | Some(Head::Prepared)) => return Ok((out, 'C', i + 1)),
                ('E', Some(Head::Bad)) => return Ok((out, 'T', i + 1)),
                ('E', Some(Head::TooNew)) => return Err(()),
                ('E', Some(Head::Real(f))) => {
                    if f != expected && !covered(f) {
                        return Ok((out, damage(self.later(i + 1)), i + 1));
                    }
                    true
                }
                (_, Some(Head::Real(f))) => self.recovery(i, expected) || (stop == 'B' && covered(f)),
                _ => false,
            };
            if !continues {
                if stop == 'Z' {
                    return Ok((out, 'C', i + 1));
                }
                let count = Self::count_valid(file, bad_at + 1) + self.later(i + 1);
                return Ok((out, damage(count), i));
            }
            i += 1;
        }
    }
}

#[test]
fn random_layouts_match_the_reference_scanner() {
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut rand = move |bound: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % bound
    };
    for round in 0..2500_u32 {
        let count = 1 + rand(5);
        let mut layout = Vec::new();
        // The oldest segment starts at, before or after the base, depending on `from`.
        let mut expected = if rand(4) == 0 { 1 + rand(4) } else { 1 };
        let mut recover_at = None;
        for idx in 0..count {
            let pick = if idx == 0 { 0 } else { rand(12) };
            let mut shape = match pick {
                0..=3 => Shape::Real {
                    first: expected,
                    n: rand(4),
                    session: false,
                    tail: [Tail::Eof, Tail::Zeros, Tail::ZeroLen][usize::try_from(rand(3)).unwrap()],
                    damage: None,
                    zeroed: false,
                    ghost: 0,
                },
                4 | 5 => Shape::Real {
                    first: expected,
                    n: 1 + rand(3),
                    session: true,
                    tail: Tail::Eof,
                    damage: None,
                    zeroed: false,
                    ghost: 0,
                },
                // A stale or gapped segment: it starts anywhere, up to a little past the log.
                6 | 7 => Shape::Real {
                    first: 1 + rand(expected + 3),
                    n: 1 + rand(3),
                    session: rand(2) == 0,
                    tail: Tail::Eof,
                    damage: None,
                    zeroed: false,
                    ghost: 0,
                },
                8 => Shape::Prepared,
                9 => Shape::BadHeader,
                10 => Shape::Newer,
                _ => Shape::Real {
                    first: expected,
                    n: 2 + rand(4),
                    session: false,
                    tail: [Tail::Eof, Tail::Zeros, Tail::ZeroLen][usize::try_from(rand(3)).unwrap()],
                    damage: None,
                    zeroed: false,
                    ghost: 0,
                },
            };
            // A recovery session that takes over where a damaged record was.
            if let Some(first) = recover_at.take()
                && rand(2) == 0
            {
                shape = Shape::Real {
                    first,
                    n: 1 + rand(3),
                    session: true,
                    tail: Tail::Eof,
                    damage: None,
                    zeroed: false,
                    ghost: 0,
                };
            }
            if let Shape::Real { n, tail, damage, zeroed, ghost, .. } = &mut shape {
                if *n > 0 && rand(3) == 0 {
                    *damage = Some(rand(*n));
                    *zeroed = rand(2) == 0;
                }
                if *tail != Tail::Eof && rand(2) == 0 {
                    *ghost = 1 + rand(3);
                }
            }
            if let Shape::Real { first, n, damage, .. } = shape {
                expected = first + n;
                recover_at = damage.map(|index| first + index);
            }
            layout.push(shape);
        }
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<Vec<u8>> = layout.iter().map(|shape| build(*shape)).collect();
        for (idx, file) in files.iter().enumerate() {
            put(dir.path(), u64::try_from(idx).unwrap() + 1, file);
        }
        compare(dir.path(), &files, &layout, round);
    }
}

/// Scans `dir` from every LSN and checks it against the reference scanner.
fn compare(dir: &Path, files: &[Vec<u8>], layout: &[Shape], round: u32) {
    let last = layout
        .iter()
        .map(|shape| if let Shape::Real { first, n, .. } = shape { first + n } else { 0 })
        .max()
        .unwrap_or(0);
    for from in 0..=last + 1 {
        let want = Reference { files, from }.scan();
        let got = Reader::scan(dir, Lsn(from));
        match (&want, got) {
            (Err(()), Err(JournalError::TooNew { .. })) => {}
            (Ok((lsns_want, kind, seg)), Ok(scan)) => {
                assert_eq!(lsns(&scan), *lsns_want, "round {round} from {from}: {layout:?}");
                let (got_kind, got_seg) = match scan.end {
                    End::Clean => ('C', None),
                    End::TornTail { at } => ('T', Some(at.segment)),
                    End::Damaged { at, .. } => ('D', Some(at.segment)),
                    _ => unreachable!(),
                };
                assert_eq!(got_kind, *kind, "round {round} from {from}: {layout:?}");
                if let Some(got_seg) = got_seg {
                    assert_eq!(got_seg, u64::try_from(*seg).unwrap() + 1, "round {round} from {from}: {layout:?}");
                }
            }
            (want, got) => panic!("round {round} from {from}: reference {:?}, scan {got:?}: {layout:?}", want.is_ok()),
        }
    }
}

#[test]
fn a_segment_that_restarts_inside_the_log_is_a_discontinuity() {
    // Segment 2 restarts at 2, overlapping segment 1 (LSNs 1-3): the increasing first LSNs hide nothing.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    put(dir.path(), 2, &segment(2, &commits(2, 4)));
    for from in 0..=3 {
        let scan = Reader::scan(dir.path(), Lsn(from)).unwrap();
        assert_eq!(lsns(&scan), (from + 1..=3).collect::<Vec<_>>(), "from {from}");
        assert_eq!(scan.end, End::Damaged { at: at(2, 0, 3), later_valid: 3, search_complete: true }, "from {from}");
    }
}

#[test]
fn the_frame_that_exposes_a_gap_before_the_base_is_the_base() {
    // Records 1, then 6 to 10: frame 6 fails the sequence check but is the record after the snapshot at 5.
    let dir = tempfile::tempdir().unwrap();
    let mut records = vec![commit(1)];
    records.extend(commits(6, 10));
    put(dir.path(), 1, &segment(1, &records));
    let scan = Reader::scan(dir.path(), Lsn(5)).unwrap();
    assert_eq!((lsns(&scan), scan.end), ((6..=10).collect::<Vec<_>>(), End::Clean));
}

#[test]
fn a_gap_covered_by_the_base_does_not_block_the_scan() {
    // Segment 1 ends exactly after LSN 3, segment 2 starts at 6; the snapshot is at 5.
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(1, &commits(1, 3)));
    put(dir.path(), 2, &segment(6, &commits(6, 8)));
    let scan = Reader::scan(dir.path(), Lsn(5)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![6, 7, 8], End::Clean));
    // A gap the base doesn't cover is a discontinuity.
    let scan = Reader::scan(dir.path(), Lsn(3)).unwrap();
    assert_eq!(lsns(&scan), Vec::<u64>::new());
    assert_eq!(scan.end, End::Damaged { at: at(2, 0, 3), later_valid: 3, search_complete: true });
}

#[test]
fn damage_counting_stops_at_a_newer_format_segment() {
    let records = commits(1, 3);
    let dir = tempfile::tempdir().unwrap();
    let mut first = segment(1, &records);
    let torn = first.len() - 3;
    first.truncate(torn);
    put(dir.path(), 1, &first);
    put(dir.path(), 2, &newer(segment(3, &commits(3, 3))));
    put(dir.path(), 3, &segment(3, &commits(3, 6)));
    let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
    assert_eq!(lsns(&scan), [1, 2]);
    assert_eq!(scan.end, End::TornTail { at: at(1, offset_of(&records, 2), 2) });
}

#[test]
fn old_damage_is_not_resynchronized_past_a_recovery_session() {
    // Segment 1: valid 1, damaged 2, intact 3-8. A recovery segment starts with session 2: 3-8 were discarded.
    let records = commits(1, 8);
    let mut bytes = segment(1, &records);
    bytes[offset_of(&records, 1) + 25] ^= 0x04;
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let mut recovered = SegmentHeader::new(Lsn(2)).encode().to_vec();
    encode(&[session(2), commit(3)], &mut recovered).unwrap();
    put(dir.path(), 2, &recovered);
    let scan = Reader::scan(dir.path(), Lsn(2)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (vec![3], End::Clean));
}

#[test]
fn old_damage_resynchronization_stops_at_a_zero_tail() {
    // Valid 1, damaged 2, valid 3, a zero tail, then stale 6-10 in the same file.
    let records = commits(1, 3);
    let mut bytes = segment(1, &records);
    bytes[offset_of(&records, 1) + 25] ^= 0x04;
    bytes.extend([0u8; 40]);
    encode(&commits(6, 10), &mut bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(5)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (Vec::<u64>::new(), End::Clean));
}

#[test]
fn zeros_inside_old_damage_are_not_a_tail() {
    // Frame 2 of 1-8 has its payload wiped to zeros (a zeroed sector); the intact frames after it hold the base.
    let records = commits(1, 8);
    let mut bytes = segment(1, &records);
    let start = offset_of(&records, 1);
    let end = offset_of(&records, 2);
    bytes[start + FRAME_HEADER_LEN..end].fill(0);
    assert!(end - start - FRAME_HEADER_LEN >= 20, "the wiped run must be a frame header long");
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(4)).unwrap();
    assert_eq!((lsns(&scan), scan.end), ((5..=8).collect::<Vec<_>>(), End::Clean));
}

#[test]
fn an_oldest_segment_that_starts_after_the_base_is_a_gap() {
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &segment(10, &commits(10, 12)));
    // 6 to 9 are missing: nothing is trusted, and the frames that are there are counted as damage.
    let scan = Reader::scan(dir.path(), Lsn(5)).unwrap();
    assert_eq!(lsns(&scan), Vec::<u64>::new());
    assert_eq!(scan.end, End::Damaged { at: at(1, 0, 5), later_valid: 3, search_complete: true });
    // Starting exactly at the base + 1, or before it, is read.
    for (from, want) in [(9, vec![10, 11, 12]), (10, vec![11, 12]), (12, vec![])] {
        let scan = Reader::scan(dir.path(), Lsn(from)).unwrap();
        assert_eq!((lsns(&scan), scan.end), (want, End::Clean), "from {from}");
    }
}

#[test]
fn a_tail_found_while_resynchronizing_ends_the_log() {
    // Segment 1: valid 1, damaged 2, valid 3, then zeros. Segment 2 holds non-session records 6-10.
    let records = commits(1, 3);
    let mut bytes = segment(1, &records);
    bytes[offset_of(&records, 1) + 25] ^= 0x04;
    bytes.extend([0u8; 40]);
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    put(dir.path(), 2, &segment(6, &commits(6, 10)));
    let scan = Reader::scan(dir.path(), Lsn(5)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (Vec::<u64>::new(), End::Clean));
}

#[test]
fn a_zero_length_at_an_aligned_boundary_is_a_tail_whatever_follows_it() {
    // Valid 1, damaged 2, valid 3, then a zero length with junk after it instead of a header's worth of zeros, then a
    // stale frame 6. The zero length ends the log.
    let records = commits(1, 3);
    let mut bytes = segment(1, &records);
    bytes[offset_of(&records, 1) + 25] ^= 0x04;
    bytes.extend([0u8; 4]);
    bytes.extend([0xAB; 40]);
    encode(&commits(6, 6), &mut bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    put(dir.path(), 1, &bytes);
    let scan = Reader::scan(dir.path(), Lsn(5)).unwrap();
    assert_eq!((lsns(&scan), scan.end), (Vec::<u64>::new(), End::Clean));
}
