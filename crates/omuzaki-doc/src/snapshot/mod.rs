//! Snapshots (design section 7): the content of the project folder at one journal position, in a content-addressed
//! store, so a big file is stored once however many snapshots hold it.
//!
//! - **What's in one.** Every project-owned file, deduplicated by hash: the tree's TOML files and the files beside them
//!   (`assets.lock`, takes). Left out: `.omuzaki/`, names starting with `.`, symbolic links, names that aren't UTF-8
//!   (reported), and the derived paths crates declare with `register_derived_path!`. A file over the huge threshold
//!   (1 GB) is included, with a warning naming it. Nothing is dropped silently.
//! - **How it's stored.** A [`Manifest`] (canonical JSON; its hash is the snapshot id), one pack per snapshot holding
//!   the small files (under 1 MiB) that changed since the parent, and large files as objects of their own. Assets are
//!   listed by hash, never copied. The history export at the snapshot's LSN is one more object.
//! - **Published last.** Everything is read back through `Store::get` (which re-hashes) before the ref is written, and
//!   the ref is written atomically, so a ref never names an incomplete snapshot.
//! - **Refs** (`snapshots/refs/*.toml`) hold the facts ([`SnapshotInfo`]), newest first by LSN.
//! - **Two ways to make one.** [`Snapshotter::take_rendered`] renders the shadow project's in-memory texts (what a save
//!   writes); [`Snapshotter::take_raw`] keeps the bytes on disk as they are, even a broken folder.
//! - **Garbage collection** ([`Snapshotter::sweep`]) removes objects no ref reaches, after a grace period.

mod manifest;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use omuzaki_core::fsx::write_atomic;
use omuzaki_history_api::{
    HistoryExport, Lsn, SnapshotError, SnapshotId, SnapshotInfo, Trigger, Verification, VerifyHow, WallTime, is_derived,
};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, StoreError as ObjectError, StoreGc, SweepReport};

pub use manifest::{FileRow, MANIFEST_FORMAT, Manifest};

use crate::Project;
use crate::disk::PROJECT_FILE;

/// The folder inside `.omuzaki/` that holds the snapshot store and its refs.
pub const SNAPSHOT_DIR: &str = "snapshots";

/// Files of at least this size are objects of their own; smaller ones go in the snapshot's pack.
pub const LARGE_FILE: u64 = 1 << 20;

/// Files over this size are still included, and named in a warning.
pub const HUGE_FILE: u64 = 1 << 30;

/// Objects younger than this are never swept, so one written for a ref not yet published is never taken.
pub const SWEEP_GRACE: Duration = Duration::from_secs(3600);

/// The asset list beside the tree, whose rows the manifest's `assets` lists.
const ASSETS_LOCK: &str = "assets.lock";

/// The extension of the tree's files.
const TOML: &str = ".toml";

/// The snapshot store and refs folder of the project in `project_dir`: `.omuzaki/snapshots/`.
pub fn snapshot_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".omuzaki").join(SNAPSHOT_DIR)
}

/// The refs folder.
fn refs_dir(project_dir: &Path) -> PathBuf {
    snapshot_dir(project_dir).join("refs")
}

/// The facts of a snapshot the caller knows before it is taken.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Facts {
    /// The journal position it represents.
    pub lsn: Lsn,
    /// The session it is taken in.
    pub session: String,
    /// Why.
    pub trigger: Trigger,
    /// When.
    pub created: WallTime,
    /// The last transaction's label.
    pub label: String,
    /// A named version's name.
    pub name: Option<String>,
    /// A take's stem.
    pub stem: Option<String>,
    /// A pin.
    pub keep_until: Option<WallTime>,
}

impl Facts {
    /// The facts of a snapshot at `lsn` in `session`, taken for `trigger` at `created`.
    pub fn new(lsn: Lsn, session: impl Into<String>, trigger: Trigger, created: WallTime) -> Self {
        Self {
            lsn,
            session: session.into(),
            trigger,
            created,
            label: String::new(),
            name: None,
            stem: None,
            keep_until: None,
        }
    }

