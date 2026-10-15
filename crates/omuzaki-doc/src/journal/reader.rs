//! Reading a journal folder (journal.md 8.2): which records are trustworthy, and how the log ended.
//!
//! The reader never applies anything after a bad frame. It then looks past the bad frame for valid ones: if it finds
//! some, the log was damaged, not just cut short by a crash, and recovery keeps the raw segment.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use omuzaki_history_api::Lsn;

use super::{
    FRAME_HEADER_LEN, FrameEnd, HEADER_LEN, HeaderFault, JournalError, MAX_PAYLOAD, Record, SegmentHeader, decode,
    kind, read_frame, segment_seq,
};

/// Where a scan stopped: a segment (its sequence number), a byte offset in it, and the last good LSN before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    /// The segment's sequence number (the one in its file name).
    pub segment: u64,
    /// The byte offset in the segment.
    pub offset: u64,
    /// The last good LSN before this position; the scan's `from` when nothing good was read.
    pub after: Lsn,
}

/// How the log ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// At the end of the last segment, a zero-filled tail, or a prepared segment.
    Clean,
    /// At a bad frame or header with nothing valid after it: the unflushed end of the last batch.
    TornTail {
        /// Where the log ends.
        at: Position,
    },
    /// At a bad frame with `later_valid` valid frames after it: damage. Recovery keeps the raw segment.
    Damaged {
        /// Where the log ends.
        at: Position,
        /// How many valid frames lie after `at`, in this and later segments.
        later_valid: usize,
    },
}

/// What a scan read.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Scan {
    /// Every record after `from`, in LSN order. Unknown skippable kinds are left out.
    pub records: Vec<(Lsn, Record)>,
    /// How the log ended.
    pub end: End,
}

/// Reads journal folders.
pub struct Reader;

impl Reader {
    /// Reads the segments of `dir`, in sequence order, starting with the one holding `from + 1`. It returns every
    /// record after `from`, up to where the log ends. When no segment starts at or before `from + 1` (the earlier ones
    /// were deleted), it starts at the first segment, and the LSN of the first record shows the gap.
    ///
    /// # Errors
    /// [`JournalError::Io`] for a folder or file that can't be read. [`JournalError::TooNew`] for a newer header or an
    /// unknown required kind: nothing is returned, because the project opens read-only (4.5).
    /// [`JournalError::BadPayload`] from [`decode`].
    pub fn scan(dir: &Path, from: Lsn) -> Result<Scan, JournalError> {
        let segments = list_segments(dir)?;
        let start = start_index(&segments, from);
        for segment in segments.iter().skip(start) {
            if let Err(HeaderFault::TooNew { version }) = segment.header {
                return Err(JournalError::TooNew { version, kind: None });
            }
        }
        let mut scanner = Scanner {
            segments: &segments,
            from,
            records: Vec::new(),
            after: from,
            next: from,
            done: false,
            budget: RESYNC_BUDGET,
            exhausted: false,
        };
        let end = scanner.run(start)?;
        Ok(Scan { records: scanner.records, end })
    }
}

/// A segment file and what its header says.
struct Segment {
    seq: u64,
    path: PathBuf,
    header: Result<SegmentHeader, HeaderFault>,
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> JournalError + '_ {
    move |source| JournalError::Io { path: path.to_path_buf(), source }
}

/// A `usize` offset as the `u64` a [`Position`] holds.
fn offset_u64(offset: usize) -> u64 {
    u64::try_from(offset).unwrap_or(u64::MAX)
}

/// The bytes of a segment from `offset` on; empty past the end.
fn tail(bytes: &[u8], offset: usize) -> &[u8] {
    bytes.get(offset..).unwrap_or(&[])
}

/// The segments of `dir` in sequence order, with their headers read.
fn list_segments(dir: &Path) -> Result<Vec<Segment>, JournalError> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_error(dir))? {
        let entry = entry.map_err(io_error(dir))?;
        let Some(seq) = entry.file_name().to_str().and_then(segment_seq) else {
            continue;
        };
        let path = entry.path();
        let mut head = Vec::with_capacity(HEADER_LEN);
        let limit = offset_u64(HEADER_LEN);
        fs::File::open(&path).and_then(|file| file.take(limit).read_to_end(&mut head)).map_err(io_error(&path))?;
        segments.push(Segment { seq, path, header: SegmentHeader::decode(&head) });
    }
    segments.sort_by_key(|s| s.seq);
    Ok(segments)
}

