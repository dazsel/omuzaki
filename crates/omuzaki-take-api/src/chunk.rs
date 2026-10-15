//! The chunk framing constants, record kinds and header (design T2).

use omuzaki_core::time::Seconds;

use crate::error::TakeError;

/// The format's major version. A newer major is refused; an older one keeps its decoder (T3).
pub const MAJOR: u8 = 1;
/// The format's minor version. Minors are additive: new record kinds, longer bodies, new flags in bits 0 to 7.
pub const MINOR: u8 = 0;
/// The first four bytes of every chunk.
pub const CHUNK_MAGIC: [u8; 4] = *b"TKCH";
/// The first bytes of a `.input` file.
pub const FILE_MAGIC: [u8; 10] = *b"OMUZAKIINP";
/// The size of a chunk header, in bytes.
pub const CHUNK_HEADER_LEN: usize = 32;
/// The size of a record header (`kind u8 | rflags u8 | reserved u16 | len u32`), in bytes.
pub const RECORD_HEADER_LEN: usize = 8;
/// The first four bytes of the index that follows the last chunk of a clean take (T4).
pub const INDEX_MAGIC: [u8; 4] = *b"TKIX";
/// The last four bytes of a clean take's trailer (T4).
pub const TRAILER_MAGIC: [u8; 4] = *b"TKND";
/// The size of the trailer at the end of a clean take: `index_offset u64 | chunks u64 | crc u32 | magic`, in bytes (T4).
pub const TRAILER_LEN: usize = 24;
/// The most serialised bytes a `State` record may take (T2). Above it the minimal State is written instead.
pub const STATE_MAX_BYTES: usize = 16 * 1024;
/// The most serialised bytes of events one chunk may carry (T2). The rest wait for the next chunk.
pub const EVENTS_MAX_BYTES: usize = 32 * 1024;
/// The most provider names a `State` lists in `omitted` before it counts the rest in `omitted_more` (T2).
pub const OMITTED_NAMES_MAX: usize = 32;

/// What a record is. The number is the byte on disk. A reader meeting an unknown number skips the record by its length
/// and counts it (`Record::Unknown`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
#[repr(u8)]
pub enum RecordKind {
    /// A tracker frame the stage polled.
    Track = 1,
    /// An audio-derived layer (visemes) with its performance time.
    Voice = 2,
    /// An audio cue sample (loudness, onset, laugh).
    Cue = 3,
    /// One cast member's final channels, after the life layer.
    Bus = 4,
    /// A journal entry, in full or as a placeholder.
    Event = 5,
    /// A journal entry too large to carry, resolved from the project journal.
    EventRef = 6,
    /// The start state of a group.
    State = 7,
    /// The start or end of a muted or private run.
    Withheld = 8,
    /// The playout delay, when it changes.
    Delay = 9,
    /// A hole in the event stream.
    Incomplete = 10,
    /// How far the events in this and earlier chunks are complete.
    Watermark = 11,
}

impl RecordKind {
    /// The kind a byte names, or `None` for a number this version does not know.
    pub const fn from_u8(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Track,
            2 => Self::Voice,
            3 => Self::Cue,
            4 => Self::Bus,
            5 => Self::Event,
            6 => Self::EventRef,
            7 => Self::State,
            8 => Self::Withheld,
            9 => Self::Delay,
            10 => Self::Incomplete,
            11 => Self::Watermark,
            _ => return None,
        })
    }
}

/// A chunk's flag word. Bits 0 to 7 are optional (an unknown one is ignored); bits 8 to 15 are must-understand (an
/// unknown one makes a reader refuse the file, T3).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ChunkFlags(pub u16);

impl ChunkFlags {
    /// The chunk carries a `State` first: a replay clip may start here.
    pub const GROUP_START: u16 = 1 << 0;
    /// Some run in this chunk's window was withheld.
    pub const HAS_WITHHELD: u16 = 1 << 1;
    /// The chunk belongs to the closing tail written at stop or save (T6).
    pub const TAIL: u16 = 1 << 2;
    /// Bits a reader must understand; an unknown one in this byte means "refuse".
    pub const MUST_UNDERSTAND: u16 = 0xFF00;

    /// Whether every bit of `flag` is set.
    pub const fn contains(self, flag: u16) -> bool {
        self.0 & flag == flag
    }

