//! Where fallback font bytes come from: a download cached on disk and checked against a pinned SHA-256, or a
//! directory of files.

use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

use super::catalog::FontSpec;

/// Why a fallback font could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FontError {
    /// The file could not be read or written.
    #[error("{path}: {reason}")]
    Io {
        /// The file.
        path: String,
        /// What the system said.
        reason: String,
    },
    /// The download failed.
    #[error("downloading {url} failed: {reason}")]
    Fetch {
        /// The address.
        url: String,
        /// What the fetcher said.
        reason: String,
    },
    /// More bytes arrived than the pin allows. Reading stopped one byte past the limit, so memory stayed bounded.
    #[error("{file} is larger than the pinned {limit} bytes (reading stopped there)")]
    TooLarge {
        /// The font file.
        file: String,
        /// The pinned length.
        limit: u64,
    },
    /// The file is not the length the pin says. Nothing was hashed or cached.
    #[error("{file} is {actual} bytes, the pinned file is {expected}")]
    Size {
        /// The font file.
        file: String,
        /// The pinned length.
        expected: u64,
        /// The length received.
        actual: u64,
    },
    /// The bytes do not hash to the pin, so they are not the font that was reviewed. Never cached, never used.
    #[error("{file} has SHA-256 {actual}, the pinned file is {expected}")]
    HashMismatch {
        /// The font file.
        file: String,
        /// The pinned hash.
        expected: String,
        /// The hash of what was received.
        actual: String,
    },
    /// The bytes are not a font the layout can read.
    #[error("{file} is not a usable font: {reason}")]
    Parse {
        /// The font file.
        file: String,
        /// What the parser said.
        reason: String,
    },
    /// The sheet has no font source, so only the embedded Latin font is available.
    #[error("no fallback font source is configured")]
    NoSource,
}

/// Supplies the bytes of a [`FontSpec`]. Called at most once per font per sheet, from the thread that first needs it.
pub trait FontSource: Send + Sync {
    /// The bytes of `spec`.
    ///
    /// # Errors
    /// [`FontError`] when the font cannot be produced; the sheet then reports its characters as missing.
    fn load(&self, spec: &FontSpec) -> Result<Vec<u8>, FontError>;
}

/// Downloads a URL. The app supplies one over its HTTPS client; tests supply a mock. The crate links no network stack
/// itself.
pub trait Fetcher: Send + Sync {
    /// Opens `url` and returns its body as a stream. The downloader reads at most the pinned size plus one byte and
    /// then drops the stream, so an implementation must not buffer the whole body first and may be endless.
    ///
    /// # Errors
    /// A message saying why the request failed.
    fn open(&self, url: &str) -> Result<Box<dyn Read + Send>, String>;
}

/// Reads all of `reader` but never more than `limit` bytes: a longer stream is [`FontError::TooLarge`] after
/// `limit + 1` bytes, so an endless or oversized body cannot exhaust memory.
fn read_bounded(reader: impl Read, spec: &FontSpec) -> Result<Vec<u8>, FontError> {
    let mut bytes = Vec::with_capacity(usize::try_from(spec.size).unwrap_or(0).min(1 << 24));
    reader
        .take(spec.size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| FontError::Fetch { url: spec.url.to_owned(), reason: e.to_string() })?;
    if bytes.len() as u64 > spec.size {
        return Err(FontError::TooLarge { file: spec.file_name.to_owned(), limit: spec.size });
    }
    Ok(bytes)
}

/// Reads the file at `path` with the same bound.
fn read_file_bounded(path: &Path, spec: &FontSpec) -> Result<Vec<u8>, FontError> {
    let file = File::open(path).map_err(|e| io_error(path, &e))?;
    read_bounded(file, spec).map_err(|e| match e {
        FontError::Fetch { reason, .. } => FontError::Io { path: path.display().to_string(), reason },
        other => other,
    })
}

/// The lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut hex, b| {
        // Writing to a String cannot fail.
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// Checks `bytes` against the pin of `spec`: length first (cheap), then hash.
fn verify(spec: &FontSpec, bytes: &[u8]) -> Result<(), FontError> {
    let actual_len = bytes.len() as u64;
    if actual_len != spec.size {
        return Err(FontError::Size { file: spec.file_name.to_owned(), expected: spec.size, actual: actual_len });
    }
    let actual = sha256_hex(bytes);
    if actual == spec.sha256 {
        Ok(())
    } else {
        Err(FontError::HashMismatch { file: spec.file_name.to_owned(), expected: spec.sha256.to_owned(), actual })
    }
}

