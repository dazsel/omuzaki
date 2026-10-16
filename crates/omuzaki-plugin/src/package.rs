//! Plugin packages (design §7 "Package", §3.2, §14.2): a folder or a `.zip` of one, read as hostile input.
//!
//! - **Reading** ([`PluginPackage::open_zip`], [`PluginPackage::open_folder`]) refuses the whole package, before
//!   expanding past its limits, on: an absolute path, a drive prefix, a `.` or `..` component, an empty segment or a
//!   backslash; a symlink, a hardlink or anything that is not a regular file or a folder; a name that is not UTF-8; two
//!   entries equal after normalising (Unicode NFC, case-folded); more than [`PackageLimits::max_files`] entries; more
//!   than [`PackageLimits::max_expanded_bytes`] in all or [`PackageLimits::max_file_bytes`] in one file. It writes
//!   nothing to disk.
//! - **The content hash** ([`content_hash`]) is BLAKE3 over every file except `plugin.sig`, as one exact preimage:
//!   entries sorted by their NFC `/` path bytes, each a `u64` little-endian path length, the path, a `u64`
//!   little-endian content length, then the content.
//! - **The signature** (`plugin.sig`, [`PackageSignature`]) is ADR 0042's ed25519 signature over that hash under
//!   [`Domain::Plugin`]; [`PluginPackage::publisher`] checks it against the manifest's publisher key.
//! - **Staging** ([`PluginPackage::stage`]) writes the files into a fresh folder, and the hash is taken again over
//!   what was written, so what is hashed is what loads.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use omuzaki_identity_api::domain::Domain;
use omuzaki_identity_api::key::{KeyId, Signature};
use omuzaki_identity_api::verify::verify;
use omuzaki_plugin_api::PluginError;
use omuzaki_plugin_api::limits::{COMPONENT_MAX_BYTES, MIB};
use omuzaki_plugin_api::manifest::{Manifest, component_path_problem};
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// The manifest's file name.
pub const MANIFEST_FILE: &str = "plugin.toml";

/// The signature's file name; the one file the content hash leaves out.
pub const SIGNATURE_FILE: &str = "plugin.sig";

/// What a package may hold.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PackageLimits {
    /// The archive itself, in bytes.
    pub max_bytes: u64,
    /// Entries, folders included.
    pub max_files: u32,
    /// One file, expanded.
    pub max_file_bytes: u64,
    /// Every file together, expanded.
    pub max_expanded_bytes: u64,
}

impl Default for PackageLimits {
    /// The design's numbers: 256 entries and 32 MiB expanded; one file at most the component cap.
    fn default() -> Self {
        Self { max_bytes: 32 * MIB, max_files: 256, max_file_bytes: COMPONENT_MAX_BYTES, max_expanded_bytes: 32 * MIB }
    }
}

/// Why a package was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PackageError {
    /// The archive is not a readable `.zip`.
    #[error("not a readable .zip: {0}")]
    Archive(String),
    /// A path leaves the package or is not normalised.
    #[error("`{path}` {why}")]
    UnsafePath {
        /// The path as written.
        path: String,
        /// What is wrong with it.
        why: &'static str,
    },
    /// A symlink, a hardlink or another entry that is not a regular file or a folder.
    #[error("`{0}` is not a regular file or a folder")]
    NotRegular(String),
    /// Two entries are the same after normalising.
    #[error("`{0}` and `{1}` are the same name")]
    Duplicate(String, String),
    /// Past a limit, named from the constant that sets it.
    #[error("past the package's {what} ({limit})")]
    TooLarge {
        /// Which limit.
        what: &'static str,
        /// Its value.
        limit: u64,
    },
    /// No `plugin.toml`.
    #[error("the package has no plugin.toml")]
    MissingManifest,
    /// The manifest's `component` is not a file of the package.
    #[error("the component `{0}` is not a file in the package")]
    MissingComponent(String),
    /// The manifest does not parse or its check has errors.
    #[error("{0}")]
    Manifest(String),
    /// `plugin.sig` is unreadable.
    #[error("plugin.sig: {0}")]
    Signature(String),
    /// Reading or writing a file failed.
    #[error("{0}")]
    Io(String),
}

impl From<PackageError> for PluginError {
    fn from(error: PackageError) -> Self {
        match error {
            PackageError::Manifest(text) => PluginError::Manifest(text),
            PackageError::Signature(text) => PluginError::Signature(text),
            PackageError::Io(text) => PluginError::Io(text),
            other => PluginError::Package(other.to_string()),
        }
    }
}

