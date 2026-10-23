//! The person's acknowledgements (design D8, ADR 0016): one per publisher key and item hash, kept in
//! `<library>/acks.toml`, so the disclaimer for an item is shown once and what was accepted, by whom and when, is on
//! record.

use std::path::Path;

use omuzaki_library_api::LibraryError;
use serde::{Deserialize, Serialize};

/// The acknowledgement file inside the library folder.
pub(crate) const ACKS: &str = "acks.toml";

/// One accepted disclaimer.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acknowledgement {
    /// The publisher key (hex), or `unverified` for an unsigned item or one whose signature fails.
    pub publisher: String,
    /// The item's canonical hash.
    pub hash: String,
    /// `id@version`, for people reading the file.
    pub item: String,
    /// The licence the publisher declared.
    pub licence: String,
    /// Who accepted it.
    pub by: String,
    /// When, in seconds since 1970.
    pub at: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct File {
    #[serde(default, rename = "ack")]
    acks: Vec<Acknowledgement>,
}

fn storage(path: &Path, e: &dyn std::fmt::Display) -> LibraryError {
    LibraryError::Storage(format!("{}: {e}", path.display()))
}

/// Every acknowledgement in `path`, sorted; none when the file isn't there.
pub(crate) fn read(path: &Path) -> Result<Vec<Acknowledgement>, LibraryError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(storage(path, &e)),
    };
    let mut file: File = toml::from_str(&text).map_err(|e| storage(path, &e))?;
    file.acks.sort();
    Ok(file.acks)
}

/// Adds `ack` unless the same publisher and hash are acknowledged already; returns whether it was new. The file is
/// written sorted, through a temporary file, so the same set always writes the same bytes.
pub(crate) fn add(path: &Path, ack: Acknowledgement) -> Result<bool, LibraryError> {
    let mut acks = read(path)?;
    if acks.iter().any(|a| a.publisher == ack.publisher && a.hash == ack.hash) {
        return Ok(false);
    }
    acks.push(ack);
    acks.sort();
    let text = toml::to_string(&File { acks }).map_err(|e| storage(path, &e))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| storage(path, &e))?;
    }
    let partial = path.with_extension("toml.partial");
    std::fs::write(&partial, text).map_err(|e| storage(path, &e))?;
    std::fs::rename(&partial, path).map_err(|e| storage(path, &e))?;
    Ok(true)
}