fn io_error(path: &Path, e: &std::io::Error) -> FontError {
    FontError::Io { path: path.display().to_string(), reason: e.to_string() }
}

/// Fetches fonts on first need, checks them against their pinned SHA-256 and keeps them in a cache directory, the same
/// pattern the spec uses for models.
///
/// The cache holds each font under its content hash (`<sha256>.font`), so a changed pin never reads a stale file, and a
/// cached file is re-hashed on every read: a truncated or tampered file is deleted and fetched again instead of
/// being drawn.
pub struct CachedDownloader<F> {
    cache_dir: PathBuf,
    fetcher: F,
}

impl<F: Fetcher> CachedDownloader<F> {
    /// A downloader caching in `cache_dir` (the app's data dir; created on first write).
    pub fn new(cache_dir: impl Into<PathBuf>, fetcher: F) -> Self {
        Self { cache_dir: cache_dir.into(), fetcher }
    }

    fn cache_path(&self, spec: &FontSpec) -> PathBuf {
        self.cache_dir.join(format!("{}.font", spec.sha256))
    }

    /// Writes through a temporary name that no other attempt shares (process id and a counter, created exclusively),
    /// then renames it into place, so concurrent first uses never see each other's half-written file. The temporary
    /// file is removed on every failure.
    fn store(&self, spec: &FontSpec, bytes: &[u8]) -> Result<(), FontError> {
        static ATTEMPTS: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(&self.cache_dir).map_err(|e| io_error(&self.cache_dir, &e))?;
        let attempt = ATTEMPTS.fetch_add(1, Ordering::Relaxed);
        let partial = TempFile(self.cache_dir.join(format!("{}.{}.{attempt}.part", spec.sha256, process::id())));
        let write = || -> io::Result<()> {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&partial.0)?;
            file.write_all(bytes)?;
            file.sync_all()
        };
        write().map_err(|e| io_error(&partial.0, &e))?;
        let path = self.cache_path(spec);
        match fs::rename(&partial.0, &path) {
            Ok(()) => Ok(()),
            // Windows refuses to rename onto an existing file: another attempt won, which is as good.
            Err(_) if read_file_bounded(&path, spec).is_ok_and(|b| verify(spec, &b).is_ok()) => Ok(()),
            Err(e) => Err(io_error(&path, &e)),
        }
    }
}

/// A temporary file that deletes itself unless it was renamed away, so no failure path leaks it.
struct TempFile(PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        // Already gone after a successful rename; nothing to do about any other error.
        let _ = fs::remove_file(&self.0);
    }
}

impl<F: Fetcher> FontSource for CachedDownloader<F> {
    fn load(&self, spec: &FontSpec) -> Result<Vec<u8>, FontError> {
        let path = self.cache_path(spec);
        if let Ok(cached) = read_file_bounded(&path, spec)
            && verify(spec, &cached).is_ok()
        {
            return Ok(cached);
        }
        if path.exists() {
            tracing::warn!(file = spec.file_name, "cached font failed its check; fetching it again");
            // Best effort: the store below replaces it anyway.
            let _ = fs::remove_file(&path);
        }
        let stream =
            self.fetcher.open(spec.url).map_err(|reason| FontError::Fetch { url: spec.url.to_owned(), reason })?;
        let bytes = read_bounded(stream, spec)?;
        verify(spec, &bytes)?;
        if let Err(e) = self.store(spec, &bytes) {
            // The sheet can still be drawn; the next run downloads again.
            tracing::warn!(file = spec.file_name, error = %e, "could not cache the downloaded font");
        }
        Ok(bytes)
    }
}

/// Reads fonts from a directory by file name, for an installer that ships them or a test that ships subsets.
pub struct DirSource {
    dir: PathBuf,
    verified: bool,
}

impl DirSource {
    /// Files in `dir` must match the catalog pins.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), verified: true }
    }

    /// Files in `dir` are used as they are. For the committed test subsets, which are not the pinned fonts; never
    /// point it at a directory a user can write.
    pub fn unverified(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into(), verified: false }
    }
}

