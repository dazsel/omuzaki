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
#[derive(Clone, Debug, Default, PartialEq)]
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