fn io(error: &std::io::Error) -> PackageError {
    PackageError::Io(error.to_string())
}

/// `plugin.sig`: the publisher's key and its signature over the content hash under [`Domain::Plugin`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageSignature {
    /// The publisher's full public key, lower-case hex.
    pub key: KeyId,
    /// The signature, lower-case hex.
    pub signature: Signature,
}

/// What the signature says about who published the package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublisherCheck {
    /// Signed by this key, and the manifest names it.
    Verified(KeyId),
    /// No publisher and no signature.
    Unsigned,
    /// The signature does not hold for these files, or the key is not the manifest's publisher, or the manifest
    /// names a publisher and there is no signature.
    Mismatch,
}

/// The canonical path of an entry, or why it is refused: UTF-8, normalised (no root, drive, `.`, `..`, empty segment
/// or backslash), in NFC. A trailing `/` (a folder entry) is removed first.
fn canonical(raw: &[u8]) -> Result<String, PackageError> {
    let text = std::str::from_utf8(raw).map_err(|_| PackageError::UnsafePath {
        path: String::from_utf8_lossy(raw).into_owned(),
        why: "is not UTF-8",
    })?;
    let trimmed = text.strip_suffix('/').unwrap_or(text);
    if let Some(why) = component_path_problem(trimmed) {
        return Err(PackageError::UnsafePath { path: text.to_owned(), why });
    }
    Ok(trimmed.nfc().collect())
}

/// The key two names collide on: NFC, Unicode full case folding (`ß` and `SS`, `ﬁ` and `FI` collide), NFC again.
fn fold(path: &str) -> String {
    caseless::default_case_fold_str(&path.nfc().collect::<String>()).nfc().collect()
}