impl FontSource for DirSource {
    fn load(&self, spec: &FontSpec) -> Result<Vec<u8>, FontError> {
        let path = self.dir.join(spec.file_name);
        let bytes = read_file_bounded(&path, spec)?;
        if self.verified {
            verify(spec, &bytes)?;
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::{Arc, Barrier, Mutex};

    use super::*;
    use crate::sheet::layout::fallback::catalog::Family;
    use crate::sheet::layout::text::Weight;

    /// A spec pinned to `bytes`, as a catalog entry would be.
    fn spec_for(bytes: &[u8]) -> FontSpec {
        FontSpec {
            family: Family::Thai,
            weight: Weight::Regular,
            file_name: "Test.ttf",
            url: "https://example.invalid/Test.ttf",
            sha256: Box::leak(sha256_hex(bytes).into_boxed_str()),
            size: bytes.len() as u64,
        }
    }

    /// Serves `body`, counting requests; an optional barrier makes concurrent callers meet inside the fetch.
    struct Mock {
        body: Vec<u8>,
        calls: Mutex<u32>,
        meet: Option<Barrier>,
    }

    impl Mock {
        fn new(body: &[u8]) -> Self {
            Self { body: body.to_vec(), calls: Mutex::new(0), meet: None }
        }
    }

    impl Fetcher for &Mock {
        fn open(&self, _url: &str) -> Result<Box<dyn Read + Send>, String> {
            *self.calls.lock().unwrap() += 1;
            if let Some(barrier) = &self.meet {
                barrier.wait();
            }
            Ok(Box::new(Cursor::new(self.body.clone())))
        }
    }

    fn calls(m: &Mock) -> u32 {
        *m.calls.lock().unwrap()
    }

    fn files(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn sha256_matches_the_known_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn the_first_load_downloads_and_later_loads_come_from_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(b"font bytes");
        let spec = spec_for(&mock.body);
        let source = CachedDownloader::new(dir.path().join("fonts"), &mock);
        assert_eq!(source.load(&spec).unwrap(), mock.body);
        assert_eq!(source.load(&spec).unwrap(), mock.body);
        assert_eq!(calls(&mock), 1, "the second load must not touch the network");
        assert_eq!(files(&dir.path().join("fonts")), vec![format!("{}.font", spec.sha256)]);
    }

    #[test]
    fn a_hash_mismatch_is_refused_and_never_cached() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(b"font bytes");
        // Same length, different content: the length check passes, the hash must catch it.
        let spec = spec_for(b"evil bytes");
        let source = CachedDownloader::new(dir.path(), &mock);
        assert!(matches!(source.load(&spec), Err(FontError::HashMismatch { .. })));
        assert_eq!(files(dir.path()), Vec::<String>::new());
    }

    #[test]
    fn a_short_body_is_refused_before_hashing() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(b"short");
        let spec = spec_for(b"a much longer font");
        let err = CachedDownloader::new(dir.path(), &mock).load(&spec).unwrap_err();
        assert!(matches!(err, FontError::Size { expected: 18, actual: 5, .. }), "{err}");
    }

    #[test]
    fn an_oversized_body_is_cut_off_one_byte_past_the_pin() {
        /// Counts what the downloader actually pulled from the stream.
        struct Counting(Arc<Mutex<u64>>);
        impl Read for Counting {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let n = buf.len().min(4096);
                buf[..n].fill(7);
                *self.0.lock().unwrap() += n as u64;
                Ok(n)
            }
        }
        struct Endless(Arc<Mutex<u64>>);
        impl Fetcher for Endless {
            fn open(&self, _: &str) -> Result<Box<dyn Read + Send>, String> {
                Ok(Box::new(Counting(Arc::clone(&self.0))))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let pulled = Arc::new(Mutex::new(0));
        let spec = spec_for(&[7; 10_000]);
        let err = CachedDownloader::new(dir.path(), Endless(Arc::clone(&pulled))).load(&spec).unwrap_err();
        assert!(matches!(err, FontError::TooLarge { limit: 10_000, .. }), "{err}");
        // At most one read buffer beyond the pin was requested of an endless stream.
        assert!(*pulled.lock().unwrap() <= 10_001 + 4096, "pulled {}", pulled.lock().unwrap());
        assert_eq!(files(dir.path()), Vec::<String>::new());

        // A finite body that is merely too long is refused the same way.
        let long = Mock::new(&[7; 10_500]);
        let err = CachedDownloader::new(dir.path(), &long).load(&spec).unwrap_err();
        assert!(matches!(err, FontError::TooLarge { .. }), "{err}");
    }

