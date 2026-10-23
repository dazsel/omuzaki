//! The journal writer through its file seam (#255, journal.md 5.2 and 4.1): which flush each batch gets, that `sync`
//! never returns on a barrier alone, that a commit never creates a file, and how a failing or slow disk is reported.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::Seconds;
use omuzaki_doc::Project;
use omuzaki_doc::journal::{
    Flush, JournalFile, JournalFs, JournalOptions, ProjectJournal, Reader, Record, SessionStart, StdFs, SystemWall,
    journal_dir, reopen,
};
use omuzaki_doc::snapshot::Snapshotter;
use omuzaki_history_api::{HistoryEntry, JournalSink, JournalStatus, Lsn, SyncError};
use omuzaki_ops_api::{Actor, TxId};
use omuzaki_store_mock::MemStore;

const WAIT: Duration = Duration::from_secs(10);

/// What the writer did, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Create { file: usize, path: PathBuf, thread: String },
    Open { file: usize },
    Write { file: usize, len: usize },
    Flush { file: usize, how: Flush },
    SyncDir,
}

/// A real file system that records every call, can fail writes, can hold flushes, and can behave like Apple's (a
/// barrier that isn't durable).
struct TestFs {
    events: Mutex<Vec<Ev>>,
    heard: Mutex<Sender<Ev>>,
    files: AtomicUsize,
    fail_writes: AtomicBool,
    barrier_is_durable: bool,
    /// While true, flushes of the session's segments wait.
    hold: Mutex<bool>,
    released: Condvar,
}

impl TestFs {
    fn new(barrier_is_durable: bool) -> (Arc<Self>, Receiver<Ev>) {
        let (heard, events) = channel();
        let fs = Self {
            events: Mutex::new(Vec::new()),
            heard: Mutex::new(heard),
            files: AtomicUsize::new(0),
            fail_writes: AtomicBool::new(false),
            barrier_is_durable,
            hold: Mutex::new(false),
            released: Condvar::new(),
        };
        (Arc::new(fs), events)
    }

    fn record(&self, ev: Ev) {
        self.events.lock().unwrap().push(ev.clone());
        drop(self.heard.lock().unwrap().send(ev));
    }

    fn events(&self) -> Vec<Ev> {
        self.events.lock().unwrap().clone()
    }

    fn hold_flushes(&self, hold: bool) {
        *self.hold.lock().unwrap() = hold;
        self.released.notify_all();
    }

    fn wrap(self: &Arc<Self>, inner: Box<dyn JournalFile>) -> (usize, Box<dyn JournalFile>) {
        let file = self.files.fetch_add(1, Ordering::SeqCst);
        (file, Box::new(TestFile { fs: Arc::clone(self), inner, file }))
    }
}

/// The handle the writer gets: the trait methods take `&self`, so the shared state lives behind an `Arc`.
struct Fs(Arc<TestFs>);

impl JournalFs for Fs {
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        StdFs.create_dir_all(dir)
    }

    fn create_new(&self, path: &Path) -> io::Result<Box<dyn JournalFile>> {
        let (file, wrapped) = self.0.wrap(StdFs.create_new(path)?);
        let thread = std::thread::current().name().unwrap_or("").to_owned();
        self.0.record(Ev::Create { file, path: path.to_path_buf(), thread });
        Ok(wrapped)
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn JournalFile>> {
        let (file, wrapped) = self.0.wrap(StdFs.open_write(path)?);
        self.0.record(Ev::Open { file });
        Ok(wrapped)
    }

    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.0.record(Ev::SyncDir);
        StdFs.sync_dir(dir)
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<String>> {
        StdFs.list(dir)
    }

    fn read_start(&self, path: &Path, len: usize) -> io::Result<Vec<u8>> {
        StdFs.read_start(path, len)
    }

    fn barrier_is_durable(&self) -> bool {
        self.0.barrier_is_durable
    }
}

struct TestFile {
    fs: Arc<TestFs>,
    inner: Box<dyn JournalFile>,
    file: usize,
}

impl JournalFile for TestFile {
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if self.fs.fail_writes.load(Ordering::SeqCst) {
            return Err(io::Error::other("the disk is full"));
        }
        self.fs.record(Ev::Write { file: self.file, len: bytes.len() });
        self.inner.write_at(offset, bytes)
    }

    fn flush(&mut self, how: Flush) -> io::Result<()> {
        let mut hold = self.fs.hold.lock().unwrap();
        while *hold {
            hold = self.fs.released.wait(hold).unwrap();
        }
        drop(hold);
        self.fs.record(Ev::Flush { file: self.file, how });
        self.inner.flush(how)
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
}

fn snapshots(dir: &Path) -> Arc<Snapshotter> {
    static STORES: Mutex<BTreeMap<PathBuf, Arc<MemStore>>> = Mutex::new(BTreeMap::new());
    let store = STORES.lock().unwrap().entry(dir.to_path_buf()).or_default().clone();
    Arc::new(Snapshotter::new(dir, store))
}

