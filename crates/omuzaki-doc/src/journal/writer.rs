//! The journal writer and the keeper (journal.md 5 and 6): what makes every change durable before the project files
//! follow, so a crash loses at most the last few milliseconds and the files on disk never hold a change the journal
//! could lose.
//!
//! [`ProjectJournal`] is the executor's [`JournalSink`]. Three parties share it:
//!
//! - **The executor** calls [`append`](JournalSink::append) under its lock. The entry is queued in O(1) with no I/O;
//!   the writer is woken only if it sleeps.
//! - **The writer thread** takes everything queued (group commit: whatever arrives during one flush goes in the next),
//!   encodes it into frames, appends them to the current segment with one write and flushes it (`sync_data`). Then it
//!   hands the batch to the keeper and publishes the durable LSN, which releases [`sync`](JournalSink::sync) waiters.
//!   A failed write is retried with backoff (100 ms doubling to 5 s) and reported as `Failing` at once.
//! - **The keeper thread** applies each durable entry's patch to its own copy of the project (the shadow), saves the
//!   files 250 ms after the last change or every 2 s while changes continue, and writes a snapshot when the executor
//!   hands it a [`HistoryEntry::Mark`]. It asks for a mark every 10 minutes or 5,000 records of editing. A shadow that
//!   refuses a patch the executor applied has diverged: saving stops and new edits are refused.
//!
//! A session is started by [`ProjectJournal::start`] (its base snapshot, then a new segment whose first record is the
//! `session` record) and ended by [`ProjectJournal::close`] (the files saved, a close snapshot, the `close` record).
//! Dropping the journal without closing stops the threads without the `close` record: the next open recovers, which
//! is what a crash does too.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omuzaki_doc_api::{DocWrite, Patch};
use omuzaki_history_api::{
    HistoryEntry, HistoryExport, JournalSink, JournalStatus, Lsn, RunningJob, SnapshotId, SnapshotInfo, SyncError,
    Trigger, WallClock, WallTime,
};
use omuzaki_ops_api::OpError;

use super::{JournalError, Record, SegmentHeader, SessionInfo, encode, segment_name, segment_seq};
use crate::Project;
use crate::snapshot::{Facts, Snapshotter};

/// The folder inside `.omuzaki/` that holds the live segments.
pub const JOURNAL_DIR: &str = "journal";

/// The folder inside `.omuzaki/` that holds sealed segments behind a snapshot: the op log archive.
pub const ARCHIVE_DIR: &str = "log";

/// The service name a refusal to admit carries.
const SERVICE: &str = "journal";

/// The longest wait between two write retries.
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// The first wait after a failed write.
const FIRST_BACKOFF: Duration = Duration::from_millis(100);
/// How often the keeper wakes with nothing to do, to check the snapshot cadence.
const KEEPER_TICK: Duration = Duration::from_secs(1);

/// The tunables of a journal. [`Default`] gives the design's values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct JournalOptions {
    /// Save the files this long after the last change (design: 250 ms).
    pub save_idle: Duration,
    /// ...and at least this often while changes continue (2 s).
    pub save_max: Duration,
    /// Ask for a snapshot this long after the last one, if anything changed (10 minutes).
    pub snapshot_every: Duration,
    /// ...or after this many records (5,000).
    pub snapshot_records: u64,
    /// Start a new segment once the current one is this large (16 MiB).
    pub segment_bytes: u64,
    /// Report `Behind` with this many entries not yet durable (50,000).
    pub behind: usize,
    /// Refuse new edits with this many entries not yet durable (200,000).
    pub refuse: usize,
}

impl Default for JournalOptions {
    fn default() -> Self {
        Self {
            save_idle: Duration::from_millis(250),
            save_max: Duration::from_secs(2),
            snapshot_every: Duration::from_secs(600),
            snapshot_records: 5_000,
            segment_bytes: 16 * 1024 * 1024,
            behind: 50_000,
            refuse: 200_000,
        }
    }
}

impl JournalOptions {
    /// These options with the save delays set: `idle` after the last change, at least every `max`.
    #[must_use]
    pub fn with_save_delays(mut self, idle: Duration, max: Duration) -> Self {
        self.save_idle = idle;
        self.save_max = max;
        self
    }

