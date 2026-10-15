//! The journal's byte format (journal.md 4.1 to 4.3): segment headers, record frames and file names, checked against
//! hand-built bytes so a change to the layout fails here.

use omuzaki_doc::journal::{
    FORMAT_VERSION, Frame, FrameEnd, HEADER_LEN, HeaderFault, JournalError, MAX_PAYLOAD, SKIPPABLE, SegmentHeader,
    read_frame, segment_name, segment_seq, write_frame,
};
use omuzaki_history_api::Lsn;

/// Recomputes the header CRC (bytes 28 to 31) of a 32-byte header, so a test can change a field and keep the CRC valid.
fn with_crc(mut bytes: [u8; HEADER_LEN]) -> [u8; HEADER_LEN] {
    let crc = crc32fast::hash(&bytes[..28]);
    bytes[28..32].copy_from_slice(&crc.to_le_bytes());
    bytes
}

#[test]
fn a_header_round_trips() {
    let header = SegmentHeader::new(Lsn(42));
    let bytes = header.encode();
    assert_eq!(SegmentHeader::decode(&bytes), Ok(header));
    assert_eq!(header.version, FORMAT_VERSION);
    assert!(!header.is_prepared());
}

#[test]
fn a_prepared_header_reads_as_prepared() {
    let header = SegmentHeader::prepared();
    assert_eq!(header.first, Lsn(0));
    assert!(header.is_prepared());
    assert_eq!(SegmentHeader::decode(&header.encode()), Ok(header));
}

#[test]
fn the_header_bytes_are_the_documented_layout() {
    let mut expected = [0u8; HEADER_LEN];
    expected[0..8].copy_from_slice(b"TUBRJNL\0");
    expected[8..10].copy_from_slice(&1u16.to_le_bytes());
    expected[10..12].copy_from_slice(&32u16.to_le_bytes());
    expected[16..24].copy_from_slice(&1u64.to_le_bytes());
    let crc = crc32fast::hash(&expected[..28]);
    expected[28..32].copy_from_slice(&crc.to_le_bytes());

    assert_eq!(SegmentHeader::new(Lsn(1)).encode(), expected);
}

#[test]
fn header_faults() {
    let valid = SegmentHeader::new(Lsn(1)).encode();

    assert_eq!(SegmentHeader::decode(&valid[..31]), Err(HeaderFault::Short));

    let mut bad_magic = valid;
    bad_magic[0] ^= 0xff;
    assert_eq!(SegmentHeader::decode(&bad_magic), Err(HeaderFault::Magic));

    let mut flipped = valid;
    flipped[20] ^= 0x01;
    assert_eq!(SegmentHeader::decode(&flipped), Err(HeaderFault::Crc));

    let mut newer = valid;
    newer[8..10].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(SegmentHeader::decode(&with_crc(newer)), Err(HeaderFault::TooNew { version: 2 }));

    let mut longer = valid;
    longer[10..12].copy_from_slice(&40u16.to_le_bytes());
    assert_eq!(SegmentHeader::decode(&with_crc(longer)), Err(HeaderFault::Malformed));
}

#[test]
fn a_frame_round_trips() {
    let mut out = Vec::new();
    write_frame(Lsn(7), 3, 0, b"hello", &mut out).unwrap();
    assert_eq!(out.len(), 25);
    assert_eq!(&out[18..20], &[0, 0], "reserved bytes are written as 0");
    let frame = read_frame(&out).unwrap();
    assert_eq!(frame, Frame { lsn: Lsn(7), kind: 3, flags: 0, payload: b"hello" });
    assert_eq!(frame.len(), 25);
    assert!(!frame.skippable());
}

#[test]
fn a_skippable_frame_round_trips() {
    let mut out = Vec::new();
    write_frame(Lsn(9), 11, SKIPPABLE, b"{}", &mut out).unwrap();
    assert_eq!(&out[18..20], &[0, 0], "reserved bytes are written as 0");
    let frame = read_frame(&out).unwrap();
    assert_eq!(frame, Frame { lsn: Lsn(9), kind: 11, flags: SKIPPABLE, payload: b"{}" });
    assert!(frame.skippable());
}

#[test]
fn frame_ends() {
    let mut valid = Vec::new();
    write_frame(Lsn(5), 2, 0, b"payload", &mut valid).unwrap();

    assert_eq!(read_frame(&valid[..19]), Err(FrameEnd::Short));
    assert_eq!(read_frame(&[0u8; 20]), Err(FrameEnd::Zero));

    let mut oversize = [0u8; 20];
    oversize[0..4].copy_from_slice(&u32::try_from(MAX_PAYLOAD + 1).unwrap().to_le_bytes());
    assert_eq!(read_frame(&oversize), Err(FrameEnd::Oversize));

    assert_eq!(read_frame(&valid[..valid.len() - 1]), Err(FrameEnd::PastEnd));

    for at in [8, 16, 17, 20] {
        let mut flipped = valid.clone();
        flipped[at] ^= 0x01;
        assert_eq!(read_frame(&flipped), Err(FrameEnd::Crc), "a flipped bit at {at}");
    }
}

#[test]
fn an_oversize_payload_is_not_written() {
    let mut out = b"keep".to_vec();
    let payload = vec![0u8; MAX_PAYLOAD + 1];
    let result = write_frame(Lsn(3), 1, 0, &payload, &mut out);
    assert!(matches!(result, Err(JournalError::Unencodable { lsn: 3, .. })));
    assert_eq!(out, b"keep");
}

