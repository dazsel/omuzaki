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
fn segment_names() {
    assert_eq!(segment_name(42), "segment-000000000042.tjl");
    assert_eq!(segment_seq(&segment_name(42)), Some(42));
    assert_eq!(segment_seq("segment-42.tjl"), None);
    assert_eq!(segment_seq("segment-000000000042.tmp"), None);
    assert_eq!(segment_seq("x.tjl"), None);
}
