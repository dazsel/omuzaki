//! The reader contract (design T3, T4).

use std::collections::BTreeMap;

use crate::chunk::ChunkHeader;
use crate::error::TakeError;
use crate::header::TakeHeader;
use crate::record::Incomplete;
use crate::state::Withheld;

/// One chunk read from a take: its header and its payload of records, borrowed from the reader's buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChunkView<'a> {
    /// The decoded header.
    pub header: ChunkHeader,
    /// The records, `header.len` bytes. The codec (`omuzaki-take`) decodes them into `Record`s.
    pub payload: &'a [u8],
}

/// What a read found. Filled as chunks are read.
///
/// Later fields are added with a default in [`TakeReport::new`], so a reader built for an older version keeps compiling.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct TakeReport {
    /// Chunks read.
    pub chunks: u64,
    /// The offset of the first bad or truncated chunk, if the scan stopped there.
    pub truncated_at: Option<u64>,
    /// Unknown record kinds skipped, by kind byte.
    pub skipped: BTreeMap<u8, u64>,
    /// The muted or private runs, clamped to the window read.
    pub withheld: Vec<Withheld>,
    /// Entries carried as references.
    pub event_refs: u64,
    /// Entries that waited in the queue past their chunk.
    pub deferred: u64,
    /// Providers left out by the size cap, by name and number of States.
    pub omitted: BTreeMap<String, u64>,
    /// States that overflowed to the minimal form.
    pub state_overflow: u64,
    /// Holes in the event stream.
    pub incomplete: Vec<Incomplete>,
    /// The largest lag between an entry's chunk and its `applied_frame`, in frames.
    pub max_defer_frames: u32,
    /// The file ended before the TAIL did.
    pub incomplete_tail: bool,
    /// Whether the read used the index (a clean finish) rather than a scan.
    pub indexed: bool,
}

impl TakeReport {
    /// An empty report: nothing read yet. Set the counts a reader found with `with_*`.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same report with the number of chunks read.
    #[must_use]
    pub fn with_chunks(self, chunks: u64) -> Self {
        Self { chunks, ..self }
    }

    /// The same report with the offset of the first bad or truncated chunk.
    #[must_use]
    pub fn with_truncated_at(self, truncated_at: Option<u64>) -> Self {
        Self { truncated_at, ..self }
    }

    /// The same report with the unknown record kinds skipped, by kind byte.
    #[must_use]
    pub fn with_skipped(self, skipped: BTreeMap<u8, u64>) -> Self {
        Self { skipped, ..self }
    }

    /// The same report with the muted or private runs read.
    #[must_use]
    pub fn with_withheld(self, withheld: Vec<Withheld>) -> Self {
        Self { withheld, ..self }
    }

    /// The same report with the entries carried as references.
    #[must_use]
    pub fn with_event_refs(self, event_refs: u64) -> Self {
        Self { event_refs, ..self }
    }

    /// The same report with the entries that waited in the queue past their chunk.
    #[must_use]
    pub fn with_deferred(self, deferred: u64) -> Self {
        Self { deferred, ..self }
    }

    /// The same report with the providers left out by the size cap, by name.
    #[must_use]
    pub fn with_omitted(self, omitted: BTreeMap<String, u64>) -> Self {
        Self { omitted, ..self }
    }

    /// The same report with the States that overflowed to the minimal form.
    #[must_use]
    pub fn with_state_overflow(self, state_overflow: u64) -> Self {
        Self { state_overflow, ..self }
    }

    /// The same report with the holes in the event stream.
    #[must_use]
    pub fn with_incomplete(self, incomplete: Vec<Incomplete>) -> Self {
        Self { incomplete, ..self }
    }

    /// The same report with the largest lag between an entry's chunk and its `applied_frame`, in frames.
    #[must_use]
    pub fn with_max_defer_frames(self, max_defer_frames: u32) -> Self {
        Self { max_defer_frames, ..self }
    }

    /// The same report with whether the file ended before the TAIL did.
    #[must_use]
    pub fn with_incomplete_tail(self, incomplete_tail: bool) -> Self {
        Self { incomplete_tail, ..self }
    }

    /// The same report with whether the read used the index (a clean finish) rather than a scan.
    #[must_use]
    pub fn with_indexed(self, indexed: bool) -> Self {
        Self { indexed, ..self }
    }
}

/// A take opened for reading. One codec implements it for every user (`omuzaki-take`); the scripted mock
/// (`omuzaki-take-mock`) is the oracle it is checked against.
pub trait TakeSource {
    /// The file header.
    fn header(&self) -> &TakeHeader;

    /// Positions the reader at the newest group start at or before `frame`, so the next chunk read holds its `State`.
    ///
    /// # Errors
    /// [`TakeError`] when the take cannot be read there.
    fn seek(&mut self, frame: i64) -> Result<(), TakeError>;

    /// The next chunk, or `None` at the end of the take. The view borrows the reader's buffer until the next call.
    ///
    /// # Errors
    /// [`TakeError`] for a chunk that is bad, newer or cut short.
    fn next_chunk(&mut self) -> Result<Option<ChunkView<'_>>, TakeError>;

    /// What the reads so far found.
    fn report(&self) -> &TakeReport;
}
