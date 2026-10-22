//! Garbage collection on disk (ADR 0021, journal §7.3): listing the objects, and removing one without ever leaving
//! an object that has no data.
//!
//! Removal is two steps. First the object's directory is renamed to a `.tmp-gc-*` name in its shard: one rename, so
//! the object is gone from the store at once, and the shard is synced so that stays true after a power cut. Then the
//! renamed directory is deleted. A crash between the two leaves retired bytes, which the next sweep deletes
//! ([`StoreGc::purge_retired`]); repair leaves them alone. Deleting the object in place instead could leave a
//! directory with its provenance and no data, which the licence gate can't tell from a damaged asset.
//!
//! # Locks
//! Two advisory locks sit in the store root, and `verify` and `repair` ignore both (they only look under `objects/`).
//! `gc.lock` is held shared by every `put` from its existence check to its end, and exclusive by a removal across its
//! check and retirement. `marker.lock` is held exclusive, briefly and with no stops inside, while a commit marker is
//! published. The order is always `gc.lock` then `marker.lock`; a sweep takes only `gc.lock`, so there is no cycle.
//!
//! # When an object counts as committed
//! A `put` publishes the object, makes the publication durable, then replaces `committed-at` inside it atomically with
//! the commit time (see [`DiskStore::mark_committed`]), never moving it to an older trusted one. Grace counts from that
//! time, so a write or a stall that takes
//! longer than grace can't get the object swept the moment it lands. The process that wrote an object also remembers
//! when `put` finished and protects it for the full grace; another process relies on the record, which is redone if
//! writing it was slow. An object without a usable marker is still being committed, or was stored before markers
//! existed: it counts as committed only [`UNMARKED_TIMEOUT`] after its directory last changed, which reclaims a crash
//! between publication and the marker.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omuzaki_store_api::{ContentHash, Removal, StoreError, StoreGc};

use crate::atomic::sync_dir;
use crate::{DiskStore, Paths, io_error};

/// The file that records when an object was committed (see [`DiskStore::mark_committed`]).
pub(crate) const COMMITTED_FILE: &str = "committed-at";

/// How long an object without a usable commit marker counts as still being committed, measured from its directory's
/// last change. A crash between publication and the marker leaves exactly that; a store from before markers existed
/// falls under the same rule.
pub(crate) const UNMARKED_TIMEOUT: Duration = Duration::from_hours(24);

/// Every retired object's directory starts with this, so [`StoreGc::purge_retired`] can find them.
const RETIRED_PREFIX: &str = ".tmp-gc-";

/// Makes the names of retired objects unique within this process; the process id makes them unique across processes.
static NEXT_RETIRED: AtomicU64 = AtomicU64::new(0);

/// The default failpoint: every step of a commit goes ahead. It returns a `Result` only because tests inject errors
/// through the same field type.
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn no_failpoint(_stage: &'static str) -> io::Result<()> {
    Ok(())
}

/// Whether `path` is a retired-object directory left by GC. Repair must not quarantine these.
pub(crate) fn is_retired(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(RETIRED_PREFIX))
}

/// The advisory lock file in the store root. `put` holds it shared and a removal holds it exclusive, so a removal
/// can't interleave with a `put` of the same object, in this process or another.
pub(crate) const LOCK_FILE: &str = "gc.lock";

/// The advisory lock file taken while a commit marker is published, so the marker only moves forward. Taken after
/// [`LOCK_FILE`], never before.
pub(crate) const MARKER_LOCK_FILE: &str = "marker.lock";

/// How a [`DiskStore::lock_store`] lock is held.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LockMode {
    /// Many at once: every `put`.
    Shared,
    /// Alone: the check-and-retire step of a removal.
    Exclusive,
}

impl LockMode {
    /// The failpoint reached just before the lock is taken.
    fn acquire_step(self) -> &'static str {
        match self {
            Self::Shared => "lock_shared",
            Self::Exclusive => "lock_exclusive",
        }
    }

    /// The failpoint reached just after the lock is dropped.
    fn release_step(self) -> &'static str {
        match self {
            Self::Shared => "unlock_shared",
            Self::Exclusive => "unlock_exclusive",
        }
    }
}

/// The store lock, held until dropped. Dropping tells the failpoint, so tests that model the lock can follow it.
#[derive(Debug)]
pub(crate) struct StoreLock<'a> {
    _file: fs::File,
    store: &'a DiskStore,
    mode: LockMode,
}

impl Drop for StoreLock<'_> {
    fn drop(&mut self) {
        // A failpoint can't stop a release, and outside tests it does nothing.
        let _ = (self.store.failpoint)(self.mode.release_step());
    }
}

impl DiskStore {
    /// What the marker on disk says: the recorded commit time, or [`unmarked_time`] if there is no usable record.
    pub(crate) fn marker_time(hash: &ContentHash, paths: &Paths) -> Result<SystemTime, StoreError> {
        let marker = paths.dir.join(COMMITTED_FILE);
        match fs::read(&marker) {
            Ok(raw) => match parse_commit_time(&raw) {
                Some(time) => Ok(time),
                // Empty, pending or torn: a crash or power cut while the marker was written. It proves nothing about
                // the commit.
                None => unmarked_time(hash, &paths.dir),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => unmarked_time(hash, &paths.dir),
            Err(e) => Err(io_error("read", &marker)(e)),
        }
    }

    /// What [`StoreGc::remove_if_older`] would do with the object, without doing it: [`Removal::Removed`] means it
    /// would remove it. Takes the same lock, exclusive, so the answer is as good as the removal's.
    #[cfg(test)]
    pub(crate) fn decide(&self, hash: &ContentHash, cutoff: SystemTime) -> Result<Removal, StoreError> {
        let _exclusive = self.lock_store(LockMode::Exclusive)?;
        self.verdict(hash, cutoff)
    }

    /// The decision of a removal, for the caller that holds the exclusive store lock.
    fn verdict(&self, hash: &ContentHash, cutoff: SystemTime) -> Result<Removal, StoreError> {
        match self.stored_at(hash) {
            Ok(stored) if stored > cutoff => Ok(Removal::Fresh),
            Ok(_) => Ok(Removal::Removed),
            Err(StoreError::Missing(_)) => Ok(Removal::Absent),
            Err(e) => Err(e),
        }
    }

    /// Takes the store lock, waiting for it; dropping the returned guard releases the lock.
    ///
    /// The lock file is opened afresh for every lock. These locks belong to the open file description, not to the
    /// thread, so one handle shared by several threads would let one thread's release drop another's lock.
    pub(crate) fn lock_store(&self, mode: LockMode) -> Result<StoreLock<'_>, StoreError> {
        (self.failpoint)(mode.acquire_step()).map_err(io_error("lock", &self.lock_path))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.lock_path)
            .map_err(io_error("open", &self.lock_path))?;
        match mode {
            LockMode::Shared => file.lock_shared(),
            LockMode::Exclusive => file.lock(),
        }
        .map_err(io_error("lock", &self.lock_path))?;
        Ok(StoreLock { _file: file, store: self, mode })
    }

    /// Whether a marker stamped at `marker` is older than the slack (a quarter of the grace) at `now`. The decision
    /// rests on the marker alone, not on [`StoreGc::stored_at`]: that also counts this process's memory, which says
    /// nothing about what another process would read from disk.
    fn is_stale(&self, marker: SystemTime, now: SystemTime) -> bool {
        now.duration_since(marker).is_ok_and(|age| age > self.stamp_slack)
    }
}

/// The marker is a fixed-width record: this many zero-padded decimal digits of nanoseconds since the epoch (a `u128`
/// has at most 39), then a newline. Anything shorter is torn: a decimal prefix would otherwise parse as a valid,
/// much older time and make a fresh object look decades old.
const MARKER_DIGITS: usize = 39;

/// The default for how long a commit may run past its stamp before the stamp is redone: a quarter of a one-second
/// grace. See [`DiskStore::with_gc_grace`] for shorter graces.
pub(crate) const DEFAULT_STAMP_SLACK: Duration = Duration::from_millis(250);

/// The slack that gives other processes at least `0.75 × grace`: a quarter of it. A grace too short for the disk to
/// write a record within its slack (zero, say) never lets a stamp settle, so the record stays pending and the object
/// stays protected: safe, if slow to collect.
pub(crate) fn stamp_slack_for(grace: Duration) -> Duration {
    grace / 4
}

/// A record that was written but not trusted: the digits then this byte. It is what is left when the commit never
/// settled, and it counts as no marker, so the object stays protected.
const PENDING_END: u8 = b';';

/// How many times the commit time is stamped before giving up on it being close to the commit.
const STAMP_ATTEMPTS: usize = 3;

/// How many passes a `put` makes to leave a marker within the slack of the time it finishes.
const PUT_PASSES: usize = 3;

/// Encodes a commit time as the marker's fixed-width record, not trusted.
fn pending_record(nanos: u128) -> String {
    format!("{nanos:0MARKER_DIGITS$}{}", char::from(PENDING_END))
}

/// Encodes a commit time as the marker's fixed-width record.
fn commit_record(nanos: u128) -> String {
    format!("{nanos:0MARKER_DIGITS$}\n")
}

/// Makes the names of marker temp files unique within this process.
static NEXT_MARKER: AtomicU64 = AtomicU64::new(0);

/// When objects committed by this process finished committing. The marker on disk is stamped before the commit ends,
/// so within the process this is the exact answer; the marker is for other processes and for restarts.
///
/// The lock covers memory only: a commit takes it briefly, before and after its I/O, never during it.
#[derive(Debug, Clone)]
pub(crate) struct Completed(Arc<Mutex<CompletedMap>>);

/// What [`Completed`] remembers about one object, shared by every `put` of it in this process.
#[derive(Debug, Default)]
struct Commits {
    /// How many `put`s of the object are running. While any is, the object is protected without limit.
    in_flight: u32,
    /// The latest time a `put` of the object finished successfully.
    latest: Option<SystemTime>,
}

/// What [`Completed`] remembers.
#[derive(Debug)]
pub(crate) struct CompletedMap {
    /// One entry per object put here, by object directory.
    entries: HashMap<PathBuf, Commits>,
    /// The latest finish time among finished commits forgotten to keep `entries` small. An object with no entry may be
    /// one of them, so it is protected as if it finished then: conservative, and it ends once grace has passed.
    forgotten_until: Option<SystemTime>,
    /// How many entries are held before the older finished half is forgotten.
    limit: usize,
}

/// The most entries held by default (see [`CompletedMap::limit`]).
const COMPLETED_LIMIT: usize = 4096;

/// What this process knows about when an object's commit finished.
#[derive(Debug, PartialEq, Eq)]
enum Finish {
    /// Nothing: the object was not committed here, or long enough ago that nothing is remembered.
    Unknown,
    /// A `put` of it is still running.
    InFlight,
    /// The latest `put` finished at this time, or this is the latest a forgotten commit finished.
    At(SystemTime),
}

impl Default for Completed {
    fn default() -> Self {
        Self::with_limit(COMPLETED_LIMIT)
    }
}

