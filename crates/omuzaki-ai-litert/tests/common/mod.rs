//! Shared by the integration tests: the gate for tests that need the LiteRT archive.
//!
//! `OMUZAKI_LITERT_CACHE` names a folder holding this platform's archive under its asset name from the build table,
//! and `OMUZAKI_LITERT_SHA256` the SHA-256 the CI run that built it reported (`runtime-litert.yml`'s `collect`). There is
//! no published pin yet (#1748), so that digest is the archive's trust root: an archive without one is refused.
#![allow(clippy::unwrap_used)] // panics are test failures

use std::path::PathBuf;

use omuzaki_ai_litert::builds::select_here;
use sha2::{Digest, Sha256 as Hasher};

/// This platform's archive from `OMUZAKI_LITERT_CACHE`, checked against `OMUZAKI_LITERT_SHA256`, with that digest; or
/// `None` (with a message) when the cache is unset.
///
/// # Panics
/// When the cache is set but the digest is missing or differs: such an archive is never loaded.
pub fn litert_archive(test: &str) -> Option<(PathBuf, String)> {
    let Some(dir) = std::env::var_os("OMUZAKI_LITERT_CACHE") else {
        eprintln!("skipping {test}: set OMUZAKI_LITERT_CACHE to a folder holding the LiteRT archive");
        return None;
    };
    let expected = std::env::var("OMUZAKI_LITERT_SHA256").unwrap_or_default();
    assert!(expected.len() == 64, "refusing the archive: OMUZAKI_LITERT_SHA256 must be the build job's SHA-256");
    let path = PathBuf::from(dir).join(select_here().unwrap().asset);
    let actual = format!("{:x}", Hasher::digest(std::fs::read(&path).unwrap()));
    assert_eq!(actual, expected.to_lowercase(), "refusing {}: its SHA-256 differs", path.display());
    Some((path, actual))
}
