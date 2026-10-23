//! The manifest: what a snapshot holds, as canonical JSON whose hash is the snapshot id (design 7.1).

use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

/// The manifest layout this build writes. A reader refuses a newer one rather than misread it.
pub const MANIFEST_FORMAT: u32 = 1;

/// The files of a snapshot and the assets it references. Serialised canonically ([`Manifest::to_bytes`]): fields in
/// this order, files sorted by path, assets sorted and unique, no whitespace, so identical content always has the same
/// bytes and so the same id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Manifest {
    /// [`MANIFEST_FORMAT`] when written.
    pub format: u32,
    /// The project's schema version (0 when it names none).
    pub schema: u32,
    /// Every file, by path.
    pub files: Vec<FileRow>,
    /// The asset-store hashes the project references (from `assets.lock`): referenced, never copied.
    pub assets: Vec<ContentHash>,
}

/// One file of a snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FileRow {
    /// `/`-separated, relative to the project.
    pub path: String,
    /// The hash of its bytes.
    pub hash: ContentHash,
    /// Its length in bytes.
    pub size: u64,
    /// The pack holding it, for a small file; absent for a file stored as its own object (whose hash is `hash`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<ContentHash>,
    /// Where in the pack it starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u64>,
}

impl FileRow {
    /// A file stored as its own object.
    pub fn whole(path: impl Into<String>, hash: ContentHash, size: u64) -> Self {
        Self { path: path.into(), hash, size, pack: None, offset: None }
    }

    /// A file stored at `offset` in `pack`.
    pub fn packed(path: impl Into<String>, hash: ContentHash, size: u64, pack: ContentHash, offset: u64) -> Self {
        Self { path: path.into(), hash, size, pack: Some(pack), offset: Some(offset) }
    }
}

impl Manifest {
    /// A manifest of `files` and `assets`, put in canonical order.
    pub fn new(schema: u32, mut files: Vec<FileRow>, mut assets: Vec<ContentHash>) -> Self {
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assets.sort();
        assets.dedup();
        Self { format: MANIFEST_FORMAT, schema, files, assets }
    }

    /// The canonical bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        // A struct of strings, numbers and hashes always serialises; the fallback is never taken.
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// Reads canonical bytes back.
    ///
    /// # Errors
    /// The reason, if the bytes aren't a manifest or are from a newer format.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if manifest.format > MANIFEST_FORMAT {
            return Err(format!("manifest format {} is newer than this build reads", manifest.format));
        }
        Ok(manifest)
    }

    /// Every store object the manifest needs: its packs and its whole files (not the manifest itself, nor assets).
    pub fn objects(&self) -> impl Iterator<Item = ContentHash> + '_ {
        self.files.iter().map(|row| row.pack.unwrap_or(row.hash))
    }

    /// The row of `path`.
    pub fn file(&self, path: &str) -> Option<&FileRow> {
        self.files.binary_search_by(|row| row.path.as_str().cmp(path)).ok().map(|i| &self.files[i])
    }
}