    /// These options with the snapshot cadence set: every `every`, or every `records` records.
    #[must_use]
    pub fn with_snapshot_cadence(mut self, every: Duration, records: u64) -> Self {
        self.snapshot_every = every;
        self.snapshot_records = records;
        self
    }

    /// These options with segments rotated at `bytes`.
    #[must_use]
    pub fn with_segment_bytes(mut self, bytes: u64) -> Self {
        self.segment_bytes = bytes;
        self
    }
}

/// What a new session starts from.
#[non_exhaustive]
pub struct SessionStart {
    /// The session's id.
    pub session: String,
    /// The application version that writes it.
    pub app_version: String,
    /// The last LSN the journal holds: the base snapshot covers it, and the session's first record is one past it.
    pub base_lsn: Lsn,
    /// The executor's history as the session starts (restored, or empty), for the base snapshot.
    pub history: HistoryExport,
    /// The highest job id the project has started (0: none), carried in every snapshot so job ids never repeat.
    pub last_job: u64,
    /// Wall time, for the session record and snapshot times.
    pub wall: Arc<dyn WallClock>,
    /// Where the session's snapshots go.
    pub snapshots: Arc<Snapshotter>,
}

impl SessionStart {
    /// A session `session` of `app_version`, after `base_lsn`, starting with `history`, its snapshots kept by
    /// `snapshots`.
    pub fn new(
        session: impl Into<String>,
        app_version: impl Into<String>,
        base_lsn: Lsn,
        history: HistoryExport,
        wall: Arc<dyn WallClock>,
        snapshots: Arc<Snapshotter>,
    ) -> Self {
        Self {
            session: session.into(),
            app_version: app_version.into(),
            base_lsn,
            history,
            last_job: 0,
            wall,
            snapshots,
        }
    }

    /// This start, with `last_job` the highest job id the project has started.
    #[must_use]
    pub fn with_last_job(mut self, last_job: u64) -> Self {
        self.last_job = last_job;
        self
    }
}