fn start(dir: &Path, fs: &Arc<TestFs>, options: JournalOptions) -> Arc<ProjectJournal> {
    let reopened = reopen(dir, &snapshots(dir)).unwrap();
    let start =
        SessionStart::new("test", "0.0.0", reopened.last_lsn, reopened.history, Arc::new(SystemWall), snapshots(dir))
            .with_fs(Arc::new(Fs(Arc::clone(fs))));
    ProjectJournal::start(reopened.project, start, options).unwrap()
}

/// A record with `label` as its payload: no document change, so the keeper has nothing to save.
fn begin(n: u64, label: &str) -> HistoryEntry {
    HistoryEntry::Begin {
        tx: TxId(n),
        actor: Actor::Person { name: "pat".into() },
        label: label.into(),
        at: Seconds(0.0),
    }
}

/// The session segment: the file the test thread created or reopened at start.
fn live_file(fs: &TestFs) -> usize {
    fs.events()
        .iter()
        .find_map(|ev| match ev {
            Ev::Create { file, thread, .. } if !thread.starts_with("omuzaki-") => Some(*file),
            Ev::Open { file } => Some(*file),
            _ => None,
        })
        .unwrap()
}

/// Forgets what was heard so far (the session start's own writes and flushes).
fn drain(events: &Receiver<Ev>) {
    while events.try_recv().is_ok() {}
}

/// Waits until the writer flushes `file`, and returns how.
fn next_flush_of(events: &Receiver<Ev>, file: usize) -> Flush {
    loop {
        match events.recv_timeout(WAIT).unwrap() {
            Ev::Flush { file: f, how } if f == file => return how,
            _ => {}
        }
    }
}

fn flushes_of(fs: &TestFs, file: usize, after: usize) -> Vec<Flush> {
    fs.events()[after..]
        .iter()
        .filter_map(|ev| match ev {
            Ev::Flush { file: f, how } if *f == file => Some(*how),
            _ => None,
        })
        .collect()
}

#[test]
fn an_interactive_batch_gets_a_barrier_and_sync_waits_for_a_full_flush() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let (fs, events) = TestFs::new(false); // Apple: a barrier isn't durable
    let journal = start(tmp.path(), &fs, JournalOptions::default());
    let live = live_file(&fs);
    drain(&events);
    journal.append(begin(1, "an edit in the panel"));
    assert_eq!(next_flush_of(&events, live), Flush::Barrier, "nobody external waits on it");
    let seen = fs.events().len();

    journal.sync(WAIT).unwrap();
    assert_eq!(flushes_of(&fs, live, seen), [Flush::Full], "sync never returns on a barrier alone");
    let after: Vec<Ev> = fs.events()[seen..].to_vec();
    assert!(!after.iter().any(|ev| matches!(ev, Ev::Write { file, .. } if *file == live)), "a flush with no new data");
    drop(journal);
}

#[test]
fn concurrent_sync_waiters_share_one_full_flush() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let (fs, events) = TestFs::new(false);
    let journal = start(tmp.path(), &fs, JournalOptions::default());
    let live = live_file(&fs);
    drain(&events);
    journal.append(begin(1, "one"));
    assert_eq!(next_flush_of(&events, live), Flush::Barrier);
    let seen = fs.events().len();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| journal.sync(WAIT).unwrap());
        }
    });
    assert_eq!(flushes_of(&fs, live, seen), [Flush::Full], "one full flush releases every waiter it covers");
}

#[test]
fn where_a_barrier_is_durable_sync_needs_no_second_flush() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let (fs, events) = TestFs::new(true); // Linux, Windows
    let journal = start(tmp.path(), &fs, JournalOptions::default());
    let live = live_file(&fs);
    drain(&events);
    journal.append(begin(1, "one"));
    assert_eq!(next_flush_of(&events, live), Flush::Barrier);
    let seen = fs.events().len();
    assert_eq!(journal.sync(WAIT).unwrap(), Lsn(2));
    assert_eq!(flushes_of(&fs, live, seen), []);
}