    /// These facts with the last transaction's label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// These facts with a named version's name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// These facts with a take's stem.
    #[must_use]
    pub fn with_stem(mut self, stem: impl Into<String>) -> Self {
        self.stem = Some(stem.into());
        self
    }

    /// These facts pinned until `until`.
    #[must_use]
    pub fn pinned_until(mut self, until: WallTime) -> Self {
        self.keep_until = Some(until);
        self
    }
}

/// What a take stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Written {
    /// Files whose bytes were already stored (in the parent's pack, or as objects).
    pub reused: usize,
    /// Small files written into this snapshot's pack.
    pub packed: usize,
    /// Large files written as objects of their own.
    pub objects: usize,
    /// Bytes written to the store, manifest and history included.
    pub bytes: u64,
}

/// A snapshot just taken.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Taken {
    /// Its ref, as written.
    pub info: SnapshotInfo,
    /// Its manifest.
    pub manifest: Manifest,
    /// What was stored.
    pub written: Written,
    /// Files included with a warning (over the huge threshold), and names left out because they aren't UTF-8.
    pub warnings: Vec<String>,
}

/// One file to store: its path and bytes.
struct Entry {
    path: String,
    bytes: Vec<u8>,
}

/// Takes, lists, loads and sweeps the snapshots of one project folder, over an injected store (a `DiskStore` at
/// `.omuzaki/snapshots/` in the app; a `MemStore` in tests). The keeper is its only writer.
pub struct Snapshotter {
    dir: PathBuf,
    refs: PathBuf,
    store: Arc<dyn StoreGc>,
    large: u64,
    huge: u64,
    /// The manifest of the newest snapshot this process took or read: where unchanged files already are.
    parent: Mutex<Option<Manifest>>,
}

impl std::fmt::Debug for Snapshotter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshotter").field("dir", &self.dir).finish_non_exhaustive()
    }
}

/// The provenance every snapshot object gets: made here, internal, never published.
fn internal() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "LicenseRef-Omuzaki-Internal".to_owned(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    }
}

fn io_error(error: impl std::fmt::Display) -> SnapshotError {
    SnapshotError::Io(error.to_string())
}

impl Snapshotter {
    /// The snapshots of the project in `project_dir`, kept in `store`.
    pub fn new(project_dir: impl Into<PathBuf>, store: Arc<dyn StoreGc>) -> Self {
        let dir = project_dir.into();
        Self { refs: refs_dir(&dir), dir, store, large: LARGE_FILE, huge: HUGE_FILE, parent: Mutex::new(None) }
    }

    /// The same, with other size thresholds (tests).
    #[must_use]
    pub fn with_thresholds(mut self, large: u64, huge: u64) -> Self {
        self.large = large;
        self.huge = huge;
        self
    }

    /// The project folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The store the snapshots are kept in.
    pub fn store(&self) -> &Arc<dyn StoreGc> {
        &self.store
    }

    /// Takes a snapshot of `project` as a save would leave it (rendered from memory), with the files beside the tree
    /// as they are on disk, and `history`.
    ///
    /// # Errors
    /// [`SnapshotError::Io`] if a file can't be read or rendered, or the store or the ref can't be written;
    /// [`SnapshotError::Corrupt`] if what was written doesn't read back.
    pub fn take_rendered(
        &self,
        project: &Project,
        facts: Facts,
        history: &HistoryExport,
    ) -> Result<Taken, SnapshotError> {
        let mut warnings = Vec::new();
        let mut entries: Vec<Entry> = project
            .rendered_files()
            .map_err(io_error)?
            .into_iter()
            .map(|file| Entry { path: slash(&file.path), bytes: file.text.into_bytes() })
            .collect();
        // The tree's files come from memory; everything else from disk.
        walk(&self.dir, &mut Vec::new(), &mut |rel| !rel.ends_with(TOML), &mut entries, &mut warnings)?;
        let schema = project.schema_version().unwrap_or(0);
        self.take(entries, schema, facts, history, warnings)
    }