impl Completed {
    /// A map that forgets its older finished half once it holds `limit` entries.
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self(Arc::new(Mutex::new(CompletedMap { entries: HashMap::new(), forgotten_until: None, limit })))
    }

    /// Locks the map. Memory only, so it is never held across I/O.
    pub(crate) fn lock(&self) -> MutexGuard<'_, CompletedMap> {
        // Every critical section leaves the map consistent, so a poisoned lock is still usable.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl CompletedMap {
    /// Notes that a `put` of the object at `dir` has started. At the limit the older finished half is forgotten
    /// first, so memory stays bounded however fast objects are put; objects with a `put` running are never forgotten,
    /// and there are only as many of those as there are concurrent `put`s.
    pub(crate) fn reserve(&mut self, dir: &Path) {
        if self.entries.len() >= self.limit && !self.entries.contains_key(dir) {
            self.forget_older_half();
        }
        self.entries.entry(dir.to_path_buf()).or_default().in_flight += 1;
    }

    /// Notes that a `put` of the object at `dir` ended at `now`. A success keeps the latest of the finish times, so
    /// overlapping `put`s end in any order with the same result; a failure only stops being in flight, and never
    /// touches what another `put` recorded.
    pub(crate) fn complete(&mut self, dir: &Path, now: SystemTime, succeeded: bool) {
        let Some(commits) = self.entries.get_mut(dir) else { return };
        commits.in_flight = commits.in_flight.saturating_sub(1);
        if succeeded {
            commits.latest = Some(commits.latest.map_or(now, |latest| latest.max(now)));
        }
        if commits.in_flight == 0 && commits.latest.is_none() {
            self.entries.remove(dir);
        }
    }

    fn forget_older_half(&mut self) {
        let mut finished: Vec<(SystemTime, PathBuf)> = self
            .entries
            .iter()
            .filter(|(_, commits)| commits.in_flight == 0)
            .filter_map(|(dir, commits)| Some((commits.latest?, dir.clone())))
            .collect();
        finished.sort_unstable();
        for (at, dir) in finished.iter().take(finished.len() / 2) {
            self.entries.remove(dir);
            self.forgotten_until = Some(self.forgotten_until.map_or(*at, |seen| seen.max(*at)));
        }
    }

    /// What is known about when the latest `put` of the object at `dir` finished.
    fn finish(&self, dir: &Path) -> Finish {
        match self.entries.get(dir) {
            Some(commits) if commits.in_flight > 0 => Finish::InFlight,
            Some(Commits { latest: Some(at), .. }) => Finish::At(*at),
            _ => self.forgotten_until.map_or(Finish::Unknown, Finish::At),
        }
    }

    /// Drops the entry of a removed object, unless a `put` of it is running.
    fn forget(&mut self, dir: &Path) {
        if self.entries.get(dir).is_some_and(|commits| commits.in_flight == 0) {
            self.entries.remove(dir);
        }
    }
}

impl DiskStore {
    /// Writes the commit marker of the object at `paths`. The caller (`finish_put`) keeps the in-memory side.
    ///
    /// The record is written to a temp file and renamed over the marker, and nothing slow happens after the rename: no
    /// fsync (see below). A record is therefore never visible to another process until it is published, and it is
    /// published only after the clock, read again, shows the write took no longer than the slack (a quarter of the
    /// grace, see [`DiskStore::with_gc_grace`]). If it took longer the unpublished temp file is discarded and the stamp
    /// redone, up to a few times; if it never settles a pending record is published, which counts as no marker and
    /// keeps the object protected. So no process ever reads an expired stamp that a retry later replaces, and another
    /// process collects a fresh object no sooner than `0.75 × grace` after `put` returns (apart from the scheduler
    /// stalling this thread between the last clock read and the rename).
    ///
    /// The record is never fsynced. A power cut can leave the marker missing, empty, torn, or rolled back to an older
    /// record. Missing, empty and torn count as no marker. A rollback is harmless: every caller that could hold an
    /// uncommitted reference to the object is gone, and `put` made the data durable before it returned, so an object
    /// that no durable state references is garbage and collecting it is correct.
    pub(crate) fn mark_committed(&self, paths: &Paths) -> Result<(), StoreError> {
        (self.failpoint)("after_publish").map_err(io_error("commit", &paths.dir))?;
        let marker = paths.dir.join(COMMITTED_FILE);
        (self.failpoint)("before_stamp").map_err(io_error("commit", &marker))?;
        let mut stamped = (self.clock)();
        for _ in 0..STAMP_ATTEMPTS {
            let nanos = stamped.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos());
            if self.publish_marker(paths, commit_record(nanos).as_bytes(), stamped)? {
                return Ok(());
            }
            stamped = (self.clock)();
        }
        // Never quick: the stamp can't be trusted to be near the commit, so publish a record that says so.
        self.publish_pending(paths)
    }

    /// Publishes a record that is not to be trusted, which keeps the object protected as if unmarked. Its stamp is a
    /// clock read under the marker lock, just before the rename, and it replaces the marker only if that is later than
    /// the stamp already there, as every record does.
    fn publish_pending(&self, paths: &Paths) -> Result<(), StoreError> {
        let marker = paths.dir.join(COMMITTED_FILE);
        let _publishing = self.lock_marker()?;
        let ours = (self.clock)();
        if Self::recorded_stamp(&marker)?.is_some_and(|(on_disk, _)| on_disk >= ours) {
            return Ok(());
        }
        let nanos = ours.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos());
        let seq = NEXT_MARKER.fetch_add(1, Ordering::Relaxed);
        let temp = paths.dir.join(format!(".{COMMITTED_FILE}-{}-{seq}", std::process::id()));
        let written = fs::write(&temp, pending_record(nanos)).map_err(io_error("write", &temp));
        let renamed = written.and_then(|()| fs::rename(&temp, &marker).map_err(io_error("write", &marker)));
        if renamed.is_err() {
            let _ = fs::remove_file(&temp);
        }
        renamed
    }

    /// Ends a `put` that found its object in the store (`published` says whether it put it there): the object gets
    /// the commit time of this `put`, on disk for other processes and in memory for this one. Called with the store
    /// lock held shared, so no removal can take the object meanwhile.
    ///
    /// Each pass notes the `put` as running, writes the marker if it is new or older than the slack (a quarter of the
    /// grace; a hot dedup writes nothing), reads the marker's stamp, then, under the map lock, reads the clock, notes
    /// the `put` as finished at that time and judges the stamp against it. Every successful `put` finishes, so
    /// overlapping `put`s keep the latest finish time. If a stall in the marker I/O, in the read or in waiting for the
    /// lock aged the stamp past the slack, another pass follows; if it is still stale after a few, a pending record
    /// keeps other processes from collecting the object.
    pub(crate) fn finish_put(&self, hash: &ContentHash, paths: &Paths, published: bool) -> Result<(), StoreError> {
        let mut write_marker = published || self.is_stale(Self::marker_time(hash, paths)?, (self.clock)());
        for _ in 0..PUT_PASSES {
            self.completed.lock().reserve(&paths.dir);
            let observed = self.write_and_read(paths, write_marker);
            // No trusted stamp (the marker is missing or pending) is as good as a stale one: another writer's older
            // stamp may land next, so this put stamps for itself.
            if self.settle(paths, observed, true)? {
                return Ok(());
            }
            write_marker = true;
        }
        // Still stale after every pass: a pending record keeps other processes from collecting the object, but only
        // while it is the record on disk and was read lately, so it is judged the same way, against the finish time.
        for _ in 0..PUT_PASSES {
            self.completed.lock().reserve(&paths.dir);
            let observed = self.publish_pending(paths).and_then(|()| self.read_marker(paths));
            if self.settle(paths, observed, false)? {
                return Ok(());
            }
        }
        Err(StoreError::Io(format!("{}: could not establish a commit time for the object", paths.dir.display())))
    }

    /// Ends a pass of [`DiskStore::finish_put`] given what it observed on disk (`Err` if the pass failed): under the
    /// map lock, reads the clock, notes the `put` as finished at that time and judges the observed stamp against it,
    /// with no I/O. A wait for the lock thus counts as time the put took, and a stamp it left stale is caught, not
    /// returned. Says whether the put can return; `trusted_only` requires the stamp to be a trusted one.
    fn settle(
        &self,
        paths: &Paths,
        observed: Result<Option<(SystemTime, bool)>, StoreError>,
        trusted_only: bool,
    ) -> Result<bool, StoreError> {
        let mut completed = self.completed.lock();
        let now = (self.clock)();
        completed.complete(&paths.dir, now, observed.is_ok());
        Ok(observed?.is_some_and(|(stamp, trusted)| (trusted || !trusted_only) && !self.is_stale(stamp, now)))
    }

    /// One pass of [`DiskStore::finish_put`]: writes the marker if `write_marker`, then reads what it holds.
    fn write_and_read(&self, paths: &Paths, write_marker: bool) -> Result<Option<(SystemTime, bool)>, StoreError> {
        if write_marker {
            self.mark_committed(paths)?;
        }
        (self.failpoint)("before_final_check").map_err(io_error("commit", &paths.dir))?;
        self.read_marker(paths)
    }

    /// The stamp the marker holds and whether it is trusted, if it holds a complete record.
    fn read_marker(&self, paths: &Paths) -> Result<Option<(SystemTime, bool)>, StoreError> {
        let marker = Self::recorded_stamp(&paths.dir.join(COMMITTED_FILE))?;
        (self.failpoint)("before_finish_lock").map_err(io_error("commit", &paths.dir))?;
        Ok(marker)
    }

    /// Writes `record`, a trusted stamp read at `stamped`, to a temp file and renames it over the marker, unless the
    /// write took longer than the slack since then, in which case the temp file is discarded and `false` returned.
    fn publish_marker(&self, paths: &Paths, record: &[u8], stamped: SystemTime) -> Result<bool, StoreError> {
        let seq = NEXT_MARKER.fetch_add(1, Ordering::Relaxed);
        let temp = paths.dir.join(format!(".{COMMITTED_FILE}-{}-{seq}", std::process::id()));
        let result = self.write_marker(&temp, paths, record, stamped);
        if !matches!(result, Ok(true)) {
            // Best effort: a leftover temp file is harmless, and must not mask the error that got us here.
            let _ = fs::remove_file(&temp);
        }
        result
    }

    fn write_marker(&self, temp: &Path, paths: &Paths, record: &[u8], stamped: SystemTime) -> Result<bool, StoreError> {
        (self.failpoint)("before_stamp_write").map_err(io_error("commit", temp))?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(temp).map_err(io_error("write", temp))?;
        file.write_all(record).map_err(io_error("write", temp))?;
        (self.failpoint)("before_marker_fsync").map_err(io_error("commit", temp))?;
        (self.failpoint)("before_confirm").map_err(io_error("commit", temp))?;
        // A clock that went backwards counts as a quick write.
        let slow = (self.clock)().duration_since(stamped).is_ok_and(|took| took > self.stamp_slack);
        if slow {
            return Ok(false);
        }
        let marker = paths.dir.join(COMMITTED_FILE);
        let _publishing = self.lock_marker()?;
        // One order for every record, trusted or pending: the marker only moves to a strictly later stamp (or from none
        // at all, or a torn one). An equal or newer stamp already published stays, and ours is dropped.
        if Self::recorded_stamp(&marker)?.is_some_and(|(on_disk, _)| on_disk >= stamped) {
            let _ = fs::remove_file(temp);
            return Ok(true);
        }
        fs::rename(temp, &marker).map_err(io_error("write", &marker))?;
        Ok(true)
    }

    /// Takes the marker lock, waiting for it; dropping the returned file releases it. Opened afresh each time, for the
    /// reason given at [`DiskStore::lock_store`].
    fn lock_marker(&self) -> Result<fs::File, StoreError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.marker_lock_path)
            .map_err(io_error("open", &self.marker_lock_path))?;
        file.lock().map_err(io_error("lock", &self.marker_lock_path))?;
        Ok(file)
    }

    /// The stamp a marker holds, and whether it is trusted, if it holds a complete record (not missing, empty or
    /// torn). A pending record's stamp counts for ordering, so an older trusted stamp can't replace it either.
    fn recorded_stamp(marker: &Path) -> Result<Option<(SystemTime, bool)>, StoreError> {
        match fs::read(marker) {
            Ok(raw) => Ok(parse_record(&raw)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error("read", marker)(e)),
        }
    }
}

/// Parses a marker written by [`DiskStore::mark_committed`]; `None` if it isn't a complete, trusted record (empty,
/// torn or pending).
fn parse_commit_time(raw: &[u8]) -> Option<SystemTime> {
    parse_record(raw).filter(|(_, trusted)| *trusted).map(|(time, _)| time)
}

/// Parses any complete record, trusted (`\n`) or pending (`;`), into its stamp and whether it is trusted.
fn parse_record(raw: &[u8]) -> Option<(SystemTime, bool)> {
    let (digits, trusted) = match raw.split_last()? {
        (b'\n', digits) => (digits, true),
        (&PENDING_END, digits) => (digits, false),
        _ => return None,
    };
    if digits.len() != MARKER_DIGITS || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let nanos: u128 = std::str::from_utf8(digits).ok()?.parse().ok()?;
    let secs = u64::try_from(nanos / 1_000_000_000).ok()?;
    let subsec = u32::try_from(nanos % 1_000_000_000).ok()?;
    Some((UNIX_EPOCH.checked_add(Duration::new(secs, subsec))?, trusted))
}

