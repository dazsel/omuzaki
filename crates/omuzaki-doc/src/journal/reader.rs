//! Reading a journal folder (journal.md 8.2): which records are trustworthy, and how the log ended.
//!
//! The reader never applies anything after a bad frame. It then looks past the bad frame for valid ones: if it finds
//! some, the log was damaged, not just cut short by a crash, and recovery keeps the raw segment.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use omuzaki_history_api::Lsn;

use super::{
    FRAME_HEADER_LEN, Frame, FrameEnd, HEADER_LEN, HeaderFault, JournalError, MAX_PAYLOAD, Record, SegmentHeader,
    decode, kind, read_frame, segment_seq,
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
        /// How many valid frames lie after `at`, in this and later segments. A lower bound: the scan that looks past
        /// the bad frame has a work budget, and when it runs out the log is still reported as damaged, with at least 1
        /// here although none of the frame-looking data after `at` may be intact.
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
    /// Reads the segments of `dir`, in sequence order, from the one holding `from + 1`, and returns every record after
    /// `from` up to where the log ends.
    ///
    /// # Scan rules
    /// Journal.md 8.2, as this reader applies it. Every rule is tested in `tests/journal_reader.rs`.
    /// 1. **Logical ends.** A prepared segment, a segment with a bad header, a zero-filled tail, and the end of the last
    ///    segment end the log. Nothing after one is read, chosen as a start, or allowed to fail the scan.
    /// 2. **Start.** Among the segments before the first logical end whose first LSNs strictly increase in sequence
    ///    order, the last one with first LSN at most `from + 1`. A segment that restarts at an earlier LSN is never a
    ///    start: it is reached from the one before and reported as damage. With `from` 0, or when no segment
    ///    qualifies (earlier ones deleted), the first segment; the first record's LSN shows the gap.
    /// 3. **Old damage.** Before the scan has reached `from + 1`, a bad frame, a gap or a zero run in the start segment
    ///    is skipped by looking for the frame `from + 1` after it: the snapshot covers that prefix. If there is no
    ///    such frame, that is the end of the log.
    /// 4. **Reachability.** A file is opened only when the scan reaches it. Unreadable or unlistable files after a
    ///    logical end are ignored, as is a newer-format header there; reaching a newer header or an unknown required
    ///    kind is [`JournalError::TooNew`].
    /// 5. **Rotation.** The log continues into the next segment only from an exact end of file, when it starts one past
    ///    the last record; after a bad frame or a zero tail only into a recovery `session` (8.5).
    /// 6. **Classification.** After a bad frame or a discontinuous segment, the valid frames after it are counted. None
    ///    is a [`End::TornTail`]; some is [`End::Damaged`], also when the work budget runs out (then a lower bound).
    ///    Files that can't be read are counted as holding none.
    ///
    /// # Errors
    /// [`JournalError::Io`] for a folder, or a file the scan reaches, that can't be read. [`JournalError::TooNew`] as
    /// in rule 4: nothing is returned, because the project opens read-only (4.5).
    /// [`JournalError::BadPayload`] from [`decode`].
    ///
    /// Looking past a bad frame for valid ones has a fixed work budget. If it runs out, the result is
    /// [`End::Damaged`] and its `later_valid` is a lower bound, at least 1.
    pub fn scan(dir: &Path, from: Lsn) -> Result<Scan, JournalError> {
        Self::scan_with_budget(dir, from, RESYNC_BUDGET)
    }

    /// [`scan`](Self::scan) with `budget` bytes of work for looking past a bad frame, so a test can exhaust it.
    ///
    /// # Errors
    /// As [`scan`](Self::scan).
    #[doc(hidden)]
    pub fn scan_with_budget(dir: &Path, from: Lsn, budget: usize) -> Result<Scan, JournalError> {
        let segments = list_segments(dir)?;
        let start = start_index(&segments, from);
        let mut scanner = Scanner {
            segments: &segments,
            from,
            records: Vec::new(),
            after: from,
            next: from,
            done: false,
            budget,
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
}

type Head = Result<SegmentHeader, HeaderFault>;

impl Segment {
    /// What the header says. Opened when asked for, so an unreadable file only matters once the scan reaches it.
    fn head(&self) -> Result<Head, JournalError> {
        let mut head = Vec::with_capacity(HEADER_LEN);
        let limit = offset_u64(HEADER_LEN);
        fs::File::open(&self.path)
            .and_then(|file| file.take(limit).read_to_end(&mut head))
            .map_err(io_error(&self.path))?;
        Ok(SegmentHeader::decode(&head))
    }
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

/// The segments of `dir` in sequence order.
fn list_segments(dir: &Path) -> Result<Vec<Segment>, JournalError> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_error(dir))? {
        let entry = entry.map_err(io_error(dir))?;
        let Some(seq) = entry.file_name().to_str().and_then(segment_seq) else {
            continue;
        };
        segments.push(Segment { seq, path: entry.path() });
    }
    segments.sort_by_key(|s| s.seq);
    Ok(segments)
}

/// The segment a scan from `from` starts in (rule 2 of [`Reader::scan`]). Headers are read only up to the first segment
/// that starts past `from + 1`, a logical end, or one that can't be read.
fn start_index(segments: &[Segment], from: Lsn) -> usize {
    if from == Lsn(0) {
        return 0;
    }
    let wanted = from.0.saturating_add(1);
    let (mut start, mut previous) = (0, 0);
    for (idx, segment) in segments.iter().enumerate() {
        let Ok(Ok(header)) = segment.head() else { break };
        if header.is_prepared() || header.first.0 <= previous || header.first.0 > wanted {
            break;
        }
        previous = header.first.0;
        start = idx;
    }
    start
}

/// The most payload bytes one scan hashes or skims while it looks past a bad frame (journal.md 8.2): 256 MiB, a
/// fraction of a second of work however much damaged data the folder holds.
const RESYNC_BUDGET: usize = 256 * 1024 * 1024;

/// What looking for a frame at one offset found.
enum Probe<'a> {
    /// No valid frame starts here.
    No,
    /// The budget ran out before this offset could be judged.
    Exhausted,
    Frame(Frame<'a>),
}

/// Whether a valid frame starts at the start of `rest`.
///
/// A candidate is hashed only if its length fits the data and its payload looks like what a record holds (a JSON
/// object, 4.5, possibly padded with JSON whitespace), so noise costs a few byte reads per offset, not a CRC over a long
/// payload. The payload bytes hashed, and the padding skipped, are charged to `budget`.
fn probe<'a>(rest: &'a [u8], budget: &mut usize) -> Probe<'a> {
    let Some(len) = frame_len(rest) else {
        return Probe::No;
    };
    let Some((looks_like_object, padding)) = skim_payload(rest, len, *budget) else {
        return Probe::Exhausted;
    };
    *budget -= padding;
    if !looks_like_object {
        return Probe::No;
    }
    if len > *budget {
        return Probe::Exhausted;
    }
    *budget -= len;
    read_frame(rest).map_or(Probe::No, Probe::Frame)
}