    /// Takes a snapshot of the folder's bytes as they are, unparsed (before a migration or a recovery writes
    /// anything): even a folder that doesn't open is kept exactly.
    ///
    /// # Errors
    /// As [`take_rendered`](Self::take_rendered).
    pub fn take_raw(&self, facts: Facts, history: &HistoryExport) -> Result<Taken, SnapshotError> {
        let mut warnings = Vec::new();
        let mut entries = Vec::new();
        walk(&self.dir, &mut Vec::new(), &mut |_| true, &mut entries, &mut warnings)?;
        let schema = entries
            .iter()
            .find(|e| e.path == PROJECT_FILE)
            .and_then(|e| std::str::from_utf8(&e.bytes).ok())
            .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
            .and_then(|doc| doc.get("schema").and_then(toml_edit::Item::as_integer))
            .and_then(|schema| u32::try_from(schema).ok())
            .unwrap_or(0);
        self.take(entries, schema, facts, history, warnings)
    }

    fn take(
        &self,
        entries: Vec<Entry>,
        schema: u32,
        facts: Facts,
        history: &HistoryExport,
        mut warnings: Vec<String>,
    ) -> Result<Taken, SnapshotError> {
        let parent = self.parent_manifest();
        let known: HashMap<ContentHash, (Option<ContentHash>, Option<u64>)> =
            parent.iter().flat_map(|m| m.files.iter()).map(|row| (row.hash, (row.pack, row.offset))).collect();
        let mut written = Written::default();
        let mut rows = Vec::with_capacity(entries.len());
        let mut pack = Vec::new();
        let mut in_pack: Vec<(usize, u64)> = Vec::new();
        let mut pack_offsets: HashMap<ContentHash, u64> = HashMap::new();
        let mut new_objects = Vec::new();
        let mut assets = Vec::new();
        for entry in entries {
            let size = entry.bytes.len() as u64;
            let hash = ContentHash::of(&entry.bytes);
            if entry.path == ASSETS_LOCK {
                match std::str::from_utf8(&entry.bytes)
                    .map_err(|e| e.to_string())
                    .and_then(|text| omuzaki_assets_lock::AssetsLock::parse(text).map_err(|e| e.to_string()))
                {
                    Ok(lock) => assets.extend(lock.rows.iter().map(|row| row.hash)),
                    Err(reason) => {
                        warnings.push(format!("{ASSETS_LOCK} doesn't read, so its assets aren't listed: {reason}"))
                    }
                }
            }
            if size > self.huge {
                warnings.push(format!("{} is {} MB; it is included in the snapshot", entry.path, size >> 20));
            }
            if size >= self.large {
                if !self.store.contains(&hash) {
                    self.store.put(&entry.bytes, internal()).map_err(io_error)?;
                    new_objects.push(hash);
                    written.objects += 1;
                    written.bytes += size;
                } else {
                    written.reused += 1;
                }
                rows.push(FileRow::whole(entry.path, hash, size));
                continue;
            }
            match known.get(&hash) {
                Some((Some(pack), Some(offset))) if self.store.contains(pack) => {
                    rows.push(FileRow::packed(entry.path, hash, size, *pack, *offset));
                    written.reused += 1;
                }
                _ => {
                    // Two files with the same bytes share one place in the pack.
                    let offset = *pack_offsets.entry(hash).or_insert_with(|| {
                        let at = pack.len() as u64;
                        pack.extend_from_slice(&entry.bytes);
                        at
                    });
                    in_pack.push((rows.len(), offset));
                    // The pack's hash is known only once it is whole; the row is completed below.
                    rows.push(FileRow::packed(entry.path, hash, size, hash, 0));
                    written.packed += 1;
                }
            }
        }
        let pack_hash = if in_pack.is_empty() {
            None
        } else {
            let hash = self.store.put(&pack, internal()).map_err(io_error)?;
            written.bytes += pack.len() as u64;
            for (row, offset) in in_pack {
                rows[row].pack = Some(hash);
                rows[row].offset = Some(offset);
            }
            Some(hash)
        };
        let manifest = Manifest::new(schema, rows, assets);
        let manifest_bytes = manifest.to_bytes();
        let id = SnapshotId(self.store.put(&manifest_bytes, internal()).map_err(io_error)?);
        let history_bytes = serde_json::to_vec(history).map_err(io_error)?;
        let history_hash = self.store.put(&history_bytes, internal()).map_err(io_error)?;
        written.bytes += (manifest_bytes.len() + history_bytes.len()) as u64;

        // Read everything new back (`get` re-hashes) before the ref makes the snapshot visible.
        let check = |hash: &ContentHash| -> Result<(), SnapshotError> {
            self.store.get(hash).map(drop).map_err(|e| SnapshotError::Corrupt { id, reason: e.to_string() })
        };
        for hash in [id.0, history_hash].iter().chain(pack_hash.iter()).chain(new_objects.iter()) {
            check(hash)?;
        }
        let mut info = SnapshotInfo::new(id, facts.lsn, facts.session, facts.created, facts.trigger);
        info.label = facts.label;
        info.history = Some(history_hash);
        info.name = facts.name;
        info.stem = facts.stem;
        info.keep_until = facts.keep_until;
        info.verified = Some(Verification::new(facts.created, VerifyHow::Hash, true));
        self.write_ref(&info)?;
        *self.parent.lock().unwrap_or_else(PoisonError::into_inner) = Some(manifest.clone());
        Ok(Taken { info, manifest, written, warnings })
    }

