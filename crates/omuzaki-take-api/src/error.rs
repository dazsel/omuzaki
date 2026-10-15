//! What can go wrong reading a take (design T3, T4, T6).

/// A take could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TakeError {
    /// The file or a chunk is from a newer major than this reader decodes.
    #[error("this take is newer than this reader supports ({found:?} against {supported:?}): {hint}")]
    Newer {
        /// The (major, minor) the take carries.
        found: (u16, u16),
        /// The newest (major, minor) this reader decodes.
        supported: (u8, u8),
        /// What to do about it: `update Omuzaki to read this take`.
        hint: String,
    },
    /// A chunk sets a must-understand flag (bits 8 to 15) this reader does not know, so it refuses the take (T3).
    #[error(
        "this take sets must-understand flag bits {flags:#06x}, which this reader does not know: update Omuzaki to read it"
    )]
    UnknownFlag {
        /// The must-understand bits the chunk set (`flags & 0xFF00`).
        flags: u16,
    },
    /// A file or chunk does not start with its magic.
    #[error("not a take: the magic bytes are wrong")]
    BadMagic,
    /// A checksum failed at this byte offset.
    #[error("checksum mismatch at byte {offset}")]
    Crc {
        /// Where the bad header or chunk starts.
        offset: u64,
    },
    /// The file ends inside a header or chunk that starts at this offset.
    #[error("the take is cut short at byte {offset}")]
    Truncated {
        /// Where the incomplete header or chunk starts.
        offset: u64,
    },
    /// The journal entries between the snapshot and the requested frame are not contiguous.
    #[error("the journal has a gap from entry {}", from_lsn.0)]
    StateGap {
        /// The first missing entry.
        from_lsn: omuzaki_history_api::Lsn,
    },
    /// A forward reader met an `end` snapshot, or the reverse (a file never mixes the two).
    #[error("the take's snapshot is on the other side of the recording than this reader replays")]
    WrongSnapshotSide,
    /// A JSON part (header, State, event) does not parse.
    #[error("bad JSON in the take: {0}")]
    Json(String),
    /// The file could not be read.
    #[error("take I/O: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_names_both_versions_and_the_hint() {
        let e = TakeError::Newer { found: (2, 0), supported: (1, 0), hint: "update Omuzaki to read this take".into() };
        let text = e.to_string();
        assert!(text.contains("(2, 0)") && text.contains("(1, 0)") && text.contains("update Omuzaki"), "{text}");
    }

    #[test]
    fn unknown_flag_names_the_flag_bits() {
        let text = TakeError::UnknownFlag { flags: 0x0200 }.to_string();
        assert!(text.contains("0x0200") && text.contains("must-understand"), "{text}");
    }
}