impl StoreGc for DiskStore {
    /// Every directory under `objects/` whose name is a hash. Staging directories and strays are not objects, so
    /// they are never listed.
    fn hashes(&self) -> Result<Vec<ContentHash>, StoreError> {
        let mut hashes = Vec::new();
        for (shard, shard_dir) in subdirectories(&self.objects)? {
            // Only canonical shards hold objects; `abc/<61 digits>` would otherwise parse as a hash that isn't stored.
            if !is_shard_name(&shard) {
                continue;
            }
            for (rest, _) in subdirectories(&shard_dir)? {
                // The shard and the rest of the name together are the hash, so a misnamed directory just fails to
                // parse.
                if let Ok(hash) = format!("{shard}{rest}").parse() {
                    hashes.push(hash);
                }
            }
        }
        hashes.sort_unstable();
        Ok(hashes)
    }

    /// When the object counts as committed: its recorded commit time. Without a usable marker it is
    /// [`unmarked_time`].
    fn stored_at(&self, hash: &ContentHash) -> Result<SystemTime, StoreError> {
        let paths = self.paths(hash);
        let durable = Self::marker_time(hash, &paths)?;
        // A commit finished by this process is known exactly, and protects the object for the full grace.
        let finish = self.completed.lock().finish(&paths.dir);
        match finish {
            Finish::Unknown => Ok(durable),
            Finish::At(done) => Ok(done.max(durable)),
            // Still being committed here: not old, whatever the record says.
            Finish::InFlight => {
                Ok((self.clock)().checked_add(UNMARKED_TIMEOUT).map_or(durable, |until| until.max(durable)))
            }
        }
    }

    /// Re-reads the commit time under the exclusive store lock, so a `put` of the same object (which holds the lock
    /// shared from its existence check to its confirm) either finished first, making the object [`Removal::Fresh`], or
    /// starts after the retirement and stores it again.
    fn remove_if_older(&self, hash: &ContentHash, cutoff: SystemTime) -> Result<Removal, StoreError> {
        let paths = self.paths(hash);
        (self.failpoint)("before_remove").map_err(io_error("remove", &paths.dir))?;
        let retired = {
            let _exclusive = self.lock_store(LockMode::Exclusive)?;
            match self.verdict(hash, cutoff)? {
                Removal::Removed => {}
                other => return Ok(other),
            }
            // Renaming and syncing the shard are the only I/O under the lock; the deletion comes after it.
            let Some(retired) = retire_object(&paths.dir, &paths.shard)? else {
                return Ok(Removal::Absent);
            };
            // Still under the lock: once it is released a `put` may recreate the object and record its completion,
            // and forgetting then would erase the new object's protection.
            self.completed.lock().forget(&paths.dir);
            (self.failpoint)("after_retire").map_err(io_error("remove", &paths.dir))?;
            retired
        };
        match fs::remove_dir_all(&retired) {
            Ok(()) => {}
            // A concurrent `purge_retired` already deleted it: the cleanup is done, which is what was wanted.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error("remove", &retired)(e)),
        }
        sync_dir(&paths.shard).map_err(io_error("sync", &paths.shard))?;
        Ok(Removal::Removed)
    }

    /// Deletes every retired-object directory under the canonical shards of `objects/`. Only GC creates those names,
    /// so unlike a `put`'s `.tmp-*` staging they are never quarantined: their contents were already judged garbage.
    fn purge_retired(&self) -> Result<usize, StoreError> {
        let mut purged = 0;
        for (shard, shard_dir) in subdirectories(&self.objects)? {
            // A stray directory under `objects/` may belong to someone else, so its contents are never ours to delete.
            if !is_shard_name(&shard) {
                continue;
            }
            for (name, path) in subdirectories(&shard_dir)? {
                if !name.starts_with(RETIRED_PREFIX) {
                    continue;
                }
                // The rename that retired the object may not be durable yet (a removal in another thread or process
                // has not synced the shard, or crashed before it did), and deleting the contents first would let a
                // power cut bring the original directory back without its data: sync the shard before deleting.
                (self.failpoint)("purge_sync").map_err(io_error("purge", &path))?;
                sync_dir(&shard_dir).map_err(io_error("sync", &shard_dir))?;
                (self.failpoint)("purge_delete").map_err(io_error("purge", &path))?;
                match fs::remove_dir_all(&path) {
                    Ok(()) => {}
                    // Another removal finished it between our listing and now.
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(io_error("purge", &path)(e)),
                }
                sync_dir(&shard_dir).map_err(io_error("sync", &shard_dir))?;
                purged += 1;
            }
        }
        Ok(purged)
    }
}

/// When an object without a usable marker counts as committed: its directory's last change plus [`UNMARKED_TIMEOUT`].
fn unmarked_time(hash: &ContentHash, dir: &Path) -> Result<SystemTime, StoreError> {
    let meta = fs::metadata(dir).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => StoreError::Missing(*hash),
        _ => io_error("inspect", dir)(e),
    })?;
    let changed = meta.modified().map_err(io_error("inspect", dir))?;
    changed
        .checked_add(UNMARKED_TIMEOUT)
        .ok_or_else(|| StoreError::Io(format!("{}: its modification time is out of range", dir.display())))
}

/// True for the two lowercase hex digits that name a canonical shard directory.
fn is_shard_name(name: &str) -> bool {
    name.len() == 2 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The subdirectories of `dir` as (name, path). Names that aren't UTF-8 get replacement characters and so match no
/// hash; they are skipped by the caller.
fn subdirectories(dir: &Path) -> Result<Vec<(String, PathBuf)>, StoreError> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(io_error("list", dir))? {
        let entry = entry.map_err(io_error("list", dir))?;
        if entry.file_type().map_err(io_error("inspect", &entry.path()))?.is_dir() {
            found.push((entry.file_name().to_string_lossy().into_owned(), entry.path()));
        }
    }
    Ok(found)
}