/// The journal of one open project: the [`JournalSink`] the executor writes to. See the [module docs](self).
pub struct ProjectJournal {
    shared: Arc<Shared>,
    snapshots: Arc<Snapshotter>,
    keeper: Sender<KeeperMsg>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

/// One item in the writer's queue.
enum Item {
    /// An entry the executor appended, with its LSN (a mark has the LSN of the record before it and isn't written).
    Entry(Lsn, HistoryEntry),
    /// The `close` record.
    Close(Lsn, SnapshotId),
}

struct Queue {
    items: Vec<Item>,
    /// The LSN the next record gets.
    next: Lsn,
    /// The writer waits for work; an append wakes it.
    asleep: bool,
    /// Set by close and drop: the writer finishes what is queued and exits.
    stop: bool,
}

#[derive(Default)]
struct Progress {
    durable: Lsn,
    /// The keeper has applied and saved everything up to here.
    saved: Lsn,
    /// The writer's last failure, while it retries.
    write_error: Option<String>,
    /// The last save failure; cleared by a save that works.
    save_error: Option<String>,
    /// Saving has stopped for good (a thread died, the shadow diverged, an entry that can't be encoded).
    dead: Option<String>,
    closed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    progress: Mutex<Progress>,
    progressed: Condvar,
    mark_wanted: AtomicBool,
    options: JournalOptions,
}

enum KeeperMsg {
    /// Durable items, in order.
    Durable(Vec<Item>),
    /// Save the files now, then answer.
    SaveNow(Sender<Result<(), String>>),
    /// Take a snapshot for the service (a named version, a take, a manual one) and answer with its ref.
    Take(Trigger, Option<String>, Sender<Result<SnapshotInfo, String>>),
    /// Save, write the close snapshot with this history, answer with its id, and stop.
    Close(HistoryExport, Sender<Result<SnapshotId, String>>),
    /// Stop without closing (the journal was dropped): what is durable is in the journal, and the next open recovers.
    Stop,
}

/// The mutex's data even if a thread panicked holding it: every critical section here leaves it consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl ProjectJournal {
    /// Starts a session of the project whose files and executor document are `project` (the keeper's shadow: it must
    /// be the document the executor starts from). Writes the base snapshot at `start.base_lsn` and a new segment whose
    /// first record is the session's, both durable before this returns, then starts the writer and the keeper.
    ///
    /// # Errors
    /// [`JournalError::Io`] if the snapshot, the segment or a thread can't be made; [`JournalError::Unencodable`] for
    /// a document or history JSON can't hold.
    pub fn start(project: Project, start: SessionStart, options: JournalOptions) -> Result<Arc<Self>, JournalError> {
        let dir = project.dir().to_path_buf();
        let created = start.wall.now();
        let base = Facts::new(start.base_lsn, &start.session, Trigger::Session, created).with_last_job(start.last_job);
        let base_id = start
            .snapshots
            .take_rendered(&project, base, &start.history)
            .map_err(|e| JournalError::Snapshot(e.to_string()))?
            .info
            .id;

        let journal_dir = journal_dir(&dir);
        let session_lsn = Lsn(start.base_lsn.0 + 1);
        let info =
            SessionInfo::new(start.session.clone(), created, start.app_version, project.schema_version().unwrap_or(0))
                .with_base(base_id, start.base_lsn)
                .with_process(std::process::id(), host_name());
        let mut first = Vec::new();
        encode(&[(session_lsn, Record::Session(info))], &mut first)?;
        let segment = Segment::create(&journal_dir, next_segment_seq(&journal_dir)?, session_lsn, &first)?;

        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { items: Vec::new(), next: Lsn(session_lsn.0 + 1), asleep: false, stop: false }),
            wake: Condvar::new(),
            progress: Mutex::new(Progress { durable: session_lsn, saved: session_lsn, ..Progress::default() }),
            progressed: Condvar::new(),
            mark_wanted: AtomicBool::new(false),
            options,
        });
        let (keeper_tx, keeper_rx) = mpsc::channel();
        let keeper = Keeper {
            shared: Arc::clone(&shared),
            shadow: project,
            snapshots: Arc::clone(&start.snapshots),
            session: start.session,
            label: String::new(),
            last_job: start.last_job,
            running: BTreeMap::new(),
            published: start.base_lsn,
            wall: start.wall,
            applied: session_lsn,
            dirty_since: None,
            last_change: None,
            records_since_snapshot: 0,
            last_snapshot: Instant::now(),
        };
        let writer = Writer { shared: Arc::clone(&shared), segment, keeper: keeper_tx.clone(), dir: journal_dir };
        let threads = vec![
            spawn("omuzaki-journal", &dir, move || writer.run())?,
            spawn("omuzaki-keeper", &dir, move || keeper.run(&keeper_rx))?,
        ];
        Ok(Arc::new(Self { shared, snapshots: start.snapshots, keeper: keeper_tx, threads: Mutex::new(threads) }))
    }

    /// Saves the project files now and returns once they hold everything appended before the call (`project.save`,
    /// tests, a commit to version control).
    ///
    /// # Errors
    /// As [`sync`](JournalSink::sync); [`SyncError::Failing`] with the reason when the files can't be saved.
    pub fn save_now(&self, timeout: Duration) -> Result<Lsn, SyncError> {
        let durable = self.sync(timeout)?;
        let (reply, answer) = mpsc::channel();
        self.keeper.send(KeeperMsg::SaveNow(reply)).map_err(|_| self.stopped())?;
        match answer.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(durable),
            Ok(Err(reason)) => Err(SyncError::Failing { reason }),
            Err(_) => Err(SyncError::Timeout { durable }),
        }
    }

    /// Takes a snapshot of everything appended before the call and returns its ref once the ref exists: a named
    /// version (`name` is the name), a take's state (`name` is the stem) or a manual one.
    ///
    /// # Errors
    /// As [`sync`](JournalSink::sync); [`SyncError::Failing`] with the reason when the snapshot can't be stored.
    pub fn take_snapshot(
        &self,
        trigger: Trigger,
        name: Option<String>,
        timeout: Duration,
    ) -> Result<SnapshotInfo, SyncError> {
        let durable = self.sync(timeout)?;
        let (reply, answer) = mpsc::channel();
        self.keeper.send(KeeperMsg::Take(trigger, name, reply)).map_err(|_| self.stopped())?;
        match answer.recv_timeout(timeout) {
            Ok(Ok(info)) => Ok(info),
            Ok(Err(reason)) => Err(SyncError::Failing { reason }),
            Err(_) => Err(SyncError::Timeout { durable }),
        }
    }

    /// The session's snapshots: list, load and sweep them.
    pub fn snapshots(&self) -> &Arc<Snapshotter> {
        &self.snapshots
    }

    /// Ends the session cleanly: everything appended is made durable, the files are saved, a close snapshot of the
    /// document and `history` (the executor's, after its last change) is written, then the `close` record. Returns the
    /// close snapshot's id. New edits are refused from the start of the call.
    ///
    /// # Errors
    /// [`SyncError`] if a step fails or takes longer than `timeout`. The threads stop either way; without the `close`
    /// record the next open recovers from the journal, so nothing is lost.
    pub fn close(&self, history: HistoryExport, timeout: Duration) -> Result<SnapshotId, SyncError> {
        lock(&self.shared.progress).closed = true;
        let result = self.close_steps(history, timeout);
        self.stop();
        result
    }

    fn close_steps(&self, history: HistoryExport, timeout: Duration) -> Result<SnapshotId, SyncError> {
        let durable = self.sync(timeout)?;
        let (reply, answer) = mpsc::channel();
        self.keeper.send(KeeperMsg::Close(history, reply)).map_err(|_| self.stopped())?;
        let id = match answer.recv_timeout(timeout) {
            Ok(Ok(id)) => id,
            Ok(Err(reason)) => return Err(SyncError::Failing { reason }),
            Err(_) => return Err(SyncError::Timeout { durable }),
        };
        {
            let mut queue = lock(&self.shared.queue);
            let lsn = queue.next;
            queue.next = Lsn(lsn.0 + 1);
            queue.items.push(Item::Close(lsn, id));
            self.shared.wake.notify_one();
        }
        self.sync(timeout)?;
        Ok(id)
    }

    /// Stops the writer once it has written what is queued, and the keeper once it has processed it, and waits for
    /// both.
    fn stop(&self) {
        {
            let mut queue = lock(&self.shared.queue);
            queue.stop = true;
            self.shared.wake.notify_one();
        }
        // The writer first: everything it wrote reaches the keeper's inbox before the keeper is told to stop. A thread
        // that panicked has already marked the journal dead; there is nothing more to report here.
        let threads = std::mem::take(&mut *lock(&self.threads));
        let mut threads = threads.into_iter();
        if let Some(writer) = threads.next() {
            drop(writer.join());
        }
        drop(self.keeper.send(KeeperMsg::Stop));
        for keeper in threads {
            drop(keeper.join());
        }
    }

    fn stopped(&self) -> SyncError {
        SyncError::Failing { reason: self.failure().unwrap_or_else(|| "the journal has stopped".to_owned()) }
    }

    /// Why saving fails, if it does.
    fn failure(&self) -> Option<String> {
        let progress = lock(&self.shared.progress);
        progress.dead.clone().or_else(|| progress.write_error.clone())
    }
}