    #[test]
    fn an_oversized_cache_file_is_read_no_further_than_the_pin_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(b"font bytes");
        let spec = spec_for(&mock.body);
        fs::write(dir.path().join(format!("{}.font", spec.sha256)), vec![0_u8; 1_000_000]).unwrap();
        assert_eq!(CachedDownloader::new(dir.path(), &mock).load(&spec).unwrap(), mock.body);
        assert_eq!(calls(&mock), 1);
        assert_eq!(fs::read(dir.path().join(format!("{}.font", spec.sha256))).unwrap(), mock.body);
        // The directory source applies the same bound.
        fs::write(dir.path().join("Test.ttf"), vec![0_u8; 1_000_000]).unwrap();
        assert!(matches!(DirSource::unverified(dir.path()).load(&spec), Err(FontError::TooLarge { .. })));
    }

    #[test]
    fn a_corrupt_cache_file_is_replaced_by_a_fresh_download() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(b"font bytes");
        let spec = spec_for(&mock.body);
        fs::write(dir.path().join(format!("{}.font", spec.sha256)), b"tampered!!").unwrap();
        let loaded = CachedDownloader::new(dir.path(), &mock).load(&spec).unwrap();
        assert_eq!(loaded, mock.body);
        assert_eq!(calls(&mock), 1);
        assert_eq!(fs::read(dir.path().join(format!("{}.font", spec.sha256))).unwrap(), mock.body);
    }

    #[test]
    fn two_threads_loading_the_same_font_at_once_leave_one_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut mock = Mock::new(b"font bytes");
        mock.meet = Some(Barrier::new(2));
        let spec = spec_for(&mock.body);
        let source = CachedDownloader::new(dir.path(), &mock);
        let results: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..2).map(|_| scope.spawn(|| source.load(&spec))).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for result in results {
            assert_eq!(result.unwrap(), mock.body);
        }
        assert_eq!(calls(&mock), 2, "both fetched: the barrier made them meet before either cached");
        assert_eq!(files(dir.path()), vec![format!("{}.font", spec.sha256)], "no temporary file left behind");
        assert_eq!(fs::read(dir.path().join(format!("{}.font", spec.sha256))).unwrap(), mock.body);
    }

    #[test]
    fn a_failed_store_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let mock = Mock::new(b"font bytes");
        let spec = spec_for(&mock.body);
        // A directory in the final name's place makes the rename fail after the temporary file was written.
        fs::create_dir(dir.path().join(format!("{}.font", spec.sha256))).unwrap();
        fs::write(dir.path().join(format!("{}.font", spec.sha256)).join("keep"), b"x").unwrap();
        let source = CachedDownloader::new(dir.path(), &mock);
        assert_eq!(source.load(&spec).unwrap(), mock.body, "the font is still returned");
        assert_eq!(files(dir.path()), vec![format!("{}.font", spec.sha256)]);
    }

    #[test]
    fn a_failed_download_is_an_error_value() {
        struct Offline;
        impl Fetcher for Offline {
            fn open(&self, _: &str) -> Result<Box<dyn Read + Send>, String> {
                Err("no route".into())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let err = CachedDownloader::new(dir.path(), Offline).load(&spec_for(b"x")).unwrap_err();
        assert!(matches!(err, FontError::Fetch { ref reason, .. } if reason == "no route"), "{err}");
    }

    #[test]
    fn a_directory_source_checks_pins_unless_told_not_to() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Test.ttf"), b"other bytes").unwrap();
        let spec = spec_for(b"font bytes");
        assert!(matches!(DirSource::new(dir.path()).load(&spec), Err(FontError::TooLarge { .. })));
        fs::write(dir.path().join("Test.ttf"), b"evil bytes").unwrap();
        assert!(matches!(DirSource::new(dir.path()).load(&spec), Err(FontError::HashMismatch { .. })));
        fs::write(dir.path().join("Test.ttf"), b"other byte").unwrap();
        assert_eq!(DirSource::unverified(dir.path()).load(&spec).unwrap(), b"other byte");
        assert!(matches!(DirSource::new(dir.path().join("missing")).load(&spec), Err(FontError::Io { .. })));
    }
}