/// The last segment with a real header whose first LSN is at most `from + 1`; the first segment when there is none, and
/// always the first segment when `from` is 0 (a scan of the whole log must not skip a segment that restarts at LSN 1).
fn start_index(segments: &[Segment], from: Lsn) -> usize {
    if from == Lsn(0) {
        return 0;
    }
    let wanted = from.0.saturating_add(1);
    segments.iter().rposition(|s| s.header.is_ok_and(|h| !h.is_prepared() && h.first.0 <= wanted)).unwrap_or(0)
}

/// The most payload bytes one scan hashes while it looks past a bad frame (journal.md 8.2): 256 MiB, a fraction of a
/// second of CRC work however much damaged data the folder holds.
const RESYNC_BUDGET: usize = 256 * 1024 * 1024;

/// How many valid frames `bytes` holds, trying every byte offset and chaining from each frame found.
///
/// A candidate is hashed only if its length fits the data and its payload looks like what a record holds (a JSON
/// object, 4.5), so noise costs a few byte reads per offset, not a CRC over a long payload. The payload bytes hashed
/// are charged to `budget`; when it runs out the count stops (`exhausted` is then true) and is a lower bound.
fn count_frames(bytes: &[u8], budget: &mut usize) -> (usize, bool) {
    let (mut count, mut offset) = (0, 0);
    while offset < bytes.len() {
        let rest = tail(bytes, offset);
        let Some(len) = candidate_len(rest) else {
            offset += 1;
            continue;
        };
        if len > *budget {
            return (count, true);
        }
        *budget -= len;
        match read_frame(rest) {
            Ok(frame) => {
                count += 1;
                offset += frame.len();
            }
            Err(_) => offset += 1,
        }
    }
    (count, false)
}

/// The payload length of a frame that could start `rest`: a length in `1..=MAX_PAYLOAD` that fits, and a payload that
/// starts with `{` and ends with `}`. Cheap; it can say yes to a frame whose CRC then fails, never no to a real record.
fn candidate_len(rest: &[u8]) -> Option<usize> {
    let header = rest.first_chunk::<FRAME_HEADER_LEN>()?;
    let len = usize::try_from(u32::from_le_bytes([header[0], header[1], header[2], header[3]])).ok()?;
    if len == 0 || len > MAX_PAYLOAD {
        return None;
    }
    let payload = rest.get(FRAME_HEADER_LEN..FRAME_HEADER_LEN.checked_add(len)?)?;
    (payload.first() == Some(&b'{') && payload.last() == Some(&b'}')).then_some(len)
}

/// Why reading one segment's frames stopped.
enum Stop {
    /// Exactly the end of the file: the log may continue in the next segment (a rotation).
    Eof,
    /// A zero-filled tail, or zeros short of a frame: the log ends here, unless the next segment is a recovery
    /// session (8.2).
    ZeroTail,
    /// A bad frame at this offset.
    Bad(usize),
}

struct Scanner<'a> {
    segments: &'a [Segment],
    from: Lsn,
    records: Vec<(Lsn, Record)>,
    /// The last good LSN.
    after: Lsn,
    /// The LSN the next frame must have.
    next: Lsn,
    /// Whether the last frame had LSN `u64::MAX`, the end of the sequence: no frame can follow it.
    done: bool,
    /// What is left of the [`RESYNC_BUDGET`].
    budget: usize,
    /// Whether the budget ran out while looking past a bad frame.
    exhausted: bool,
}

