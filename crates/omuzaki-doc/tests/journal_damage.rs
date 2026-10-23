//! Damage to a journal never makes a scan trust a record that wasn't written (journal.md 8.2): every truncation, every
//! single-bit flip, zero tails, LSN gaps and oversize lengths, and a property test over random damage.
//!
//! The journal is three segments of ten one-field commits each (LSN 1 to 30), a few hundred bytes a segment, so the
//! whole suite runs in seconds.
#![allow(clippy::unwrap_used)] // Helpers outside `#[test]` fns; a failing helper should just panic.

use std::fs;
use std::path::Path;

use omuzaki_doc::journal::{End, HEADER_LEN, MAX_PAYLOAD, Reader, Record, Scan, SegmentHeader, encode, segment_name};
use omuzaki_history_api::{HistoryEntry, Lsn};
use omuzaki_ops_api::TxId;
use proptest::prelude::*;

fn commit(lsn: u64) -> (Lsn, Record) {
    (Lsn(lsn), Record::Entry(HistoryEntry::commit(TxId(lsn))))
}

fn frame_len(record: &(Lsn, Record)) -> usize {
    let mut out = Vec::new();
    encode(std::slice::from_ref(record), &mut out).unwrap();
    out.len()
}

/// A segment file holding `records`, whose first LSN is the first record's.
fn segment(records: &[(Lsn, Record)]) -> Vec<u8> {
    let first = records.first().map_or(Lsn(1), |(lsn, _)| *lsn);
    let mut bytes = SegmentHeader::new(first).encode().to_vec();
    encode(records, &mut bytes).unwrap();
    bytes
}

/// The records `first..=last`.
fn commits(first: u64, last: u64) -> Vec<(Lsn, Record)> {
    (first..=last).map(commit).collect()
}

/// Writes `segments` as files 1, 2, ... into `dir`, replacing what was there.
fn write_journal(dir: &Path, segments: &[Vec<u8>]) {
    for entry in fs::read_dir(dir).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    for (bytes, seq) in segments.iter().zip(1u64..) {
        fs::write(dir.join(segment_name(seq).unwrap()), bytes).unwrap();
    }
}

/// The three-segment journal: the records of each segment, and their files.
type Journal = ([Vec<(Lsn, Record)>; 3], Vec<Vec<u8>>);

fn journal() -> Journal {
    let records = [commits(1, 10), commits(11, 20), commits(21, 30)];
    let files = records.iter().map(|r| segment(r)).collect();
    (records, files)
}

fn all(records: &[Vec<(Lsn, Record)>; 3]) -> Vec<(Lsn, Record)> {
    records.iter().flatten().cloned().collect()
}

/// Where each frame of the last segment starts, and where the segment ends.
fn boundaries(records: &[(Lsn, Record)]) -> Vec<usize> {
    let mut at = HEADER_LEN;
    let mut out = vec![at];
    for record in records {
        at += frame_len(record);
        out.push(at);
    }
    out
}

fn scan_last(dir: &Path, files: &[Vec<u8>], last: Vec<u8>) -> Scan {
    let mut files = files.to_vec();
    files[2] = last;
    write_journal(dir, &files);
    Reader::scan(dir, Lsn(0)).unwrap()
}

#[test]
fn every_truncation_reads_a_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let (records, files) = journal();
    let written = all(&records);
    let bounds = boundaries(&records[2]);
    for len in 0..=files[2].len() {
        let scan = scan_last(dir.path(), &files, files[2][..len].to_vec());
        // The frames that fit whole.
        let fitting = if len < HEADER_LEN { 0 } else { bounds.iter().skip(1).take_while(|&&end| end <= len).count() };
        assert_eq!(scan.records, written[..20 + fitting], "cut at {len}");
        let on_boundary = len >= HEADER_LEN && bounds.contains(&len);
        assert_eq!(scan.end == End::Clean, on_boundary, "cut at {len}: {:?}", scan.end);
        if !on_boundary {
            assert!(matches!(scan.end, End::TornTail { .. }), "cut at {len}: {:?}", scan.end);
        }
    }
}