    /// The manifest unchanged files are found in: the last one this process used, or the newest ref's.
    fn parent_manifest(&self) -> Option<Manifest> {
        let mut parent = self.parent.lock().unwrap_or_else(PoisonError::into_inner);
        if parent.is_none() {
            *parent = self.refs().ok()?.into_iter().find_map(|info| self.manifest(&info.id).ok());
        }
        parent.clone()
    }

    /// Writes `info` as its ref file, atomically, replacing the ref of the same snapshot and LSN.
    ///
    /// # Errors
    /// [`SnapshotError::Io`].
    pub fn write_ref(&self, info: &SnapshotInfo) -> Result<(), SnapshotError> {
        let text = toml::to_string(info).map_err(io_error)?;
        fs::create_dir_all(&self.refs).map_err(io_error)?;
        write_atomic(&self.refs.join(ref_name(info)), &text).map_err(io_error)
    }

    /// Removes the ref of `info` (retention, a removed name). Its objects go at the next sweep.
    ///
    /// # Errors
    /// [`SnapshotError::Io`] if it exists and can't be removed.
    pub fn remove_ref(&self, info: &SnapshotInfo) -> Result<(), SnapshotError> {
        match fs::remove_file(self.refs.join(ref_name(info))) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(io_error(e)),
            _ => Ok(()),
        }
    }

    /// Every ref, newest first (by LSN, then time). Refs that don't read are skipped with a warning.
    ///
    /// # Errors
    /// [`SnapshotError::Io`] if the refs folder exists and can't be listed.
    pub fn refs(&self) -> Result<Vec<SnapshotInfo>, SnapshotError> {
        let entries = match fs::read_dir(&self.refs) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error(e)),
        };
        let mut refs = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }
            match fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| toml::from_str::<SnapshotInfo>(&text).map_err(|e| e.to_string()))
            {
                Ok(info) => refs.push(info),
                Err(error) => tracing::warn!(path = %path.display(), %error, "a snapshot ref doesn't read; skipped"),
            }
        }
        refs.sort_by(|a, b| (b.lsn, b.created, b.id).cmp(&(a.lsn, a.created, a.id)));
        Ok(refs)
    }

    /// The manifest of snapshot `id`.
    ///
    /// # Errors
    /// [`SnapshotError::NotFound`] if the store doesn't hold it, [`SnapshotError::Corrupt`] if it doesn't read.
    pub fn manifest(&self, id: &SnapshotId) -> Result<Manifest, SnapshotError> {
        let bytes = self.store.get(&id.0).map_err(|e| self.missing(id, &e))?;
        Manifest::from_bytes(&bytes).map_err(|reason| SnapshotError::Corrupt { id: *id, reason })
    }

    fn missing(&self, id: &SnapshotId, error: &ObjectError) -> SnapshotError {
        match error {
            ObjectError::Missing(hash) if *hash == id.0 => SnapshotError::NotFound(*id),
            other => SnapshotError::Corrupt { id: *id, reason: other.to_string() },
        }
    }

    /// Every file of snapshot `id`, by path, each checked against its hash.
    ///
    /// # Errors
    /// [`SnapshotError::NotFound`] or [`SnapshotError::Corrupt`].
    pub fn files(&self, id: &SnapshotId) -> Result<BTreeMap<String, Vec<u8>>, SnapshotError> {
        let manifest = self.manifest(id)?;
        let corrupt = |reason: String| SnapshotError::Corrupt { id: *id, reason };
        let mut packs: HashMap<ContentHash, Vec<u8>> = HashMap::new();
        let mut files = BTreeMap::new();
        for row in &manifest.files {
            let bytes = match (row.pack, row.offset) {
                (Some(pack), Some(offset)) => {
                    if !packs.contains_key(&pack) {
                        packs.insert(pack, self.store.get(&pack).map_err(|e| corrupt(e.to_string()))?);
                    }
                    let start = usize::try_from(offset).map_err(|e| corrupt(e.to_string()))?;
                    let end = start + usize::try_from(row.size).map_err(|e| corrupt(e.to_string()))?;
                    packs
                        .get(&pack)
                        .and_then(|bytes| bytes.get(start..end))
                        .ok_or_else(|| corrupt(format!("{} runs past the end of its pack", row.path)))?
                        .to_vec()
                }
                _ => self.store.get(&row.hash).map_err(|e| corrupt(format!("{}: {e}", row.path)))?,
            };
            if ContentHash::of(&bytes) != row.hash {
                return Err(corrupt(format!("{} doesn't match its hash", row.path)));
            }
            files.insert(row.path.clone(), bytes);
        }
        Ok(files)
    }

    /// The project as snapshot `id` holds it, loaded from its files by the folder's own rules, saving into the
    /// project folder. Nothing on disk is read or written but the store.
    ///
    /// # Errors
    /// [`SnapshotError::NotFound`] or [`SnapshotError::Corrupt`], including files that don't open as a project.
    pub fn load(&self, id: &SnapshotId) -> Result<Project, SnapshotError> {
        let files = self.files(id)?;
        Project::open_files(&self.dir, files).map_err(|e| SnapshotError::Corrupt { id: *id, reason: e.to_string() })
    }

    /// The history export a ref names (empty for a ref with none).
    ///
    /// # Errors
    /// [`SnapshotError::Corrupt`] if it is missing or doesn't read.
    pub fn history(&self, info: &SnapshotInfo) -> Result<HistoryExport, SnapshotError> {
        let Some(hash) = info.history else { return Ok(HistoryExport::empty()) };
        let corrupt = |reason: String| SnapshotError::Corrupt { id: info.id, reason: format!("its history: {reason}") };
        let bytes = self.store.get(&hash).map_err(|e| corrupt(e.to_string()))?;
        serde_json::from_slice(&bytes).map_err(|e| corrupt(e.to_string()))
    }

    /// The newest snapshot that loads, with its project and history, skipping damaged ones (recovery falls back to the
    /// one before).
    ///
    /// # Errors
    /// [`SnapshotError::Io`] if the refs can't be listed.
    pub fn newest_loadable(&self) -> Result<Option<(SnapshotInfo, Project, HistoryExport)>, SnapshotError> {
        for info in self.refs()? {
            match self.load(&info.id).and_then(|project| Ok((project, self.history(&info)?))) {
                Ok((project, history)) => return Ok(Some((info, project, history))),
                Err(error) => tracing::warn!(id = %info.id, %error, "a snapshot doesn't load; trying the one before"),
            }
        }
        Ok(None)
    }

    /// Removes store objects that no ref reaches and that are older than `grace` (design 7.3).
    ///
    /// # Errors
    /// [`SnapshotError::Io`] from listing refs or the store.
    pub fn sweep(&self, grace: Duration, now: SystemTime) -> Result<SweepReport, SnapshotError> {
        let mut reachable = BTreeSet::new();
        for info in self.refs()? {
            reachable.insert(info.id.0);
            reachable.extend(info.history);
            reachable.extend(info.thumbnail);
            match self.manifest(&info.id) {
                Ok(manifest) => reachable.extend(manifest.objects()),
                // A ref whose manifest is unreadable can't say what it needs; nothing it might need is taken, because
                // a manifest that is gone can't be repaired by keeping its packs either. Keep going with the others.
                Err(error) => tracing::warn!(id = %info.id, %error, "a snapshot's manifest doesn't read"),
            }
        }
        self.store.sweep(&|hash| reachable.contains(hash), grace, now).map_err(io_error)
    }
}

