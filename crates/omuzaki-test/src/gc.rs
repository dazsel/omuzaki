//! `omuzaki-test gc`: remove golden images that no branch wants any more.
//!
//! The object store is shared by every branch and only ever grows ([`crate::objects`]). An image is **wanted** when the
//! manifest of any of these names it:
//!
//! - what every ref points at: local and remote-tracking branches, tags, the stash, anything else git keeps a ref for
//!   (an open PR's branch is one), and every entry of the stash log with the commits it is built from;
//! - the checked-out commit of every worktree, which covers a detached HEAD;
//! - the manifest *files* of every worktree, so a bless that is not committed yet is safe;
//! - any extra revision the caller names (a bare commit id, say).
//!
//! Anything else is garbage, but nothing younger than [`MIN_AGE`] is ever removed, whatever the manifests say. The age
//! is counted from the **newest** clock reading the file system keeps for the file:
//!
//! - its modification time (`mtime`), which [`ObjectStore::put`] renews whenever an image is blessed again, and which
//!   `rsync -a`, `cp -p` and a restore from backup copy from the source, so on its own it makes a fresh copy look old;
//! - its change time (`ctime`), which the kernel sets on every change to the file, its creation included, and which no
//!   tool can carry over from a source;
//! - its creation time, where the file system has one (the only one of the two on Windows).
//!
//! So a copy is as new as the copy. Readings that can't be trusted keep the image instead of deleting it, and say so in
//! [`Plan::warnings`]: one dated more than [`CLOCK_SLACK`] in the future (a clock set wrong; the image waits until that
//! time has passed), and a file system that reports neither a change nor a creation time (a copy could not be told
//! from the original).
//!
//! [`plan`] only looks; [`Plan::apply`] deletes. **Everything fails closed**: whenever `gc` cannot be sure, it deletes
//! nothing and returns an error (exit 2). That covers git failing for any ref or manifest, a manifest that can't be
//! parsed, any I/O error other than a clean "no such file" (a worktree folder without permission is not a deleted
//! one), a shallow clone or a remote that fetches only some branches, a manifest folder spelled in another letter case
//! than the file system or the repository uses, a locked worktree whose folder is gone, and, in [`Plan::apply`], any
//! change to what the refs, worktrees and manifests name since the plan was made, or to an image's file.
//!
//! Remote-tracking branches are as of the last `git fetch`, and a teammate's newest PR branch is invisible until it is
//! fetched. [`plan`] warns when the last fetch is older than [`MAX_FETCH_AGE`] (or none is recorded, or every record is
//! dated in the future), and [`Plan::apply`] then refuses to delete until the repository has fetched again. Git writes
//! `FETCH_HEAD` per worktree but a fetch updates branches every worktree shares, so the newest record of the main
//! checkout and of the linked worktrees counts.
//!
//! The clock is the machine's, with one exception that exists only for the test-suite: see [`clock`].

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omuzaki_store_api::ContentHash;

use crate::error::{GoldenError, gc_refused, io_err};
use crate::fsio::sorted_entries;
use crate::git::{FetchRecord, Repo, Worktree};
use crate::goldens::Goldens;
use crate::manifest::{Manifest, suite_files};
use crate::objects::{ObjectStore, object_hash};

/// Images younger than this are never removed. Fixed on purpose: there is no flag to shorten it.
pub const MIN_AGE: Duration = Duration::from_hours(7 * 24);

/// When the repository's last `git fetch` is older than this, a teammate's newest PR branch may be missing from the
/// clone, so [`plan`] warns and [`Plan::apply`] refuses. Fixed on purpose, like [`MIN_AGE`]: fetching takes seconds.
pub const MAX_FETCH_AGE: Duration = Duration::from_hours(3 * 24);

/// How far a clock reading may run ahead of `now` before it is called a wrong clock. A little is normal: `now` is read
/// before the files are listed, so a bless in between is dated after it, and file systems round times.
pub const CLOCK_SLACK: Duration = Duration::from_mins(5);

/// The environment variable that sets the time `gc` counts ages from, in whole seconds since 1970. It exists for the
/// test-suite and [`clock`] honours it only for a store that holds a [`TEST_CLOCK_MARKER`] file.
pub const TEST_CLOCK_ENV: &str = "OMUZAKI_GC_TEST_NOW";

/// The file in the store root (next to `objects/`) that says the store was made by the test-suite, and so may be run
/// as of another day. The test fixtures create it; nothing else does.
pub const TEST_CLOCK_MARKER: &str = ".omuzaki-test-clock";

