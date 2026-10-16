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
    /// list. Whatever fails, nothing is left in staging. Pruning is best effort: an old version that can't be removed
    /// stays in the list, oldest first, and is tried again on the next install.
    ///
    /// # Errors
    /// [`PackageError::Io`] when a file can't be written; [`PackageError::Signature`] when the staged files hash
    /// differently from the package read (the disk changed them).
    pub fn install(&self, package: &PluginPackage) -> Result<Installed, PackageError> {
        let id = package.manifest.id.clone();
        let (staged, hash) = package.stage(&self.staging())?;
        let placed = self.place(&id, &staged, hash, package.hash);
        if staged.exists() {
            // Leave no staging folder behind; the step's own error is the one to report.
            let _cleaned = fs::remove_dir_all(&staged);
        }
        let dir = placed?;
        let plugin_dir = self.plugin_dir(&id);
        let mut versions: Vec<ContentHash> = self.versions(&id).into_iter().filter(|h| *h != hash).collect();
        versions.push(hash);
        let mut kept = Vec::new();
        while versions.len() > KEPT_VERSIONS {
            let old = versions.remove(0);
            let old_dir = plugin_dir.join(old.to_string());
            if old_dir.exists() && fs::remove_dir_all(&old_dir).is_err() {
                kept.push(old);
            }
        }
        kept.extend(versions);
        let listing = kept.iter().map(|h| format!("{h}\n")).collect::<Vec<_>>().concat();
        write_atomic(&plugin_dir.join(VERSIONS_FILE), listing.as_bytes())?;
        Ok(Installed { id, hash, dir })
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
    fs::rename(&temp, path).map_err(|e| io(&e))
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