#[test]
fn an_empty_payload_is_not_written() {
    let mut out = Vec::new();
    write_frame(Lsn(1), 1, 0, b"first", &mut out).unwrap();
    let before = out.clone();
    let result = write_frame(Lsn(2), 1, 0, b"", &mut out);
    assert!(matches!(result, Err(JournalError::Unencodable { lsn: 2, .. })));
    assert_eq!(out, before, "the rejection leaves the existing bytes unchanged");
    let first = read_frame(&out).unwrap();
    assert_eq!(read_frame(&out[first.len()..]), Err(FrameEnd::Short));
}

#[test]
fn every_accepted_frame_reads_back() {
    // One buffer for the largest payload, so the 64 MiB is allocated once.
    let big = vec![0xa5u8; MAX_PAYLOAD];
    let lengths = [1, 2, 19, 20, 21, 255, 256, 65_535, 65_536, MAX_PAYLOAD];
    for len in lengths {
        let payload = &big[..len];
        for kind in [0u8, 1, 127, 128, 255] {
            for flags in [0u8, SKIPPABLE, 0xff] {
                let lsn = Lsn(u64::from(kind) + 1);
                let mut out = Vec::new();
                write_frame(lsn, kind, flags, payload, &mut out).unwrap();
                let frame =
                    read_frame(&out).unwrap_or_else(|end| panic!("len {len} kind {kind} flags {flags}: {end:?}"));
                assert_eq!(frame, Frame { lsn, kind, flags, payload });
                assert_eq!(frame.len(), out.len());
            }
        }
    }
}

#[test]
fn an_oversize_payload_is_refused_before_it_is_copied() {
    // The length check reads only the slice length, so a slice that is never touched is enough; the one 64 MiB buffer
    // is shared with the largest accepted payload.
    let big = vec![0u8; MAX_PAYLOAD + 1];
    let mut out = Vec::new();
    let result = write_frame(Lsn(1), 1, 0, &big, &mut out);
    assert!(matches!(result, Err(JournalError::Unencodable { .. })));
    assert_eq!(out.len(), 0);
    assert_eq!(out.capacity(), 0, "nothing was reserved");
}

#[test]
fn every_accepted_header_reads_back() {
    for first in [0u64, 1, 2, u64::from(u32::MAX), u64::MAX - 1, u64::MAX] {
        let header = SegmentHeader::new(Lsn(first));
        assert_eq!(SegmentHeader::decode(&header.encode()), Ok(header), "first {first}");
        assert_eq!(header.is_prepared(), first == 0);
    }
    let prepared = SegmentHeader::prepared();
    assert_eq!(SegmentHeader::decode(&prepared.encode()), Ok(prepared));
    assert!(SegmentHeader::decode(&prepared.encode()).unwrap().is_prepared());
}

#[test]
fn segment_names_stop_at_twelve_digits() {
    let last = segment_name(999_999_999_999).unwrap();
    assert_eq!(last, "segment-999999999999.tjl");
    assert_eq!(segment_seq(&last), Some(999_999_999_999));
    assert_eq!(segment_name(1_000_000_000_000), None);
    assert_eq!(segment_name(u64::MAX), None);
}

#[test]
fn segment_names_sort_like_their_numbers() {
    let seqs = [0u64, 1, 9, 10, 99, 100, 999_999_999_999];
    let names: Vec<String> = seqs.iter().map(|&n| segment_name(n).unwrap()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    for (n, name) in seqs.iter().zip(&names) {
        assert_eq!(segment_seq(name), Some(*n));
    }
}

/// The frozen v1 segment (tests/fixtures/journal/v1/README.md): written once by a script that does not use the journal
/// code, and never regenerated. Every expected value below is written out here, not computed by `write_frame`.
#[test]
fn the_frozen_v1_segment_reads() {
    let bytes = include_bytes!("fixtures/journal/v1/segment-000000000001.tjl");
    assert_eq!(bytes.len(), 131);

    assert_eq!(&bytes[0..8], b"TUBRJNL\0");
    assert_eq!(&bytes[28..32], &0xe82a_a879_u32.to_le_bytes(), "the header CRC");
    let header = SegmentHeader::decode(bytes).unwrap();
    assert_eq!(header, SegmentHeader { version: 1, first: Lsn(1) });

    // (lsn, kind, flags, payload, CRC), in file order.
    let expected: [(u64, u8, u8, &[u8], u32); 3] = [
        (1, 2, 0, br#"{"a":1}"#, 0x40e0_f6a2),
        (2, 200, SKIPPABLE, br#"{"skip":true}"#, 0x41f6_1cdb),
        (300, 7, 0, br#"{"k":"v","n":[1,2]}"#, 0x8bab_b05f),
    ];
    let mut at = HEADER_LEN;
    for (lsn, kind, flags, payload, crc) in expected {
        assert_eq!(&bytes[at + 4..at + 8], &crc.to_le_bytes(), "the CRC of record {lsn}");
        let frame = read_frame(&bytes[at..]).unwrap();
        assert_eq!(frame, Frame { lsn: Lsn(lsn), kind, flags, payload });
        assert_eq!(frame.skippable(), flags == SKIPPABLE);
        at += frame.len();
    }
    assert_eq!(at, bytes.len());
    assert_eq!(read_frame(&bytes[at..]), Err(FrameEnd::Short));
}

#[test]
fn segment_names() {
    assert_eq!(segment_name(42).as_deref(), Some("segment-000000000042.tjl"));
    assert_eq!(segment_seq(&segment_name(42).unwrap()), Some(42));
    assert_eq!(segment_seq("segment-42.tjl"), None);
    assert_eq!(segment_seq("segment-000000000042.tmp"), None);
    assert_eq!(segment_seq("x.tjl"), None);
}