/// The content hash of `files` (path to bytes), leaving out `plugin.sig`.
pub fn content_hash<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> ContentHash {
    let mut entries: Vec<(&str, &[u8])> = files.into_iter().filter(|(path, _)| *path != SIGNATURE_FILE).collect();
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut hasher = blake3::Hasher::new();
    for (path, bytes) in entries {
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    ContentHash(*hasher.finalize().as_bytes())
}

/// The content hash of the folder `root`, read under `limits` as a package is: the same bytes on every platform,
/// whatever its file system does to names (they are hashed in NFC).
///
/// # Errors
/// [`PackageError`] for anything a package reader refuses.
pub fn hash_folder(root: &Path, limits: &PackageLimits) -> Result<ContentHash, PackageError> {
    let mut collector = Collector::new(*limits);
    walk(root, root, &mut collector)?;
    Ok(content_hash(collector.files.iter().map(|(p, b)| (p.as_str(), b.as_slice()))))
}

/// The files of a package as they are read, checked against the limits as they arrive.
struct Collector {
    limits: PackageLimits,
    files: BTreeMap<String, Vec<u8>>,
    folded: BTreeMap<String, String>,
    entries: u32,
    expanded: u64,
}

impl Collector {
    fn new(limits: PackageLimits) -> Self {
        Self { limits, files: BTreeMap::new(), folded: BTreeMap::new(), entries: 0, expanded: 0 }
    }

    /// Counts an entry (a file or a folder) and refuses a name that collides with an earlier one.
    fn entry(&mut self, path: &str) -> Result<(), PackageError> {
        self.entries += 1;
        if self.entries > self.limits.max_files {
            return Err(PackageError::TooLarge { what: "max_files", limit: u64::from(self.limits.max_files) });
        }
        if let Some(earlier) = self.folded.insert(fold(path), path.to_owned()) {
            return Err(PackageError::Duplicate(earlier, path.to_owned()));
        }
        Ok(())
    }

    /// Reads one file of declared size `declared`, refusing as soon as it would pass a limit, whatever the archive
    /// claimed.
    fn file(&mut self, path: String, declared: u64, reader: &mut dyn Read) -> Result<(), PackageError> {
        self.entry(&path)?;
        let room = self.limits.max_file_bytes.min(self.limits.max_expanded_bytes - self.expanded);
        if declared > room {
            return Err(self.too_large(declared));
        }
        let mut bytes = Vec::new();
        reader.take(room + 1).read_to_end(&mut bytes).map_err(|e| io(&e))?;
        let read = bytes.len() as u64;
        if read > room {
            return Err(self.too_large(read));
        }
        self.expanded += read;
        self.files.insert(path, bytes);
        Ok(())
    }

    fn too_large(&self, bytes: u64) -> PackageError {
        if bytes > self.limits.max_file_bytes {
            PackageError::TooLarge { what: "max_file_bytes", limit: self.limits.max_file_bytes }
        } else {
            PackageError::TooLarge { what: "max_expanded_bytes", limit: self.limits.max_expanded_bytes }
        }
    }
}

/// A package read and checked in memory.
#[derive(Clone, Debug)]
pub struct PluginPackage {
    /// The parsed manifest; its check has no errors.
    pub manifest: Manifest,
    /// Every file, by canonical path.
    pub files: BTreeMap<String, Vec<u8>>,
    /// The content hash.
    pub hash: ContentHash,
    /// `plugin.sig`, when present.
    pub signature: Option<PackageSignature>,
    /// `plugin.sig` as it was read, staged with the files so an installed package verifies again when reopened.
    pub signature_file: Option<Vec<u8>>,
}

/// The `S_IFMT` bits of a Unix mode, and the two kinds a package may hold.
const S_IFMT: u32 = 0o170_000;
const S_IFREG: u32 = 0o100_000;
const S_IFDIR: u32 = 0o040_000;

impl PluginPackage {
    /// Reads a `.zip` package.
    ///
    /// # Errors
    /// [`PackageError`] for the first thing refused.
    pub fn open_zip(bytes: &[u8], limits: &PackageLimits) -> Result<Self, PackageError> {
        if bytes.len() as u64 > limits.max_bytes {
            return Err(PackageError::TooLarge { what: "max_bytes", limit: limits.max_bytes });
        }
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| PackageError::Archive(e.to_string()))?;
        if archive.len() as u64 > u64::from(limits.max_files) {
            return Err(PackageError::TooLarge { what: "max_files", limit: u64::from(limits.max_files) });
        }
        let mut collector = Collector::new(*limits);
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(|e| PackageError::Archive(e.to_string()))?;
            let path = canonical(entry.name_raw())?;
            let kind = entry.unix_mode().map(|mode| mode & S_IFMT);
            if entry.is_symlink() || kind.is_some_and(|k| k != 0 && k != S_IFREG && k != S_IFDIR) {
                return Err(PackageError::NotRegular(path));
            }
            if entry.is_dir() {
                collector.entry(&path)?;
            } else {
                let declared = entry.size();
                collector.file(path, declared, &mut entry)?;
            }
        }
        Self::from_files(collector.files)
    }

    /// Reads a package folder. Links and anything but regular files and folders are refused, never followed.
    ///
    /// # Errors
    /// [`PackageError`] for the first thing refused.
    pub fn open_folder(root: &Path, limits: &PackageLimits) -> Result<Self, PackageError> {
        let mut collector = Collector::new(*limits);
        walk(root, root, &mut collector)?;
        Self::from_files(collector.files)
    }

    fn from_files(mut files: BTreeMap<String, Vec<u8>>) -> Result<Self, PackageError> {
        let text = files.get(MANIFEST_FILE).ok_or(PackageError::MissingManifest)?;
        let text = std::str::from_utf8(text).map_err(|_| PackageError::Manifest("plugin.toml is not UTF-8".into()))?;
        let manifest = Manifest::parse(text).map_err(|e| PackageError::Manifest(e.to_string()))?;
        if let Some(error) = manifest.check().errors().next() {
            return Err(PackageError::Manifest(error.to_string()));
        }
        if !files.contains_key(&manifest.component) {
            return Err(PackageError::MissingComponent(manifest.component.clone()));
        }
        let signature_file = files.remove(SIGNATURE_FILE);
        let signature = match &signature_file {
            Some(bytes) => Some(serde_json::from_slice(bytes).map_err(|e| PackageError::Signature(e.to_string()))?),
            None => None,
        };
        let hash = content_hash(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())));
        Ok(Self { manifest, files, hash, signature, signature_file })
    }

    /// The component's bytes, one of the hashed files.
    pub fn component(&self) -> &[u8] {
        self.files.get(&self.manifest.component).map_or(&[], Vec::as_slice)
    }

    /// Who published it, from the signature and the manifest's publisher key.
    pub fn publisher(&self) -> PublisherCheck {
        match (&self.signature, &self.manifest.publisher) {
            (None, None) => PublisherCheck::Unsigned,
            (None, Some(_)) => PublisherCheck::Mismatch,
            (Some(sig), publisher) => {
                let named = publisher.as_ref().is_none_or(|p| key_matches(&p.key, &sig.key));
                if named && verify(Domain::Plugin, &self.hash, &sig.key, &sig.signature).is_ok() {
                    PublisherCheck::Verified(sig.key)
                } else {
                    PublisherCheck::Mismatch
                }
            }
        }
    }

    /// Writes the files and `plugin.sig` into a fresh folder under `staging_root` and returns it with the hash of what
    /// was written. The signature is staged so the installed package verifies again when it is reopened.
    ///
    /// # Errors
    /// [`PackageError::Io`] when a file can't be written; [`PackageError::MissingComponent`] when the staged component
    /// is not a regular file inside the folder.
    pub fn stage(&self, staging_root: &Path) -> Result<(PathBuf, ContentHash), PackageError> {
        fs::create_dir_all(staging_root).map_err(|e| io(&e))?;
        let guard = Staged { dir: fresh_dir(staging_root)?, kept: false };
        let dir = guard.dir.clone();
        let signature = self.signature_file.as_ref().map(|bytes| (SIGNATURE_FILE, bytes));
        for (path, bytes) in self.files.iter().map(|(p, b)| (p.as_str(), b)).chain(signature) {
            let target = dir.join(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| io(&e))?;
            }
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .and_then(|mut file| std::io::Write::write_all(&mut file, bytes))
                .map_err(|e| io(&e))?;
        }
        let component = dir.join(&self.manifest.component);
        let inside = fs::symlink_metadata(&component).is_ok_and(|m| m.file_type().is_file())
            && fs::canonicalize(&component)
                .ok()
                .zip(fs::canonicalize(&dir).ok())
                .is_some_and(|(file, root)| file.starts_with(root));
        if !inside {
            return Err(PackageError::MissingComponent(self.manifest.component.clone()));
        }
        let staged = PluginPackage::open_folder(&dir, &PackageLimits::default())?;
        Ok((guard.keep(), staged.hash))
    }
}

