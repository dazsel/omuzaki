//! Where fallback font bytes come from: the shared verified cache (every pinned file, checked against its SHA-256), or a
//! directory of files.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omuzaki_ai_api::{FetchError, FileSource, NoProgress, Sha256};
use sha2::Digest as _;

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

/// Maps a shared-cache failure onto the font error the sheet reports. The messages keep their wording; the `file` and
/// `url` fields name the address the cache was given, because the cache does not know the font's file name.
impl From<FetchError> for FontError {
    fn from(error: FetchError) -> Self {
        match error {
            FetchError::Network { url, reason } => Self::Fetch { url, reason },
            FetchError::TooLarge { url, limit } => Self::TooLarge { file: url, limit },
            FetchError::Size { url, expected, actual } => Self::Size { file: url, expected, actual },
            FetchError::Hash { url, expected, actual } => {
                Self::HashMismatch { file: url, expected: expected.to_string(), actual: actual.to_string() }
            }
            FetchError::Io { path, reason } => Self::Io { path, reason },
            FetchError::AllFailed(errors) => Self::Fetch {
                url: "the pinned URLs".to_owned(),
                reason: errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "),
            },
            other => Self::Fetch { url: "the pinned font".to_owned(), reason: other.to_string() },
        }
    }
}

/// Supplies the bytes of a [`FontSpec`]. Called at most once per font per sheet, from the thread that first needs it.
pub trait FontSource: Send + Sync {
    /// The bytes of `spec`.
    ///
    /// # Errors
    /// [`FontError`] when the font cannot be produced; the sheet then reports its characters as missing.
    fn load(&self, spec: &FontSpec) -> Result<Vec<u8>, FontError>;
}

/// Reads the file at `path`, never more than the pinned size plus one byte: a longer file is [`FontError::TooLarge`]
/// and reading stops there, so an oversized file cannot exhaust memory.
#[allow(clippy::disallowed_methods, reason = "a sheet font file the font source chose, never a path from an op's arguments")]
fn read_file_bounded(path: &Path, spec: &FontSpec) -> Result<Vec<u8>, FontError> {
    let file = File::open(path).map_err(|e| io_error(path, &e))?;
    let mut bytes = Vec::with_capacity(usize::try_from(spec.size).unwrap_or(0).min(1 << 24));
    file.take(spec.size.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| io_error(path, &e))?;
    if bytes.len() as u64 > spec.size {
        return Err(FontError::TooLarge { file: spec.file_name.to_owned(), limit: spec.size });
    }
    Ok(bytes)
}

/// The SHA-256 of `bytes`, in the type the catalogue pins and the shared cache use.
pub(super) fn hash_of(bytes: &[u8]) -> Sha256 {
    Sha256(sha2::Sha256::digest(bytes).into())
}

/// Checks `bytes` against the pin of `spec`: length first (cheap), then hash.
fn verify(spec: &FontSpec, bytes: &[u8]) -> Result<(), FontError> {
    let actual_len = bytes.len() as u64;
    if actual_len != spec.size {
        return Err(FontError::Size { file: spec.file_name.to_owned(), expected: spec.size, actual: actual_len });
    }
    let actual = hash_of(bytes);
    if actual == spec.sha256 {
        Ok(())
    } else {
        Err(FontError::HashMismatch {
            file: spec.file_name.to_owned(),
            expected: spec.sha256.to_string(),
            actual: actual.to_string(),
        })
    }
}

fn io_error(path: &Path, e: &std::io::Error) -> FontError {
    FontError::Io { path: path.display().to_string(), reason: e.to_string() }
}

/// Fonts from the shared verified cache: the cache fetches, checks and stores each pinned file (`omuzaki_ai`'s
/// `VerifiedCache` in the app), and this reads the file back. The bytes are checked again here, so a font is never
/// drawn from bytes that did not pass the pin.
pub struct PinnedFonts {
    source: Arc<dyn FileSource>,
}

impl PinnedFonts {
    /// Fonts from `source`.
    pub fn new(source: Arc<dyn FileSource>) -> Self {
        Self { source }
    }
}

impl FontSource for PinnedFonts {
    fn load(&self, spec: &FontSpec) -> Result<Vec<u8>, FontError> {
        let path = self.source.get(&spec.pin(), &NoProgress)?;
        let bytes = read_file_bounded(&path, spec)?;
        verify(spec, &bytes)?;
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
#[allow(clippy::disallowed_methods, reason = "tests write fixture fonts into a temporary folder; not an op")]
mod tests {
    use std::fs;

    use super::*;
    use crate::sheet::layout::fallback::catalog::Family;
    use crate::sheet::layout::text::Weight;
    use omuzaki_ai::VerifiedCache;
    use omuzaki_ai_mock::MemFetcher;

    /// A committed subset of a pinned Noto font (3 KB), served as if it were the download.
    const SUBSET: &[u8] = include_bytes!("../../../../tests/fixtures/fonts/NotoSansThai-Regular.ttf");
    const URLS: &[&str] = &["https://example.invalid/NotoSansThai-Regular.ttf"];

    /// A spec pinned to `bytes`, as a catalog entry would be.
    fn spec_for(bytes: &[u8], urls: &'static [&'static str]) -> FontSpec {
        FontSpec {
            family: Family::Thai,
            weight: Weight::Regular,
            file_name: "Test.ttf",
            urls,
            sha256: hash_of(bytes),
            size: bytes.len() as u64,
        }
    }

    #[test]
    fn sha256_matches_the_known_vector() {
        assert_eq!(hash_of(b"abc").to_string(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn a_pinned_font_is_fetched_once_through_the_verified_cache_and_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(VerifiedCache::new(dir.path(), MemFetcher::new().serving(URLS[0], SUBSET)));
        let fonts = PinnedFonts::new(cache.clone());
        let spec = spec_for(SUBSET, URLS);
        assert_eq!(fonts.load(&spec).unwrap(), SUBSET);
        assert_eq!(fonts.load(&spec).unwrap(), SUBSET);
        assert_eq!(cache.fetcher().requests(), 1, "the second load comes from the cache");
    }

    #[test]
    fn a_wrong_pin_is_refused_and_later_loads_still_work() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(VerifiedCache::new(dir.path(), MemFetcher::new().serving(URLS[0], SUBSET)));
        let fonts = PinnedFonts::new(cache);
        let mut wrong = spec_for(SUBSET, URLS);
        wrong.sha256 = hash_of(b"not the reviewed font");
        let err = fonts.load(&wrong).unwrap_err();
        assert!(matches!(err, FontError::Fetch { .. }) && err.to_string().contains("SHA-256"), "{err}");
        assert_eq!(fonts.load(&spec_for(SUBSET, URLS)).unwrap(), SUBSET);
    }

    #[test]
    fn a_directory_source_checks_pins_unless_told_not_to() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Test.ttf"), b"other bytes").unwrap();
        let spec = spec_for(b"font bytes", URLS);
        assert!(matches!(DirSource::new(dir.path()).load(&spec), Err(FontError::TooLarge { .. })));
        fs::write(dir.path().join("Test.ttf"), b"evil bytes").unwrap();
        assert!(matches!(DirSource::new(dir.path()).load(&spec), Err(FontError::HashMismatch { .. })));
        fs::write(dir.path().join("Test.ttf"), b"other byte").unwrap();
        assert_eq!(DirSource::unverified(dir.path()).load(&spec).unwrap(), b"other byte");
        assert!(matches!(DirSource::new(dir.path().join("missing")).load(&spec), Err(FontError::Io { .. })));
    }
}
