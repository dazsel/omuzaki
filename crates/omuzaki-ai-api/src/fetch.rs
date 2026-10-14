//! Verified downloads: a file pinned by its URLs, size and SHA-256; the [`Fetcher`] the app supplies (its HTTPS
//! client); and the [`FileSource`] that hands out verified local copies, downloading on first need.
//!
//! Model weights, runtime libraries (ONNX Runtime per platform), eval fixtures and fallback fonts all go through this
//! one path, so "pinned, size-capped, hash-checked, cached atomically" is implemented once (`omuzaki-ai`) and the
//! installer stays under 150 MB (spec §10 `installer.size`, QG-06).

use std::io::Read;
use std::path::PathBuf;

use crate::{Progress, Sha256};

/// Files at least this large are downloaded resumably: an interrupted download keeps its partial file and continues
/// from where it stopped (an HTTP range request), and the whole file is hashed again at the end. 500 MiB.
pub const RESUME_MIN_SIZE: u64 = 500 * 1024 * 1024;

/// A file we ship by reference: where it can be fetched, how long it is and what it hashes to.
///
/// `urls` is an ordered list of places with the same bytes (the upstream first, then any mirror). The hash makes them
/// interchangeable: a dead link, a wrong length or a wrong hash at one falls through to the next, so a mirror is one
/// more URL in a manifest and never a code change. Each URL names an immutable revision (a commit or release tag).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedFile {
    /// `https://` URLs of immutable revisions, tried in order.
    pub urls: Vec<String>,
    /// The SHA-256 the bytes must have.
    pub sha256: Sha256,
    /// The exact length in bytes. A download is cut off one byte past it, so a wrong or endless body cannot exhaust
    /// memory or disk, and a wrong length is refused before hashing.
    pub size: u64,
}

impl PinnedFile {
    /// A pin from its parts.
    pub fn new<U: Into<String>>(urls: impl IntoIterator<Item = U>, sha256: Sha256, size: u64) -> Self {
        Self { urls: urls.into_iter().map(Into::into).collect(), sha256, size }
    }

    /// True if it should be downloaded resumably ([`RESUME_MIN_SIZE`]).
    pub fn resumable(&self) -> bool {
        self.size >= RESUME_MIN_SIZE
    }
}

/// Why a pinned file could not be had. Says what to do, because people see it (offline, disk full, bad mirror).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FetchError {
    /// The request failed (no network, refused, HTTP error).
    #[error("downloading {url} failed: {reason}; check the connection and try again")]
    Network {
        /// The address.
        url: String,
        /// What the fetcher said.
        reason: String,
    },
    /// More bytes arrived than the pin allows; reading stopped one byte past it.
    #[error("{url} is larger than the pinned {limit} bytes, so it is not the reviewed file")]
    TooLarge {
        /// The address.
        url: String,
        /// The pinned length.
        limit: u64,
    },
    /// The body has another length than the pin; nothing was hashed or kept.
    #[error("{url} is {actual} bytes, the pinned file is {expected}")]
    Size {
        /// The address.
        url: String,
        /// The pinned length.
        expected: u64,
        /// What arrived.
        actual: u64,
    },
    /// The bytes don't hash to the pin: not the reviewed file. Never kept, never used. For a resumed download this
    /// is reported only after one complete fresh download also failed.
    #[error("{url} has SHA-256 {actual}, the pinned file is {expected}")]
    Hash {
        /// The address.
        url: String,
        /// The pin.
        expected: Sha256,
        /// What arrived.
        actual: Sha256,
    },
    /// Every URL of the pin failed; one error per URL, in order.
    #[error("no source had the file: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
    AllFailed(Vec<FetchError>),
    /// Reading or writing the cache failed.
    #[error("{path}: {reason}")]
    Io {
        /// The file.
        path: String,
        /// What the system said.
        reason: String,
    },
    /// The caller cancelled through [`Progress::is_cancelled`].
    #[error("cancelled")]
    Cancelled,
}

/// A response body from [`Fetcher::open`].
pub struct Body {
    /// The byte offset the stream starts at: the requested one if the server honoured the range, 0 if it sent the
    /// whole file instead.
    pub start: u64,
    /// The bytes, from `start` on.
    pub reader: Box<dyn Read + Send>,
}

/// Opens a URL as a byte stream. The app supplies one over its HTTPS client; tests supply an in-memory one.
/// Nothing below the app links a network stack.
pub trait Fetcher: Send + Sync {
    /// Opens `url` from byte `from` on (an HTTP `Range: bytes=from-` request when `from > 0`) and returns the body as
    /// a stream, saying where it really starts. The caller reads at most the pinned size plus one byte and then drops
    /// it, so an implementation must stream (never buffer the whole body first) and may be endless.
    ///
    /// # Errors
    /// [`FetchError::Network`] when the request fails.
    fn open(&self, url: &str, from: u64) -> Result<Body, FetchError>;
}

/// Verified local copies of pinned files: the cache every model, runtime library, fixture and font comes from.
/// `omuzaki-ai` implements it over a [`Fetcher`] and a cache directory in the app's data folder.
pub trait FileSource: Send + Sync {
    /// A local path holding exactly `pin`'s bytes, fetched on first need:
    /// - the URLs are tried in order, and any failure at one (network, length, hash) moves on to the next;
    ///   [`FetchError::AllFailed`] lists them all;
    /// - the bytes are checked (size, then SHA-256) before they are stored, under their hash and by atomic rename;
    /// - a [resumable](PinnedFile::resumable) file continues an interrupted download from its partial file, and the
    ///   whole file is hashed again at the end; on a mismatch the partial file is deleted and downloaded once more
    ///   from the start, and a second mismatch is an error;
    /// - a cached file whose size or modification time changed since it was stored is checked again, and fetched
    ///   again if it fails.
    ///
    /// Callers only read the file. Blocking and cold (it may download hundreds of MB): call it from a job, never from a
    /// real-time loop.
    ///
    /// # Errors
    /// [`FetchError`]; nothing unverified is ever returned.
    fn get(&self, pin: &PinnedFile, progress: &dyn Progress) -> Result<PathBuf, FetchError>;

    /// The local path of the verified blob with hash `sha256`, if this source already holds it. It never downloads.
    /// `Ok(None)` means the blob is absent, or present but failing its hash; a bad blob is deleted, as [`get`] deletes
    /// one it finds. An I/O failure is an error, never `Ok(None)`, so a caller can't mistake it for "absent".
    /// A source that keeps no cache answers `Ok(None)`.
    ///
    /// # Errors
    /// [`FetchError::Io`] when the cache can't be read or cleaned.
    fn lookup(&self, _sha256: Sha256) -> Result<Option<PathBuf>, FetchError> {
        Ok(None)
    }

    /// Removes the blob with hash `sha256` from this source, so its bytes are fetched again on the next need. A blob
    /// that isn't there is already forgotten, so this is idempotent. Callers must not forget a blob that another
    /// installed model still uses. A source that keeps no cache has nothing to forget.
    ///
    /// # Errors
    /// [`FetchError::Io`] when a file can't be deleted.
    fn forget(&self, _sha256: Sha256) -> Result<(), FetchError> {
        Ok(())
    }
}
