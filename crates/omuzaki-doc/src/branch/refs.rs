//! Branch refs (design 4.3 and 5.6): `.omuzaki/branches/<id>/ref.toml`, one per branch, written atomically and only
//! after the point it names is in the store.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use omuzaki_core::fsx::write_atomic;
use omuzaki_history_api::WallTime;
use omuzaki_history_api::branch::{BranchError, BranchId, BranchInfo, BranchName, LaneState};
use omuzaki_store_api::Store;

use super::{branch_root, branches_dir};

/// How long a deleted branch stays in the bin before [`Refs::purge`] removes it: 30 days.
pub const BIN_DAYS: u64 = 30;

/// The ref's file name inside the branch's folder.
const REF_FILE: &str = "ref.toml";

/// The refs of one project. Every write goes through one mutex, so one process never interleaves two read-modify-write
/// steps; each file is replaced atomically, so a crash leaves the old ref or the new one.
pub struct Refs {
    project_dir: PathBuf,
    write: Mutex<()>,
}

impl std::fmt::Debug for Refs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Refs").field("project_dir", &self.project_dir).finish_non_exhaustive()
    }
}

fn io(path: &Path, e: impl std::fmt::Display) -> BranchError {
    BranchError::Io(format!("{}: {e}", path.display()))
}

/// Whether `a` and `b` are the same name, as names compare (case-insensitively, design 4.7).
fn same_name(a: &BranchName, b: &BranchName) -> bool {
    a.as_str().eq_ignore_ascii_case(b.as_str())
}

impl Refs {
    /// The refs of the project in `project_dir`.
    pub fn new(project_dir: impl Into<PathBuf>) -> Self {
        Self { project_dir: project_dir.into(), write: Mutex::new(()) }
    }

    /// The project folder.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    fn guard(&self) -> MutexGuard<'_, ()> {
        self.write.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn path(&self, id: &BranchId) -> PathBuf {
        let root = if id.is_main() {
            branches_dir(&self.project_dir).join(id.as_str())
        } else {
            branch_root(&self.project_dir, id)
        };
        root.join(REF_FILE)
    }

