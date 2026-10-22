//! Session ids that never repeat across restarts (`docs/design/branches.md` 5.3): the counter every lane shares
//! starts at the high-water mark kept in `.omuzaki/session-ids`, and the mark is moved ahead of the counter before the
//! counter reaches it, so not even a crash lets the next run hand out an id this one did. A clean close writes the
//! counter itself, so the numbers stay small.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The mark's file in the project's `.omuzaki` folder.
const FILE: &str = "session-ids";
/// How far ahead of the counter the mark is kept.
const AHEAD: u64 = 1024;

/// The shared session counter and its mark on disk.
pub(crate) struct IdMark {
    path: PathBuf,
    counter: Arc<AtomicU64>,
    /// The mark as last written: every id below it may have been handed out.
    reserved: AtomicU64,
}

impl IdMark {
    /// Reads the mark of the project in `dir` (none yet reads as 1) and starts `counter` (the next id) at it or above.
    #[allow(clippy::disallowed_methods, reason = "the host reading its own bookkeeping in `.omuzaki`; not an op")]
    pub(crate) fn open(dir: &Path, counter: Arc<AtomicU64>) -> Self {
        let path = dir.join(".omuzaki").join(FILE);
        let mark = std::fs::read_to_string(&path).ok().and_then(|text| text.trim().parse::<u64>().ok()).unwrap_or(1);
        counter.fetch_max(mark, Ordering::SeqCst);
        Self { path, counter, reserved: AtomicU64::new(mark) }
    }

    /// Moves the mark [`AHEAD`] past the counter once the counter is within half of that of it. Called after the
    /// lanes open (their histories may move the counter up) and on the reaper's tick.
    pub(crate) fn keep_ahead(&self) {
        let next = self.counter.load(Ordering::SeqCst);
        if next + AHEAD / 2 > self.reserved.load(Ordering::SeqCst) {
            self.write(next + AHEAD);
        }
    }

    /// At a clean close: the next run starts exactly where this one stopped.
    pub(crate) fn close(&self) {
        self.write(self.counter.load(Ordering::SeqCst));
    }

    fn write(&self, mark: u64) {
        match omuzaki_core::fsx::write_atomic(&self.path, &format!("{mark}\n")) {
            Ok(()) => self.reserved.store(mark, Ordering::SeqCst),
            Err(error) => {
                tracing::warn!(%error, "the session id mark couldn't be written; ids may repeat after a crash");
            }
        }
    }
}
