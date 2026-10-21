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
#[non_exhaustive]
pub enum End {
    /// At the end of the last segment, a zero-filled tail, or a prepared segment.
    Clean,
    /// At a bad frame or header with nothing valid after it: the unflushed end of the last batch.
    TornTail {
        /// Where the log ends.
        at: Position,
    },
    /// At a bad frame, with valid frames after it or a search for them that didn't finish: damage, not a torn tail.
    /// Recovery keeps the raw segment.
    Damaged {
        /// Where the log ends.
        at: Position,
        /// How many valid frames were verified after `at`, in this and later segments. Always a count of frames that
        /// were read and checked, possibly 0.
        later_valid: usize,
        /// Whether the search for valid frames after `at` covered everything. The search has a work budget; when it
        /// runs out this is false, `later_valid` counts only what was verified before, and the log is treated as
        /// damaged and never as a torn tail, because it may hold more.
        search_complete: bool,
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
    /// Reads the segments of `dir` in sequence order, from the first, and returns every record after `from` up to
    /// where the log ends.
    ///
    /// # Scan rules
    /// Journal.md 8.2, as this reader applies it. The invariant: **nothing beyond a logical end can change the result
    /// of a scan**, other than being counted as damage when valid frames lie after a *bad* frame. Every rule is tested
    /// in `tests/journal_reader.rs`, and a randomized test checks the walk against a naive model.
    /// 1. **One walk.** There is no separate start selection: the scan walks the segments from the first, and keeps
    ///    the records after `from`. Segments before the base are read, because that is what shows a later segment is
    ///    reachable and not stale. The first segment may start at or before `from + 1`; one that starts later leaves a
    ///    gap after the base, so nothing is returned and the log is reported as damaged (rule 6) at that segment.
    /// 2. **Logical ends.** A prepared segment, a segment with a bad header, a zero-filled tail, a bad frame and the end
    ///    of the last segment end the log, and nothing after one is read, except under rules 3 and 4.
    /// 3. **One boundary decision.** After each segment, one function decides from the segment's stop reason and the
    ///    next segment's header: after an exact end of file the log continues if the next segment starts one past the
    ///    last record (a rotation), and a segment that doesn't is a discontinuity; after a zero tail or a bad frame it
    ///    continues only into a recovery `session` (8.5), a segment that begins one past the last good record with a
    ///    CRC-valid, decodable `session` record.
    /// 4. **Old damage.** A bad frame or a gap before the base (the snapshot covers it) doesn't end the scan: after a bad
    ///    frame it looks for the frame `from + 1` in the same segment, and failing that, like after an exact end of
    ///    file, it continues into a next segment that starts after the last good record and no later than `from + 1`.
    ///    A zero tail is never old damage, and zeros
    ///    are a tail only at a frame boundary the search has established (after a valid frame, or where the bad
    ///    frame's own length says the next one starts), never at an arbitrary offset inside damage. At such a
    ///    boundary a zero length is a tail, whatever follows it, and a tail found that way ends the log (rule 2): it
    ///    is not a bad frame that a later segment could continue past.
    /// 5. **Reachability.** A file is opened only when the walk reaches it. A newer-format header or an unknown
    ///    required kind there is [`JournalError::TooNew`], and an unreadable file is [`JournalError::Io`]; the same
    ///    beyond a logical end is ignored.
    /// 6. **Classification.** After a bad frame or a discontinuity the valid frames after it are counted, in the rest of
    ///    its segment and in the later segments up to a prepared, bad-header or newer-format one. None is a [`End::TornTail`], some
    ///    is [`End::Damaged`], also when the work budget runs out (then `search_complete` is false). A file that can't be read
    ///    holds none.
    ///
    /// # Errors
    /// [`JournalError::Io`] for a folder, or a file the walk reaches, that can't be read. [`JournalError::TooNew`] as
    /// in rule 5: nothing is returned, because the project opens read-only (4.5). [`JournalError::BadPayload`] from
    /// [`decode`], for a record after `from`.
    ///
    /// Looking past a bad frame for valid ones has a fixed work budget. If it runs out, the result is
    /// [`End::Damaged`] with `search_complete` false.
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
        let mut scanner = Scanner {
            segments: &segments,
            from,
            records: Vec::new(),
            after: from,
            next: Lsn(0),
            done: false,
            budget,
            exhausted: false,
        };
        let end = scanner.run()?;
        Ok(Scan { records: scanner.records, end })
    }
}

/// A segment file and what its header says.
struct Segment {
    seq: u64,
    path: PathBuf,
}

/// What a segment's header says.
enum Class {
    Real(SegmentHeader),
    /// Prepared and unused (first LSN 0): ends the log.
    Prepared,
    /// A header that doesn't read: ends the log, or is a newer format ([`HeaderFault::TooNew`]).
    Fault(HeaderFault),
}