    /// The ref of `id`, in the bin or not; `None` when there is none.
    ///
    /// # Errors
    /// [`BranchError::Io`] when it exists and doesn't read.
    pub fn read(&self, id: &BranchId) -> Result<Option<BranchInfo>, BranchError> {
        let path = self.path(id);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut info: BranchInfo = toml::from_str(&text).map_err(|e| io(&path, e))?;
                if info.id != *id {
                    return Err(io(&path, format!("names branch {} but is filed under {id}", info.id)));
                }
                info.lane = LaneState::Closed;
                Ok(Some(info))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io(&path, e)),
        }
    }

    /// Every ref, the bin's included: `main` first, then by when each was made. A ref that doesn't read is skipped
    /// with a warning, so one damaged file doesn't hide the others.
    ///
    /// # Errors
    /// [`BranchError::Io`] when the folder exists and can't be listed.
    pub fn list(&self) -> Result<Vec<BranchInfo>, BranchError> {
        let dir = branches_dir(&self.project_dir);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io(&dir, e)),
        };
        let mut refs = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let Some(id) = entry.file_name().to_str().and_then(|name| name.parse::<BranchId>().ok()) else { continue };
            match self.read(&id) {
                Ok(Some(info)) => refs.push(info),
                Ok(None) => {}
                Err(error) => tracing::warn!(branch = %id, %error, "a branch ref doesn't read; skipped"),
            }
        }
        refs.sort_by(|a, b| (!a.id.is_main(), a.created, &a.id).cmp(&(!b.id.is_main(), b.created, &b.id)));
        Ok(refs)
    }

    /// The live branch called `name` (case-insensitively); `None` when no live branch has it.
    ///
    /// # Errors
    /// As [`list`](Self::list).
    pub fn by_name(&self, name: &BranchName) -> Result<Option<BranchInfo>, BranchError> {
        Ok(self.list()?.into_iter().find(|info| info.deleted.is_none() && same_name(&info.name, name)))
    }

    /// Writes `info` as its branch's ref, replacing the old one. Refused when `store` doesn't hold the point its head
    /// names, so a ref never names a missing point.
    ///
    /// # Errors
    /// [`BranchError::PointNotFound`] for a head the store lacks; [`BranchError::Io`].
    pub fn write(&self, info: &BranchInfo, store: &dyn Store) -> Result<(), BranchError> {
        let _guard = self.guard();
        self.write_locked(info, store)
    }

    fn write_locked(&self, info: &BranchInfo, store: &dyn Store) -> Result<(), BranchError> {
        if !store.contains(&info.head.0) {
            return Err(BranchError::PointNotFound(info.head));
        }
        let path = self.path(&info.id);
        let mut table = toml::Table::try_from(info).map_err(|e| io(&path, e))?;
        // What the lane is doing is the host's to say, never stored.
        table.remove("lane");
        let text = toml::to_string(&table).map_err(|e| io(&path, e))?;
        write_atomic(&path, &text).map_err(|e| io(&path, e))
    }

    /// Writes the ref of a new branch. Refused when the id has a ref already or a live branch has the name.
    ///
    /// # Errors
    /// [`BranchError::NameTaken`], [`BranchError::Invalid`] for an id in use, and as [`write`](Self::write).
    pub fn create(&self, info: &BranchInfo, store: &dyn Store) -> Result<(), BranchError> {
        let _guard = self.guard();
        if self.read(&info.id)?.is_some() {
            return Err(BranchError::Invalid(format!("branch id {} is in use", info.id)));
        }
        self.refuse_taken(&info.name, &info.id)?;
        self.write_locked(info, store)
    }

    fn refuse_taken(&self, name: &BranchName, except: &BranchId) -> Result<(), BranchError> {
        let taken =
            self.list()?.into_iter().any(|b| b.deleted.is_none() && b.id != *except && same_name(&b.name, name));
        if taken { Err(BranchError::NameTaken(name.clone())) } else { Ok(()) }
    }

    /// Reads `id`'s ref, lets `change` edit it, and writes it back, all under the write lock. `change` may refuse.
    ///
    /// # Errors
    /// [`BranchError::NotFound`] when there is no such ref; what `change` returns; as [`write`](Self::write).
    pub fn update(
        &self,
        id: &BranchId,
        store: &dyn Store,
        change: impl FnOnce(&mut BranchInfo) -> Result<(), BranchError>,
    ) -> Result<BranchInfo, BranchError> {
        let _guard = self.guard();
        let mut info = self.read(id)?.ok_or_else(|| BranchError::NotFound(id.to_string()))?;
        change(&mut info)?;
        self.write_locked(&info, store)?;
        Ok(info)
    }

    /// Renames a live branch and sets its title; the id doesn't change (design 5.6). `main` keeps its name.
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::Refused`] for `main`, [`BranchError::NameTaken`].
    pub fn rename(
        &self,
        id: &BranchId,
        name: BranchName,
        title: Option<String>,
        store: &dyn Store,
    ) -> Result<BranchInfo, BranchError> {
        if id.is_main() {
            return Err(BranchError::Refused("`main` keeps its name".to_owned()));
        }
        let _guard = self.guard();
        let mut info = self.live_locked(id)?;
        self.refuse_taken(&name, id)?;
        info.name = name;
        info.title = title;
        self.write_locked(&info, store)?;
        Ok(info)
    }

    fn live_locked(&self, id: &BranchId) -> Result<BranchInfo, BranchError> {
        match self.read(id)? {
            Some(info) if info.deleted.is_none() => Ok(info),
            _ => Err(BranchError::NotFound(id.to_string())),
        }
    }

    /// Puts a live branch in the bin at `at`. `main` is never deleted; the host refuses the folder branch and a
    /// branch with an open lane before calling this.
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::Refused`] for `main`.
    pub fn delete(&self, id: &BranchId, at: WallTime, store: &dyn Store) -> Result<BranchInfo, BranchError> {
        if id.is_main() {
            return Err(BranchError::Refused("`main` can't be deleted".to_owned()));
        }
        let _guard = self.guard();
        let mut info = self.live_locked(id)?;
        info.deleted = Some(at);
        self.write_locked(&info, store)?;
        Ok(info)
    }

    /// Brings a branch back from the bin, unless a live branch took its name meanwhile.
    ///
    /// # Errors
    /// [`BranchError::NotFound`] when it isn't in the bin; [`BranchError::NameTaken`].
    pub fn undelete(&self, id: &BranchId, store: &dyn Store) -> Result<BranchInfo, BranchError> {
        let _guard = self.guard();
        let mut info = match self.read(id)? {
            Some(info) if info.deleted.is_some() => info,
            _ => return Err(BranchError::NotFound(id.to_string())),
        };
        self.refuse_taken(&info.name, id)?;
        info.deleted = None;
        self.write_locked(&info, store)?;
        Ok(info)
    }

    /// Removes the branches that have been in the bin for [`BIN_DAYS`] by `now`: their folders (ref, journal and
    /// archive) go, and their points become collectable. Returns the ids removed.
    ///
    /// # Errors
    /// [`BranchError::Io`] when a folder can't be removed; those removed before stay removed.
    pub fn purge(&self, now: WallTime) -> Result<Vec<BranchId>, BranchError> {
        let _guard = self.guard();
        let keep_ms = BIN_DAYS * 24 * 3600 * 1000;
        let mut purged = Vec::new();
        for info in self.list()? {
            let Some(deleted) = info.deleted else { continue };
            if info.id.is_main() || now.0.saturating_sub(deleted.0) < keep_ms {
                continue;
            }
            let root = branch_root(&self.project_dir, &info.id);
            std::fs::remove_dir_all(&root).map_err(|e| io(&root, e))?;
            purged.push(info.id);
        }
        Ok(purged)
    }
}