/// The time `gc` works as of: the machine's clock, or, for a store of the test-suite, the time the test asks for.
///
/// The kernel stamps every file with the real moment of its last change and nothing can set that back, so a test can
/// only make an image look old by moving the clock forward. Anywhere else that would be a hole in the 7-day floor
/// ([`MIN_AGE`]): set far enough ahead, the variable makes every image look old and `--apply` deletes what a
/// bless wrote seconds ago. So it is refused, on a dry run as well as on `--apply`, unless the store root holds a
/// [`TEST_CLOCK_MARKER`] file. `setting` is the value of [`TEST_CLOCK_ENV`], if it is set.
///
/// # Errors
/// [`GoldenError::GcRefused`] when `setting` is given but the store has no marker file, or the setting is not whole
/// seconds since 1970.
pub fn clock(goldens: &Goldens, setting: Option<&str>) -> Result<SystemTime, GoldenError> {
    let Some(setting) = setting else { return Ok(SystemTime::now()) };
    let marker = goldens.store().join(TEST_CLOCK_MARKER);
    if !fs::metadata(&marker).is_ok_and(|meta| meta.is_file()) {
        return Err(gc_refused(
            format!(
                "{TEST_CLOCK_ENV} is set, but {} has no {TEST_CLOCK_MARKER} file, so this is not a store of the \
                 test-suite and gc will not take its time from the environment",
                goldens.store().display()
            ),
            format!("unset {TEST_CLOCK_ENV}"),
        ));
    }
    let seconds = setting.trim().parse::<u64>().map_err(|_| {
        gc_refused(
            format!("{TEST_CLOCK_ENV} must be whole seconds since 1970, not '{setting}'"),
            format!("unset {TEST_CLOCK_ENV} or give whole seconds"),
        )
    })?;
    Ok(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// What a file system says about when a file was written or changed. Equality is how [`Plan::apply`] notices that a
/// file was touched since the plan: any reading moving, the change time included, counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    /// Last write to the content (`mtime`). Anyone can set it, and copy tools carry it over from the source.
    modified: SystemTime,
    /// Last change to the file itself (`ctime`), set by the kernel and so never carried over by a copy. `None` where
    /// the platform has none, or where the file system reports no real time (the epoch).
    changed: Option<SystemTime>,
    /// When the file was created, where the file system keeps that. `None` otherwise.
    created: Option<SystemTime>,
}

impl Stamp {
    fn of(meta: &fs::Metadata) -> io::Result<Self> {
        Ok(Self {
            modified: meta.modified()?,
            changed: changed_time(meta),
            created: meta.created().ok().filter(|time| *time > UNIX_EPOCH),
        })
    }

    /// Each reading with the name a warning gives it.
    fn readings(&self) -> [(&'static str, Option<SystemTime>); 3] {
        [("mtime", Some(self.modified)), ("ctime", self.changed), ("creation time", self.created)]
    }

    /// Whether a copy that kept the old modification time could be told from the original: only a reading the copy
    /// tool can't carry over (change time, creation time) does that.
    fn tells_copies(&self) -> bool {
        self.changed.is_some() || self.created.is_some()
    }

    /// How long ago the file was last stored, copied or changed. Zero when that can't be told (see
    /// [`Stamp::tells_copies`]) or the newest reading is in the future: an image is then kept, never deleted.
    fn age(&self, now: SystemTime) -> Duration {
        if !self.tells_copies() {
            return Duration::ZERO;
        }
        let newest = self.readings().into_iter().filter_map(|(_, time)| time).max().unwrap_or(self.modified);
        now.duration_since(newest).unwrap_or(Duration::ZERO)
    }
}

/// The change time of a file, where the platform keeps one.
#[cfg(unix)]
fn changed_time(meta: &fs::Metadata) -> Option<SystemTime> {
    use std::os::unix::fs::MetadataExt;
    unix_time(meta.ctime(), meta.ctime_nsec())
}

/// Windows keeps no change time in `std`; its creation time stands in (see [`Stamp::created`]).
#[cfg(not(unix))]
fn changed_time(_meta: &fs::Metadata) -> Option<SystemTime> {
    None
}

/// A `stat` time as a [`SystemTime`]. The epoch and anything before it is what a file system without the field
/// reports, not a time a file was changed, so it is no reading at all.
#[cfg(unix)]
fn unix_time(secs: i64, nanos: i64) -> Option<SystemTime> {
    let secs = u64::try_from(secs).ok().filter(|secs| *secs > 0)?;
    let nanos = u32::try_from(nanos).ok().filter(|nanos| *nanos < 1_000_000_000)?;
    UNIX_EPOCH.checked_add(Duration::new(secs, nanos))
}

/// One image in the object store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    /// The image's hash (its file name without `.png`).
    pub hash: ContentHash,
    /// Where the file is.
    pub path: PathBuf,
    /// Its size on disk.
    pub bytes: u64,
    /// How long ago it was stored, last chosen as a golden, copied or changed: counted from the newest of its
    /// modification, change and creation times. Zero when that can't be told or is in the future (the image is kept).
    pub age: Duration,
    /// How long ago its modification time alone says it was stored or chosen. Longer than `age` for a file that was
    /// copied or restored with the old time kept.
    pub modified_age: Duration,
    /// The readings `age` was computed from; [`Plan::apply`] removes the file only if they are unchanged.
    stamp: Stamp,
}

impl Object {
    fn new(hash: ContentHash, path: PathBuf, bytes: u64, stamp: Stamp, now: SystemTime) -> Self {
        let modified_age = now.duration_since(stamp.modified).unwrap_or(Duration::ZERO);
        Self { hash, path, bytes, age: stamp.age(now), modified_age, stamp }
    }

    /// Whether the file changed on disk (copied, restored, re-permissioned) well after its content was last stored or
    /// chosen, which is why it counts as younger than its modification time says.
    pub fn changed_since_stored(&self) -> bool {
        self.stamp.tells_copies() && self.modified_age >= self.age + Duration::from_hours(24)
    }
}

/// What was read to decide which images are wanted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scan {
    /// Distinct commits whose manifests were read: what every ref points at, every stash entry, every worktree HEAD,
    /// every extra revision.
    pub commits: usize,
    /// Worktrees whose files were read, including the one you ran this from.
    pub worktrees: usize,
    /// Registered worktrees whose folder no longer exists: nothing to read there, and nothing uncommitted left to lose.
    pub missing_worktrees: usize,
    /// Manifest files read: each distinct committed file once, plus the files on disk.
    pub manifests: usize,
}

