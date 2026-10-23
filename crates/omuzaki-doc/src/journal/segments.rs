//! The live segment and the one prepared behind it (journal.md 4.1): a commit never creates a file.
//!
//! The preparer thread keeps one segment ready ahead of the writer: created with a "prepared, unused" header (first
//! LSN 0), flushed, its folder synced. Rotation writes the real header and the batch into it at offset 0 and flushes
//! that: a data flush of a file whose directory entry has long been durable. If the preparer is ever behind (it has a
//! whole segment of commits to catch up), the writer waits for one it is making, or makes the file itself when it isn't
//! making one, and says so in the log.
//!
//! A prepared segment ends the log for the reader, so one must never sit before a used one: the writer only ever
//! rotates into the next sequence number, and the preparer only ever prepares that one.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use omuzaki_history_api::Lsn;

use super::fs::{Flush, JournalFile, JournalFs};
use super::{HEADER_LEN, JournalError, SegmentHeader, segment_name, segment_seq};

/// The first wait before the preparer tries again after a failure, and the longest.
const PREP_RETRY: (Duration, Duration) = (Duration::from_millis(100), Duration::from_secs(5));

/// The segment the writer appends to.
pub(super) struct Segment {
    file: Box<dyn JournalFile>,
    /// The end of the last good write.
    pub(super) len: u64,
    /// Its sequence number.
    pub(super) seq: u64,
}

impl Segment {
    /// The session's first segment, in `dir`, starting at `first` with `records` after its header, durable (its folder
    /// too) before it returns. A prepared segment a previous session left at the end of the log is used; otherwise a
    /// new one is made, one past the highest.
    pub(super) fn open_first(fs: &dyn JournalFs, dir: &Path, first: Lsn, records: &[u8]) -> Result<Self, JournalError> {
        let io_at = |path: &Path| {
            let path = path.to_path_buf();
            move |source| JournalError::Io { path, source }
        };
        fs.create_dir_all(dir).map_err(io_at(dir))?;
        let last = match fs.list(dir) {
            Ok(names) => names.iter().filter_map(|name| segment_seq(name)).max(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(source) => return Err(JournalError::Io { path: dir.to_path_buf(), source }),
        };
        if let Some(seq) = last {
            let path = segment_path(dir, seq, first)?;
            let header = fs.read_start(&path, HEADER_LEN).map_err(io_at(&path))?;
            if SegmentHeader::decode(&header).is_ok_and(|h| h.is_prepared()) {
                let file = fs.open_write(&path).map_err(io_at(&path))?;
                return Self::fill(Prepared { file, seq }, first, records, Flush::Full).map_err(io_at(&path));
            }
        }
        let seq = last.map_or(1, |seq| seq + 1);
        let path = segment_path(dir, seq, first)?;
        let file = fs.create_new(&path).map_err(io_at(&path))?;
        let segment = Self::fill(Prepared { file, seq }, first, records, Flush::Full).map_err(io_at(&path))?;
        fs.sync_dir(dir).map_err(io_at(dir))?;
        Ok(segment)
    }

    /// Writes the real header and `records` into `prepared` at offset 0 in one write and flushes them.
    pub(super) fn fill(mut prepared: Prepared, first: Lsn, records: &[u8], how: Flush) -> io::Result<Self> {
        let mut bytes = SegmentHeader::new(first).encode().to_vec();
        bytes.extend_from_slice(records);
        prepared.file.write_at(0, &bytes).and_then(|()| prepared.file.flush(how))?;
        Ok(Self { file: prepared.file, len: bytes.len() as u64, seq: prepared.seq })
    }

    /// Appends `bytes` at the end of the last good write and flushes them. On failure the file is cut back to that
    /// end, so a retry never leaves a partial frame between two good ones.
    pub(super) fn append(&mut self, bytes: &[u8], how: Flush) -> io::Result<()> {
        match self.file.write_at(self.len, bytes).and_then(|()| self.file.flush(how)) {
            Ok(()) => {
                self.len += bytes.len() as u64;
                Ok(())
            }
            Err(e) => {
                // Best effort: if even this fails, the reader stops at the partial frame, which was never durable.
                drop(self.file.set_len(self.len));
                Err(e)
            }
        }
    }

    /// Flushes what is written, with nothing new (a `sync` caller waits on records a barrier covered).
    pub(super) fn flush(&mut self, how: Flush) -> io::Result<()> {
        self.file.flush(how)
    }
}

/// A segment made ahead of time, durable with its prepared header.
pub(super) struct Prepared {
    file: Box<dyn JournalFile>,
    seq: u64,
}

/// What the writer and the preparer share.
struct Slot {
    /// The prepared segment, once made.
    ready: Option<Prepared>,
    /// The sequence number the next prepared segment gets.
    next: u64,
    /// The preparer is making `next - 1`: the writer waits for it rather than make that number itself.
    making: bool,
    stop: bool,
}

/// The preparer's side and the writer's: one prepared segment always waits.
pub(super) struct Preparer {
    slot: Mutex<Slot>,
    changed: Condvar,
    fs: Arc<dyn JournalFs>,
    dir: PathBuf,
}

impl Preparer {
    /// A preparer for `dir` whose first segment will be `next`.
    pub(super) fn new(fs: Arc<dyn JournalFs>, dir: PathBuf, next: u64) -> Arc<Self> {
        let slot = Slot { ready: None, next, making: false, stop: false };
        Arc::new(Self { slot: Mutex::new(slot), changed: Condvar::new(), fs, dir })
    }