#[test]
fn a_commit_never_creates_a_file_and_every_record_reads_back_across_rotations() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let (fs, events) = TestFs::new(true);
    // Every batch below is bigger than a segment, so every one rotates.
    let journal = start(tmp.path(), &fs, JournalOptions::default().with_segment_bytes(256));
    let prepared = |fs: &TestFs| {
        fs.events()
            .iter()
            .filter(|ev| matches!(ev, Ev::Create { thread, .. } if thread == "omuzaki-journal-prep"))
            .count()
    };
    let mut labels = Vec::new();
    for n in 1..=8u64 {
        // The preparer has made the segment this batch rotates into.
        while (prepared(&fs) as u64) < n {
            events.recv_timeout(WAIT).unwrap();
        }
        let label = format!("{n}: {}", "x".repeat(300));
        journal.append(begin(n, &label));
        journal.sync(WAIT).unwrap();
        labels.push(label);
    }
    drop(journal);

    let creates: Vec<String> = fs
        .events()
        .iter()
        .filter_map(|ev| match ev {
            Ev::Create { thread, .. } => Some(thread.clone()),
            _ => None,
        })
        .collect();
    assert!(
        creates.iter().skip(1).all(|t| t == "omuzaki-journal-prep"),
        "only the preparer creates files: {creates:?}"
    );
    assert!(!creates.contains(&"omuzaki-journal".to_owned()));

    let scan = Reader::scan(&journal_dir(tmp.path()), Lsn(0)).unwrap();
    let read: Vec<String> = scan
        .records
        .iter()
        .filter_map(|(_, record)| match record {
            Record::Entry(HistoryEntry::Begin { label, .. }) => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(read, labels, "what was appended comes out of the reader exactly");
}

#[test]
fn a_prepared_segment_left_by_a_session_is_the_next_session_s_first() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let (fs, events) = TestFs::new(true);
    let journal = start(tmp.path(), &fs, JournalOptions::default());
    while !fs.events().iter().any(|ev| matches!(ev, Ev::Create { thread, .. } if thread == "omuzaki-journal-prep")) {
        events.recv_timeout(WAIT).unwrap();
    }
    journal.append(begin(1, "first session"));
    journal.sync(WAIT).unwrap();
    drop(journal); // a crash, with segment 2 prepared and unused

    let (fs, _events) = TestFs::new(true);
    let journal = start(tmp.path(), &fs, JournalOptions::default());
    assert!(matches!(fs.events()[0], Ev::Open { .. }), "the prepared segment is reopened: {:?}", fs.events());
    journal.append(begin(2, "second session"));
    journal.sync(WAIT).unwrap();
    drop(journal);
    let scan = Reader::scan(&journal_dir(tmp.path()), Lsn(0)).unwrap();
    let labels: Vec<&str> = scan
        .records
        .iter()
        .filter_map(|(_, record)| match record {
            Record::Entry(HistoryEntry::Begin { label, .. }) => Some(label.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(labels, ["first session", "second session"], "no prepared segment sits between two used ones");
}

#[test]
fn a_failing_disk_is_reported_at_once_and_saving_resumes_when_it_heals() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let (fs, _events) = TestFs::new(true);
    let journal = start(tmp.path(), &fs, JournalOptions::default());
    fs.fail_writes.store(true, Ordering::SeqCst);
    journal.append(begin(1, "one"));
    let failed = journal.sync(WAIT).unwrap_err();
    assert!(matches!(&failed, SyncError::Failing { reason } if reason.contains("the disk is full")), "{failed:?}");
    assert!(matches!(journal.status(), JournalStatus::Failing { .. }), "{:?}", journal.status());

    // While it retries, `sync` says so at once rather than wait out its timeout; the retry lands within the backoff.
    fs.fail_writes.store(false, Ordering::SeqCst);
    let deadline = Instant::now() + WAIT;
    let synced = loop {
        match journal.sync(WAIT) {
            Ok(lsn) => break lsn,
            Err(SyncError::Failing { .. }) if Instant::now() < deadline => std::thread::yield_now(),
            Err(error) => panic!("{error:?}"),
        }
    };
    assert_eq!(synced, Lsn(2));
    assert_eq!(journal.status(), JournalStatus::Healthy);
    journal.admit().unwrap();
}

#[test]
fn a_slow_disk_is_behind_at_the_soft_limit_and_refuses_edits_at_the_hard_one() {
    let tmp = tempfile::tempdir().unwrap();
    Project::create(tmp.path(), "demo").unwrap();
    let (fs, events) = TestFs::new(true);
    let journal = start(tmp.path(), &fs, JournalOptions::default().with_limits(3, 6));
    let live = live_file(&fs);
    fs.hold_flushes(true);
    journal.append(begin(1, "one"));
    // The writer is inside the flush of the first batch: it wrote it, and the flush waits.
    loop {
        if let Ev::Write { file, .. } = events.recv_timeout(WAIT).unwrap()
            && file == live
        {
            break;
        }
    }
    for n in 2..=4 {
        journal.append(begin(n, "more"));
    }
    assert_eq!(journal.status(), JournalStatus::Behind { pending: 4 });
    journal.admit().unwrap();
    for n in 5..=6 {
        journal.append(begin(n, "more"));
    }
    assert!(journal.admit().is_err(), "the hard limit refuses new edits");

    fs.hold_flushes(false);
    assert_eq!(journal.sync(WAIT).unwrap(), Lsn(7));
    assert_eq!(journal.status(), JournalStatus::Healthy);
    journal.admit().unwrap();
}