/// Step one of a removal: renames the object's directory to a fresh retired name in `shard`, then syncs the shard.
/// Returns the retired path, or `None` if the object wasn't there. This is the point where the object disappears
/// from the store; after it, only the deletion of the retired directory is left.
fn retire_object(dir: &Path, shard: &Path) -> Result<Option<PathBuf>, StoreError> {
    loop {
        let seq = NEXT_RETIRED.fetch_add(1, Ordering::Relaxed);
        let retired = shard.join(format!("{RETIRED_PREFIX}{}-{seq}", std::process::id()));
        // Renaming onto an existing empty directory succeeds on Unix, so never rename onto a name that is taken.
        if retired.exists() {
            continue;
        }
        return match fs::rename(dir, &retired) {
            Ok(()) => {
                sync_dir(shard).map_err(io_error("sync", shard))?;
                Ok(Some(retired))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error("remove", dir)(e)),
        };
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::ffi::OsStr;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use omuzaki_store_api::{Commercial, ContentHash, Removal, Store, StoreError, StoreGc};
    use omuzaki_store_mock::conformance::{self, payload, sample_provenance};
    use omuzaki_store_mock::explore;

    use crate::atomic::write_file;
    use crate::tests::open;
    use crate::{DATA_FILE, DiskStore, Problem};

    use super::{
        COMMITTED_FILE, Completed, Finish, MARKER_DIGITS, UNMARKED_TIMEOUT, commit_record, pending_record,
        retire_object,
    };

    /// The grace period every test sweeps with.
    const GRACE: Duration = Duration::from_hours(1);

    /// A step to pause at, with the channels to say so and to be told to go on.
    type Pause = (&'static str, std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);

    /// What to run, and at which step of the store's work.
    type StageHook = (&'static str, Box<dyn Fn()>);

    thread_local! {
        /// The fake clock. It starts at the real time, so the directories' own timestamps agree with it.
        static NOW: Cell<SystemTime> = Cell::new(SystemTime::now());
        /// A stall: the fake clock moves forward by the duration when the named failpoint is reached.
        static STALL: Cell<Option<(&'static str, Duration)>> = const { Cell::new(None) };
        /// A crash: the commit fails when the named failpoint is reached, as if the process had stopped there.
        /// Run at the `before_confirm` step of every commit attempt, as another process looking at the store then.
        static PROBE: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
        /// Told at the `after_publish` step of a commit on this thread.
        static SIGNAL: RefCell<Option<std::sync::mpsc::Sender<()>>> = const { RefCell::new(None) };
        /// Run once when the named step is reached, as another process acting at that moment.
        static HOOK: RefCell<Option<StageHook>> = const { RefCell::new(None) };
        /// This thread's handle to the interleaving explorer, if it is one of the explored actors.
        static STEPPER: RefCell<Option<explore::Stepper>> = const { RefCell::new(None) };
        /// Pauses this thread when the named step is reached: tells the sender, then waits for the receiver.
        static PAUSE: RefCell<Option<Pause>> = const { RefCell::new(None) };
        /// Like `STALL`, but the clock moves on every time the step is reached, not once.
        static STALL_EVERY: Cell<Option<(&'static str, Duration)>> = const { Cell::new(None) };
        /// Whether the failpoint records the steps it sees in `REACHED`.
        static HOOK_LOG: Cell<bool> = const { Cell::new(false) };
        static CRASH_AT: Cell<Option<&'static str>> = const { Cell::new(None) };
    }

    fn now() -> SystemTime {
        NOW.with(Cell::get)
    }

    fn advance(by: Duration) {
        NOW.with(|clock| clock.set(clock.get() + by));
    }

    fn failpoint(stage: &'static str) -> io::Result<()> {
        if HOOK_LOG.with(Cell::get) {
            REACHED.with(|reached| reached.borrow_mut().push(stage));
        }
        // Under the interleaving explorer, the steps that matter park the actor, and the lock is modelled.
        if let Some(stepper) = STEPPER.with(|stepper| stepper.borrow().clone()) {
            match stage {
                "lock_shared" => stepper.acquire(explore::LockMode::Shared),
                "lock_exclusive" => stepper.acquire(explore::LockMode::Exclusive),
                "unlock_shared" => stepper.release(explore::LockMode::Shared),
                "unlock_exclusive" => stepper.release(explore::LockMode::Exclusive),
                "before_publish" | "before_remove" => stepper.stop(),
                // One of these two per scenario: stopping at both multiplies the schedules past what can be run (the
                // retries after a stale stamp add a pair of stops each). Before the rename, a writer's older stamp
                // can be overtaken; between reading the marker and finishing, a wait can age the stamp it read.
                "before_confirm" if !EXPLORE_FINISH_STOP.load(std::sync::atomic::Ordering::SeqCst) => stepper.stop(),
                "before_finish_lock" if EXPLORE_FINISH_STOP.load(std::sync::atomic::Ordering::SeqCst) => {
                    stepper.stop();
                }
                _ => {}
            }
        }
        // Taken out first: the hook may itself reach this step, and must run once.
        let hook = HOOK.with(|hook| {
            let mut hook = hook.borrow_mut();
            if hook.as_ref().is_some_and(|(at, _)| *at == stage) { hook.take() } else { None }
        });
        if let Some((_, hook)) = hook {
            hook();
        }
        let pause = PAUSE.with(|pause| {
            let mut pause = pause.borrow_mut();
            if pause.as_ref().is_some_and(|(at, ..)| *at == stage) { pause.take() } else { None }
        });
        if let Some((_, reached, resume)) = pause {
            reached.send(()).unwrap();
            resume.recv().unwrap();
        }
        if stage == "before_publish" {
            SIGNAL.with(|signal| {
                if let Some(signal) = signal.borrow().as_ref() {
                    let _ = signal.send(());
                }
            });
        }
        // What another process would see at this step of the commit.
        if stage == "before_confirm" {
            PROBE.with(|probe| {
                if let Some(probe) = probe.borrow().as_ref() {
                    probe();
                }
            });
        }
        if let Some((_, by)) = STALL_EVERY.with(Cell::get).filter(|(at, _)| *at == stage) {
            advance(by);
        }
        // A stall happens once, so a retry of the same step is not stalled again.
        if let Some((_, by)) = STALL.with(Cell::get).filter(|(at, _)| *at == stage) {
            STALL.with(|stall| stall.set(None));
            advance(by);
        }
        if CRASH_AT.with(Cell::get) == Some(stage) {
            return Err(io::Error::other(format!("injected crash at {stage}")));
        }
        Ok(())
    }

    /// A store on the fake clock, with the failpoints wired in. The clock restarts at the real time, so a test may
    /// build several stores in turn.
    fn fake_store() -> (tempfile::TempDir, DiskStore) {
        NOW.with(|clock| clock.set(SystemTime::now()));
        let (dir, mut store) = open();
        store.clock = now;
        store.failpoint = failpoint;
        (dir, store)
    }

    /// Writes like the real writer, then moves the fake clock on by two grace periods, as a slow write would.
    fn slow_data_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
        write_file(path, bytes)?;
        if path.file_name() == Some(OsStr::new(DATA_FILE)) {
            advance(GRACE * 2);
        }
        Ok(())
    }

    /// The clock of the interleaving test: shared by every actor, which a thread-local one could not be.
    static EXPLORE_NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn explore_now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_nanos(EXPLORE_NOW.load(std::sync::atomic::Ordering::SeqCst))
    }

    /// How many steps of the explored schedule have moved the clock on (see `explore_tick`).
    static EXPLORE_TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// Whether the explorer stops each `put` between reading the marker and finishing, instead of before it renames
    /// the marker into place.
    static EXPLORE_FINISH_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// The most steps of one schedule that move the clock on.
    const EXPLORE_MAX_TICKS: u64 = 12;

    /// In the timed scenarios, every step of a schedule moves the clock on by an eighth of the grace, up to twelve
    /// times: less than the slack, so a schedule sees time pass in small steps and crosses every threshold.
    fn explore_tick() {
        use std::sync::atomic::Ordering;
        if EXPLORE_TICKS.fetch_add(1, Ordering::SeqCst) < EXPLORE_MAX_TICKS {
            explore_advance(GRACE / 8);
        }
    }

    fn explore_advance(by: Duration) {
        let by = u64::try_from(by.as_nanos()).unwrap();
        EXPLORE_NOW.fetch_add(by, std::sync::atomic::Ordering::SeqCst);
    }

    /// Which of the explored actors are one process (sharing a completion map).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Sharing {
        /// Three processes.
        Separate,
        /// The sweeper and the first writer are one process.
        SweeperWithFirstWriter,
        /// All three are one process.
        AllOne,
    }

    impl Sharing {
        /// How long after a `put` returned the sweeper must leave the object, for writer number `writer`: a full
        /// grace in the sweeper's own process, three quarters of one in another.
        fn owed(self, writer: usize) -> u128 {
            let same_process = match self {
                Self::Separate => false,
                Self::SweeperWithFirstWriter => writer == 0,
                Self::AllOne => true,
            };
            if same_process { GRACE.as_nanos() } else { GRACE.as_nanos() * 3 / 4 }
        }
    }

    /// Eventually collectable: long after every put (the caller has moved the clock past even the protection of an
    /// unmarked object), nothing is left marked as running in the actors' own stores, each of them would remove the
    /// now unreferenced object (their completion maps hold nothing that still protects it), and sweeping them in turn
    /// removes it once. A fresh store, which remembers nothing, agrees.
    fn assert_eventually_collected(schedule: &str, retained: &[DiskStore], fresh: &DiskStore, hash: &ContentHash) {
        for kept in retained {
            let running = kept.completed.lock().entries.values().filter(|commits| commits.in_flight > 0).count();
            assert_eq!(running, 0, "schedule {schedule}: a put is still marked as running");
        }
        let existed = retained[0].contains(hash);
        let cutoff = explore_now() - GRACE;
        for (which, kept) in retained.iter().enumerate() {
            let expected = if existed { Removal::Removed } else { Removal::Absent };
            assert_eq!(
                kept.decide(hash, cutoff).unwrap(),
                expected,
                "schedule {schedule}: store {which} would not collect"
            );
        }
        // The first writer's store sweeps first and removes it; the others find nothing left.
        let mut removed = Vec::new();
        for kept in retained {
            removed.push(kept.sweep(&|_| false, GRACE, explore_now()).unwrap().removed.contains(hash));
        }
        let expected = if existed { vec![true, false, false] } else { vec![false, false, false] };
        assert_eq!(removed, expected, "schedule {schedule}: removed once, by the first store to sweep");
        assert!(!fresh.contains(hash), "schedule {schedule}: a fresh store agrees");
    }

    /// Two writers put the same bytes while a sweeper sweeps, in every possible order of their steps (the commit
    /// steps, the lock, and the sweep's check and removal). Time moves either by a clock actor that jumps past the
    /// grace once, at any point, or (`timed`) by an eighth of the grace with each step, with the sweeper first waiting
    /// another three quarters of a grace.
    ///
    /// After every schedule: a writer whose `put` returned less than the grace it is owed (see [`Sharing::owed`])
    /// before the sweep's time has its object present and readable; and old unreferenced garbage is gone.
    fn disk_scenario(preexisting: bool, sharing: Sharing, timed: bool, finish_stop: bool) -> explore::Scenario {
        use std::sync::atomic::{AtomicU64, Ordering};
        let real_now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        EXPLORE_NOW.store(u64::try_from(real_now.as_nanos()).unwrap(), Ordering::SeqCst);
        EXPLORE_TICKS.store(0, Ordering::SeqCst);
        EXPLORE_FINISH_STOP.store(finish_stop, Ordering::SeqCst);
        let start_nanos = EXPLORE_NOW.load(Ordering::SeqCst);
        // Memory-backed where there is one: the exploration makes thousands of stores, and the fsyncs are not what
        // is tested.
        let dir = Arc::new(if Path::new("/dev/shm").is_dir() {
            tempfile::tempdir_in("/dev/shm").unwrap()
        } else {
            tempfile::tempdir().unwrap()
        });
        let open_store = |dir: &Path| {
            let mut store = DiskStore::open(dir).unwrap().with_gc_grace(GRACE);
            store.clock = explore_now;
            store.failpoint = failpoint;
            store
        };
        let bytes = payload("explore disk", 200);
        let hash = ContentHash::of(&bytes);
        let setup = open_store(dir.path());
        let garbage = setup.put(&payload("explore garbage", 100), sample_provenance(Commercial::Ok)).unwrap();
        if preexisting {
            setup.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        }
        drop(setup);
        explore_advance(GRACE * 4);

        let first = open_store(dir.path());
        let second = if sharing == Sharing::AllOne { first.clone() } else { open_store(dir.path()) };
        let sweeper = if sharing == Sharing::Separate { open_store(dir.path()) } else { first.clone() };
        let writers = [first, second];
        // Kept for the end: their completion maps are part of what is checked.
        let retained = [writers[0].clone(), writers[1].clone(), sweeper.clone()];
        let returned: Vec<Arc<AtomicU64>> = (0..2).map(|_| Arc::new(AtomicU64::new(u64::MAX))).collect();
        let sweep_at = Arc::new(AtomicU64::new(u64::MAX));
        let mut actors: Vec<explore::Actor> = Vec::new();
        for (writer, returned) in writers.into_iter().zip(&returned) {
            let (returned, bytes) = (Arc::clone(returned), bytes.clone());
            actors.push(Box::new(move |stepper| {
                STEPPER.with(|slot| *slot.borrow_mut() = Some(stepper));
                writer.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
                returned.store(EXPLORE_NOW.load(Ordering::SeqCst), Ordering::SeqCst);
            }));
        }
        {
            let sweep_at = Arc::clone(&sweep_at);
            actors.push(Box::new(move |stepper| {
                STEPPER.with(|slot| *slot.borrow_mut() = Some(stepper));
                if timed {
                    explore_advance(GRACE * 6 / 8);
                }
                let now = EXPLORE_NOW.load(Ordering::SeqCst);
                sweep_at.store(now, Ordering::SeqCst);
                sweeper.sweep(&|_| false, GRACE, SystemTime::UNIX_EPOCH + Duration::from_nanos(now)).unwrap();
            }));
        }
        if !timed {
            actors.push(Box::new(|_| explore_advance(GRACE * 2)));
        }

        let check = Box::new(move |schedule: &str| {
            if timed {
                // Each schedule gets its own clock steps: the setup, the sweeper's wait, and at least one step.
                let elapsed = EXPLORE_NOW.load(Ordering::SeqCst) - start_nanos;
                let least = u64::try_from((GRACE * 4 + GRACE * 6 / 8 + GRACE / 8).as_nanos()).unwrap();
                assert!(elapsed >= least, "schedule {schedule}: the clock did not step ({elapsed} ns)");
            }
            let store = open_store(dir.path());
            assert!(!store.contains(&garbage), "schedule {schedule}: old garbage must be collected");
            let sweep_at = sweep_at.load(Ordering::SeqCst);
            for (writer, returned) in returned.iter().enumerate() {
                let returned = returned.load(Ordering::SeqCst);
                assert_ne!(returned, u64::MAX, "schedule {schedule}: writer {writer} did not finish");
                let age = u128::from(sweep_at.saturating_sub(returned));
                if age < sharing.owed(writer) {
                    assert_eq!(
                        store.get(&hash).as_deref(),
                        Ok(bytes.as_slice()),
                        "schedule {schedule}: writer {writer}'s put returned {age} ns before the sweep"
                    );
                    assert!(store.provenance(&hash).is_ok(), "schedule {schedule}: provenance readable");
                }
            }
            assert!(store.verify(Duration::from_hours(48)).unwrap().is_clean(), "schedule {schedule}: store is clean");

            explore_advance(UNMARKED_TIMEOUT + GRACE);
            assert_eventually_collected(schedule, &retained, &open_store(dir.path()), &hash);
        });
        explore::Scenario { actors, check }
    }

    #[test]
    fn every_interleaving_of_two_puts_and_a_sweep_keeps_a_fresh_object() {
        use Sharing::{AllOne, Separate, SweeperWithFirstWriter};
        let mut total = 0;
        // (time moves in steps, stop between reading the marker and finishing): the clock jumps once, with a stop
        // before the rename; the clock steps, with the same stop; the clock steps, with the other stop.
        for (timed, finish_stop) in [(false, false), (true, false), (true, true)] {
            for sharing in [Separate, SweeperWithFirstWriter, AllOne] {
                for preexisting in [false, true] {
                    // The untimed runs with a shared process repeat the separate-process schedules for a new object.
                    if !timed && sharing != Separate && !preexisting {
                        continue;
                    }
                    let explored = explore::explore_timed(
                        40_000,
                        &|| disk_scenario(preexisting, sharing, timed, finish_stop),
                        &|| {
                            if timed {
                                explore_tick();
                            }
                        },
                    );
                    assert!(explored > 100, "timed={timed} {sharing:?} preexisting={preexisting}: only {explored}");
                    println!(
                        "DiskStore timed={timed} fin={finish_stop} {sharing:?} old={preexisting}: {explored} schedules"
                    );
                    total += explored;
                }
            }
        }
        println!("DiskStore: {total} schedules explored in all");
    }

    #[test]
    fn passes_the_gc_conformance_check() {
        let (_dir, store) = open();
        conformance::remove_and_sweep(&store);
    }

    #[test]
    fn a_sweep_never_touches_staging_directories_or_strays() {
        let (dir, store) = open();
        let hash = store.put(&payload("gc stray", 100), sample_provenance(Commercial::Ok)).unwrap();
        let stray = dir.path().join("objects").join("zz");
        fs::create_dir_all(&stray).unwrap();
        let staging = store.paths(&hash).shard.join(".tmp-1-1");
        fs::create_dir_all(&staging).unwrap();

        assert_eq!(store.hashes().unwrap(), vec![hash], "only object directories are listed");
        let report = store.sweep(&|_| false, Duration::ZERO, SystemTime::now()).unwrap();
        assert_eq!(report.removed, vec![hash]);
        assert!(staging.is_dir(), "a sweep leaves staging directories to verify and repair");
        assert!(stray.is_dir(), "a sweep leaves strays alone");
    }

    #[test]
    fn hashes_ignores_directories_that_are_not_canonical_shards() {
        let (dir, store) = open();
        let hash = store.put(&payload("gc shards", 100), sample_provenance(Commercial::Ok)).unwrap();
        let hex = hash.to_string();
        // The same 64 digits split at the wrong place, and with an uppercase shard: neither is an object.
        for shard in [&hex[..3], &hex[..1], &hex[..2].to_uppercase()] {
            let rest = &hex[shard.len()..];
            fs::create_dir_all(dir.path().join("objects").join(shard).join(rest)).unwrap();
        }
        assert_eq!(store.hashes().unwrap(), vec![hash], "only the canonical split is an object");
    }

    #[test]
    fn purge_leaves_retired_names_under_a_stray_directory_alone() {
        let (dir, store) = open();
        let stray = dir.path().join("objects").join("zz").join(".tmp-gc-1-1");
        fs::create_dir_all(&stray).unwrap();
        assert_eq!(store.purge_retired().unwrap(), 0);
        assert!(stray.is_dir(), "only canonical shard directories are searched");
    }

    #[test]
    fn a_crash_between_retire_and_delete_leaves_only_a_stale_staging_directory() {
        let (_dir, store) = open();
        let hash = store.put(&payload("gc crash", 500), sample_provenance(Commercial::Ok)).unwrap();
        let paths = store.paths(&hash);
        // Stop right after the rename, as a crash would.
        let retired = retire_object(&paths.dir, &paths.shard).unwrap().unwrap();

        assert!(!store.contains(&hash), "the object is gone from the store at once");
        assert_eq!(store.get(&hash), Err(StoreError::Missing(hash)));
        let report = store.verify(Duration::ZERO).unwrap();
        assert_eq!(report.problems.len(), 1, "exactly one problem: {:?}", report.problems);
        assert!(matches!(&report.problems[0], Problem::StaleStaging { path, .. } if *path == retired));
    }

    #[test]
    fn a_crash_after_retirement_is_left_to_the_sweep_and_not_quarantined() {
        let (_dir, store) = open();
        let live = store.put(&payload("gc live", 300), sample_provenance(Commercial::Ok)).unwrap();
        let dropped = store.put(&payload("gc repair", 300), sample_provenance(Commercial::Ok)).unwrap();
        let paths = store.paths(&dropped);
        let retired = retire_object(&paths.dir, &paths.shard).unwrap().unwrap();

        let repaired = store.repair(Duration::ZERO).unwrap();
        assert!(repaired.quarantined.is_empty(), "retired objects are not quarantined");
        assert!(retired.is_dir(), "repair leaves them for the sweep");

        let report = store.sweep(&|hash| *hash == live, Duration::ZERO, SystemTime::now()).unwrap();
        assert_eq!(report.purged, 1, "the sweep reclaims the retired bytes");
        assert!(!retired.exists());
        assert!(store.contains(&live), "a live object survives");
        assert!(store.verify(Duration::ZERO).unwrap().is_clean());
    }

    #[test]
    fn a_resweep_after_a_crash_deletes_the_retired_bytes_and_keeps_live_objects() {
        let (dir, store) = open();
        let kept = store.put(&payload("gc live", 300), sample_provenance(Commercial::Ok)).unwrap();
        let dropped = store.put(&payload("gc retired", 300), sample_provenance(Commercial::Ok)).unwrap();
        let paths = store.paths(&dropped);
        // A crash right after retirement: the object is out of the store but its bytes are still on disk.
        let retired = retire_object(&paths.dir, &paths.shard).unwrap().unwrap();
        drop(store);

        // Reopen, as after a restart, and sweep again.
        let store = DiskStore::open(dir.path()).unwrap();
        let report = store.sweep(&|hash| *hash == kept, Duration::ZERO, SystemTime::now()).unwrap();
        assert_eq!(report.purged, 1, "the retired bytes are deleted by the sweep");
        assert!(!retired.exists(), "nothing of the retired object is left");
        assert!(report.removed.is_empty(), "the retired object was already out of the store");
        assert_eq!(report.kept, 1);
        assert!(store.contains(&kept), "a live object survives the resweep");
        assert_eq!(store.hashes().unwrap(), vec![kept]);
        assert!(store.verify(Duration::ZERO).unwrap().is_clean());
    }

    #[test]
    fn a_stall_before_publication_longer_than_grace_does_not_make_the_commit_collectable() {
        let (_dir, store) = fake_store();
        STALL.with(|stall| stall.set(Some(("before_publish", GRACE * 2))));
        let hash = store.put(&payload("gc stall", 500), sample_provenance(Commercial::Ok)).unwrap();
        STALL.with(|stall| stall.set(None));

        let report = store.sweep(&|_| false, GRACE, now()).unwrap();
        assert!(report.removed.is_empty(), "grace counts from the commit, not from the start of the write");
        assert!(store.contains(&hash));

        advance(GRACE + Duration::from_secs(1));
        let report = store.sweep(&|_| false, GRACE, now()).unwrap();
        assert_eq!(report.removed, vec![hash], "once grace has passed since the commit, it is collected");
    }

    #[test]
    fn a_slow_write_longer_than_grace_does_not_make_the_commit_collectable() {
        let (_dir, mut store) = fake_store();
        store.write = slow_data_write;
        let hash = store.put(&payload("gc slow write", 500), sample_provenance(Commercial::Ok)).unwrap();
        let report = store.sweep(&|_| false, GRACE, now()).unwrap();
        assert!(report.removed.is_empty(), "a write longer than grace is not swept straight after its commit");
        assert!(store.contains(&hash));
    }

    #[test]
    fn a_stall_in_the_durability_work_does_not_make_the_commit_collectable() {
        for stage in ["before_marker_fsync", "before_stamp", "before_stamp_write", "before_confirm"] {
            let (_dir, store) = fake_store();
            let slow_put = || {
                STALL.with(|stall| stall.set(Some((stage, GRACE * 2))));
                let hash = store.put(&payload(stage, 500), sample_provenance(Commercial::Ok)).unwrap();
                STALL.with(|stall| stall.set(None));
                hash
            };
            conformance::slow_commit_survives_sweep(&store, &slow_put, &now, GRACE);
        }
    }

    #[test]
    fn a_crash_after_publication_is_held_until_the_unmarked_timeout() {
        for stage in ["after_publish", "before_marker_fsync", "before_stamp", "before_stamp_write", "before_confirm"] {
            let (_dir, store) = fake_store();
            let bytes = payload(stage, 300);
            let hash = ContentHash::of(&bytes);
            CRASH_AT.with(|crash| crash.set(Some(stage)));
            assert!(store.put(&bytes, sample_provenance(Commercial::Ok)).is_err(), "{stage}: the commit was stopped");
            CRASH_AT.with(|crash| crash.set(None));
            assert!(store.contains(&hash), "{stage}: the object was published before the crash");

            // Well past grace, but no commit time was recorded, so it is still being committed.
            advance(GRACE * 2);
            let report = store.sweep(&|_| false, GRACE, now()).unwrap();
            assert!(report.removed.is_empty(), "{stage}: unmarked, so still being committed");
            advance(UNMARKED_TIMEOUT);
            let report = store.sweep(&|_| false, GRACE, now()).unwrap();
            assert_eq!(report.removed, vec![hash], "{stage}: reclaimed once the timeout has passed");
        }
    }

    #[test]
    fn a_power_cut_that_loses_the_unsynced_record_leaves_the_object_protected() {
        let (dir, store) = fake_store();
        let hash = store.put(&payload("gc power cut", 300), sample_provenance(Commercial::Ok)).unwrap();
        // The record is never synced, so after a power cut the marker is the empty file that was made durable.
        fs::write(store.paths(&hash).dir.join(COMMITTED_FILE), b"").unwrap();
        drop(store);

        let store = DiskStore::open(dir.path()).unwrap();
        let swept = store.sweep(&|_| false, GRACE, SystemTime::now() + GRACE * 2).unwrap();
        assert!(swept.removed.is_empty(), "no durable commit time, so it is held until the unmarked timeout");
    }

    #[test]
    fn a_stall_far_shorter_than_an_hour_still_counts_against_a_short_grace() {
        let grace = Duration::from_millis(100);
        for stage in ["before_stamp_write", "before_marker_fsync", "before_confirm"] {
            let (_dir, store) = fake_store();
            let store = store.with_gc_grace(grace);
            let slow_put = || {
                STALL.with(|stall| stall.set(Some((stage, Duration::from_millis(200)))));
                store.put(&payload(stage, 500), sample_provenance(Commercial::Ok)).unwrap()
            };
            conformance::slow_commit_survives_sweep(&store, &slow_put, &now, grace);
        }
    }

    #[test]
    fn another_process_does_not_collect_an_object_right_after_a_slow_put_returns() {
        let grace = Duration::from_millis(100);
        for stage in ["before_stamp_write", "before_marker_fsync", "before_confirm"] {
            let (dir, writer) = fake_store();
            let writer = writer.with_gc_grace(grace);
            STALL.with(|stall| stall.set(Some((stage, Duration::from_millis(200)))));
            let hash = writer.put(&payload(stage, 500), sample_provenance(Commercial::Ok)).unwrap();

            // A second store on the same directory shares no memory with the writer, as another process wouldn't.
            let mut other = DiskStore::open(dir.path()).unwrap();
            other.clock = now;
            let report = other.sweep(&|_| false, grace * 3 / 4, now()).unwrap();
            assert!(report.removed.is_empty(), "{stage}: not before 0.75 × grace after put returned");
            assert!(other.contains(&hash));
            let report = other.sweep(&|_| false, grace, now() + grace).unwrap();
            assert_eq!(report.removed, vec![hash], "{stage}: collectable once grace has passed");
        }
    }

    #[test]
    fn the_writing_process_protects_an_object_for_the_full_grace_whatever_the_record_says() {
        let (_dir, store) = fake_store();
        let hash = store.put(&payload("gc same process", 300), sample_provenance(Commercial::Ok)).unwrap();
        // The record on disk is long expired (and so would let another process collect the object).
        let old = (now() - GRACE * 5).duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
        fs::write(store.paths(&hash).dir.join(COMMITTED_FILE), commit_record(old)).unwrap();
        let report = store.sweep(&|_| false, GRACE, now()).unwrap();
        assert!(report.removed.is_empty(), "this process knows the commit just finished");
        assert_eq!(store.sweep(&|_| false, GRACE, now() + GRACE).unwrap().removed, vec![hash]);
    }

    /// Power-loss ruling: a power cut can roll the record back to an older confirmed one. After it every caller that
    /// could hold an uncommitted reference is gone and `put` had made the data durable, so an object that no durable
    /// state references is garbage, and collecting it after the reopen is correct.
    #[test]
    fn after_a_reopen_an_old_confirmed_record_makes_an_unreferenced_object_collectable() {
        let (dir, store) = fake_store();
        let hash = store.put(&payload("gc rolled back", 300), sample_provenance(Commercial::Ok)).unwrap();
        let old = (now() - GRACE * 5).duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
        fs::write(store.paths(&hash).dir.join(COMMITTED_FILE), commit_record(old)).unwrap();
        drop(store);

        let store = DiskStore::open(dir.path()).unwrap();
        assert_eq!(store.sweep(&|_| false, GRACE, now()).unwrap().removed, vec![hash]);
    }

    #[test]
    fn a_burst_of_puts_keeps_the_map_bounded_and_every_object_protected() {
        let (_dir, mut store) = fake_store();
        store.completed = Completed::with_limit(8);
        let hashes: Vec<ContentHash> = (0..40)
            .map(|i| store.put(&payload(&format!("gc burst {i}"), 100), sample_provenance(Commercial::Ok)).unwrap())
            .collect();
        // Every commit finished at the same instant, and every record on disk is long expired, so only this process's
        // memory protects the objects.
        let old = (now() - GRACE * 5).duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
        for hash in &hashes {
            fs::write(store.paths(hash).dir.join(COMMITTED_FILE), commit_record(old)).unwrap();
        }
        assert!(store.completed.lock().entries.len() <= 8, "bounded by count, not by age");

        let report = store.sweep(&|_| false, GRACE, now()).unwrap();
        assert!(report.removed.is_empty(), "objects whose entry was forgotten are still protected for the full grace");
        assert_eq!(report.kept, 40);
        let report = store.sweep(&|_| false, GRACE, now() + GRACE).unwrap();
        assert_eq!(report.removed.len(), 40, "all collectable once grace has passed");
    }

    #[test]
    fn mid_commit_the_object_has_an_in_flight_entry_and_the_map_lock_is_free() {
        let (dir, writer) = fake_store();
        let completed = writer.completed.clone();
        let observed = Rc::new(RefCell::new(None));
        let seen = Rc::clone(&observed);
        let store = writer.clone();
        let object_dir = Rc::new(RefCell::new(PathBuf::new()));
        let wanted = Rc::clone(&object_dir);
        PROBE.with(|probe| {
            *probe.borrow_mut() = Some(Box::new(move || {
                // At the last step before the marker becomes visible.
                let free = completed.0.try_lock().is_ok();
                let state = completed.lock().finish(&wanted.borrow());
                // A GC pass started from another thread at this point must finish, and must keep the object.
                let removed = std::thread::scope(|scope| {
                    scope
                        .spawn(|| store.sweep(&|_| false, GRACE, SystemTime::now() + GRACE * 2).unwrap().removed)
                        .join()
                });
                *seen.borrow_mut() = Some((free, state, removed.unwrap().len()));
            }));
        });
        let bytes = payload("gc in flight", 300);
        *object_dir.borrow_mut() = writer.paths(&ContentHash::of(&bytes)).dir;
        let hash = writer.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        PROBE.with(|probe| *probe.borrow_mut() = None);

        let (free, state, removed) = observed.borrow_mut().take().expect("the probe ran");
        assert!(free, "the map lock is not held while the commit does I/O");
        assert_eq!(state, Finish::InFlight, "the object is reserved as in flight");
        assert_eq!(removed, 0, "a sweep started mid-commit finishes without blocking and keeps the object");
        assert!(writer.contains(&hash));
        assert!(matches!(writer.completed.lock().finish(&writer.paths(&hash).dir), Finish::At(_)), "finished now");
        drop(dir);
    }

    #[test]
    fn a_busy_map_lock_delays_the_stamp_and_never_exposes_the_object() {
        let grace = Duration::from_millis(100);
        let (dir, writer) = fake_store();
        let writer = writer.with_gc_grace(grace);
        let mut other = DiskStore::open(dir.path()).unwrap();
        other.clock = now;
        let bytes = payload("gc busy lock", 300);
        let hash = ContentHash::of(&bytes);

        let (published_tx, published_rx) = std::sync::mpsc::channel();
        // Held for far longer than the grace, by this thread, while another thread puts.
        let guard = writer.completed.lock();
        std::thread::scope(|scope| {
            let put = scope.spawn(|| {
                SIGNAL.with(|signal| *signal.borrow_mut() = Some(published_tx));
                writer.put(&bytes, sample_provenance(Commercial::Ok))
            });
            // The put is about to publish; after that it waits for the busy map lock before any stamp.
            published_rx.recv().unwrap();
            advance(grace * 10);
            let report = other.sweep(&|_| false, grace, now()).unwrap();
            assert!(report.removed.is_empty(), "no marker yet, so a second store instance keeps the object");
            drop(guard);
            assert_eq!(put.join().unwrap().unwrap(), hash, "the put returns Ok once the lock is free");
        });
        assert_eq!(writer.get(&hash).unwrap(), bytes, "and the object reads back");
        assert!(other.contains(&hash));
    }

    #[test]
    fn no_process_sees_an_expired_stamp_that_a_retry_later_replaces() {
        let grace = Duration::from_millis(100);
        let (dir, writer) = fake_store();
        let writer = writer.with_gc_grace(grace);
        let seen = Rc::new(Cell::new(0_usize));
        let observed = Rc::clone(&seen);
        let root = dir.path().to_path_buf();
        PROBE.with(|probe| {
            *probe.borrow_mut() = Some(Box::new(move || {
                // Another process (no shared memory) sweeps with the grace right now, mid-commit.
                let mut other = DiskStore::open(&root).unwrap();
                other.clock = now;
                observed.set(observed.get() + other.sweep(&|_| false, grace, now()).unwrap().removed.len());
            }));
        });
        // The first attempt stalls for twice the grace after its stamp, so it is redone.
        STALL.with(|stall| stall.set(Some(("before_confirm", grace * 2))));
        let hash = writer.put(&payload("gc interleave", 400), sample_provenance(Commercial::Ok)).unwrap();
        PROBE.with(|probe| *probe.borrow_mut() = None);

        assert_eq!(seen.get(), 0, "nothing was collected while the commit was in flight");
        assert!(writer.contains(&hash));
        let mut other = DiskStore::open(dir.path()).unwrap();
        other.clock = now;
        assert!(
            other.sweep(&|_| false, grace * 3 / 4, now()).unwrap().removed.is_empty(),
            "not collected before 0.75 × grace"
        );
    }

    #[test]
    fn the_shortest_grace_is_still_protected() {
        let grace = Duration::from_millis(1);
        for stage in ["before_stamp_write", "before_confirm"] {
            let (dir, writer) = fake_store();
            let writer = writer.with_gc_grace(grace);
            STALL.with(|stall| stall.set(Some((stage, Duration::from_millis(1)))));
            let hash = writer.put(&payload(stage, 400), sample_provenance(Commercial::Ok)).unwrap();

            let mut other = DiskStore::open(dir.path()).unwrap();
            other.clock = now;
            let report = other.sweep(&|_| false, grace, now()).unwrap();
            assert!(report.removed.is_empty(), "{stage}: not collected right after put");
            assert!(other.contains(&hash));
        }
    }

    #[test]
    fn a_pending_record_that_survives_a_power_cut_is_not_trusted_after_a_reopen() {
        let (dir, store) = fake_store();
        let hash = store.put(&payload("gc pending", 300), sample_provenance(Commercial::Ok)).unwrap();
        // An early, slow attempt's record reached disk (old time, never confirmed) and the later writes were lost.
        let old = (now() - GRACE * 2).duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
        fs::write(store.paths(&hash).dir.join(COMMITTED_FILE), pending_record(old)).unwrap();
        drop(store);

        let store = DiskStore::open(dir.path()).unwrap();
        let swept = store.sweep(&|_| false, GRACE, now()).unwrap();
        assert!(swept.removed.is_empty(), "an unconfirmed record says nothing about when the commit finished");
    }

    #[test]
    fn a_record_that_never_settles_stays_pending_and_the_object_protected() {
        let (dir, mut store) = fake_store();
        // Every clock read jumps past the slack, as if each replacement stalled.
        store.clock = || {
            advance(Duration::from_secs(5));
            now()
        };
        // Nothing ever settles, so the put cannot show a fresh stamp and fails (see the next test), but what it
        // leaves behind is a pending record, which keeps the object from being collected.
        let bytes = payload("gc slow stamp", 300);
        assert!(store.put(&bytes, sample_provenance(Commercial::Ok)).is_err());
        let hash = ContentHash::of(&bytes);
        let record = fs::read(store.paths(&hash).dir.join(COMMITTED_FILE)).unwrap();
        assert_eq!(record.last(), Some(&b';'), "the marker says the stamp is not to be trusted");
        drop(store);

        let mut other = DiskStore::open(dir.path()).unwrap();
        other.clock = now;
        assert!(other.sweep(&|_| false, GRACE, now() + GRACE * 2).unwrap().removed.is_empty(), "held as unmarked");
    }

    #[test]
    fn an_unmarked_object_is_not_swept_even_with_zero_grace() {
        let (_dir, store) = fake_store();
        let hash = store.put(&payload("gc unmarked", 300), sample_provenance(Commercial::Ok)).unwrap();
        fs::remove_file(store.paths(&hash).dir.join(COMMITTED_FILE)).unwrap();
        // Its commit time is unknown, so it counts as committed in the future; "old enough" needs a past time.
        assert!(store.stored_at(&hash).unwrap() > now());
        let report = store.sweep(&|_| false, Duration::ZERO, now()).unwrap();
        assert!(report.removed.is_empty(), "zero grace must not collect an object still inside its protection");
        assert_eq!(report.kept, 1);
    }

    #[test]
    fn a_torn_marker_counts_as_no_marker() {
        let (_dir, store) = fake_store();
        let hash = store.put(&payload("gc torn", 300), sample_provenance(Commercial::Ok)).unwrap();
        let marker = store.paths(&hash).dir.join(COMMITTED_FILE);
        let record = fs::read_to_string(&marker).unwrap();
        assert_eq!(record.len(), MARKER_DIGITS + 1, "a complete record is fixed-width");

        // An empty marker, and every truncation of a real one, including ones that still parse as a number.
        let torn = [String::new(), record[..record.len() - 1].to_owned(), record[..record.len() - 2].to_owned()]
            .into_iter()
            .chain([record.trim_end().to_owned(), record[..1].to_owned()]);
        for contents in torn {
            fs::write(&marker, &contents).unwrap();
            assert!(
                store.stored_at(&hash).unwrap() > now(),
                "{contents:?}: a torn marker proves nothing, so the object stays protected"
            );
            advance(GRACE * 2);
            assert!(store.sweep(&|_| false, GRACE, now()).unwrap().removed.is_empty(), "{contents:?}: still held");
        }
        advance(UNMARKED_TIMEOUT);
        assert_eq!(store.sweep(&|_| false, GRACE, now()).unwrap().removed, vec![hash]);
    }

    /// Sets a folder's modification time. A folder opens as a file does on Unix; Windows opens one only with backup
    /// semantics, and for the one right (`FILE_WRITE_ATTRIBUTES`) that a time change needs.
    fn set_folder_modified(dir: &Path, when: SystemTime) {
        #[cfg(windows)]
        let handle = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            fs::OpenOptions::new().access_mode(FILE_WRITE_ATTRIBUTES).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(dir)
        };
        #[cfg(not(windows))]
        let handle = fs::File::open(dir);
        handle.unwrap().set_modified(when).unwrap();
    }

    #[test]
    fn a_legacy_object_without_a_marker_is_collected_after_the_timeout() {
        let (root, store) = fake_store();
        let hash = store.put(&payload("gc legacy", 500), sample_provenance(Commercial::Ok)).unwrap();
        // A store from before markers existed: the same object without its marker, in a directory last changed two
        // days ago. Removing the file also moves the directory's time, so set it back.
        let dir = store.paths(&hash).dir;
        fs::remove_file(dir.join(COMMITTED_FILE)).unwrap();
        set_folder_modified(&dir, SystemTime::now() - Duration::from_hours(48));
        // A legacy store is one opened afresh: this process has no memory of committing the object.
        let mut store = DiskStore::open(root.path()).unwrap();
        store.clock = now;

        let report = store.sweep(&|_| false, GRACE, now()).unwrap();
        assert_eq!(report.removed, vec![hash], "legacy objects stay collectable");
    }

    /// A second store on the same directory, standing in for another process: no shared memory.
    fn another_process(dir: &tempfile::TempDir) -> DiskStore {
        let mut other = DiskStore::open(dir.path()).unwrap();
        other.clock = now;
        other
    }

    #[test]
    fn a_collector_that_checked_the_age_before_another_removed_and_a_writer_recreated_it_keeps_the_object() {
        let (dir, collector) = fake_store();
        let bytes = payload("gc replaced", 300);
        let hash = collector.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        advance(GRACE * 2);
        let cutoff = now() - GRACE;
        assert!(collector.stored_at(&hash).unwrap() <= cutoff, "collector A's age check: old enough");

        // While A is paused: collector B removes the object, and a writer stores it again.
        assert_eq!(another_process(&dir).remove_if_older(&hash, cutoff).unwrap(), Removal::Removed);
        another_process(&dir).put(&bytes, sample_provenance(Commercial::Ok)).unwrap();

        assert_eq!(collector.remove_if_older(&hash, cutoff).unwrap(), Removal::Fresh, "A re-reads the commit time");
        assert_eq!(collector.get(&hash).unwrap(), bytes, "the new object survives and reads back");
    }

    #[test]
    fn a_dedup_put_between_a_sweeps_age_check_and_its_removal_keeps_the_object() {
        let (dir, sweeper) = fake_store();
        let bytes = payload("gc re-put", 300);
        let hash = sweeper.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        advance(GRACE * 2);
        let writer = another_process(&dir);
        let again = bytes.clone();
        // After the sweep's age check, before its removal: the writer re-puts the old object.
        HOOK.with(|hook| {
            *hook.borrow_mut() = Some((
                "before_remove",
                Box::new(move || {
                    writer.put(&again, sample_provenance(Commercial::Ok)).unwrap();
                }),
            ));
        });
        let report = sweeper.sweep(&|_| false, GRACE, now()).unwrap();
        assert!(report.removed.is_empty(), "the object was freshened, so the sweep leaves it");
        assert_eq!(report.kept, 1);
        assert_eq!(sweeper.get(&hash).unwrap(), bytes);
    }

    #[test]
    fn a_dedup_put_refreshes_an_expired_marker_even_after_the_eviction_watermark_advanced() {
        let (dir, mut store) = fake_store();
        store.completed = Completed::with_limit(4);
        let bytes = payload("gc watermark", 300);
        let hash = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        advance(GRACE * 2);
        // A burst of other objects pushes the map past its limit, so the watermark moves to just now.
        for i in 0..10 {
            store.put(&payload(&format!("gc burst {i}"), 100), sample_provenance(Commercial::Ok)).unwrap();
        }
        assert!(store.completed.lock().forgotten_until.is_some_and(|until| until >= now()), "watermark advanced");

        store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        // Another process reads only the disk: the old object must have a fresh marker there.
        let report = another_process(&dir).sweep(&|hash_| *hash_ != hash, GRACE, now()).unwrap();
        assert!(report.removed.is_empty(), "the re-put refreshed the marker on disk");
        assert!(store.contains(&hash));
    }

    #[test]
    fn the_loser_of_a_publish_race_still_freshens_the_object_it_found() {
        let (dir, loser) = fake_store();
        let bytes = payload("gc race loser", 300);
        let winner = another_process(&dir);
        let winning = bytes.clone();
        // While the loser stalls before publishing, another put publishes, and then the loser is delayed a long time.
        HOOK.with(|hook| {
            *hook.borrow_mut() = Some((
                "before_publish",
                Box::new(move || {
                    winner.put(&winning, sample_provenance(Commercial::Ok)).unwrap();
                    advance(GRACE * 2);
                }),
            ));
        });
        let hash = loser.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();

        let report = another_process(&dir).sweep(&|_| false, GRACE, now()).unwrap();
        assert!(report.removed.is_empty(), "the loser's put gave the winner's object a grace of its own");
        assert!(loser.contains(&hash));
    }

    #[test]
    fn a_removal_forgets_the_completion_entry_while_it_still_holds_the_lock() {
        let (dir, store) = fake_store();
        let bytes = payload("gc forget", 300);
        let hash = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        advance(GRACE * 2);
        let object_dir = store.paths(&hash).dir;
        let lock_path = dir.path().join(super::LOCK_FILE);
        let completed = store.completed.clone();
        let seen = Rc::new(RefCell::new(None));
        let record = Rc::clone(&seen);
        HOOK.with(|hook| {
            *hook.borrow_mut() = Some((
                "after_retire",
                Box::new(move || {
                    let file = fs::OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
                    let blocked = matches!(file.try_lock_shared(), Err(fs::TryLockError::WouldBlock));
                    let forgotten = !completed.lock().entries.contains_key(&object_dir);
                    *record.borrow_mut() = Some((blocked, forgotten));
                }),
            ));
        });
        assert_eq!(store.remove_if_older(&hash, now()).unwrap(), Removal::Removed);
        assert_eq!(*seen.borrow(), Some((true, true)), "no put can interleave between the retirement and the forget");
    }

    #[test]
    fn a_hot_dedup_put_still_gets_a_full_grace_in_this_process() {
        let grace = Duration::from_millis(100);
        let (_dir, store) = fake_store();
        let store = store.with_gc_grace(grace);
        let bytes = payload("gc hot dedup", 300);
        let hash = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        advance(Duration::from_millis(20));
        let marker = store.paths(&hash).dir.join(COMMITTED_FILE);
        let written = fs::read(&marker).unwrap();
        store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        assert_eq!(fs::read(&marker).unwrap(), written, "within a quarter of the grace nothing is rewritten");

        // The re-put at 20 ms counts, though the marker still says 0 ms: at 100 ms the object is only 80 ms old.
        advance(Duration::from_millis(80));
        assert!(store.sweep(&|_| false, grace, now()).unwrap().removed.is_empty(), "80 ms after the re-put: kept");
        advance(Duration::from_millis(20));
        assert_eq!(store.sweep(&|_| false, grace, now()).unwrap().removed, vec![hash], "100 ms after it: collected");
    }

    #[test]
    fn a_delay_after_the_marker_write_still_counts_toward_the_in_process_grace() {
        let grace = Duration::from_millis(100);
        let (_dir, store) = fake_store();
        let store = store.with_gc_grace(grace);
        // The marker is stamped at 0 ms; 20 ms pass before the put checks it (within the 25 ms slack), so the put
        // returns at 20 ms with the marker still fresh enough.
        STALL.with(|stall| stall.set(Some(("before_final_check", Duration::from_millis(20)))));
        let hash = store.put(&payload("gc late check", 300), sample_provenance(Commercial::Ok)).unwrap();

        advance(Duration::from_millis(80));
        assert!(store.sweep(&|_| false, grace, now()).unwrap().removed.is_empty(), "80 ms after the put returned");
        advance(Duration::from_millis(20));
        assert_eq!(store.sweep(&|_| false, grace, now()).unwrap().removed, vec![hash]);
    }

    #[test]
    fn a_wait_for_the_map_lock_after_reading_the_marker_cannot_age_the_put() {
        let grace = Duration::from_millis(100);
        let (dir, store) = fake_store();
        let store = store.with_gc_grace(grace);
        // The put reads the marker (stamped at 0 ms), then waits twice the grace for the map lock: the stamp is stale
        // by the time it finishes, so it must stamp again instead of returning with it.
        STALL.with(|stall| stall.set(Some(("before_finish_lock", grace * 2))));
        let bytes = payload("gc lock wait", 300);
        let hash = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();

        let stamped = DiskStore::marker_time(&hash, &store.paths(&hash)).unwrap();
        assert!(now().duration_since(stamped).unwrap() <= store.stamp_slack, "the marker is within the slack of now");
        // Another process, half a grace after the put returned, reads the marker from disk only.
        let report = another_process(&dir).sweep(&|_| false, grace, now() + grace / 2).unwrap();
        assert!(report.removed.is_empty(), "the marker was restamped, so another process keeps the object");
        assert_eq!(store.get(&hash).unwrap(), bytes);
        advance(grace);
        assert_eq!(another_process(&dir).sweep(&|_| false, grace, now()).unwrap().removed, vec![hash]);
    }

    fn nanos_of(time: SystemTime) -> u128 {
        time.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn the_marker_moves_only_to_a_strictly_later_stamp_whatever_the_record_kind() {
        let (_dir, store) = fake_store();
        let store = store.with_gc_grace(Duration::from_hours(1));
        let hash = store.put(&payload("gc monotonic", 300), sample_provenance(Commercial::Ok)).unwrap();
        let paths = store.paths(&hash);
        let marker = paths.dir.join(COMMITTED_FILE);
        let (older, middle, newer) =
            (now() - Duration::from_secs(30), now() + Duration::from_secs(30), now() + Duration::from_secs(60));
        let publish = |stamp: SystemTime| {
            assert!(store.publish_marker(&paths, commit_record(nanos_of(stamp)).as_bytes(), stamp).unwrap());
        };
        let on_disk = || DiskStore::recorded_stamp(&marker).unwrap().unwrap();

        publish(middle);
        assert_eq!(on_disk(), (middle, true));
        publish(older);
        assert_eq!(on_disk(), (middle, true), "an older stamp does not move it back");
        publish(middle);
        assert_eq!(on_disk(), (middle, true), "nor does an equal one replace it");
        publish(newer);
        assert_eq!(on_disk(), (newer, true), "a strictly later one does");
        let leftovers: Vec<_> = fs::read_dir(&paths.dir).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert_eq!(leftovers.len(), 3, "the discarded stamps left no temp file: {leftovers:?}");

        // A pending record is ordered by its stamp too, read when it is published: later than what is there, it wins;
        // and then an older trusted stamp cannot replace it.
        NOW.with(|clock| clock.set(newer + Duration::from_secs(30)));
        store.publish_pending(&paths).unwrap();
        let (pending_at, trusted) = on_disk();
        assert!(!trusted && pending_at == now(), "a later pending record replaces a trusted stamp");
        publish(newer);
        assert_eq!(on_disk(), (pending_at, false), "an older trusted stamp is refused over a pending record");
        store.publish_pending(&paths).unwrap();
        assert_eq!(on_disk(), (pending_at, false), "a pending record at the same instant leaves it alone");
        publish(pending_at + Duration::from_secs(1));
        assert!(on_disk().1, "a later trusted stamp replaces a pending one");

        // A torn marker is replaced by anything.
        fs::write(&marker, b"12").unwrap();
        publish(older);
        assert_eq!(on_disk(), (older, true));
    }

    #[test]
    fn a_pending_fallback_read_earlier_than_a_trusted_stamp_leaves_it_and_a_later_older_stamp_is_refused() {
        let (_dir, store) = fake_store();
        let store = store.with_gc_grace(Duration::from_hours(1));
        let hash = store.put(&payload("gc pending fallback", 300), sample_provenance(Commercial::Ok)).unwrap();
        let paths = store.paths(&hash);
        let marker = paths.dir.join(COMMITTED_FILE);
        let at = |secs: u64| now() + Duration::from_secs(secs);
        let (ten, thirty, fifty) = (at(10), at(30), at(50));

        // A trusted stamp 50 is on disk; a pending fallback then reads the clock at 10, earlier than that.
        assert!(store.publish_marker(&paths, commit_record(nanos_of(fifty)).as_bytes(), fifty).unwrap());
        NOW.with(|clock| clock.set(ten));
        store.publish_pending(&paths).unwrap();
        let (on_disk, trusted) = DiskStore::recorded_stamp(&marker).unwrap().unwrap();
        assert!(on_disk >= fifty && trusted, "the pending record did not move the marker back: {on_disk:?}");

        // A later trusted 30 is refused too.
        assert!(store.publish_marker(&paths, commit_record(nanos_of(thirty)).as_bytes(), thirty).unwrap());
        assert_eq!(DiskStore::recorded_stamp(&marker).unwrap(), Some((fifty, true)));
    }

    #[test]
    fn a_put_that_cannot_keep_a_fresh_stamp_through_its_waits_fails_instead_of_returning_unprotected() {
        let grace = Duration::from_millis(100);
        let (dir, store) = fake_store();
        let store = store.with_gc_grace(grace);
        // Every wait between reading the marker and finishing takes twice the grace, so no stamp is ever fresh when
        // the put finishes, not even the pending record of its fallback.
        STALL_EVERY.with(|stall| stall.set(Some(("before_finish_lock", grace * 2))));
        let bytes = payload("gc cannot settle", 300);
        let result = store.put(&bytes, sample_provenance(Commercial::Ok));
        STALL_EVERY.with(|stall| stall.set(None));
        assert!(
            matches!(&result, Err(StoreError::Io(message)) if message.contains("could not establish")),
            "it must not report success without a protection it has checked: {result:?}"
        );

        // What it left behind is not collectable at once by another process.
        let hash = ContentHash::of(&bytes);
        let report = another_process(&dir).sweep(&|_| false, grace, now()).unwrap();
        assert!(report.removed.is_empty(), "the object is still protected: {hash}");
    }

    /// Held by each two-writer regression for its whole run: they share one clock, so they cannot overlap.
    static PAIR_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The clock of the two-writer regressions, shared by their threads.
    static PAIR_NOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn pair_now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_nanos(PAIR_NOW.load(std::sync::atomic::Ordering::SeqCst))
    }

    fn pair_advance(by: Duration) {
        PAIR_NOW.fetch_add(u64::try_from(by.as_nanos()).unwrap(), std::sync::atomic::Ordering::SeqCst);
    }

    /// Two writers of an old object, with grace 100 ms: B stamps 0 and stops before its rename; A publishes 20 and
    /// stops before finishing; B goes on and returns at 24; A returns at 40. B's older stamp must not replace A's, or a
    /// sweep at 100 would take the object 60 ms after A returned.
    #[test]
    fn a_slower_writer_cannot_put_an_older_stamp_over_a_newer_one() {
        use std::sync::mpsc::channel;
        let _serial = PAIR_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let grace = Duration::from_millis(100);
        let dir = tempfile::tempdir().unwrap();
        let open_store = || {
            let mut store = DiskStore::open(dir.path()).unwrap().with_gc_grace(grace);
            store.clock = pair_now;
            store.failpoint = failpoint;
            store
        };
        let real = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        PAIR_NOW.store(u64::try_from(real.as_nanos()).unwrap(), std::sync::atomic::Ordering::SeqCst);
        let bytes = payload("gc two writers", 300);
        let hash = open_store().put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        pair_advance(grace * 4);
        let start = pair_now();

        let (b_reached, b_reached_rx) = channel();
        let (b_resume_tx, b_resume) = channel();
        let (a_reached, a_reached_rx) = channel();
        let (a_resume_tx, a_resume) = channel();
        let (writer_b, writer_a) = (open_store(), open_store());
        std::thread::scope(|scope| {
            let b = scope.spawn(|| {
                PAUSE.with(|pause| *pause.borrow_mut() = Some(("before_confirm", b_reached, b_resume)));
                writer_b.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
            });
            b_reached_rx.recv().unwrap();
            pair_advance(Duration::from_millis(20));
            let a = scope.spawn(|| {
                PAUSE.with(|pause| *pause.borrow_mut() = Some(("before_finish_lock", a_reached, a_resume)));
                writer_a.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
            });
            a_reached_rx.recv().unwrap();
            pair_advance(Duration::from_millis(4));
            b_resume_tx.send(()).unwrap();
            b.join().unwrap();
            pair_advance(Duration::from_millis(16));
            a_resume_tx.send(()).unwrap();
            a.join().unwrap();
        });

        let stamp = DiskStore::marker_time(&hash, &writer_a.paths(&hash)).unwrap();
        assert_eq!(stamp, start + Duration::from_millis(20), "the marker still reads A's stamp");
        pair_advance(Duration::from_millis(60));
        let report = open_store().sweep(&|_| false, grace, pair_now()).unwrap();
        assert!(report.removed.is_empty(), "60 ms after A returned, another process keeps the object");
    }

    /// The other schedule, with a new object (grace 100 ms): B publishes the data, stamps 0 and pauses before its
    /// rename; A dedups at 20, finds no marker and pauses before finishing. Then, in either order, B goes on (renames
    /// 0, returns at 24) and A goes on (returns at 40 after stamping for itself, since it read no trusted stamp).
    /// A sweep at 100 would otherwise take the object 60 ms after the last put returned.
    fn new_object_pair(b_renames_first: bool) {
        use std::sync::mpsc::channel;
        let _serial = PAIR_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let grace = Duration::from_millis(100);
        let dir = tempfile::tempdir().unwrap();
        let open_store = || {
            let mut store = DiskStore::open(dir.path()).unwrap().with_gc_grace(grace);
            store.clock = pair_now;
            store.failpoint = failpoint;
            store
        };
        let real = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        PAIR_NOW.store(u64::try_from(real.as_nanos()).unwrap(), std::sync::atomic::Ordering::SeqCst);
        let bytes = payload("gc new object pair", 300);
        let hash = ContentHash::of(&bytes);
        let start = pair_now();

        let (b_reached, b_reached_rx) = channel();
        let (b_resume_tx, b_resume) = channel();
        let (a_reached, a_reached_rx) = channel();
        let (a_resume_tx, a_resume) = channel();
        let (writer_b, writer_a) = (open_store(), open_store());
        std::thread::scope(|scope| {
            let b = scope.spawn(|| {
                PAUSE.with(|pause| *pause.borrow_mut() = Some(("before_confirm", b_reached, b_resume)));
                writer_b.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
            });
            b_reached_rx.recv().unwrap();
            pair_advance(Duration::from_millis(20));
            let a = scope.spawn(|| {
                PAUSE.with(|pause| *pause.borrow_mut() = Some(("before_finish_lock", a_reached, a_resume)));
                writer_a.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
            });
            a_reached_rx.recv().unwrap();
            pair_advance(Duration::from_millis(4));
            if b_renames_first {
                b_resume_tx.send(()).unwrap();
                b.join().unwrap();
                pair_advance(Duration::from_millis(16));
                a_resume_tx.send(()).unwrap();
                a.join().unwrap();
            } else {
                a_resume_tx.send(()).unwrap();
                a.join().unwrap();
                pair_advance(Duration::from_millis(16));
                b_resume_tx.send(()).unwrap();
                b.join().unwrap();
            }
        });

        let stamp = DiskStore::marker_time(&hash, &writer_a.paths(&hash)).unwrap();
        assert!(stamp >= start + Duration::from_millis(24), "a put stamped for itself after reading none: {stamp:?}");
        pair_advance(Duration::from_millis(60));
        let report = open_store().sweep(&|_| false, grace, pair_now()).unwrap();
        assert!(report.removed.is_empty(), "60 ms after the last put returned, another process keeps the object");
    }

    #[test]
    fn a_put_that_read_no_trusted_stamp_stamps_for_itself_when_the_older_rename_comes_first() {
        new_object_pair(true);
    }

    #[test]
    fn a_put_that_read_no_trusted_stamp_stamps_for_itself_when_it_goes_on_first() {
        new_object_pair(false);
    }

    #[test]
    fn the_marker_lock_file_is_ignored_by_verify_and_repair() {
        let (dir, store) = fake_store();
        store.put(&payload("gc marker lock", 300), sample_provenance(Commercial::Ok)).unwrap();
        assert!(dir.path().join(super::MARKER_LOCK_FILE).is_file(), "a put leaves the marker lock file");
        assert!(dir.path().join(super::LOCK_FILE).is_file());
        assert!(store.verify(Duration::ZERO).unwrap().is_clean(), "verify ignores both lock files");
        assert!(store.repair(Duration::ZERO).unwrap().quarantined.is_empty(), "and so does repair");
    }

    #[test]
    fn overlapping_puts_keep_the_latest_finish_whatever_order_they_end_in() {
        let dir = Path::new("object");
        let at = |millis| SystemTime::UNIX_EPOCH + Duration::from_millis(millis);
        for (first, second) in [(10, 20), (20, 10)] {
            let completed = Completed::default();
            let mut map = completed.lock();
            map.reserve(dir);
            map.reserve(dir);
            assert_eq!(map.finish(dir), Finish::InFlight);
            map.complete(dir, at(first), true);
            assert_eq!(map.finish(dir), Finish::InFlight, "one put is still running");
            map.complete(dir, at(second), true);
            assert_eq!(map.finish(dir), Finish::At(at(20)), "the later finish wins, ending {first} then {second}");
        }
    }

    #[test]
    fn a_failing_put_does_not_disturb_an_overlapping_one() {
        let dir = Path::new("object");
        let at = |millis| SystemTime::UNIX_EPOCH + Duration::from_millis(millis);
        let completed = Completed::default();
        let mut map = completed.lock();
        map.reserve(dir);
        map.reserve(dir);
        map.complete(dir, at(30), true);
        map.complete(dir, at(40), false);
        assert_eq!(map.finish(dir), Finish::At(at(30)), "the failure left the other put's finish alone");

        let other = Path::new("other");
        map.reserve(other);
        map.complete(other, at(50), false);
        assert_eq!(map.finish(other), Finish::Unknown, "a put that only failed leaves nothing behind");
        assert!(!map.entries.contains_key(other));
    }

    #[test]
    fn a_dedup_put_freshens_an_old_object() {
        let (_dir, store) = fake_store();
        let bytes = payload("gc freshen", 300);
        let hash = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        advance(GRACE * 2);
        assert_eq!(store.put(&bytes, sample_provenance(Commercial::No)).unwrap(), hash);

        let report = store.sweep(&|_| false, GRACE, now()).unwrap();
        assert!(report.removed.is_empty(), "the re-put gave the object a fresh grace");
        advance(GRACE);
        assert_eq!(store.sweep(&|_| false, GRACE, now()).unwrap().removed, vec![hash], "collected one grace later");
    }

    #[test]
    fn a_dedup_put_of_a_fresh_object_writes_nothing() {
        let (_dir, store) = fake_store();
        let bytes = payload("gc no rewrite", 300);
        let hash = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        let marker = store.paths(&hash).dir.join(COMMITTED_FILE);
        let before = (fs::read(&marker).unwrap(), fs::metadata(&marker).unwrap().modified().unwrap());
        store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        let after = (fs::read(&marker).unwrap(), fs::metadata(&marker).unwrap().modified().unwrap());
        assert_eq!(before, after, "within a quarter of the grace nothing is rewritten");
    }

    #[test]
    fn a_put_holds_the_store_lock_shared_so_a_removal_waits_and_puts_do_not() {
        let (dir, store) = fake_store();
        let lock_path = dir.path().join(super::LOCK_FILE);
        let result = Rc::new(RefCell::new(None));
        let seen = Rc::clone(&result);
        let probing = lock_path.clone();
        PROBE.with(|probe| {
            *probe.borrow_mut() = Some(Box::new(move || {
                let file = fs::OpenOptions::new().read(true).write(true).open(&probing).unwrap();
                let exclusive_blocked = matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock));
                let shared_ok = file.try_lock_shared().is_ok();
                *seen.borrow_mut() = Some((exclusive_blocked, shared_ok));
            }));
        });
        store.put(&payload("gc lock", 300), sample_provenance(Commercial::Ok)).unwrap();
        PROBE.with(|probe| *probe.borrow_mut() = None);

        assert_eq!(*result.borrow(), Some((true, true)), "mid-put: removal would wait, another put would not");
        let file = fs::OpenOptions::new().read(true).write(true).open(&lock_path).unwrap();
        assert!(file.try_lock().is_ok(), "released when the put returns");
    }

    thread_local! {
        /// The steps a test store has reached, in order, for the tests that check an ordering.
        static REACHED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    #[test]
    fn a_purge_syncs_the_shard_before_it_deletes_the_retired_contents() {
        let (_dir, store) = fake_store();
        let hash = store.put(&payload("gc purge order", 300), sample_provenance(Commercial::Ok)).unwrap();
        let paths = store.paths(&hash);
        // Retired by a removal that has not synced the shard yet (or crashed before it did).
        let retired = retire_object(&paths.dir, &paths.shard).unwrap().unwrap();
        REACHED.with(|reached| reached.borrow_mut().clear());
        HOOK_LOG.with(|log| log.set(true));
        assert_eq!(store.purge_retired().unwrap(), 1);
        HOOK_LOG.with(|log| log.set(false));
        let order = REACHED.with(|reached| reached.borrow().clone());
        assert_eq!(order, ["purge_sync", "purge_delete"], "the barrier comes first");
        assert!(!retired.exists());
    }

    #[test]
    fn a_removal_whose_retired_directory_was_purged_meanwhile_still_succeeds() {
        let (dir, store) = fake_store();
        let hash = store.put(&payload("gc concurrent purge", 300), sample_provenance(Commercial::Ok)).unwrap();
        advance(GRACE * 2);
        let other = another_process(&dir);
        // While the removal waits at `after_retire` (it holds the lock and has renamed the object), another process
        // purges the retired directory.
        HOOK.with(|hook| {
            *hook.borrow_mut() = Some((
                "after_retire",
                Box::new(move || {
                    assert_eq!(other.purge_retired().unwrap(), 1, "the other process deleted the retired directory");
                }),
            ));
        });
        let removal = store.remove_if_older(&hash, now()).unwrap();
        assert_eq!(removal, Removal::Removed, "finding the cleanup already done is not an error");
        assert!(!store.contains(&hash));
    }

    #[test]
    fn removing_an_absent_object_reports_absent_and_keeps_the_store() {
        let (_dir, store) = open();
        let hash = ContentHash::of(b"gc absent");
        assert_eq!(store.remove_if_older(&hash, SystemTime::now() + GRACE).unwrap(), Removal::Absent);
        assert_eq!(store.hashes().unwrap(), Vec::<ContentHash>::new());
    }

    #[test]
    fn a_damaged_object_can_be_removed_and_stored_again() {
        let (_dir, store) = open();
        let hash = store.put(&payload("gc damaged", 200), sample_provenance(Commercial::Ok)).unwrap();
        fs::remove_file(store.paths(&hash).data).unwrap();
        assert!(store.hashes().unwrap().contains(&hash), "an object without data is still listed");
        assert_eq!(store.remove_if_older(&hash, SystemTime::now() + GRACE).unwrap(), Removal::Removed);
        assert!(store.verify(Duration::ZERO).unwrap().is_clean(), "nothing of the damaged object is left behind");
        let again = store.put(&payload("gc damaged", 200), sample_provenance(Commercial::Ok)).unwrap();
        assert_eq!(again, hash, "a fresh copy is stored");
        assert!(store.verify(Duration::ZERO).unwrap().is_clean());
    }
}
