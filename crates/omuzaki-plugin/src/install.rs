//! Where installed plugins live (design §6.1, §3.2): `<local data dir>/plugins/<id>/<hash>/`, the last two versions
//! kept for rollback, and the approval key a person's approval attaches to.

use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_identity_api::key::KeyId;
use omuzaki_plugin_api::PluginId;
use omuzaki_store_api::ContentHash;

use crate::package::{PackageError, PluginPackage, PublisherCheck};

/// Versions of one plugin kept on disk: the active one and the one before, for rollback.
pub const KEPT_VERSIONS: usize = 2;

/// The file listing a plugin's installed hashes, oldest first.
const VERSIONS_FILE: &str = "versions";

/// One installed version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The plugin.
    pub id: PluginId,
    /// Its content hash.
    pub hash: ContentHash,
    /// Its folder, `plugins/<id>/<hash>/`.
    pub dir: PathBuf,
    /// Whether this install put the version there (it was not installed before).
    pub new_version: bool,
}

/// The plugins folder under the machine's local data folder.
pub struct InstallRoot {
    root: PathBuf,
}

fn io(error: &std::io::Error) -> PackageError {
    PackageError::Io(error.to_string())
}

impl InstallRoot {
    /// The layout under `local_data_dir` (the settings design's machine folder: code never roams).
    pub fn new(local_data_dir: impl Into<PathBuf>) -> Self {
        Self { root: local_data_dir.into().join("plugins") }
    }

    /// Where a package is staged before it moves into place.
    fn staging(&self) -> PathBuf {
        self.root.join(".staging")
    }

    /// The folder of one plugin's versions.
    pub fn plugin_dir(&self, id: &PluginId) -> PathBuf {
        self.root.join(id.as_str())
    }

    /// The hashes installed for `id`, oldest first.
    pub fn versions(&self, id: &PluginId) -> Vec<ContentHash> {
        let Ok(text) = fs::read_to_string(self.plugin_dir(id).join(VERSIONS_FILE)) else { return Vec::new() };
        text.lines().filter_map(parse_hash).collect()
    }

    /// Stages `package`, checks the staged files hash to what was verified, moves them to `plugins/<id>/<hash>/` and
    /// keeps only the last [`KEPT_VERSIONS`]. Installing a hash already there changes nothing but its place in the
    /// list. Whatever fails, nothing is left in staging.
    ///
    /// The new list is written before any old version is pruned, and if it can't be written the new version is taken
    /// back. An old version is then retired by renaming it to `.retired-<hash>` and deleting that: one that can't be
    /// renamed is still whole and goes back in the list, oldest first, to be tried again; one that was renamed is never
    /// listed again, even if deleting it fails, and what is left of it is swept on the next install.
    ///
    /// # Errors
    /// [`PackageError::Io`] when a file can't be written; [`PackageError::Signature`] when the staged files hash
    /// differently from the package read (the disk changed them).
    pub fn install(&self, package: &PluginPackage) -> Result<Installed, PackageError> {
        let id = package.manifest.id.clone();
        let plugin_dir = self.plugin_dir(&id);
        sweep_retired(&plugin_dir);
        let (staged, hash) = package.stage(&self.staging())?;
        let new_version = !plugin_dir.join(hash.to_string()).exists();
        let placed = self.place(&id, &staged, hash, package.hash);
        if staged.exists() {
            // Leave no staging folder behind; the step's own error is the one to report.
            let _cleaned = fs::remove_dir_all(&staged);
        }
        let dir = placed?;
        let mut versions: Vec<ContentHash> = self.versions(&id).into_iter().filter(|h| *h != hash).collect();
        versions.push(hash);
        let pruned: Vec<ContentHash> = versions.drain(..versions.len().saturating_sub(KEPT_VERSIONS)).collect();
        // The list is written before anything is pruned, so it never names a folder that is gone; when it can't be
        // written, the version this install put there is taken back.
        if let Err(error) = self.write_versions(&id, &versions) {
            if new_version {
                retire(&plugin_dir, hash);
            }
            return Err(error);
        }
        let intact: Vec<ContentHash> = pruned.into_iter().filter(|old| retire(&plugin_dir, *old)).collect();
        if !intact.is_empty() {
            // Best effort: a version that could not be moved aside is whole, and stays a rollback target.
            let _listed = self.write_versions(&id, &[intact, versions].concat());
        }
        Ok(Installed { id, hash, dir, new_version })
    }

    /// Takes back a version an install just put there (`Installed::new_version`), for an install that failed after
    /// it: its folder is retired and it leaves the list.
    ///
    /// # Errors
    /// [`PackageError::Io`] when the list can't be written.
    pub fn remove_version(&self, id: &PluginId, hash: ContentHash) -> Result<(), PackageError> {
        let plugin_dir = self.plugin_dir(id);
        let still_there = retire(&plugin_dir, hash);
        let versions: Vec<ContentHash> = self.versions(id).into_iter().filter(|h| *h != hash || still_there).collect();
        self.write_versions(id, &versions)
    }