/// A staging folder that is removed, with whatever was written into it, unless staging finished and kept it.
struct Staged {
    dir: PathBuf,
    kept: bool,
}

impl Staged {
    fn keep(mut self) -> PathBuf {
        self.kept = true;
        self.dir.clone()
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.kept {
            // Best effort: the staging error is the one reported.
            let _cleaned = fs::remove_dir_all(&self.dir);
        }
    }
}

/// Whether a manifest's `ed25519:<fingerprint or key>` names `key`.
fn key_matches(declared: &str, key: &KeyId) -> bool {
    let Some(hex) = declared.strip_prefix("ed25519:") else { return false };
    if hex.len() == 64 { hex == key.to_hex() } else { hex == key.fingerprint() }
}

/// A new, empty folder under `root`, named from the clock and a counter so two stagings never share one.
fn fresh_dir(root: &Path) -> Result<PathBuf, PackageError> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    for attempt in 0..1000u32 {
        let dir = root.join(format!("stage-{nanos:x}-{attempt}"));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(&e)),
        }
    }
    Err(PackageError::Io("no fresh staging folder could be made".into()))
}

fn walk(root: &Path, dir: &Path, collector: &mut Collector) -> Result<(), PackageError> {
    let mut entries: Vec<_> = fs::read_dir(dir).map_err(|e| io(&e))?.collect::<Result<_, _>>().map_err(|e| io(&e))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|_| PackageError::Io("a path left the folder".into()))?;
        let raw: Option<Vec<&str>> = relative.components().map(|c| c.as_os_str().to_str()).collect();
        let Some(raw) = raw else {
            let path = relative.to_string_lossy().into_owned();
            return Err(PackageError::UnsafePath { path, why: "is not UTF-8" });
        };
        let name = canonical(raw.join("/").as_bytes())?;
        let seen = fs::symlink_metadata(&path).map_err(|e| io(&e))?;
        let kind = seen.file_type();
        if kind.is_dir() {
            collector.entry(&name)?;
            walk(root, &path, collector)?;
            // A folder swapped for a link while it was read is refused: what was read may not be the package's.
            let after = fs::symlink_metadata(&path).map_err(|e| io(&e))?;
            if !after.is_dir() || !same_file(&seen, &after) {
                return Err(PackageError::NotRegular(name));
            }
        } else if kind.is_file() {
            let Some(mut file) = open_regular(&path, &seen).map_err(|e| io(&e))? else {
                return Err(PackageError::NotRegular(name));
            };
            if hard_linked(&file).map_err(|e| io(&e))? {
                return Err(PackageError::NotRegular(name));
            }
            let declared = file.metadata().map_err(|e| io(&e))?.len();
            collector.file(name, declared, &mut file)?;
        } else {
            return Err(PackageError::NotRegular(name));
        }
    }
    Ok(())
}

