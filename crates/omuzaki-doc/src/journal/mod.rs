//! The journal (journal.md 4 and 8.2): the write-ahead log of a project's history, as bytes.
//!
//! This module holds the byte format (segment headers, record frames and segment file names), the records the frames
//! carry ([`Record`], [`encode`], [`decode`]), the [`Reader`] that scans a journal folder, the [`ProjectJournal`]
//! that writes one (the writer and the keeper, which saves the files behind it and takes the snapshots recovery starts
//! from, [`crate::snapshot`]), [`archive_before`], which moves sealed segments into the op log archive, and [`reopen`],
//! which decides on opening what is the truth and what has to be replayed.
//!
//! The invariant: every frame [`write_frame`] accepts is one [`read_frame`] reads back. A zero length marks the end
//! of the log, so [`write_frame`] refuses an empty payload.

mod format;
mod open;
mod reader;
mod record;
mod writer;

use std::path::PathBuf;

pub use format::{
    FORMAT_VERSION, FRAME_HEADER_LEN, Frame, FrameEnd, HEADER_LEN, HeaderFault, MAGIC, MAX_PAYLOAD, SKIPPABLE,
    SegmentHeader, read_frame, segment_name, segment_seq, write_frame,
};
pub use open::{Recover, Recovery, Reopened, reopen, reopen_with};
pub use reader::{End, Position, Reader, Scan};
pub use record::{MAX_DEPTH, Record, SessionInfo, decode, encode, kind};
pub use writer::{
    ARCHIVE_DIR, JOURNAL_DIR, JournalOptions, ProjectJournal, SessionStart, SystemWall, archive_before, archive_dir,
    journal_dir,
};

/// Why the journal can't be read or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum JournalError {
    /// A file or folder of the journal can't be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What the operating system said.
        #[source]
        source: std::io::Error,
    },
    /// A newer format, or a record kind this build doesn't know and may not skip (4.5).
    #[error("this project was being edited by a newer Omuzaki; open it there to recover the last changes")]
    TooNew {
        /// The format version the segment names.
        version: u16,
        /// The record kind, when the newer thing is a kind rather than the format.
        kind: Option<u8>,
    },
    /// A record that must not or can't be written.
    #[error("record {lsn} can't be written: {reason}")]
    Unencodable {
        /// The record's log sequence number.
        lsn: u64,
        /// What is wrong with it.
        reason: String,
    },
    /// A frame whose CRC holds but whose payload doesn't read as its kind.
    #[error("record {lsn} (kind {kind}) has a payload that doesn't read: {reason}")]
    BadPayload {
        /// The record's log sequence number.
        lsn: u64,
        /// The record kind.
        kind: u8,
        /// What is wrong with the payload.
        reason: String,
    },
    /// The project folder itself doesn't open, or a snapshot's tree can't be stored in it.
    #[error(transparent)]
    Store(#[from] crate::StoreError),
    /// A snapshot can't be taken or read.
    #[error("{0}")]
    Snapshot(String),
}