/// Why the remote-tracking branches may be missing a teammate's newest branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Staleness {
    /// The last `git fetch` was longer than [`MAX_FETCH_AGE`] ago.
    Old {
        /// The newest record of a fetch (a `FETCH_HEAD`).
        file: PathBuf,
        /// How long ago it was written.
        age: Duration,
    },
    /// The repository has remotes, but no fetch is recorded in any worktree: it was cloned and never fetched since.
    Never {
        /// Where the main checkout's record would be.
        file: PathBuf,
    },
    /// Every record of a fetch is dated further in the future than [`CLOCK_SLACK`], so the clock can't say how long ago
    /// the fetch was.
    InFuture {
        /// The record closest to `now`.
        file: PathBuf,
        /// How far ahead of `now` it is dated.
        ahead: Duration,
    },
}

impl fmt::Display for Staleness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Old { file, age } => write!(
                f,
                "the last `git fetch` was {} ago ({} was written then), so a teammate's newest PR branch may be \
                 missing from this clone and its images would look unwanted",
                span(*age),
                file.display()
            ),
            Self::Never { file } => write!(
                f,
                "no `git fetch` is recorded ({} does not exist), so a teammate's PR branches may be missing from this \
                 clone and their images would look unwanted",
                file.display()
            ),
            Self::InFuture { file, ahead } => write!(
                f,
                "{}, the record of the last `git fetch`, is dated {} in the future, so there is no telling how long \
                 ago the remote branches were fetched",
                file.display(),
                span(*ahead)
            ),
        }
    }
}

/// Something the person running `gc` should know. The images concerned are kept, and the run goes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// The remote-tracking branches may be out of date. [`Plan::apply`] refuses to delete anything until the
    /// repository has fetched again.
    StaleRemotes(Staleness),
    /// One of an unwanted image's clock readings is dated in the future. The image is kept until that time has passed.
    FutureTime {
        /// The image.
        path: PathBuf,
        /// Which reading: `mtime`, `ctime` or `creation time`.
        field: &'static str,
        /// How far ahead of `now` it is dated.
        ahead: Duration,
    },
    /// The file system reports neither a change time nor a creation time for an unwanted image, so a copy that kept the
    /// old modification time could not be told from the original. The image is kept.
    NoChangeTime {
        /// The image.
        path: PathBuf,
    },
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleRemotes(why) => write!(f, "{why}; run `git fetch --all` (--apply refuses to delete until then)"),
            Self::FutureTime { path, field, ahead } => write!(
                f,
                "{}: its {field} is {} in the future (a clock set wrong, or a copy from a machine with one?); \
                 the image is kept until then",
                path.display(),
                span(*ahead)
            ),
            Self::NoChangeTime { path } => write!(
                f,
                "{}: the file system reports neither a change time (ctime) nor a creation time for it, so a copy that \
                 kept the old modification time could not be told from the original; the image is kept",
                path.display()
            ),
        }
    }
}

/// A span for people, to the nearest whole unit: `3 days`, `1 hour`, `12 minutes`.
fn span(duration: Duration) -> String {
    let secs = duration.as_secs();
    let (seconds, unit) = if secs >= 86_400 {
        (86_400, "day")
    } else if secs >= 3_600 {
        (3_600, "hour")
    } else {
        (60, "minute")
    };
    let count = (secs + seconds / 2) / seconds;
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

/// What a `gc` would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// What was read to decide.
    pub scan: Scan,
    /// Images in the store that some manifest names.
    pub wanted: usize,
    /// Unwanted, but not provably [`MIN_AGE`] old: younger than that, or with clock readings that can't be trusted
    /// (see [`Plan::warnings`]). Kept.
    pub young: Vec<Object>,
    /// Unwanted and old enough: what [`Plan::apply`] removes, in hash order.
    pub garbage: Vec<Object>,
    /// What to tell the person: stale remote branches first, then the images with doubtful clock readings.
    pub warnings: Vec<Warning>,
    /// What the plan was made from, so [`Plan::apply`] can ask again. Absent when there was nothing to decide.
    basis: Option<Basis>,
}

