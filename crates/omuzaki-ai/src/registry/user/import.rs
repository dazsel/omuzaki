//! Copying a person's file into the cache: hashed while it is copied, stored under its hash.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use omuzaki_ai_api::{Progress, RegistryError, Sha256};
use sha2::{Digest, Sha256 as Hasher};

/// Bytes copied per read, so a multi-GB model is never held in memory.
const CHUNK: usize = 64 * 1024;

/// One file now in the cache.
#[derive(Clone, Debug)]
pub(super) struct Imported {
    /// The original's file name, which the manifest calls it by.
    pub name: String,
    pub sha256: Sha256,
    pub size: u64,
}

/// Where the cache keeps the blob with hash `sha256` (the layout `VerifiedCache` reads and `FileSource::lookup`
/// finds again).
pub(super) fn blob_path(cache: &Path, sha256: Sha256) -> PathBuf {
    cache.join("sha256").join(sha256.to_string())
}

fn io_error(what: &str, e: &std::io::Error) -> RegistryError {
    RegistryError::Io(format!("{what}: {e}"))
}

/// Copies `source` into `cache` under its SHA-256, by atomic rename, and returns what it found. Errors name the file,
/// never the folder it came from.
pub(super) fn import(cache: &Path, source: &Path, progress: &dyn Progress) -> Result<Imported, RegistryError> {
    let name = source.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let mut from = File::open(source).map_err(|e| io_error(&name, &e))?;
    let dir = cache.join("sha256");
    fs::create_dir_all(&dir).map_err(|e| io_error("the model cache", &e))?;
    let mut tmp =
        tempfile::Builder::new().suffix(".tmp").tempfile_in(&dir).map_err(|e| io_error("the model cache", &e))?;
    let (mut hasher, mut size, mut buf) = (Hasher::new(), 0u64, vec![0u8; CHUNK]);
    loop {
        if progress.is_cancelled() {
            return Err(RegistryError::Cancelled);
        }
        let n = from.read(&mut buf).map_err(|e| io_error(&name, &e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        tmp.write_all(&buf[..n]).map_err(|e| io_error("the model cache", &e))?;
        size += n as u64;
    }
    tmp.as_file().sync_all().map_err(|e| io_error("the model cache", &e))?;
    let sha256 = Sha256(hasher.finalize().into());
    tmp.persist(blob_path(cache, sha256)).map_err(|e| io_error("the model cache", &e.error))?;
    Ok(Imported { name, sha256, size })
}