/// The ref file's name: the time, the LSN and the id's first 12 digits, so names sort by time and two snapshots of
/// one moment don't collide.
fn ref_name(info: &SnapshotInfo) -> String {
    let id = info.id.to_string();
    format!("{}-{:020}-{}.toml", utc_name(info.created), info.lsn.0, &id[..id.len().min(12)])
}

/// `20261012T143205.123Z` for a wall time in milliseconds since the Unix epoch.
fn utc_name(at: WallTime) -> String {
    let ms = at.0;
    let (days, rest) = (ms / 86_400_000, ms % 86_400_000);
    let (h, m, s, milli) = (rest / 3_600_000, rest / 60_000 % 60, rest / 1000 % 60, rest % 1000);
    let (y, mo, d) = civil_from_days(i64::try_from(days).unwrap_or(i64::MAX));
    format!("{y:04}{mo:02}{d:02}T{h:02}{m:02}{s:02}.{milli:03}Z")
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, u32::try_from(m).unwrap_or(1), u32::try_from(d).unwrap_or(1))
}

/// `a/b/c.toml` for a relative path.
fn slash(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

/// Adds every project-owned file under `dir/rel` that `include` accepts to `entries`: not hidden, not a link, not a
/// derived path. Names that aren't UTF-8 are reported in `warnings`.
fn walk(
    dir: &Path,
    rel: &mut Vec<String>,
    include: &mut dyn FnMut(&str) -> bool,
    entries: &mut Vec<Entry>,
    warnings: &mut Vec<String>,
) -> Result<(), SnapshotError> {
    let mut path = dir.to_path_buf();
    path.extend(rel.iter());
    let mut found: Vec<(String, fs::FileType)> = Vec::new();
    for entry in fs::read_dir(&path).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        match entry.file_name().into_string() {
            Ok(name) if name.starts_with('.') => {}
            Ok(name) => found.push((name, entry.file_type().map_err(io_error)?)),
            Err(name) => {
                warnings.push(format!("{} isn't kept in snapshots: its name isn't UTF-8", path.join(name).display()))
            }
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, kind) in found {
        rel.push(name);
        let joined = rel.join("/");
        if kind.is_dir() {
            if !is_derived(&joined) {
                walk(dir, rel, include, entries, warnings)?;
            }
        } else if kind.is_file() && !is_derived(&joined) && include(&joined) {
            let mut file = dir.to_path_buf();
            file.extend(rel.iter());
            entries.push(Entry { path: joined, bytes: fs::read(&file).map_err(io_error)? });
        }
        rel.pop();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_names_sort_by_time_and_read_as_utc() {
        assert_eq!(utc_name(WallTime(0)), "19700101T000000.000Z");
        assert_eq!(utc_name(WallTime(1_791_815_525_123)), "20261012T143205.123Z");
        assert!(utc_name(WallTime(9_999)) < utc_name(WallTime(10_000)));
    }
}