/// The inputs and the answer of one scan: [`Plan::apply`] repeats the scan and deletes only if the answer is the same.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Basis {
    goldens: Goldens,
    extra_revs: Vec<String>,
    wanted: BTreeSet<ContentHash>,
    /// Kept out of the public warnings so that nobody can lift the refusal by editing them.
    stale: Option<Staleness>,
}

/// What [`Plan::apply`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Removal {
    /// Deleted.
    pub removed: Vec<Object>,
    /// Kept after all: the file was stored again, copied over or otherwise changed after the plan was made.
    pub touched: Vec<Object>,
}

/// The total size of `objects` in bytes.
pub fn total_bytes(objects: &[Object]) -> u64 {
    objects.iter().map(|object| object.bytes).sum()
}

/// Works out which images in the store nobody wants, as of `now`.
///
/// `extra_revs` are additional git revisions (a commit id, a branch name) whose manifests also count; they only ever
/// add to what is kept. The manifest folder must be inside the git checkout. Nothing is changed.
///
/// # Errors
/// [`GoldenError::Git`] when git can't answer for any ref, worktree or revision,
/// [`GoldenError::GcRefused`] when the clone can't see every branch (shallow, single-branch), the manifest folder is
/// spelled in another letter case than the file system or repository uses, or a locked worktree's folder is gone,
/// [`GoldenError::NoManifestsAnywhere`] when the store holds images but no manifest exists anywhere (the manifest folder
/// is probably wrong), [`GoldenError::Manifest`] for a manifest that can't be parsed, [`GoldenError::Io`] for any
/// folder or file that exists but can't be read.
pub fn plan(goldens: &Goldens, extra_revs: &[String], now: SystemTime) -> Result<Plan, GoldenError> {
    // The store is listed before the manifests are read, so any image stored after this point is seen as modified
    // when applying (see `Plan::apply`) even if the manifest naming it was written too late for the scan.
    let stored = list_objects(goldens.objects(), now)?;
    if stored.is_empty() {
        return Ok(Plan::default());
    }
    let (repo, folder) = Repo::locate(goldens.manifests())?;
    let (wanted, scan) = wanted_hashes(goldens, &repo, &folder, extra_revs)?;
    let stale = staleness(&repo.fetch_record()?, now);
    let (kept, unwanted): (Vec<_>, Vec<_>) = stored.into_iter().partition(|object| wanted.contains(&object.hash));
    let (young, garbage): (Vec<Object>, Vec<Object>) = unwanted.into_iter().partition(|object| object.age < MIN_AGE);
    // Only an image that is kept for want of a trustworthy age can have a doubtful reading: one that is old enough has
    // every reading in the past.
    let mut warnings: Vec<_> = stale.iter().cloned().map(Warning::StaleRemotes).collect();
    warnings.extend(young.iter().flat_map(|object| clock_warnings(object, now)));
    let basis = Basis { goldens: goldens.clone(), extra_revs: extra_revs.to_vec(), wanted, stale };
    Ok(Plan { scan, wanted: kept.len(), young, garbage, warnings, basis: Some(basis) })
}

impl Plan {
    /// Deletes the garbage, after repeating the scan: the refs, worktrees and manifests are read again, and if they now
    /// name a different set of images than when the plan was made (a branch fetched, a checkout, a bless, a stash) the
    /// whole run is abandoned with nothing deleted. An image whose file changed since the plan (a bless chose it again,
    /// the manifest naming it not written yet; a copy over it, even a `chmod`) is skipped.
    ///
    /// # Errors
    /// [`GoldenError::GcRefused`] when the remote-tracking branches may be out of date (see [`Warning::StaleRemotes`]),
    /// when what is wanted changed since the plan was made, or when the plan has nothing to repeat the scan from;
    /// anything [`plan`] can fail with; [`GoldenError::Io`] when a file can't be inspected or removed (images removed
    /// before that stay removed).
    pub fn apply(&self) -> Result<Removal, GoldenError> {
        let mut removal = Removal::default();
        if self.garbage.is_empty() {
            return Ok(removal);
        }
        let Some(basis) = &self.basis else {
            return Err(gc_refused("the plan does not say what it was made from", "make a new plan with gc::plan"));
        };
        if let Some(stale) = &basis.stale {
            return Err(gc_refused(stale.to_string(), "run `git fetch --all`, then run gc again"));
        }
        let (repo, folder) = Repo::locate(basis.goldens.manifests())?;
        let (wanted_now, _) = wanted_hashes(&basis.goldens, &repo, &folder, &basis.extra_revs)?;
        if wanted_now != basis.wanted {
            let (before, after) = (basis.wanted.len(), wanted_now.len());
            return Err(gc_refused(
                format!(
                    "the branches, worktrees or manifests changed while it ran (they named {before} distinct images \
                     when the plan was made, {after} now)"
                ),
                "run gc again",
            ));
        }
        for object in &self.garbage {
            let stamp = match fs::symlink_metadata(&object.path) {
                Ok(meta) if meta.is_file() => Stamp::of(&meta).map_err(io_err(&object.path))?,
                // Replaced by a link or a folder since the scan: not the file that was judged.
                Ok(_) => {
                    removal.touched.push(object.clone());
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_err(&object.path)(e)),
            };
            if stamp != object.stamp {
                removal.touched.push(object.clone());
                continue;
            }
            match fs::remove_file(&object.path) {
                Ok(()) => removal.removed.push(object.clone()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err(&object.path)(e)),
            }
        }
        Ok(removal)
    }
}

