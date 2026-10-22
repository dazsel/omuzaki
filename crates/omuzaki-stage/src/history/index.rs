//! The thumbnail index: each transaction's thumbnail, by the PNG's hash in the asset store, kept in
//! `.omuzaki/history/thumbs.toml` in the project folder. It is keyed by transaction id, so it carries over when the
//! history itself is persisted.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use omuzaki_ops_api::TxId;
use serde::{Deserialize, Serialize};

/// The index file's place under the project folder.
pub const INDEX_FILE: &str = ".omuzaki/history/thumbs.toml";

/// The file as written: transaction ids (as text, TOML's keys) to hashes.
#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    #[serde(default)]
    thumbs: BTreeMap<String, String>,
}

/// Transaction id to the hash of its thumbnail, backed by the index file.
#[derive(Debug)]
pub struct Index {
    path: PathBuf,
    thumbs: BTreeMap<TxId, String>,
}

impl Index {
    /// The index of the project in `project`, read from its file; empty when there is none. A file that does not read
    /// is set aside with a warning rather than failing the history: the thumbnails are rendered again as edits land.
    #[must_use]
    pub fn load(project: &Path) -> Self {
        let path = project.join(INDEX_FILE);
        // Not an op's file access: the history's own state under the project's `.omuzaki/` (design D10).
        #[allow(clippy::disallowed_methods)]
        let text = std::fs::read_to_string(&path);
        let thumbs = match text {
            Ok(text) => match toml::from_str::<IndexFile>(&text) {
                Ok(file) => file
                    .thumbs
                    .into_iter()
                    .filter_map(|(tx, hash)| tx.parse().ok().map(|tx| (TxId(tx), hash)))
                    .collect(),
                Err(e) => {
                    tracing::warn!("the history thumbnail index does not read, starting it again: {e}");
                    BTreeMap::new()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                tracing::warn!("the history thumbnail index could not be read, starting it again: {e}");
                BTreeMap::new()
            }
        };
        Self { path, thumbs }
    }

    /// `tx`'s thumbnail hash.
    #[must_use]
    pub fn get(&self, tx: TxId) -> Option<&str> {
        self.thumbs.get(&tx).map(String::as_str)
    }

    /// How many transactions have a thumbnail.
    #[must_use]
    pub fn len(&self) -> usize {
        self.thumbs.len()
    }

    /// True when no transaction has one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.thumbs.is_empty()
    }

    /// Records `tx`'s thumbnail.
    pub fn insert(&mut self, tx: TxId, hash: String) {
        self.thumbs.insert(tx, hash);
    }

    /// Writes the index through a temporary file and a rename, so a crash leaves the old file or the new one.
    ///
    /// # Errors
    /// The folder can't be made or the file can't be written.
    pub fn save(&self) -> io::Result<()> {
        let file = IndexFile { thumbs: self.thumbs.iter().map(|(tx, hash)| (tx.0.to_string(), hash.clone())).collect() };
        let text = toml::to_string(&file).map_err(io::Error::other)?;
        let dir = self.path.parent().ok_or_else(|| io::Error::other("the index file has no folder"))?;
        let temp = self.path.with_extension("toml.tmp");
        // Not an op's file access: the history's own state under the project's `.omuzaki/` (design D10).
        #[allow(clippy::disallowed_methods)]
        {
            std::fs::create_dir_all(dir)?;
            let mut out = std::fs::File::create(&temp)?;
            out.write_all(text.as_bytes())?;
            out.sync_all()?;
            std::fs::rename(&temp, &self.path)
        }
    }
}