impl Drop for ProjectJournal {
    fn drop(&mut self) {
        self.stop();
    }
}

impl JournalSink for ProjectJournal {
    fn append(&self, entry: HistoryEntry) -> Lsn {
        let mut queue = lock(&self.shared.queue);
        let lsn = if matches!(entry, HistoryEntry::Mark { .. }) {
            self.shared.mark_wanted.store(false, Ordering::Relaxed);
            Lsn(queue.next.0 - 1)
        } else {
            let lsn = queue.next;
            queue.next = Lsn(lsn.0 + 1);
            lsn
        };
        queue.items.push(Item::Entry(lsn, entry));
        if queue.asleep {
            queue.asleep = false;
            self.shared.wake.notify_one();
        }
        lsn
    }

    fn admit(&self) -> Result<(), OpError> {
        let pending = self.pending();
        let progress = lock(&self.shared.progress);
        if let Some(reason) = &progress.dead {
            return Err(OpError::unavailable(SERVICE).hint(format!(
                "saving stopped: {reason}; please report it. Nothing new is applied until it restarts"
            )));
        }
        if progress.closed {
            return Err(OpError::unavailable(SERVICE).hint("the project is closing"));
        }
        if pending >= self.shared.options.refuse {
            let why = progress.write_error.as_deref().unwrap_or("saving is far behind");
            return Err(OpError::unavailable(SERVICE)
                .hint(format!("changes can't be saved: {why}; nothing new is applied until saving catches up")));
        }
        Ok(())
    }

    fn wants_mark(&self) -> bool {
        self.shared.mark_wanted.load(Ordering::Relaxed)
    }