/// The images in `store`, in hash order. Anything that isn't a regular `<hash>.png` file is left alone, a symbolic link
/// to an image included: what it points at may live outside the store.
fn list_objects(store: &ObjectStore, now: SystemTime) -> Result<Vec<Object>, GoldenError> {
    let mut objects = Vec::new();
    for path in sorted_entries(store.dir())? {
        let Some(hash) = object_hash(&path) else { continue };
        let meta = fs::symlink_metadata(&path).map_err(io_err(&path))?;
        if !meta.is_file() {
            continue;
        }
        let stamp = Stamp::of(&meta).map_err(io_err(&path))?;
        objects.push(Object::new(hash, path, meta.len(), stamp, now));
    }
    Ok(objects)
}

/// What is wrong with the clock readings of `object`, if anything.
fn clock_warnings(object: &Object, now: SystemTime) -> Vec<Warning> {
    let future = object.stamp.readings().into_iter().filter_map(|(field, time)| {
        let ahead = time?.duration_since(now).ok().filter(|ahead| *ahead > CLOCK_SLACK)?;
        Some(Warning::FutureTime { path: object.path.clone(), field, ahead })
    });
    let blind = (!object.stamp.tells_copies()).then(|| Warning::NoChangeTime { path: object.path.clone() });
    future.chain(blind).collect()
}

/// Whether the remote-tracking branches may be behind the remotes, judged by the repository's last fetch as of `now`.
///
/// The newest record that is not dated in the future counts: a worktree whose `FETCH_HEAD` came from a machine with a
/// fast clock does not hide that another worktree fetched yesterday, nor make a fresh fetch look stale. Only when every
/// record is in the future is the clock the problem.
fn staleness(record: &FetchRecord, now: SystemTime) -> Option<Staleness> {
    let records = match record {
        FetchRecord::NoRemotes => return None,
        FetchRecord::Never { file } => return Some(Staleness::Never { file: file.clone() }),
        FetchRecord::At(records) => records,
    };
    let ages = records.iter().map(|(file, time)| (file, now.duration_since(*time).map_err(|e| e.duration())));
    let newest_past = ages
        .clone()
        .filter_map(|(file, age)| match age {
            Ok(age) => Some((file, age)),
            Err(ahead) => (ahead <= CLOCK_SLACK).then_some((file, Duration::ZERO)),
        })
        .min_by_key(|(_, age)| *age);
    if let Some((file, age)) = newest_past {
        return (age > MAX_FETCH_AGE).then(|| Staleness::Old { file: file.clone(), age });
    }
    let nearest_future =
        ages.filter_map(|(file, age)| age.err().map(|ahead| (file, ahead))).min_by_key(|(_, ahead)| *ahead);
    nearest_future.map(|(file, ahead)| Staleness::InFuture { file: file.clone(), ahead })
}

/// Every image hash named by the manifests on any ref, any worktree and any of `extra_revs`. `folder` is the manifest
/// folder inside `repo`, as [`Repo::locate`] returns it.
fn wanted_hashes(
    goldens: &Goldens,
    repo: &Repo,
    folder: &str,
    extra_revs: &[String],
) -> Result<(BTreeSet<ContentHash>, Scan), GoldenError> {
    repo.require_all_branches()?;
    let worktrees = repo.worktrees()?;

    let mut commits = repo.ref_tips()?;
    commits.extend(repo.stash_commits()?);
    commits.extend(worktrees.iter().filter_map(|tree| tree.head.clone()));
    for rev in extra_revs {
        commits.push(repo.resolve(rev)?);
    }
    commits.sort();
    commits.dedup();

    let mut wanted = BTreeSet::new();
    let mut scan = Scan { commits: commits.len(), ..Scan::default() };
    let mut add = |manifest: &Manifest| wanted.extend(manifest.cases().map(|(_, entry)| entry.hash));

    // Branches share most of their manifests: read each distinct file once.
    let mut read = BTreeSet::new();
    for commit in &commits {
        repo.require_exact_folder(commit, folder)?;
        for file in repo.manifests_at(commit, folder)? {
            if read.insert(file.blob.clone()) {
                let source = PathBuf::from(format!("{commit:.12}:{folder}/{}", file.file));
                add(&Manifest::parse(&repo.text_of(&file.blob)?, &source)?);
                scan.manifests += 1;
            }
        }
    }

    // Files on disk: blesses that are not committed yet. The folder we were pointed at counts even if git doesn't
    // list it as a worktree.
    let mut folders = vec![goldens.manifests().to_owned()];
    for tree in worktrees.iter().filter(|tree| !tree.bare) {
        if is_on_disk(tree)? {
            scan.worktrees += 1;
            folders.push(tree.path.join(folder));
        } else {
            scan.missing_worktrees += 1;
        }
    }
    folders.sort();
    folders.dedup();
    for dir in &folders {
        for (_, path) in suite_files(dir)? {
            add(&Manifest::load(&path)?);
            scan.manifests += 1;
        }
    }

    if scan.manifests == 0 {
        return Err(GoldenError::NoManifestsAnywhere { folder: folder.to_owned() });
    }
    Ok((wanted, scan))
}

