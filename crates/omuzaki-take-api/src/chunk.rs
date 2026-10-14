//! The chunk framing constants, record kinds and header (design T2).

use omuzaki_core::time::Seconds;

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
}
