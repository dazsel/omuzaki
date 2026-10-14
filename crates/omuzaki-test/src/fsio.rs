//! Small file-system helpers shared by the object store, the manifests and the evidence folder.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{GoldenError, io_err};

/// Creates the folder `path` lives in.
pub(crate) fn ensure_parent(path: &Path) -> Result<(), GoldenError> {
    match path.parent() {
        Some(dir) => fs::create_dir_all(dir).map_err(io_err(dir)),
        None => Ok(()),
    }
}

/// Writes `bytes` to `path` so that nobody ever sees half a file: other tests may be reading it right now.
///
/// The bytes go to a staging file first, are flushed to disk, and are then renamed over the target. The flush matters
/// after a crash: without it a power cut can leave the rename done and the contents empty. The staging name is unique
/// per call (process id plus a counter) because two tests blessing the same image at once both write the same object.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), GoldenError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    ensure_parent(path)?;
    let mut name = path.file_name().map(std::ffi::OsString::from).unwrap_or_default();
    name.push(format!(".{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let staging = path.with_file_name(name);
    write_synced(&staging, bytes).map_err(io_err(&staging))?;
    fs::rename(&staging, path).map_err(|e| {
        // Best effort: a leftover staging file is harmless, and the rename error is the one worth reporting.
        let _ = fs::remove_file(&staging);
        io_err(path)(e)
    })
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// The entries of `dir` in name order, so output never depends on directory order. A missing folder is empty.
pub(crate) fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>, GoldenError> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io_err(dir)(e)),
    };
    let mut paths = entries.map(|entry| entry.map(|e| e.path())).collect::<Result<Vec<_>, _>>().map_err(io_err(dir))?;
    paths.sort();
    Ok(paths)
}