    fn write_versions(&self, id: &PluginId, versions: &[ContentHash]) -> Result<(), PackageError> {
        let listing = versions.iter().map(|h| format!("{h}\n")).collect::<Vec<_>>().concat();
        write_atomic(&self.plugin_dir(id).join(VERSIONS_FILE), listing.as_bytes())
    }

    /// Moves the staged folder to `plugins/<id>/<hash>/`, unless that version is already there. `hash` is what the
    /// staged files hash to, `verified` the package's own hash.
    fn place(
        &self,
        id: &PluginId,
        staged: &Path,
        hash: ContentHash,
        verified: ContentHash,
    ) -> Result<PathBuf, PackageError> {
        if hash != verified {
            return Err(PackageError::Signature("the staged files do not hash to the package's hash".into()));
        }
        let plugin_dir = self.plugin_dir(id);
        fs::create_dir_all(&plugin_dir).map_err(|e| io(&e))?;
        let dir = plugin_dir.join(hash.to_string());
        if !dir.exists() {
            fs::rename(staged, &dir).map_err(|e| io(&e))?;
        }
        Ok(dir)
    }
}

/// The prefix of a version folder being deleted.
const RETIRED_PREFIX: &str = ".retired-";

/// Retires the version `hash` under `plugin_dir`: renames it out of the way, then deletes it. True when it is still
/// there, whole (the rename failed), so it stays in the list.
fn retire(plugin_dir: &Path, hash: ContentHash) -> bool {
    let dir = plugin_dir.join(hash.to_string());
    if !dir.exists() {
        return false;
    }
    let retired = plugin_dir.join(format!("{RETIRED_PREFIX}{hash}"));
    if fs::rename(&dir, &retired).is_err() {
        return true;
    }
    // Best effort: a part left behind is never listed, and the next install sweeps it.
    let _removed = fs::remove_dir_all(&retired);
    false
}

/// Deletes what is left of versions retired before; best effort, as a leftover is never read.
fn sweep_retired(plugin_dir: &Path) {
    let Ok(entries) = fs::read_dir(plugin_dir) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(RETIRED_PREFIX) {
            let _removed = fs::remove_dir_all(entry.path());
        }
    }
}

fn parse_hash(hex: &str) -> Option<ContentHash> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(ContentHash(bytes))
}

/// Writes `bytes` to `path` through a temporary file and a rename, so a reader never sees half of it.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), PackageError> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes).map_err(|e| io(&e))?;
    fs::rename(&temp, path).map_err(|e| {
        // No half-made write is left behind; the rename's error is the one to report.
        let _cleaned = fs::remove_file(&temp);
        io(&e)
    })
}

/// This machine, as approvals made with a development key name it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MachineId(pub String);

/// Who an approval trusts (design §3.2, §14.2): a publisher key, a person's development key on one machine, or an
/// unsigned package's one hash.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Signer {
    /// A publisher's key.
    Publisher(KeyId),
    /// A development key, which never leaves the machine it was made on.
    Dev {
        /// The key.
        key: KeyId,
        /// The machine.
        machine: MachineId,
    },
    /// No signature.
    Unsigned,
}

/// What a person's approval attaches to: the content hash and the signer, never the plugin's name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ApprovalKey {
    /// The approved package's hash.
    pub hash: ContentHash,
    /// Who signed it.
    pub signer: Signer,
}

impl ApprovalKey {
    /// The key of a package verified as `check`, on `machine`, whose development keys are `dev_keys`.
    ///
    /// # Errors
    /// [`PackageError::Signature`] when the signature does not hold: nothing can be approved.
    pub fn of(
        hash: ContentHash,
        check: &PublisherCheck,
        dev_keys: &[KeyId],
        machine: &MachineId,
    ) -> Result<Self, PackageError> {
        let signer = match check {
            PublisherCheck::Verified(key) if dev_keys.contains(key) => {
                Signer::Dev { key: *key, machine: machine.clone() }
            }
            PublisherCheck::Verified(key) => Signer::Publisher(*key),
            PublisherCheck::Unsigned => Signer::Unsigned,
            PublisherCheck::Mismatch => {
                return Err(PackageError::Signature(
                    "the signature does not match the package or its publisher".into(),
                ));
            }
        };
        Ok(Self { hash, signer })
    }

    /// Whether an approval made for `self` covers `other` on `machine`. The same hash and signer always does; an
    /// update signed by the same publisher keeps the approval for what it already covered (design §3.2); a
    /// development key's approval covers its machine only; an unsigned approval covers its one hash.
    pub fn covers(&self, other: &ApprovalKey, machine: &MachineId) -> bool {
        match (&self.signer, &other.signer) {
            (Signer::Publisher(a), Signer::Publisher(b)) => a == b,
            (Signer::Dev { key: a, machine: made_on }, Signer::Dev { key: b, .. }) => a == b && made_on == machine,
            (Signer::Unsigned, Signer::Unsigned) => self.hash == other.hash,
            _ => false,
        }
    }
}