#[test]
fn every_single_bit_flip_is_caught() {
    let dir = tempfile::tempdir().unwrap();
    let (records, files) = journal();
    let written = all(&records);
    let bounds = boundaries(&records[2]);
    for byte in 0..files[2].len() {
        for bit in 0..8 {
            let mut flipped = files[2].clone();
            flipped[byte] ^= 1 << bit;
            let scan = scan_last(dir.path(), &files, flipped.clone());
            // The frame holding the flipped byte; the header counts as "before the first frame".
            let hit = bounds.iter().skip(1).position(|&end| byte < end).filter(|_| byte >= HEADER_LEN);
            let frames_before = hit.unwrap_or(0);
            assert_eq!(scan.records, written[..20 + frames_before], "byte {byte} bit {bit}");
            match scan.end {
                End::Clean => {
                    let in_length = hit.is_some() && byte < bounds[frames_before] + 4;
                    let zeroed = flipped[bounds[frames_before]..bounds[frames_before] + 4] == [0; 4];
                    assert!(in_length && zeroed, "byte {byte} bit {bit} ended Clean");
                }
                End::TornTail { at } => {
                    // A bad header ends the log whatever follows (rule 2); a bad frame is a torn tail only at the end.
                    let ends_the_segment = hit.is_none_or(|h| h + 1 == records[2].len());
                    assert!(ends_the_segment, "byte {byte} bit {bit}: a torn tail with valid frames after it");
                    assert_eq!(at.after, written[19 + frames_before].0);
                }
                End::Damaged { at, later_valid, .. } => {
                    assert!(hit.is_some(), "byte {byte} bit {bit}: a header fault is never damage");
                    let after = records[2].len() - frames_before - 1;
                    assert_eq!(later_valid, after, "byte {byte} bit {bit}");
                    assert_eq!(at.after, written[19 + frames_before].0);
                }
                other => panic!("byte {byte} bit {bit}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_flip_in_an_earlier_segment_is_damage() {
    let dir = tempfile::tempdir().unwrap();
    let (records, files) = journal();
    let written = all(&records);
    for (segment_index, frame_in_segment) in [(0usize, 3usize), (1, 0), (1, 9)] {
        let mut files = files.clone();
        let kept = segment_index * 10 + frame_in_segment;
        // A payload byte of the frame.
        files[segment_index][boundaries(&records[segment_index])[frame_in_segment] + 25] ^= 0x04;
        write_journal(dir.path(), &files);
        let scan = Reader::scan(dir.path(), Lsn(0)).unwrap();
        assert_eq!(scan.records, written[..kept]);
        match scan.end {
            End::Damaged { later_valid, .. } => assert!(later_valid > 0),
            other => panic!("segment {segment_index} frame {frame_in_segment}: {other:?}"),
        }
    }
}

#[test]
fn zero_tails_are_clean() {
    let dir = tempfile::tempdir().unwrap();
    let (records, files) = journal();
    for zeros in [1, 19, 20, 4096] {
        let mut last = files[2].clone();
        last.extend(std::iter::repeat_n(0u8, zeros));
        let scan = scan_last(dir.path(), &files, last);
        assert_eq!((scan.records, scan.end), (all(&records), End::Clean), "{zeros} zeros");
    }
}

#[test]
fn a_gap_ends_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let (records, files) = journal();
    let mut with_gap = commits(21, 24);
    with_gap.extend(commits(26, 30));
    let scan = scan_last(dir.path(), &files, segment(&with_gap));
    assert_eq!(scan.records, all(&records)[..24]);
    assert!(matches!(scan.end, End::Damaged { later_valid: 4, .. }), "{:?}", scan.end);
}

#[test]
fn an_oversize_length_ends_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let (records, files) = journal();
    let fifth = boundaries(&records[2])[4];
    for length in [u32::try_from(MAX_PAYLOAD).unwrap() + 1, u32::MAX] {
        let mut last = files[2].clone();
        last[fifth..fifth + 4].copy_from_slice(&length.to_le_bytes());
        let scan = scan_last(dir.path(), &files, last);
        assert_eq!(scan.records, all(&records)[..24], "length {length}");
        assert!(matches!(scan.end, End::Damaged { later_valid: 5, .. }), "length {length}: {:?}", scan.end);
    }
}

/// A random journal and random damage to it.
#[derive(Debug)]
struct Case {
    count: usize,
    /// Where the second and third segments start, as fractions of the record count in 0..=1000.
    splits: [usize; 2],
    truncate: Option<usize>,
    flips: Vec<(usize, usize, u8)>,
    garbage: Vec<u8>,
}

fn case() -> impl Strategy<Value = Case> {
    (
        1usize..=40,
        [0usize..=1000, 0usize..=1000],
        proptest::option::of(any::<usize>()),
        proptest::collection::vec((any::<usize>(), any::<usize>(), 0u8..8), 0..4),
        proptest::collection::vec(any::<u8>(), 0..48),
    )
        .prop_map(|(count, splits, truncate, flips, garbage)| Case { count, splits, truncate, flips, garbage })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn scans_never_trust_damage(case in case()) {
        let written = commits(1, u64::try_from(case.count).unwrap());
        let mut cuts = [case.splits[0] * case.count / 1000, case.splits[1] * case.count / 1000];
        cuts.sort_unstable();
        let parts = [&written[..cuts[0]], &written[cuts[0]..cuts[1]], &written[cuts[1]..]];
        // An empty part is not a segment: segments always hold at least the record they start at.
        let mut files: Vec<Vec<u8>> = parts.iter().filter(|p| !p.is_empty()).map(|p| segment(p)).collect();

        let undamaged = case.truncate.is_none() && case.flips.is_empty() && case.garbage.is_empty();
        if let Some(cut) = case.truncate
            && let Some(last) = files.last_mut()
        {
            last.truncate(cut % (last.len() + 1));
        }
        for &(which, byte, bit) in &case.flips {
            let index = which % files.len();
            let file = &mut files[index];
            let at = byte % file.len().max(1);
            if let Some(b) = file.get_mut(at) {
                *b ^= 1 << bit;
            }
        }
        if let Some(last) = files.last_mut() {
            last.extend_from_slice(&case.garbage);
        }

        let dir = tempfile::tempdir().unwrap();
        write_journal(dir.path(), &files);
        let scan = Reader::scan(dir.path(), Lsn(0));
        // A CRC-protected journal gives no payload or header that reads as something else, so no scan fails.
        let scan = scan.map_err(|e| TestCaseError::fail(format!("{e:?}")))?;
        prop_assert!(scan.records.len() <= written.len());
        prop_assert_eq!(&scan.records[..], &written[..scan.records.len()]);
        if undamaged {
            prop_assert_eq!(scan.records.len(), written.len());
            prop_assert_eq!(scan.end, End::Clean);
        }
    }
}