/// Whether a worktree's folder exists. Only a clean "no such folder" means it was deleted, and then nothing
/// uncommitted is left to lose. A folder that can't be inspected (no permission) or isn't a folder might still hold an
/// uncommitted bless, so that is an error, as is a missing folder of a *locked* worktree, which is kept for the case
/// that its drive is merely unplugged.
fn is_on_disk(tree: &Worktree) -> Result<bool, GoldenError> {
    match fs::metadata(&tree.path) {
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(io_err(&tree.path)(io::Error::new(io::ErrorKind::NotADirectory, "a worktree, but not a folder"))),
        Err(e) if e.kind() == io::ErrorKind::NotFound && tree.locked => Err(gc_refused(
            format!("the folder of the locked worktree {} is missing (an unplugged drive?)", tree.path.display()),
            "plug it in, or run `git worktree unlock` and `git worktree prune` if it is really gone",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_err(&tree.path)(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_hours(24);

    /// A fixed "today" far from the epoch, so no test depends on the real clock.
    fn today() -> SystemTime {
        UNIX_EPOCH + Duration::from_hours(24 * 365 * 50)
    }

    fn stamp(modified: Duration, changed: Option<Duration>, created: Option<Duration>) -> Stamp {
        let ago = |age: Duration| today() - age;
        Stamp { modified: ago(modified), changed: changed.map(ago), created: created.map(ago) }
    }

    fn object(stamp: Stamp) -> Object {
        Object::new(ContentHash([1; 32]), PathBuf::from("/store/objects/image.png"), 1, stamp, today())
    }

    #[test]
    fn a_plan_that_cannot_repeat_its_scan_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("image.png");
        fs::write(&path, b"x").unwrap();
        let stamp = Stamp::of(&fs::metadata(&path).unwrap()).unwrap();
        let old = Object { age: MIN_AGE * 4, ..Object::new(ContentHash([1; 32]), path.clone(), 1, stamp, today()) };

        let plan = Plan { garbage: vec![old], ..Plan::default() };
        assert!(matches!(plan.apply(), Err(GoldenError::GcRefused { .. })));
        assert!(path.exists());
        assert_eq!(Plan::default().apply().unwrap(), Removal::default(), "nothing to delete needs no scan");
    }

    #[test]
    fn a_copy_that_kept_the_old_mtime_is_as_new_as_the_copy() {
        // `rsync -a` an hour ago: the content says 30 days, the kernel says an hour.
        let copied = object(stamp(30 * DAY, Some(Duration::from_hours(1)), None));
        assert_eq!((copied.age, copied.modified_age), (Duration::from_hours(1), 30 * DAY));
        assert!(copied.age < MIN_AGE && copied.changed_since_stored());

        let untouched = object(stamp(30 * DAY, Some(30 * DAY), Some(31 * DAY)));
        assert_eq!((untouched.age, untouched.modified_age), (30 * DAY, 30 * DAY));
        assert!(!untouched.changed_since_stored());

        // A bless sets the mtime and the kernel the ctime within moments of each other: nothing to explain.
        let blessed = object(stamp(Duration::from_hours(2), Some(Duration::from_hours(1)), None));
        assert_eq!(blessed.age, Duration::from_hours(1));
        assert!(!blessed.changed_since_stored(), "a day's difference is what counts as copied");
    }

    #[test]
    fn the_newest_of_all_three_readings_decides() {
        for (modified, changed, created, age) in [
            (30 * DAY, 20 * DAY, 10 * DAY, 10 * DAY),
            (30 * DAY, 10 * DAY, 20 * DAY, 10 * DAY),
            (10 * DAY, 20 * DAY, 30 * DAY, 10 * DAY),
        ] {
            assert_eq!(object(stamp(modified, Some(changed), Some(created))).age, age);
        }
        // No ctime (Windows): the creation time is what tells a copy.
        assert_eq!(object(stamp(30 * DAY, None, Some(DAY))).age, DAY);
    }

    #[test]
    fn without_a_change_or_creation_time_a_copy_cannot_be_told_and_the_image_is_kept() {
        let blind = object(stamp(90 * DAY, None, None));
        assert_eq!(blind.age, Duration::ZERO, "never provably old");
        assert_eq!(blind.modified_age, 90 * DAY);
        assert!(!blind.changed_since_stored());
        assert_eq!(
            clock_warnings(&blind, today()),
            vec![Warning::NoChangeTime { path: PathBuf::from("/store/objects/image.png") }]
        );
    }

    #[test]
    fn a_reading_from_the_future_keeps_the_image_and_names_the_field() {
        let path = PathBuf::from("/store/objects/image.png");
        let ahead = |field, ahead| vec![Warning::FutureTime { path: path.clone(), field, ahead }];
        let in_future = |age: Duration| today() + age;

        let mtime = Stamp { modified: in_future(3 * DAY), changed: Some(today() - 30 * DAY), created: None };
        let skewed = object(mtime);
        assert_eq!((skewed.age, skewed.modified_age), (Duration::ZERO, Duration::ZERO));
        assert_eq!(clock_warnings(&skewed, today()), ahead("mtime", 3 * DAY));

        let ctime = Stamp { modified: today() - 30 * DAY, changed: Some(in_future(2 * DAY)), created: None };
        assert_eq!(object(ctime).age, Duration::ZERO);
        assert_eq!(clock_warnings(&object(ctime), today()), ahead("ctime", 2 * DAY));

        let born =
            Stamp { modified: today() - 30 * DAY, changed: Some(today() - 30 * DAY), created: Some(in_future(DAY)) };
        assert_eq!(object(born).age, Duration::ZERO);
        assert_eq!(clock_warnings(&object(born), today()), ahead("creation time", DAY));

        // The slack is for rounding and for a bless a moment after `now` was read: not a wrong clock.
        let just_inside = Stamp { modified: in_future(CLOCK_SLACK), ..mtime };
        assert_eq!(clock_warnings(&object(just_inside), today()), vec![]);
        let just_outside = Stamp { modified: in_future(CLOCK_SLACK + Duration::from_secs(1)), ..mtime };
        assert_eq!(clock_warnings(&object(just_outside), today()).len(), 1);
    }

    #[test]
    fn a_stale_fetch_record_is_found_by_age_and_never_by_guessing() {
        let file = PathBuf::from("/repo/.git/FETCH_HEAD");
        let fetched = |ago: Duration| FetchRecord::At(vec![(file.clone(), today() - ago)]);
        let in_future = |ahead: Duration| FetchRecord::At(vec![(file.clone(), today() + ahead)]);

        assert_eq!(staleness(&FetchRecord::NoRemotes, today()), None, "nothing to be behind");
        assert_eq!(staleness(&fetched(Duration::ZERO), today()), None);
        assert_eq!(staleness(&fetched(MAX_FETCH_AGE), today()), None, "the edge itself is still fresh");
        let one_second_over = MAX_FETCH_AGE + Duration::from_secs(1);
        assert_eq!(
            staleness(&fetched(one_second_over), today()),
            Some(Staleness::Old { file: file.clone(), age: one_second_over })
        );
        assert_eq!(
            staleness(&FetchRecord::Never { file: file.clone() }, today()),
            Some(Staleness::Never { file: file.clone() })
        );
        // A record dated after `now` is a wrong clock somewhere: only a hair of it is rounding.
        assert_eq!(staleness(&in_future(CLOCK_SLACK), today()), None);
        let ahead = CLOCK_SLACK + Duration::from_secs(1);
        assert_eq!(staleness(&in_future(ahead), today()), Some(Staleness::InFuture { file, ahead }));
    }

    #[test]
    fn the_newest_record_of_any_worktree_decides_and_a_future_one_never_outvotes_a_past_one() {
        let (main, one, two) = (
            PathBuf::from("/repo/.git/FETCH_HEAD"),
            PathBuf::from("/repo/.git/worktrees/one/FETCH_HEAD"),
            PathBuf::from("/repo/.git/worktrees/two/FETCH_HEAD"),
        );
        let records = |list: &[(&PathBuf, Duration, bool)]| {
            FetchRecord::At(
                list.iter()
                    .map(|(file, d, future)| ((*file).clone(), if *future { today() + *d } else { today() - *d }))
                    .collect(),
            )
        };

        // The main checkout is stale, a linked worktree fetched yesterday: fresh.
        assert_eq!(staleness(&records(&[(&main, 9 * DAY, false), (&one, DAY, false)]), today()), None);
        // The newest of several stale ones is the one named.
        assert_eq!(
            staleness(&records(&[(&main, 9 * DAY, false), (&one, 4 * DAY, false), (&two, 6 * DAY, false)]), today()),
            Some(Staleness::Old { file: one.clone(), age: 4 * DAY })
        );
        // A record from the future beside a fresh one is ignored; beside a stale one it does not make it fresh.
        assert_eq!(staleness(&records(&[(&main, DAY, false), (&one, 2 * DAY, true)]), today()), None);
        assert_eq!(
            staleness(&records(&[(&main, 5 * DAY, false), (&one, 2 * DAY, true)]), today()),
            Some(Staleness::Old { file: main.clone(), age: 5 * DAY })
        );
        // Only when every record is in the future is it the clock; the nearest is named.
        assert_eq!(
            staleness(&records(&[(&main, 3 * DAY, true), (&one, 2 * DAY, true)]), today()),
            Some(Staleness::InFuture { file: one, ahead: 2 * DAY })
        );
    }

    #[test]
    fn the_test_clock_is_honoured_only_for_a_store_that_says_it_is_a_test_store() {
        let tmp = tempfile::tempdir().unwrap();
        let goldens = Goldens::new(tmp.path().join("store"), tmp.path().join("manifests"), tmp.path().join("out"));
        let a_year_on = (today() + 365 * DAY).duration_since(UNIX_EPOCH).unwrap().as_secs().to_string();

        // Not set: the machine's clock, marker or not.
        let before = SystemTime::now();
        assert!(clock(&goldens, None).unwrap() >= before);

        // Set, no marker (no store at all, a store without the file, a folder of that name): refused, naming both.
        let refused = |case: &str| {
            let Err(GoldenError::GcRefused { reason, fix }) = clock(&goldens, Some(&a_year_on)) else {
                panic!("{case}: a clock was taken from the environment of a store without the marker");
            };
            assert!(reason.contains(TEST_CLOCK_ENV) && reason.contains(TEST_CLOCK_MARKER), "{case}: {reason}");
            assert!(reason.contains(&goldens.store().display().to_string()), "{case}: {reason}");
            assert_eq!(fix, format!("unset {TEST_CLOCK_ENV}"));
        };
        refused("no store");
        fs::create_dir_all(goldens.store()).unwrap();
        refused("no marker");
        fs::create_dir(goldens.store().join(TEST_CLOCK_MARKER)).unwrap();
        refused("a folder is no marker");

        // With the marker (a file): the time asked for, to the second, and only if it is whole seconds.
        fs::remove_dir(goldens.store().join(TEST_CLOCK_MARKER)).unwrap();
        fs::write(goldens.store().join(TEST_CLOCK_MARKER), b"").unwrap();
        assert_eq!(clock(&goldens, Some(&a_year_on)).unwrap(), today() + 365 * DAY);
        assert_eq!(clock(&goldens, Some(" 86400\n")).unwrap(), UNIX_EPOCH + DAY);
        for bad in ["", "soon", "-5", "1.5", "0x10"] {
            let Err(GoldenError::GcRefused { reason, .. }) = clock(&goldens, Some(bad)) else {
                panic!("'{bad}' was taken for a time");
            };
            assert!(
                reason.contains(&format!("{TEST_CLOCK_ENV} must be whole seconds since 1970, not '{bad}'")),
                "{reason}"
            );
        }
    }

    #[test]
    fn warnings_name_the_exact_file_and_say_what_to_do() {
        let file = PathBuf::from("/repo/.git/FETCH_HEAD");
        let old = Warning::StaleRemotes(Staleness::Old { file: file.clone(), age: 5 * DAY }).to_string();
        assert!(old.contains("/repo/.git/FETCH_HEAD") && old.contains("5 days ago"), "{old}");
        assert!(old.contains("git fetch --all") && old.contains("--apply refuses"), "{old}");
        let never = Warning::StaleRemotes(Staleness::Never { file: file.clone() }).to_string();
        assert!(never.contains("no `git fetch` is recorded") && never.contains("/repo/.git/FETCH_HEAD"), "{never}");
        let future = Staleness::InFuture { file, ahead: Duration::from_hours(1) }.to_string();
        assert!(future.contains("1 hour in the future"), "{future}");
        // To the nearest unit: an age a second short of three days is not "2 days".
        assert_eq!(span(Duration::from_secs(3 * 86_400 - 1)), "3 days");
        assert_eq!(span(DAY + Duration::from_hours(11)), "1 day");
        assert_eq!(span(Duration::from_mins(61)), "1 hour");

        let path = PathBuf::from("/store/objects/x.png");
        let skew =
            Warning::FutureTime { path: path.clone(), field: "ctime", ahead: Duration::from_mins(7) }.to_string();
        assert!(
            skew.contains("/store/objects/x.png") && skew.contains("its ctime is 7 minutes in the future"),
            "{skew}"
        );
        let blind = Warning::NoChangeTime { path }.to_string();
        assert!(blind.contains("/store/objects/x.png") && blind.contains("neither a change time"), "{blind}");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_system_that_reports_no_real_ctime_gives_no_reading() {
        assert_eq!(unix_time(0, 0), None, "the epoch is what a file system without the field reports");
        assert_eq!(unix_time(-5, 0), None);
        assert_eq!(unix_time(5, -1), None);
        assert_eq!(unix_time(5, 1_000_000_000), None, "nanoseconds are below a second");
        assert_eq!(unix_time(5, 7), Some(UNIX_EPOCH + Duration::new(5, 7)));
        let _ = unix_time(i64::MAX, 999_999_999); // a huge reading is a value or nothing, never a panic
    }

    #[cfg(unix)]
    #[test]
    fn the_readings_of_a_real_file_are_the_ones_the_kernel_keeps() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("image.png");
        fs::write(&path, b"x").unwrap();
        let old = SystemTime::now() - 30 * DAY;
        fs::File::options().write(true).open(&path).unwrap().set_modified(old).unwrap();

        let stamp = Stamp::of(&fs::metadata(&path).unwrap()).unwrap();
        assert_eq!(stamp.modified, old);
        let changed = stamp.changed.expect("a unix file has a ctime");
        assert!(changed > old + 29 * DAY, "setting the mtime back changed the file: the ctime is now, not 30 days ago");
        assert!(stamp.age(SystemTime::now()) < MIN_AGE);
    }
}