/// How many valid frames `bytes` holds, trying every byte offset and chaining from each frame found. When the budget
/// runs out the count stops (`exhausted` is then true) and is a lower bound.
fn count_frames(bytes: &[u8], budget: &mut usize) -> (usize, bool) {
    let (mut count, mut offset) = (0, 0);
    while offset < bytes.len() {
        match probe(tail(bytes, offset), budget) {
            Probe::Exhausted => return (count, true),
            Probe::Frame(frame) => {
                count += 1;
                offset += frame.len();
            }
            Probe::No => offset += 1,
        }
    }
    (count, false)
}

/// The payload length a frame at the start of `rest` would have: in `1..=MAX_PAYLOAD` and fitting the data.
fn frame_len(rest: &[u8]) -> Option<usize> {
    let header = rest.first_chunk::<FRAME_HEADER_LEN>()?;
    let len = usize::try_from(u32::from_le_bytes([header[0], header[1], header[2], header[3]])).ok()?;
    let fits = len != 0 && len <= MAX_PAYLOAD && rest.len() >= FRAME_HEADER_LEN.checked_add(len)?;
    fits.then_some(len)
}

/// Whether the `len` payload bytes after the frame header in `rest` start with `{` and end with `}`, ignoring JSON
/// whitespace, and how many padding bytes that took to see. `None` when that is more than `budget`.
fn skim_payload(rest: &[u8], len: usize, budget: usize) -> Option<(bool, usize)> {
    let payload = rest.get(FRAME_HEADER_LEN..FRAME_HEADER_LEN + len)?;
    let blank = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r');
    let lead = payload.iter().take(budget.saturating_add(1)).take_while(|b| blank(b)).count();
    if lead > budget {
        return None;
    }
    let trail = payload.iter().rev().take(budget.saturating_add(1)).take_while(|b| blank(b)).count();
    if lead.saturating_add(trail) > budget {
        return None;
    }
    let object = lead < len && payload.get(lead) == Some(&b'{') && payload.get(len - 1 - trail) == Some(&b'}');
    Some((object, lead + trail))
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

    /// Valid frames in the segments from index `first` on, past their headers; a file that can't be read holds none.
    fn count_from(&mut self, first: usize) -> usize {
        let mut count = 0;
        for segment in self.segments.iter().skip(first) {
            if let Ok(bytes) = Self::read(segment) {
                count += self.count_in(&bytes, HEADER_LEN);
            }
        }
        count
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
            let header = match segment.head()? {
                Err(HeaderFault::TooNew { version }) => return Err(JournalError::TooNew { version, kind: None }),
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
                    Some(next) => match next.head()? {
                        Ok(h) if h.is_prepared() => return Ok(End::Clean),
                        Ok(h) if self.done || h.first != self.next => {
                            let later_valid = self.count_from(idx + 1);
                            return Ok(self.bad_end(self.position(next, 0), later_valid));
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
                    let later_valid = self.count_in(&bytes, offset + 1) + self.count_from(idx + 1);
                    return Ok(self.bad_end(at, later_valid));
                }
            }
        }
    }

    /// Whether `segment` is the one an earlier recovery started (8.5): it begins one past the last good record, with a
    /// `session` record. A segment that can't be read is not one.
    fn starts_recovery_session(&self, segment: &Segment) -> bool {
        let Ok(Ok(header)) = segment.head() else {
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

    /// Looks for the frame `from + 1` after `offset`: the next record the snapshot doesn't cover. Rule 3 of
    /// [`Reader::scan`]. Spends the resync budget.
    fn find_base(&mut self, bytes: &[u8], offset: usize) -> Option<usize> {
        let wanted = self.from.0.checked_add(1)?;
        let mut at = offset + 1;
        while at < bytes.len() {
            match probe(tail(bytes, at), &mut self.budget) {
                Probe::Exhausted => {
                    self.exhausted = true;
                    return None;
                }
                Probe::Frame(frame) if frame.lsn.0 == wanted => return Some(at),
                Probe::Frame(frame) => at += frame.len(),
                Probe::No => at += 1,
            }
        }
        None
    }

    /// Reads the frames of one segment, keeping the records after `from`.
    fn read_frames(&mut self, bytes: &[u8]) -> Result<Stop, JournalError> {
        let mut offset = HEADER_LEN;
        loop {
            let rest = tail(bytes, offset);
            let stop = match read_frame(rest) {
                Ok(frame) if !self.done && frame.lsn == self.next => {
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
                    continue;
                }
                Err(FrameEnd::Short) if rest.is_empty() => return Ok(Stop::Eof),
                Err(FrameEnd::Zero) => Stop::ZeroTail,
                Err(FrameEnd::Short) if rest.iter().all(|&b| b == 0) => Stop::ZeroTail,
                Ok(_) | Err(_) => Stop::Bad(offset),
            };
            // Damage before the base is old (8.2): skip to the record after it, if there is one.
            if self.next <= self.from
                && let Some(found) = self.find_base(bytes, offset)
            {
                self.next = Lsn(self.from.0 + 1);
                offset = found;
                continue;
            }
            return Ok(stop);
        }
    }
}
