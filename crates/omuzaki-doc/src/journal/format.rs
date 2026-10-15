//! The journal's byte format (journal.md 4.1 to 4.3): segment headers, record frames and segment file names.
//!
//! Everything here is little-endian. Reserved fields are written as 0 and not checked on read; the CRCs cover them.

use omuzaki_history_api::Lsn;

use super::JournalError;

/// The first 8 bytes of every segment.
pub const MAGIC: [u8; 8] = *b"TUBRJNL\0";
/// The format version this build writes. Version 0 is never used.
pub const FORMAT_VERSION: u16 = 1;
/// Bytes in a segment header.
pub const HEADER_LEN: usize = 32;
/// Bytes in a record frame before its payload.
pub const FRAME_HEADER_LEN: usize = 20;
/// The payload cap (4.3): 64 MiB.
pub const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
/// Flag bit 0: a reader that doesn't know the kind may skip the record.
pub const SKIPPABLE: u8 = 1;

/// A segment's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentHeader {
    /// The format version.
    pub version: u16,
    /// The segment's first LSN; `Lsn(0)` while the segment is prepared and unused.
    pub first: Lsn,
}

impl SegmentHeader {
    /// A header of [`FORMAT_VERSION`] for a segment whose first record is `first`.
    #[must_use]
    pub fn new(first: Lsn) -> Self {
        Self { version: FORMAT_VERSION, first }
    }

    /// The header of a segment that is prepared ahead of time and not yet used.
    #[must_use]
    pub fn prepared() -> Self {
        Self::new(Lsn(0))
    }

    /// Whether this header is the one of a prepared, unused segment.
    #[must_use]
    pub fn is_prepared(&self) -> bool {
        self.first == Lsn(0)
    }

    /// The 32 bytes: magic, version, header length 32, reserved 0, first LSN, reserved 0, then the CRC-32 of bytes 0
    /// to 27.
    #[must_use]
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0..8].copy_from_slice(&MAGIC);
        out[8..10].copy_from_slice(&self.version.to_le_bytes());
        // The header length is 32 (4.2); the constant is written out so the cast lint stays quiet.
        out[10..12].copy_from_slice(&32u16.to_le_bytes());
        out[16..24].copy_from_slice(&self.first.0.to_le_bytes());
        let crc = crc32fast::hash(&out[..28]);
        out[28..32].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Reads a header from the start of `bytes`.
    ///
    /// The checks run in order: length, magic, CRC, then the version and header length.
    ///
    /// # Errors
    /// The [`HeaderFault`] of the first check that fails.
    pub fn decode(bytes: &[u8]) -> Result<Self, HeaderFault> {
        let Some(h) = bytes.first_chunk::<HEADER_LEN>() else {
            return Err(HeaderFault::Short);
        };
        if h[0..8] != MAGIC {
            return Err(HeaderFault::Magic);
        }
        if crc32fast::hash(&h[..28]) != u32::from_le_bytes([h[28], h[29], h[30], h[31]]) {
            return Err(HeaderFault::Crc);
        }
        let version = u16::from_le_bytes([h[8], h[9]]);
        if version > FORMAT_VERSION {
            return Err(HeaderFault::TooNew { version });
        }
        let header_len = u16::from_le_bytes([h[10], h[11]]);
        if version == 0 || usize::from(header_len) != HEADER_LEN {
            return Err(HeaderFault::Malformed);
        }
        let mut first = [0u8; 8];
        first.copy_from_slice(&h[16..24]);
        Ok(Self { version, first: Lsn(u64::from_le_bytes(first)) })
    }
}

/// Why a segment header doesn't read. Checked in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderFault {
    /// Fewer than 32 bytes.
    Short,
    /// The first 8 bytes aren't the journal magic.
    Magic,
    /// The CRC-32 of bytes 0 to 27 doesn't match.
    Crc,
    /// A valid header of a newer format.
    TooNew {
        /// The version the header names.
        version: u16,
    },
    /// Version 0, or a header length other than 32.
    Malformed,
}

/// One record frame as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame<'a> {
    /// The record's log sequence number.
    pub lsn: Lsn,
    /// The record kind (4.4).
    pub kind: u8,
    /// The flags byte; bit 0 is [`SKIPPABLE`].
    pub flags: u8,
    /// The payload: one JSON object (4.5).
    pub payload: &'a [u8],
}

impl Frame<'_> {
    /// Bytes the frame takes: 20 plus the payload.
    // A frame always has its 20-byte header, so it is never empty and `is_empty` would be meaningless.
    #[allow(clippy::len_without_is_empty)]
    #[must_use]
    pub fn len(&self) -> usize {
        FRAME_HEADER_LEN + self.payload.len()
    }

    /// Whether a reader that doesn't know the kind may skip this record.
    #[must_use]
    pub fn skippable(&self) -> bool {
        self.flags & SKIPPABLE != 0
    }
}

