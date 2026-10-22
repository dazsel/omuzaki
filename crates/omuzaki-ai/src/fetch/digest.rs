//! SHA-256 of files and of model manifests: the only place either is computed.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use omuzaki_ai_api::{FetchError, ModelManifest, ModelRef, Progress, Sha256};
use sha2::{Digest, Sha256 as Hasher};

use super::io_err;

/// How much is read from a file per call while hashing: small enough to stay out of the way, large enough to be fast.
const CHUNK: usize = 64 * 1024;

/// Copies `from` into `to` in chunks while hashing, so a multi-GB file is never held in memory, and checks for
/// cancellation between chunks. Each side comes with its path, for the error. Returns the SHA-256 and the length.
///
/// # Errors
/// [`FetchError::Io`] naming the side that failed; [`FetchError::Cancelled`] when `progress` says stop.
pub(super) fn copy_hashing(
    (from, from_path): (&mut dyn Read, &Path),
    (to, to_path): (&mut dyn Write, &Path),
    progress: &dyn Progress,
) -> Result<(Sha256, u64), FetchError> {
    let (mut hasher, mut size, mut buf) = (Hasher::new(), 0u64, vec![0u8; CHUNK]);
    loop {
        if progress.is_cancelled() {
            return Err(FetchError::Cancelled);
        }
        let n = from.read(&mut buf).map_err(|e| io_err(from_path, &e))?;
        if n == 0 {
            return Ok((finish(hasher), size));
        }
        hasher.update(&buf[..n]);
        to.write_all(&buf[..n]).map_err(|e| io_err(to_path, &e))?;
        size += n as u64;
    }
}

/// The SHA-256 of the file at `path`, read in chunks so a multi-GB model is never held in memory.
///
/// # Errors
/// [`FetchError::Io`] if the file can't be opened or read.
pub fn sha256_file(path: &Path) -> Result<Sha256, FetchError> {
    let mut file = File::open(path).map_err(|e| io_err(path, &e))?;
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf).map_err(|e| io_err(path, &e))?;
        if n == 0 {
            return Ok(finish(hasher));
        }
        hasher.update(&buf[..n]);
    }
}

/// The identity of a model: its id and version, and the SHA-256 of its [`ModelManifest::digest_text`]. This is the
/// only place that digest is computed, so it can't drift between callers; reordering the files of a manifest keeps
/// it, because `digest_text` sorts them.
pub fn model_ref(manifest: &ModelManifest) -> ModelRef {
    let mut hasher = Hasher::new();
    hasher.update(manifest.digest_text().as_bytes());
    ModelRef { id: manifest.id.clone(), version: manifest.version, digest: finish(hasher) }
}

/// The finished digest as the contract's type.
pub(super) fn finish(hasher: Hasher) -> Sha256 {
    Sha256(hasher.finalize().into())
}