    /// The must-understand bits set in this word.
    pub const fn must_understand(self) -> u16 {
        self.0 & Self::MUST_UNDERSTAND
    }
}

/// A decoded chunk header. The magic and the CRC are checked while decoding and not kept.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ChunkHeader {
    /// The chunk's major version.
    pub major: u8,
    /// The chunk's minor version.
    pub minor: u8,
    /// The flag word.
    pub flags: ChunkFlags,
    /// The video frame index in the session's timebase (the video packet's pts).
    pub frame: i64,
    /// Pipeline time of the frame tick, in seconds.
    pub tick: Seconds,
    /// The payload length, in bytes.
    pub len: u32,
}

impl ChunkHeader {
    /// A header with every field the 32-byte layout carries.
    pub fn new(major: u8, minor: u8, flags: ChunkFlags, frame: i64, tick: Seconds, len: u32) -> Self {
        Self { major, minor, flags, frame, tick, len }
    }

    /// Whether this reader may decode the chunk (T3): a newer major, or a must-understand flag it does not know (it
    /// knows none yet), is refused as [`TakeError::Newer`], the one upgrade path.
    ///
    /// # Errors
    /// [`TakeError::Newer`], for a newer major or an unknown must-understand flag.
    pub fn check(&self) -> Result<(), TakeError> {
        if self.major > MAJOR || self.flags.must_understand() != 0 {
            return Err(TakeError::newer((self.major.into(), self.minor.into())));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn must_understand_is_the_high_byte() {
        assert_eq!(ChunkFlags::MUST_UNDERSTAND, 0xFF00);
        let flags = ChunkFlags(ChunkFlags::GROUP_START | 0x0100);
        assert!(flags.contains(ChunkFlags::GROUP_START));
        assert!(!flags.contains(ChunkFlags::TAIL));
        assert_eq!(flags.must_understand(), 0x0100);
    }

    fn header(major: u8, flags: u16) -> ChunkHeader {
        ChunkHeader::new(major, 1, ChunkFlags(flags), 0, Seconds(0.0), 0)
    }

    #[test]
    fn an_unknown_must_understand_flag_is_the_newer_refusal() {
        // T3: the high byte of the flag word and a newer major are the same refusal, `Newer`, with the one hint.
        assert_eq!(header(1, ChunkFlags::GROUP_START | ChunkFlags::TAIL).check(), Ok(()));
        for flags in [0x0100, 0x8000, 0xFF00 | ChunkFlags::GROUP_START] {
            match header(1, flags).check() {
                Err(TakeError::Newer { found, supported, hint }) => {
                    assert_eq!((found, supported), ((1, 1), (MAJOR, MINOR)));
                    assert_eq!(hint, crate::error::NEWER_HINT);
                }
                other => panic!("flags {flags:#06x}: expected Newer, got {other:?}"),
            }
        }
        assert!(matches!(header(2, 0).check(), Err(TakeError::Newer { found: (2, 1), .. })));
    }

    #[test]
    fn kinds_round_trip_through_their_byte() {
        for byte in 1..=11u8 {
            let kind = RecordKind::from_u8(byte);
            assert_eq!(kind.map(|k| k as u8), Some(byte));
        }
        assert_eq!(RecordKind::from_u8(0), None);
        assert_eq!(RecordKind::from_u8(12), None);
    }

    #[test]
    fn the_constants_match_the_layout() {
        assert_eq!(&CHUNK_MAGIC, b"TKCH");
        assert_eq!(FILE_MAGIC.len(), 10);
        assert_eq!((MAJOR, MINOR, CHUNK_HEADER_LEN), (1, 0, 32));
    }

    #[test]
    fn the_index_and_trailer_match_t4() {
        assert_eq!(&INDEX_MAGIC, b"TKIX");
        assert_eq!(&TRAILER_MAGIC, b"TKND");
        // index_offset u64 | chunks u64 | crc u32 | magic [u8; 4]
        assert_eq!(TRAILER_LEN, 8 + 8 + 4 + 4);
    }

    #[test]
    fn the_t2_limits_are_the_design_values() {
        assert_eq!((STATE_MAX_BYTES, EVENTS_MAX_BYTES, OMITTED_NAMES_MAX), (16_384, 32_768, 32));
    }
}