/// Opens `path` as the regular file `seen` described, or `None` when what opened is not that file: a path swapped
/// for a link after it was looked at is never followed into.
#[cfg(unix)]
fn open_regular(path: &Path, seen: &fs::Metadata) -> std::io::Result<Option<fs::File>> {
    let file = fs::File::open(path)?;
    let opened = file.metadata()?;
    Ok((opened.is_file() && same_file(seen, &opened)).then_some(file))
}

/// Windows: the open does not follow a link (`FILE_FLAG_OPEN_REPARSE_POINT`), so a link swapped in opens as the link
/// itself and is refused.
#[cfg(windows)]
fn open_regular(path: &Path, _seen: &fs::Metadata) -> std::io::Result<Option<fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
    let file = fs::OpenOptions::new().read(true).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT).open(path)?;
    let opened = file.metadata()?;
    Ok((opened.is_file() && !opened.file_type().is_symlink()).then_some(file))
}

/// Elsewhere a swapped path can't be told from the original, so nothing opens.
#[cfg(not(any(unix, windows)))]
fn open_regular(_: &Path, _: &fs::Metadata) -> std::io::Result<Option<fs::File>> {
    Ok(None)
}

/// Whether two looks at a path saw the same file (device and inode on Unix).
#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// Windows: std gives no stable file identity, so a folder is held to being a folder, not a link, at both looks.
#[cfg(not(unix))]
fn same_file(_: &fs::Metadata, b: &fs::Metadata) -> bool {
    !b.file_type().is_symlink()
}

/// Whether an open regular file has other names: a hardlink could make one staged file another file's bytes. Read
/// from the open handle, so the file checked is the file read.
#[cfg(unix)]
fn hard_linked(file: &fs::File) -> std::io::Result<bool> {
    Ok(std::os::unix::fs::MetadataExt::nlink(&file.metadata()?) > 1)
}

/// Windows: the link count from `FILE_STANDARD_INFO` (std's `number_of_links` is not stable).
#[cfg(windows)]
#[allow(unsafe_code)] // One OS call with no safe std equivalent; its SAFETY reasoning is below.
fn hard_linked(file: &fs::File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FILE_STANDARD_INFO, FileStandardInfo, GetFileInformationByHandleEx};
    // SAFETY: `info` is a zeroed `FILE_STANDARD_INFO` (plain data, so zero is a valid value) and the call is told its
    // exact size; the handle is the open `file`'s and outlives the call.
    let mut info: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    let size = u32::try_from(std::mem::size_of::<FILE_STANDARD_INFO>()).map_err(std::io::Error::other)?;
    let ok = unsafe {
        GetFileInformationByHandleEx(file.as_raw_handle(), FileStandardInfo, std::ptr::from_mut(&mut info).cast(), size)
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info.NumberOfLinks > 1)
}

/// Elsewhere the link count can't be read, so every file is refused rather than a hardlink accepted.
#[cfg(not(any(unix, windows)))]
fn hard_linked(_: &fs::File) -> std::io::Result<bool> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_link_after_it_was_looked_at_is_not_opened() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let (path, outside) = (dir.path().join("plugin.wasm"), dir.path().join("outside"));
        fs::write(&path, b"package").unwrap_or_else(|e| panic!("{e}"));
        fs::write(&outside, b"secret").unwrap_or_else(|e| panic!("{e}"));
        let seen = fs::symlink_metadata(&path).unwrap_or_else(|e| panic!("{e}"));
        assert!(open_regular(&path, &seen).unwrap_or_else(|e| panic!("{e}")).is_some(), "the file it saw opens");
        fs::remove_file(&path).unwrap_or_else(|e| panic!("{e}"));
        std::os::unix::fs::symlink(&outside, &path).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            open_regular(&path, &seen).unwrap_or_else(|e| panic!("{e}")).is_none(),
            "the swapped-in link is refused"
        );
    }
}