    /// The preparer thread: makes the next segment whenever none waits, until stopped.
    pub(super) fn run(&self) {
        let mut retry = PREP_RETRY.0;
        loop {
            let seq = {
                let mut slot = super::writer::lock(&self.slot);
                while slot.ready.is_some() && !slot.stop {
                    slot = self.changed.wait(slot).unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                if slot.stop {
                    return;
                }
                slot.making = true;
                slot.next
            };
            let made = self.make(seq);
            let mut slot = super::writer::lock(&self.slot);
            slot.making = false;
            match made {
                Ok(prepared) if slot.next == seq => {
                    slot.ready = Some(prepared);
                    slot.next = seq + 1;
                    retry = PREP_RETRY.0;
                }
                // The writer made this number itself meanwhile; this file is never used and the next one is made.
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, seq, "the next journal segment couldn't be prepared; trying again");
                    self.changed.notify_all();
                    let (guard, _) =
                        self.changed.wait_timeout(slot, retry).unwrap_or_else(std::sync::PoisonError::into_inner);
                    slot = guard;
                    retry = (retry * 2).min(PREP_RETRY.1);
                }
            }
            self.changed.notify_all();
            drop(slot);
        }
    }

    /// Makes segment `seq`: created (or reopened, after a failure part way), its prepared header written and flushed,
    /// the folder synced.
    fn make(&self, seq: u64) -> io::Result<Prepared> {
        let path = segment_path(&self.dir, seq, Lsn(0)).map_err(io::Error::other)?;
        let mut file = match self.fs.create_new(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => self.fs.open_write(&path)?,
            Err(e) => return Err(e),
        };
        file.write_at(0, &SegmentHeader::prepared().encode())?;
        file.flush(Flush::Full)?;
        self.fs.sync_dir(&self.dir)?;
        Ok(Prepared { file, seq })
    }

    /// The segment to rotate into after `current`: the prepared one, or one the preparer is making (waited for), or,
    /// when it is making none, one the writer makes now (logged: a file created on the commit path).
    pub(super) fn take(&self, current: u64) -> io::Result<Prepared> {
        let mut slot = super::writer::lock(&self.slot);
        loop {
            if let Some(prepared) = slot.ready.take() {
                debug_assert_eq!(prepared.seq, current + 1, "the prepared segment is the next one");
                self.changed.notify_all();
                return Ok(prepared);
            }
            if !slot.making {
                break;
            }
            slot = self.changed.wait(slot).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        let seq = slot.next;
        slot.next = seq + 1;
        drop(slot);
        self.changed.notify_all();
        tracing::warn!(seq, "no journal segment was prepared; the writer made one on the commit path");
        let path = segment_path(&self.dir, seq, Lsn(0)).map_err(io::Error::other)?;
        let file = self.fs.create_new(&path)?;
        let prepared = Prepared { file, seq };
        self.fs.sync_dir(&self.dir)?;
        Ok(prepared)
    }

    /// Stops the preparer thread.
    pub(super) fn stop(&self) {
        super::writer::lock(&self.slot).stop = true;
        self.changed.notify_all();
    }
}

fn segment_path(dir: &Path, seq: u64, first: Lsn) -> Result<PathBuf, JournalError> {
    let name = segment_name(seq)
        .ok_or_else(|| JournalError::Unencodable { lsn: first.0, reason: format!("segment {seq} has no name") })?;
    Ok(dir.join(name))
}