    fn sync(&self, timeout: Duration) -> Result<Lsn, SyncError> {
        let target = Lsn(lock(&self.shared.queue).next.0 - 1);
        let deadline = Instant::now() + timeout;
        let mut progress = lock(&self.shared.progress);
        loop {
            if progress.durable >= target {
                return Ok(progress.durable);
            }
            if let Some(reason) = progress.dead.clone().or_else(|| progress.write_error.clone()) {
                return Err(SyncError::Failing { reason });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(SyncError::Timeout { durable: progress.durable });
            }
            progress = self.shared.progressed.wait_timeout(progress, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    fn status(&self) -> JournalStatus {
        let pending = self.pending();
        let progress = lock(&self.shared.progress);
        if let Some(reason) = progress.dead.as_ref().or(progress.write_error.as_ref()) {
            return JournalStatus::Failing { reason: reason.clone() };
        }
        if let Some(reason) = &progress.save_error {
            return JournalStatus::Failing { reason: format!("the project files can't be saved: {reason}") };
        }
        if pending >= self.shared.options.behind {
            return JournalStatus::Behind { pending };
        }
        JournalStatus::Healthy
    }
}

impl ProjectJournal {
    /// Records appended and not yet durable.
    fn pending(&self) -> usize {
        let last = lock(&self.shared.queue).next.0 - 1;
        let durable = lock(&self.shared.progress).durable.0;
        usize::try_from(last.saturating_sub(durable)).unwrap_or(usize::MAX)
    }
}

impl Shared {
    fn die(&self, reason: String) {
        let mut progress = lock(&self.progress);
        progress.dead.get_or_insert(reason);
        self.progressed.notify_all();
    }
}

/// The live segment the writer appends to.
struct Segment {
    file: File,
    len: u64,
}

impl Segment {
    /// Creates segment `seq` in `dir` starting at `first`, with `records` after its header, durable (the folder too,
    /// on Unix) before it returns.
    fn create(dir: &Path, seq: u64, first: Lsn, records: &[u8]) -> Result<Self, JournalError> {
        fs::create_dir_all(dir).map_err(|source| JournalError::Io { path: dir.to_path_buf(), source })?;
        let name = segment_name(seq)
            .ok_or_else(|| JournalError::Unencodable { lsn: first.0, reason: format!("segment {seq} has no name") })?;
        let path = dir.join(name);
        let io = |source| JournalError::Io { path: path.clone(), source };
        let mut file = OpenOptions::new().write(true).create_new(true).open(&path).map_err(io)?;
        let mut bytes = SegmentHeader::new(first).encode().to_vec();
        bytes.extend_from_slice(records);
        file.write_all(&bytes).map_err(io)?;
        file.sync_data().map_err(io)?;
        #[cfg(unix)]
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|source| JournalError::Io { path: dir.to_path_buf(), source })?;
        Ok(Self { file, len: bytes.len() as u64 })
    }

    /// Appends `bytes` at the end of the last good write and flushes them. On failure the file is cut back to that
    /// end, so a retry never leaves a partial frame between two good ones.
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let written = self
            .file
            .seek(SeekFrom::Start(self.len))
            .and_then(|_| self.file.write_all(bytes))
            .and_then(|()| self.file.sync_data());
        match written {
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
}

struct Writer {
    shared: Arc<Shared>,
    segment: Segment,
    keeper: Sender<KeeperMsg>,
    dir: PathBuf,
}

impl Writer {
    fn run(mut self) {
        let mut buffer = Vec::new();
        while let Some(batch) = self.next_batch() {
            if let Err(reason) = self.write(&batch, &mut buffer) {
                self.shared.die(reason);
                return;
            }
            let last = batch.last().map(|item| match item {
                Item::Entry(lsn, _) | Item::Close(lsn, _) => *lsn,
            });
            // To the keeper before the durable mark moves, so whoever waited for this LSN and then talks to the keeper
            // is behind this batch.
            drop(self.keeper.send(KeeperMsg::Durable(batch)));
            let mut progress = lock(&self.shared.progress);
            if let Some(lsn) = last {
                progress.durable = progress.durable.max(lsn);
            }
            progress.write_error = None;
            self.shared.progressed.notify_all();
        }
    }

    /// Everything queued, waiting if there is nothing; `None` once stopped with nothing left.
    fn next_batch(&self) -> Option<Vec<Item>> {
        let mut queue = lock(&self.shared.queue);
        loop {
            if !queue.items.is_empty() {
                return Some(std::mem::take(&mut queue.items));
            }
            if queue.stop {
                return None;
            }
            queue.asleep = true;
            queue = self.shared.wake.wait(queue).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Encodes and writes one batch, retrying a failed write until it lands. Errors only for what a retry can't fix:
    /// an entry that can't be encoded, a segment that can't be named.
    fn write(&mut self, batch: &[Item], buffer: &mut Vec<u8>) -> Result<(), String> {
        let records: Vec<(Lsn, Record)> = batch
            .iter()
            .filter_map(|item| match item {
                Item::Entry(_, HistoryEntry::Mark { .. }) => None,
                Item::Entry(lsn, entry) => Some((*lsn, Record::Entry(entry.clone()))),
                Item::Close(lsn, snapshot) => Some((*lsn, Record::Close { snapshot: *snapshot })),
            })
            .collect();
        let Some(first) = records.first().map(|(lsn, _)| *lsn) else { return Ok(()) };
        buffer.clear();
        encode(&records, buffer).map_err(|e| format!("a change can't be written to the journal: {e}"))?;
        let mut backoff = FIRST_BACKOFF;
        loop {
            let written = if self.segment.len + buffer.len() as u64 > self.shared.options.segment_bytes {
                self.rotate(first, buffer)
            } else {
                self.segment.append(buffer).map_err(|e| e.to_string())
            };
            match written {
                Ok(()) => return Ok(()),
                Err(reason) => {
                    let mut progress = lock(&self.shared.progress);
                    progress.write_error = Some(format!("changes can't be saved: {reason}"));
                    self.shared.progressed.notify_all();
                }
            }
            thread::sleep(backoff);
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    /// Starts the next segment with this batch in it.
    fn rotate(&mut self, first: Lsn, records: &[u8]) -> Result<(), String> {
        let seq = next_segment_seq(&self.dir).map_err(|e| e.to_string())?;
        self.segment = Segment::create(&self.dir, seq, first, records).map_err(|e| e.to_string())?;
        Ok(())
    }
}

struct Keeper {
    shared: Arc<Shared>,
    shadow: Project,
    snapshots: Arc<Snapshotter>,
    session: String,
    /// The last transaction's label, for the next snapshot's ref.
    label: String,
    /// The highest job id started, for the next snapshot's ref.
    last_job: u64,
    /// The jobs started and not ended, by id, with their ops: for the next snapshot's ref.
    running: BTreeMap<u64, String>,
    /// The LSN of the newest snapshot published in this session, the base included: compaction moves what lies
    /// wholly before the one before the newest.
    published: Lsn,
    wall: Arc<dyn WallClock>,
    /// The last LSN whose changes the shadow holds.
    applied: Lsn,
    /// When the shadow first differed from the files since the last save.
    dirty_since: Option<Instant>,
    last_change: Option<Instant>,
    records_since_snapshot: u64,
    last_snapshot: Instant,
}

impl Keeper {
    fn run(mut self, inbox: &Receiver<KeeperMsg>) {
        loop {
            match inbox.recv_timeout(self.wait()) {
                Ok(KeeperMsg::Durable(batch)) => {
                    if let Err(reason) = self.take(batch) {
                        self.shared.die(reason);
                        return;
                    }
                }
                Ok(KeeperMsg::SaveNow(reply)) => drop(reply.send(self.save())),
                Ok(KeeperMsg::Take(trigger, name, reply)) => {
                    drop(reply.send(self.snapshot_named(trigger, name)));
                }
                Ok(KeeperMsg::Close(history, reply)) => {
                    let closed = self.save().and_then(|()| self.snapshot(Trigger::Close, self.applied, &history));
                    drop(reply.send(closed));
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Ok(KeeperMsg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            }
            if self.save_due() {
                // A failure is on the status and is retried at the next due time.
                drop(self.save());
            }
            self.ask_for_mark();
        }
    }

    /// How long to wait for the next message: until a save is due, or the snapshot tick.
    fn wait(&self) -> Duration {
        let options = &self.shared.options;
        let (Some(since), Some(last)) = (self.dirty_since, self.last_change) else { return KEEPER_TICK };
        let due = (last + options.save_idle).min(since + options.save_max);
        due.saturating_duration_since(Instant::now()).min(KEEPER_TICK)
    }

    fn save_due(&self) -> bool {
        let options = &self.shared.options;
        let (Some(since), Some(last)) = (self.dirty_since, self.last_change) else { return false };
        let now = Instant::now();
        now >= last + options.save_idle || now >= since + options.save_max
    }

    /// Applies a durable batch to the shadow and writes the snapshots its marks ask for.
    fn take(&mut self, batch: Vec<Item>) -> Result<(), String> {
        for item in batch {
            let Item::Entry(lsn, entry) = item else { continue };
            if let HistoryEntry::Mark { history } = entry {
                self.snapshot(Trigger::Auto, lsn, &history)?;
                continue;
            }
            match &entry {
                HistoryEntry::Begin { label, .. } => label.clone_into(&mut self.label),
                HistoryEntry::Op { record, commits: true } => record.op.clone_into(&mut self.label),
                HistoryEntry::JobStarted { job, op, .. } => {
                    self.last_job = self.last_job.max(job.0);
                    self.running.insert(job.0, op.clone());
                }
                HistoryEntry::JobEnded { job, .. } => drop(self.running.remove(&job.0)),
                _ => {}
            }
            if let Some(patch) = patch_of(&entry) {
                self.shadow.apply(patch).map_err(|e| {
                    format!("the saved copy of the project disagrees with the open one at record {}: {e}", lsn.0)
                })?;
                let now = Instant::now();
                self.dirty_since.get_or_insert(now);
                self.last_change = Some(now);
            }
            self.records_since_snapshot += 1;
            self.applied = lsn;
        }
        Ok(())
    }

    /// Saves the files if anything changed, and records the result on the status.
    fn save(&mut self) -> Result<(), String> {
        let saved = if self.shadow.is_dirty() { self.shadow.save().map(drop) } else { Ok(()) };
        let mut progress = lock(&self.shared.progress);
        match saved {
            Ok(()) => {
                progress.save_error = None;
                progress.saved = self.applied;
                self.dirty_since = None;
                self.last_change = None;
                Ok(())
            }
            Err(e) => {
                let reason = e.to_string();
                progress.save_error = Some(reason.clone());
                // Retry later, not at once: the file may be open in an editor.
                self.dirty_since = Some(Instant::now());
                Err(reason)
            }
        }
    }

    fn snapshot(&mut self, trigger: Trigger, lsn: Lsn, history: &HistoryExport) -> Result<SnapshotId, String> {
        let facts = Facts::new(lsn, &self.session, trigger, self.wall.now()).with_label(self.label.clone());
        self.publish(facts, history).map(|info| info.id)
    }

    /// A snapshot asked for through the service: a named version, a take's state, a manual one. It covers what the
    /// shadow holds, which is everything durable.
    fn snapshot_named(&mut self, trigger: Trigger, name: Option<String>) -> Result<SnapshotInfo, String> {
        let mut facts =
            Facts::new(self.applied, &self.session, trigger, self.wall.now()).with_label(self.label.clone());
        match (trigger, name) {
            (Trigger::Take, Some(stem)) => facts = facts.with_stem(stem),
            (_, Some(name)) => facts = facts.with_name(name),
            (_, None) => {}
        }
        // The executor's undo history isn't the keeper's to know between marks; a version restores as one new step.
        self.publish(facts, &HistoryExport::empty())
    }

    fn publish(&mut self, facts: Facts, history: &HistoryExport) -> Result<SnapshotInfo, String> {
        let running = self.running.iter().map(|(job, op)| RunningJob::new(*job, op)).collect();
        let facts = facts.with_last_job(self.last_job).with_running_jobs(running);
        let lsn = facts.lsn;
        let taken = self
            .snapshots
            .take_rendered(&self.shadow, facts, history)
            .map_err(|e| format!("a snapshot can't be written: {e}"))?;
        for warning in &taken.warnings {
            tracing::warn!(%warning, "snapshot");
        }
        self.records_since_snapshot = 0;
        self.last_snapshot = Instant::now();
        // Compaction keeps one snapshot of slack: what lies wholly before the previous snapshot moves to the archive.
        let previous = std::mem::replace(&mut self.published, lsn);
        if let Err(error) = archive_before(self.shadow.dir(), previous) {
            tracing::warn!(%error, "the journal couldn't be compacted; it is tried again at the next snapshot");
        }
        Ok(taken.info)
    }

    /// Raises `wants_mark` when a snapshot is due by the cadence.
    fn ask_for_mark(&self) {
        let options = &self.shared.options;
        let due = self.records_since_snapshot >= options.snapshot_records
            || (self.records_since_snapshot > 0 && self.last_snapshot.elapsed() >= options.snapshot_every);
        if due {
            self.shared.mark_wanted.store(true, Ordering::Relaxed);
        }
    }
}

/// The document change an entry carries, if it carries one.
pub(crate) fn patch_of(entry: &HistoryEntry) -> Option<&Patch> {
    match entry {
        HistoryEntry::Op { record, .. } => Some(&record.patch),
        HistoryEntry::Rollback { patch, .. }
        | HistoryEntry::Undo { patch, .. }
        | HistoryEntry::Redo { patch, .. }
        | HistoryEntry::TxRevert { patch, .. } => Some(patch),
        _ => None,
    }
}

/// The archive of sealed segments (the op log) of the project in `project_dir`.
pub fn archive_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".omuzaki").join(ARCHIVE_DIR)
}

/// Moves into the archive the sealed segments of the project in `project_dir` whose records all have an LSN at or
/// before `before` (journal.md 7.3). The newest segment is never moved: it is the one being written. Returns how many
/// moved.
///
/// # Errors
/// [`JournalError::Io`] if a folder can't be listed or a segment can't be moved; what moved before stays moved.
pub fn archive_before(project_dir: &Path, before: Lsn) -> Result<usize, JournalError> {
    let live = journal_dir(project_dir);
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |source| JournalError::Io { path, source }
    };
    let mut segments: Vec<(u64, PathBuf)> = match fs::read_dir(&live) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|e| Some((segment_seq(e.file_name().to_str()?)?, e.path())))
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(JournalError::Io { path: live, source }),
    };
    segments.sort();
    // The first LSN of each segment, from its header; a prepared or unreadable one ends the run (nothing after it is
    // known to be sealed).
    let mut firsts = Vec::with_capacity(segments.len());
    for (_, path) in &segments {
        let mut header = [0u8; super::HEADER_LEN];
        let read = File::open(path).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut header));
        match read.ok().and_then(|()| SegmentHeader::decode(&header).ok()).filter(|h| !h.is_prepared()) {
            Some(h) => firsts.push(h.first),
            None => break,
        }
    }
    let archive = archive_dir(project_dir);
    let mut moved = 0;
    for (i, (_, path)) in segments.iter().enumerate().take(firsts.len().saturating_sub(1)) {
        // A segment holds the records from its first LSN up to the next one's first, exclusive.
        if firsts[i + 1].0.saturating_sub(1) > before.0 {
            break;
        }
        fs::create_dir_all(&archive).map_err(io(&archive))?;
        let Some(name) = path.file_name() else { continue };
        fs::rename(path, archive.join(name)).map_err(io(path))?;
        moved += 1;
    }
    if moved > 0 {
        sync_dir(&archive).map_err(io(&archive))?;
        sync_dir(&live).map_err(io(&live))?;
    }
    Ok(moved)
}

/// Makes the renames in `dir` durable. Windows has no folder flush; a rename there is durable with the file system's
/// own journal.
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    if cfg!(unix) { File::open(dir)?.sync_all() } else { Ok(()) }
}

/// The live journal folder of the project in `project_dir`.
pub fn journal_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".omuzaki").join(JOURNAL_DIR)
}

/// One past the highest segment sequence number in `dir` (1 for an empty or missing folder).
fn next_segment_seq(dir: &Path) -> Result<u64, JournalError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(1),
        Err(source) => return Err(JournalError::Io { path: dir.to_path_buf(), source }),
    };
    let last = entries.filter_map(Result::ok).filter_map(|e| segment_seq(e.file_name().to_str()?)).max();
    Ok(last.map_or(1, |seq| seq + 1))
}

fn spawn(name: &str, dir: &Path, run: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, JournalError> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(run)
        .map_err(|source| JournalError::Io { path: dir.to_path_buf(), source })
}

/// The machine's name for the session record, or empty when the environment doesn't say.
fn host_name() -> String {
    std::env::var("HOSTNAME").or_else(|_| std::env::var("COMPUTERNAME")).unwrap_or_default()
}

/// A wall clock reading the system time.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemWall;

impl WallClock for SystemWall {
    fn now(&self) -> WallTime {
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        WallTime(u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
    }
}
