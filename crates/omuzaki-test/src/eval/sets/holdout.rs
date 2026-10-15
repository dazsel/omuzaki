//! `eval sets holdout-check` (design 11.3): real and held-out items may never be training data.
//!
//! Every item of a `real` set, and of every `holdout` split, is test-only. A training manifest (ML-12 writes them) must
//! not name one. The check does not need to know the manifest's format: it looks for the item hashes in the manifest's
//! text, which is how a TOML table, a JSON list and a plain list of hashes all give the item away.

use std::fs;
use std::path::{Path, PathBuf};

use super::store::{SetsError, parse_hash};
use crate::eval::Discovery;

/// A protected item found in a training manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leak {
    /// The training manifest.
    pub manifest: PathBuf,
    /// The set the item is in.
    pub set: String,
    /// The item's hash, as its manifest writes it.
    pub item: String,
    /// Why the set is protected: `real` or `holdout`.
    pub why: &'static str,
}

/// One protected item to look for.
struct Protected {
    set: String,
    item: String,
    hex: String,
    why: &'static str,
}

/// Names the protected item hashes `manifests` contain.
///
/// # Errors
/// [`SetsError::Io`] when a manifest cannot be read; [`SetsError::Invalid`] when a protected set holds an item whose hash
/// is malformed, since such an item cannot be searched for and a check that skipped it would pass for the wrong reason.
pub fn holdout_check(discovery: &Discovery, manifests: &[PathBuf]) -> Result<Vec<Leak>, SetsError> {
    let mut protected = Vec::new();
    for (path, set) in &discovery.set_files {
        if !set.is_protected() {
            continue;
        }
        let why = if set.is_holdout().unwrap_or(true) { "holdout" } else { "real" };
        for item in &set.items {
            let hash = parse_hash(&item.hash).map_err(|_| {
                SetsError::Invalid(format!(
                    "{}: set `{}` is test-only and holds the malformed hash `{}`; fix it before checking training data",
                    path.display(),
                    set.id,
                    item.hash
                ))
            })?;
            protected.push(Protected { set: set.id.clone(), item: item.hash.clone(), hex: hash.to_string(), why });
        }
    }
    let mut leaks = Vec::new();
    for manifest in manifests {
        let text = read_lowercase(manifest)?;
        for p in protected.iter().filter(|p| text.contains(p.hex.as_str())) {
            leaks.push(Leak { manifest: manifest.clone(), set: p.set.clone(), item: p.item.clone(), why: p.why });
        }
    }
    Ok(leaks)
}

fn read_lowercase(path: &Path) -> Result<String, SetsError> {
    let bytes = fs::read(path).map_err(|source| SetsError::Io { path: path.to_owned(), source })?;
    Ok(String::from_utf8_lossy(&bytes).to_lowercase())
}