impl Scanner<'_> {
    fn position(&self, segment: &Segment, offset: usize) -> Position {
        Position { segment: segment.seq, offset: offset_u64(offset), after: self.after }
    }

    fn read(segment: &Segment) -> Result<Vec<u8>, JournalError> {
        fs::read(&segment.path).map_err(io_error(&segment.path))
    }

    /// Valid frames in `bytes` past offset `from`, spending the resync budget.
    fn count_in(&mut self, bytes: &[u8], from: usize) -> usize {
        let (count, exhausted) = count_frames(tail(bytes, from), &mut self.budget);
        self.exhausted |= exhausted;
        count
    }

    /// Valid frames in the segments from index `first` on, past their headers.
    fn count_from(&mut self, first: usize) -> Result<usize, JournalError> {
        let mut count = 0;
        for segment in self.segments.iter().skip(first) {
            count += self.count_in(&Self::read(segment)?, HEADER_LEN);
        }
        Ok(count)
    }

    /// The end for a bad frame at `at` with `later_valid` valid frames found after it. When the resync budget ran out
    /// the count is a lower bound, and a log that is that full of frame-looking data is damaged, not torn.
    fn bad_end(&self, at: Position, later_valid: usize) -> End {
        let later_valid = if self.exhausted { later_valid.max(1) } else { later_valid };
        if later_valid > 0 { End::Damaged { at, later_valid } } else { End::TornTail { at } }
    }

    /// Reads the segments in order from index `idx` (rules 2 to 5 of journal.md 8.2) until the log ends.
    fn run(&mut self, mut idx: usize) -> Result<End, JournalError> {
        loop {
            let Some(segment) = self.segments.get(idx) else {
                return Ok(End::Clean);
            };
            let header = match segment.header {
                Err(_) => return Ok(End::TornTail { at: self.position(segment, 0) }),
                Ok(header) if header.is_prepared() => return Ok(End::Clean),
                Ok(header) => header,
            };
            self.next = header.first;
            self.done = false;
            let bytes = Self::read(segment)?;
            let next_segment = self.segments.get(idx + 1);
            match self.read_frames(&bytes)? {
                Stop::ZeroTail => {
                    if next_segment.is_some_and(|next| self.starts_recovery_session(next)) {
                        idx += 1;
                        continue;
                    }
                    return Ok(End::Clean);
                }
                Stop::Eof => match next_segment {
                    None => return Ok(End::Clean),
                    Some(next) => match next.header {
                        Ok(h) if h.is_prepared() => return Ok(End::Clean),
                        Ok(h) if self.done || h.first != self.next => {
                            let later_valid = self.count_from(idx + 1)?;
                            return Ok(End::Damaged { at: self.position(next, 0), later_valid });
                        }
                        _ => idx += 1,
                    },
                },
                Stop::Bad(offset) => {
                    if next_segment.is_some_and(|next| self.starts_recovery_session(next)) {
                        idx += 1;
                        continue;
                    }
                    let at = self.position(segment, offset);
                    let later_valid = self.count_in(&bytes, offset + 1) + self.count_from(idx + 1)?;
                    return Ok(self.bad_end(at, later_valid));
                }
            }
        }
    }

    /// Whether `segment` is the one an earlier recovery started (8.5): it begins one past the last good record, with a
    /// `session` record. A segment that can't be read is not one.
    fn starts_recovery_session(&self, segment: &Segment) -> bool {
        let Ok(header) = segment.header else {
            return false;
        };
        if header.is_prepared() || self.done || header.first != self.next {
            return false;
        }
        let Ok(bytes) = Self::read(segment) else {
            return false;
        };
        read_frame(tail(&bytes, HEADER_LEN)).is_ok_and(|f| f.kind == kind::SESSION && f.lsn == header.first)
    }

    /// Reads the frames of one segment, keeping the records after `from`.
    fn read_frames(&mut self, bytes: &[u8]) -> Result<Stop, JournalError> {
        let mut offset = HEADER_LEN;
        loop {
            let rest = tail(bytes, offset);
            let frame = match read_frame(rest) {
                Ok(frame) => frame,
                Err(FrameEnd::Short) if rest.is_empty() => return Ok(Stop::Eof),
                Err(FrameEnd::Zero) => return Ok(Stop::ZeroTail),
                Err(FrameEnd::Short) if rest.iter().all(|&b| b == 0) => return Ok(Stop::ZeroTail),
                Err(_) => return Ok(Stop::Bad(offset)),
            };
            if self.done || frame.lsn != self.next {
                return Ok(Stop::Bad(offset));
            }
            if frame.lsn > self.from
                && let Some(record) = decode(&frame)?
            {
                self.records.push((frame.lsn, record));
            }
            self.after = frame.lsn;
            match frame.lsn.0.checked_add(1) {
                Some(next) => self.next = Lsn(next),
                None => self.done = true,
            }
            offset += frame.len();
        }
    }
}