/// Why no frame reads at an offset (journal.md 8.2). Checked in this order. Each one ends the log there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameEnd {
    /// Fewer than 20 bytes remain.
    Short,
    /// The length is 0: a zero-filled tail. [`write_frame`] never writes one, so a zero length is never a record.
    Zero,
    /// The length is over [`MAX_PAYLOAD`].
    Oversize,
    /// The frame runs past the end of the data.
    PastEnd,
    /// The CRC (over bytes 0 to 3, 8 to 19 and the payload) doesn't match.
    Crc,
}

/// The CRC-32 of a frame: over the length, bytes 8 to 19 (LSN, kind, flags, reserved) and the payload.
fn frame_crc(len: [u8; 4], rest: &[u8; FRAME_HEADER_LEN - 8], payload: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&len);
    hasher.update(rest);
    hasher.update(payload);
    hasher.finalize()
}

/// Appends one frame (length, CRC, LSN, kind, flags, reserved 0, payload) to `out`.
///
/// Every frame this accepts is one [`read_frame`] reads back: a zero length reads as the end of the log, so an empty
/// payload is refused.
///
/// # Errors
/// [`JournalError::Unencodable`] when the payload is empty or over [`MAX_PAYLOAD`]; `out` is unchanged then.
pub fn write_frame(lsn: Lsn, kind: u8, flags: u8, payload: &[u8], out: &mut Vec<u8>) -> Result<(), JournalError> {
    let unencodable = |reason: String| JournalError::Unencodable { lsn: lsn.0, reason };
    if payload.is_empty() {
        return Err(unencodable("the payload is empty, and a zero length reads as the end of the log".into()));
    }
    if payload.len() > MAX_PAYLOAD {
        return Err(unencodable(format!("the payload is {} bytes, over the {} byte cap", payload.len(), MAX_PAYLOAD)));
    }
    let len =
        u32::try_from(payload.len()).map_err(|_| unencodable("the payload length doesn't fit in 32 bits".into()))?;
    let len = len.to_le_bytes();
    let mut rest = [0u8; FRAME_HEADER_LEN - 8];
    rest[0..8].copy_from_slice(&lsn.0.to_le_bytes());
    rest[8] = kind;
    rest[9] = flags;
    let crc = frame_crc(len, &rest, payload).to_le_bytes();
    out.reserve(FRAME_HEADER_LEN + payload.len());
    out.extend_from_slice(&len);
    out.extend_from_slice(&crc);
    out.extend_from_slice(&rest);
    out.extend_from_slice(payload);
    Ok(())
}

/// The frame at the start of `bytes`.
///
/// # Errors
/// The [`FrameEnd`] of the first check that fails. Each one ends the log at this offset.
pub fn read_frame(bytes: &[u8]) -> Result<Frame<'_>, FrameEnd> {
    let Some(h) = bytes.first_chunk::<FRAME_HEADER_LEN>() else {
        return Err(FrameEnd::Short);
    };
    let len_bytes = [h[0], h[1], h[2], h[3]];
    let len = usize::try_from(u32::from_le_bytes(len_bytes)).unwrap_or(usize::MAX);
    if len == 0 {
        return Err(FrameEnd::Zero);
    }
    if len > MAX_PAYLOAD {
        return Err(FrameEnd::Oversize);
    }
    let Some(payload) = bytes.get(FRAME_HEADER_LEN..FRAME_HEADER_LEN + len) else {
        return Err(FrameEnd::PastEnd);
    };
    let mut rest = [0u8; FRAME_HEADER_LEN - 8];
    rest.copy_from_slice(&h[8..FRAME_HEADER_LEN]);
    if frame_crc(len_bytes, &rest, payload) != u32::from_le_bytes([h[4], h[5], h[6], h[7]]) {
        return Err(FrameEnd::Crc);
    }
    let mut lsn = [0u8; 8];
    lsn.copy_from_slice(&rest[0..8]);
    Ok(Frame { lsn: Lsn(u64::from_le_bytes(lsn)), kind: rest[8], flags: rest[9], payload })
}

/// The largest sequence number a segment file name holds: twelve digits.
const MAX_SEGMENT_SEQ: u64 = 999_999_999_999;

/// `segment-<12 digits>.tjl` (4.1); `None` when `seq` is over 999 999 999 999, which twelve digits can't hold. Every
/// name this returns is one [`segment_seq`] reads back, and names sort in the order of their numbers.
#[must_use]
pub fn segment_name(seq: u64) -> Option<String> {
    (seq <= MAX_SEGMENT_SEQ).then(|| format!("segment-{seq:012}.tjl"))
}

/// The sequence number in a segment file name; `None` for any other name.
#[must_use]
pub fn segment_seq(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("segment-")?.strip_suffix(".tjl")?;
    if digits.len() != 12 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}
