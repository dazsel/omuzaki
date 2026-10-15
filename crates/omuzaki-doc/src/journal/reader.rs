//! Reading a journal folder (journal.md 8.2): which records are trustworthy, and how the log ended.
//!
//! The reader never applies anything after a bad frame. It then looks past the bad frame for valid ones: if it finds
//! some, the log was damaged, not just cut short by a crash, and recovery keeps the raw segment.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use omuzaki_history_api::Lsn;

use super::{
    FrameEnd, HEADER_LEN, HeaderFault, JournalError, Record, SegmentHeader, decode, kind, read_frame, segment_seq,
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
        let mut scanner = Scanner { segments: &segments, from, records: Vec::new(), after: from, next: from };
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

/// The last segment with a real header whose first LSN is at most `from + 1`; the first segment when there is none.
fn start_index(segments: &[Segment], from: Lsn) -> usize {
    let wanted = from.0.saturating_add(1);
    segments.iter().rposition(|s| s.header.is_ok_and(|h| !h.is_prepared() && h.first.0 <= wanted)).unwrap_or(0)
}

/// How many valid frames `bytes` holds, trying every byte offset and chaining from each frame found.
fn count_frames(bytes: &[u8]) -> usize {
    let (mut count, mut offset) = (0, 0);
    while offset < bytes.len() {
        match read_frame(tail(bytes, offset)) {
            Ok(frame) => {
                count += 1;
                offset += frame.len();
            }
            Err(_) => offset += 1,
        }
    }
    count
}

/// Why reading one segment's frames stopped.
enum Stop {
    /// The end of the segment: end of file, a zero-filled tail, or zeros short of a frame.
    Boundary,
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
}

impl Scanner<'_> {
    fn position(&self, segment: &Segment, offset: usize) -> Position {
        Position { segment: segment.seq, offset: offset_u64(offset), after: self.after }
    }

    fn read(segment: &Segment) -> Result<Vec<u8>, JournalError> {
        fs::read(&segment.path).map_err(io_error(&segment.path))
    }

    /// Valid frames in the segments from index `first` on, past their headers.
    fn count_from(&self, first: usize) -> Result<usize, JournalError> {
        let mut count = 0;
        for segment in self.segments.iter().skip(first) {
            count += count_frames(tail(&Self::read(segment)?, HEADER_LEN));
        }
        Ok(count)
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
            let bytes = Self::read(segment)?;
            let next_segment = self.segments.get(idx + 1);
            match self.read_frames(&bytes)? {
                Stop::Boundary => match next_segment {
                    None => return Ok(End::Clean),
                    Some(next) => match next.header {
                        Ok(h) if h.is_prepared() => return Ok(End::Clean),
                        Ok(h) if h.first != self.next => {
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
                    let later_valid = count_frames(tail(&bytes, offset + 1)) + self.count_from(idx + 1)?;
                    return Ok(if later_valid > 0 { End::Damaged { at, later_valid } } else { End::TornTail { at } });
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
        if header.is_prepared() || header.first != self.next {
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
                Err(FrameEnd::Zero) => return Ok(Stop::Boundary),
                Err(FrameEnd::Short) if rest.iter().all(|&b| b == 0) => return Ok(Stop::Boundary),
                Err(_) => return Ok(Stop::Bad(offset)),
            };
            if frame.lsn != self.next {
                return Ok(Stop::Bad(offset));
            }
            if frame.lsn > self.from
                && let Some(record) = decode(&frame)?
            {
                self.records.push((frame.lsn, record));
            }
            self.after = frame.lsn;
            self.next = Lsn(frame.lsn.0.saturating_add(1));
            offset += frame.len();
        }
    }
}