impl Segment {
    /// What the header says. Opened when asked for, so an unreadable file only matters once the walk reaches it.
    fn class(&self) -> Result<Class, JournalError> {
        let mut head = Vec::with_capacity(HEADER_LEN);
        let limit = offset_u64(HEADER_LEN);
        fs::File::open(&self.path)
            .and_then(|file| file.take(limit).read_to_end(&mut head))
            .map_err(io_error(&self.path))?;
        Ok(match SegmentHeader::decode(&head) {
            Ok(header) if header.is_prepared() => Class::Prepared,
            Ok(header) => Class::Real(header),
            Err(fault) => Class::Fault(fault),
        })
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

/// Whether a frame boundary at the start of `rest` is a zero tail (journal.md 8.2): a zero length, or zeros for the
/// length of a frame header. The end of the file is not one. Only for a boundary that is known to be one.
fn is_zero_tail(rest: &[u8]) -> bool {
    !rest.is_empty()
        && (matches!(read_frame(rest), Err(FrameEnd::Zero)) || rest.iter().take(FRAME_HEADER_LEN).all(|&b| b == 0))
}

/// What looking for the frame after the base found in old damage.
enum Resync {
    /// The frame, at this offset.
    Base(usize),
    /// A zero tail first: the log ends there, whatever lies after it.
    Tail,
    /// Neither.
    Nothing,
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

/// What happens at the end of a segment (rule 3 of [`Reader::scan`]).
enum Boundary {
    /// The log continues in the next segment.
    Continue(SegmentHeader),
    /// The log ends here: clean after an exact end or a zero tail, classified by looking past it after a bad frame.
    End,
    /// The next segment's header is bad, after an exact end of file: a torn tail.
    Torn,
    /// The next segment doesn't start where the log left off.
    Discontinuity,
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

    /// Valid frames in the segments from index `first` on, past their headers, up to a header that ends the log
    /// (prepared, bad or newer format: rule 6). A file that can't be read holds none.
    fn count_after(&mut self, first: usize) -> usize {
        let mut count = 0;
        for segment in self.segments.iter().skip(first) {
            match segment.class() {
                Ok(Class::Real(_)) => {
                    if let Ok(bytes) = Self::read(segment) {
                        count += self.count_in(&bytes, HEADER_LEN);
                    }
                }
                Err(_) => {}
                Ok(Class::Prepared | Class::Fault(_)) => break,
            }
        }
        count
    }

    /// The end for a bad frame at `at` with `later_valid` valid frames found after it. When the resync budget ran out
    /// the search is incomplete and the log is damaged, not torn, whatever the count.
    fn bad_end(&self, at: Position, later_valid: usize) -> End {
        if later_valid > 0 || self.exhausted {
            End::Damaged { at, later_valid, search_complete: !self.exhausted }
        } else {
            End::TornTail { at }
        }
    }

    /// Walks the segments in order until the log ends (rules 1 to 3 of [`Reader::scan`]).
    fn run(&mut self) -> Result<End, JournalError> {
        let segments = self.segments;
        let Some(first) = segments.first() else {
            return Ok(End::Clean);
        };
        let mut header = match first.class()? {
            Class::Real(header) => header,
            Class::Prepared => return Ok(End::Clean),
            Class::Fault(HeaderFault::TooNew { version }) => return Err(JournalError::TooNew { version, kind: None }),
            Class::Fault(_) => return Ok(End::TornTail { at: self.position(first, 0) }),
        };
        if header.first.0 > self.from.0.saturating_add(1) {
            // The oldest segment starts after the base: records are missing, and nothing after them is trusted.
            let later_valid = self.count_after(0);
            return Ok(self.bad_end(self.position(first, 0), later_valid));
        }
        for (idx, segment) in segments.iter().enumerate() {
            self.next = header.first;
            self.done = false;
            let bytes = Self::read(segment)?;
            let next = segments.get(idx + 1);
            let stop = self.read_frames(&bytes, next)?;
            match (self.boundary(next, &stop)?, next) {
                (Boundary::Continue(next_header), _) => header = next_header,
                (Boundary::End, _) => {
                    let Stop::Bad(offset) = stop else {
                        return Ok(End::Clean);
                    };
                    let later_valid = self.count_in(&bytes, offset + 1) + self.count_after(idx + 1);
                    return Ok(self.bad_end(self.position(segment, offset), later_valid));
                }
                (Boundary::Torn, Some(next)) => return Ok(End::TornTail { at: self.position(next, 0) }),
                (Boundary::Discontinuity, Some(next)) => {
                    let later_valid = self.count_after(idx + 1);
                    return Ok(self.bad_end(self.position(next, 0), later_valid));
                }
                (Boundary::Torn | Boundary::Discontinuity, None) => return Ok(End::Clean),
            }
        }
        Ok(End::Clean)
    }

    /// What follows a segment that stopped for `stop`, given the segment after it, if any. The one decision that the
    /// walk makes at every boundary.
    fn boundary(&self, next: Option<&Segment>, stop: &Stop) -> Result<Boundary, JournalError> {
        let Some(next) = next else {
            return Ok(Boundary::End);
        };
        let exact = matches!(stop, Stop::Eof);
        let class = match next.class() {
            Ok(class) => class,
            // Only an exact end of file leads into the next file; for the others it is a look, not a read.
            Err(err) if exact => return Err(err),
            Err(_) => return Ok(Boundary::End),
        };
        Ok(match class {
            Class::Fault(HeaderFault::TooNew { version }) if exact => {
                return Err(JournalError::TooNew { version, kind: None });
            }
            Class::Fault(_) if exact => Boundary::Torn,
            Class::Real(header) if exact => {
                if (!self.done && header.first == self.next) || self.skips_old_damage(&header) {
                    Boundary::Continue(header)
                } else {
                    Boundary::Discontinuity
                }
            }
            Class::Real(header) => {
                if self.starts_recovery_session(next, &header)
                    || (matches!(stop, Stop::Bad(_)) && self.skips_old_damage(&header))
                {
                    Boundary::Continue(header)
                } else {
                    Boundary::End
                }
            }
            Class::Prepared | Class::Fault(_) => Boundary::End,
        })
    }

    /// Whether `segment` is the one an earlier recovery started (8.5): it begins one past the last good record, with a
    /// `session` record that is valid and decodes. A segment that can't be read is not one.
    fn starts_recovery_session(&self, segment: &Segment, header: &SegmentHeader) -> bool {
        if self.done || header.first != self.next {
            return false;
        }
        let Ok(bytes) = Self::read(segment) else {
            return false;
        };
        read_frame(tail(&bytes, HEADER_LEN)).is_ok_and(|frame| {
            frame.kind == kind::SESSION
                && frame.lsn == header.first
                && matches!(decode(&frame), Ok(Some(Record::Session(_))))
        })
    }

    /// Whether `next` is the recovery session segment that continues the log at the last good record (8.5).
    fn recovers_into(&self, next: Option<&Segment>) -> bool {
        next.is_some_and(|segment| {
            matches!(segment.class(), Ok(Class::Real(header)) if self.starts_recovery_session(segment, &header))
        })
    }

    /// Whether the damage or gap the snapshot covers (rule 4) is followed by `header`'s segment: it starts after the
    /// last good record and no later than the base.
    fn skips_old_damage(&self, header: &SegmentHeader) -> bool {
        !self.done
            && self.next <= self.from
            && self.next < header.first
            && header.first.0 <= self.from.0.saturating_add(1)
    }

    /// Looks for the frame `from + 1` at or after `offset`: the next record the snapshot doesn't cover. Rule 4 of
    /// [`Reader::scan`]. Spends the resync budget.
    fn find_base(&mut self, bytes: &[u8], offset: usize) -> Resync {
        let Some(wanted) = self.from.0.checked_add(1) else {
            return Resync::Nothing;
        };
        let mut at = offset;
        while at < bytes.len() {
            let rest = tail(bytes, at);
            match probe(rest, &mut self.budget) {
                Probe::Exhausted => {
                    self.exhausted = true;
                    return Resync::Nothing;
                }
                Probe::Frame(frame) if frame.lsn.0 == wanted => return Resync::Base(at),
                Probe::Frame(frame) => {
                    at += frame.len();
                    // A frame boundary the search has established: a zero tail here ends the log.
                    if is_zero_tail(tail(bytes, at)) {
                        return Resync::Tail;
                    }
                }
                Probe::No => {
                    // The bad frame's own length, if it fits, says where the next frame should start. Zeros at an
                    // arbitrary offset inside damage are not a tail.
                    if at == offset
                        && let Some(len) = frame_len(rest)
                        && is_zero_tail(tail(bytes, at + FRAME_HEADER_LEN + len))
                    {
                        return Resync::Tail;
                    }
                    at += 1;
                }
            }
        }
        Resync::Nothing
    }

    /// Reads the frames of one segment, keeping the records after `from`.
    fn read_frames(&mut self, bytes: &[u8], next: Option<&Segment>) -> Result<Stop, JournalError> {
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
                Err(FrameEnd::Zero) => return Ok(Stop::ZeroTail),
                Err(FrameEnd::Short) if rest.iter().all(|&b| b == 0) => return Ok(Stop::ZeroTail),
                Ok(_) | Err(_) => Stop::Bad(offset),
            };
            // Damage before the base is old (8.2): skip to the record after it, if there is one. Not when a recovery
            // session follows the last good record: the rest of this segment was discarded.
            if self.next <= self.from && !self.recovers_into(next) {
                match self.find_base(bytes, offset) {
                    Resync::Base(found) => {
                        self.next = Lsn(self.from.0 + 1);
                        offset = found;
                        continue;
                    }
                    // A genuine end of the log, not damage: it must not become a bad frame that a later segment
                    // could continue past.
                    Resync::Tail => return Ok(Stop::ZeroTail),
                    Resync::Nothing => {}
                }
            }
            return Ok(stop);
        }
    }
}
