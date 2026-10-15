//! The registry's person-level state file, `<data dir>/models/state.toml`: which shipped versions are installed (with
//! their manifests, so a rollback target survives an app update that stops registering it), the per-slot default,
//! rollback target and selection, the last evaluation of each model and why a model is disabled.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use omuzaki_ai_api::{EvalReport, ModelManifest, ModelRef, RegistryError, Sha256, SlotRef};
use serde::{Deserialize, Serialize};

/// An installed shipped version.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Installed {
    pub model: ModelRef,
    pub manifest: ModelManifest,
    pub eval: Option<EvalReport>,
}

/// What one slot remembers. `default` is what runs when nothing is selected; `previous` is the version it replaced.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct SlotRecord {
    pub slot: SlotRef,
    pub default: Option<ModelRef>,
    pub previous: Option<ModelRef>,
    pub selected: Option<ModelRef>,
}

/// A model that failed its install, with the words people read and the numbers behind them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Health {
    pub reason: String,
    pub model: ModelRef,
    pub eval: Option<EvalReport>,
}

/// A model the catalogue delivered after the build. Its manifest is kept here, so it is known after a restart whatever
/// the app registers; `epoch` is the root epoch of the catalogue that last listed it (its provenance).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Import {
    pub model: ModelRef,
    pub manifest: ModelManifest,
    /// The hash of the manifest file `omuzaki-update` verified; the model digest covers the files, not this.
    pub manifest_sha256: Sha256,
    pub epoch: u32,
    /// Quarantined: never selectable or installable, ignored by the version rule. A revoked record is kept (not
    /// deleted) so it can't be offered again unnoticed, and so the Models page can say why.
    #[serde(default)]
    pub revoked: bool,
}

/// A model the person added. A failed one is kept too, with its `problem`, so it is listed as disabled with the reason.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct UserModel {
    pub model: ModelRef,
    pub manifest: ModelManifest,
    /// Why it can't be used, if it can't.
    #[serde(default)]
    pub problem: Option<String>,
    pub eval: Option<EvalReport>,
}

/// A person's acknowledgement of the disclaimer for one digest under one manifest (ADR 0016 rule 2).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Acknowledgement {
    pub digest: Sha256,
    /// The hash of the manifest the person saw ([`manifest_hash`]): the digest covers only the files, and a manifest
    /// that changes the card (dropping `imitates`, say) or the licence must be accepted again. A record without it
    /// covers nothing.
    #[serde(default)]
    pub manifest: Option<Sha256>,
    pub person: String,
    /// The `DISCLAIMER_VERSION` whose words were accepted. A later version doesn't void this.
    pub disclaimer_version: u32,
    /// Seconds since the Unix epoch.
    pub at: u64,
}

/// The whole file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(super) struct State {
    #[serde(default)]
    pub installed: Vec<Installed>,
    #[serde(default)]
    pub slots: Vec<SlotRecord>,
    #[serde(default)]
    pub health: Vec<Health>,
    /// Models delivered by catalogues, revoked ones included.
    #[serde(default)]
    pub imports: Vec<Import>,
    /// The highest catalogue epoch seen.
    #[serde(default)]
    pub catalogue_epoch: u32,
    /// Models the person added, failed ones included.
    #[serde(default)]
    pub user: Vec<UserModel>,
    /// One acknowledgement per model digest.
    #[serde(default)]
    pub acknowledgements: Vec<Acknowledgement>,
}

impl State {
    pub fn installed(&self, model: &ModelRef) -> Option<&Installed> {
        self.installed.iter().find(|i| &i.model == model)
    }

    /// True if `model` is only known as a revoked import (a live record of the same model wins).
    pub fn revoked(&self, model: &ModelRef) -> bool {
        let mut records = self.imports.iter().filter(|i| &i.model == model).peekable();
        records.peek().is_some() && records.all(|i| i.revoked)
    }

    pub fn user(&self, model: &ModelRef) -> Option<&UserModel> {
        self.user.iter().find(|u| &u.model == model)
    }

    /// True if the person accepted `model`'s files under exactly this `manifest`.
    pub fn acknowledged(&self, model: &ModelRef, manifest: &ModelManifest) -> bool {
        let hash = manifest_hash(manifest);
        self.acknowledgements.iter().any(|a| a.digest == model.digest && a.manifest.is_some() && a.manifest == hash)
    }

    pub fn health(&self, model: &ModelRef) -> Option<&Health> {
        self.health.iter().find(|h| &h.model == model)
    }

    pub fn slot(&self, slot: &SlotRef) -> Option<&SlotRecord> {
        self.slots.iter().find(|r| &r.slot == slot)
    }

    /// The slot's record, created empty on first use.
    pub fn slot_mut(&mut self, slot: &SlotRef) -> &mut SlotRecord {
        let at = self.slots.iter().position(|r| &r.slot == slot).unwrap_or_else(|| {
            self.slots.push(SlotRecord { slot: slot.clone(), default: None, previous: None, selected: None });
            self.slots.len() - 1
        });
        &mut self.slots[at]
    }

    /// Records an installed version, replacing any earlier record of it and clearing an earlier failure (an install
    /// that now passes).
    pub fn set_installed(&mut self, installed: Installed) {
        self.health.retain(|h| h.model != installed.model);
        self.installed.retain(|i| i.model != installed.model);
        self.installed.push(installed);
    }

    /// Notes why `model` is disabled, replacing any earlier note.
    pub fn set_health(&mut self, health: Health) {
        self.health.retain(|h| h.model != health.model);
        self.health.push(health);
    }
}

/// The SHA-256 of a manifest's canonical form (its JSON, in field order): what an acknowledgement covers beyond the
/// files. `None` only if it can't be written, which then covers nothing.
pub(super) fn manifest_hash(manifest: &ModelManifest) -> Option<Sha256> {
    use sha2::Digest;
    let text = serde_json::to_vec(manifest).ok()?;
    Some(Sha256(sha2::Sha256::digest(&text).into()))
}

/// Where the state file lives.
pub(super) fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("state.toml")
}

/// What reading the file found.
pub(super) enum Loaded {
    /// No file yet: a first run.
    Missing,
    /// A good file.
    Read(State),
    /// A file that doesn't parse; it was moved aside to the returned path.
    Corrupt { moved_to: PathBuf, problem: String },
}

fn io_error(path: &Path, e: &io::Error) -> RegistryError {
    RegistryError::Io(format!("{}: {e}", path.display()))
}

/// Reads the state file. A file that doesn't parse is renamed to `state.toml.corrupt` (kept for a bug report) and
/// reported, never an error: the caller rebuilds from what is on disk.
pub(super) fn load(data_dir: &Path) -> Result<Loaded, RegistryError> {
    let file = path(data_dir);
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Missing),
        // Not UTF-8: as unreadable as bad TOML, so it is moved aside and rebuilt the same way.
        Err(e) if e.kind() == io::ErrorKind::InvalidData => String::from("\0"),
        Err(e) => return Err(io_error(&file, &e)),
    };
    match toml::from_str::<State>(&text) {
        Ok(state) => Ok(Loaded::Read(state)),
        Err(e) => {
            let moved_to = file.with_extension("toml.corrupt");
            fs::rename(&file, &moved_to).map_err(|e| io_error(&file, &e))?;
            Ok(Loaded::Corrupt { moved_to, problem: e.to_string() })
        }
    }
}

/// Writes the state file atomically: a temporary file in the same folder, flushed, then renamed over the old one, so
/// a crash leaves either the old file or the new one.
pub(super) fn save(data_dir: &Path, state: &State) -> Result<(), RegistryError> {
    let file = path(data_dir);
    let dir = file.parent().unwrap_or(data_dir);
    let text = toml::to_string_pretty(state).map_err(|e| RegistryError::Io(format!("writing the state file: {e}")))?;
    fs::create_dir_all(dir).map_err(|e| io_error(dir, &e))?;
    let mut tmp =
        tempfile::Builder::new().prefix("state").suffix(".tmp").tempfile_in(dir).map_err(|e| io_error(dir, &e))?;
    tmp.write_all(text.as_bytes()).map_err(|e| io_error(tmp.path(), &e))?;
    tmp.as_file().sync_all().map_err(|e| io_error(tmp.path(), &e))?;
    tmp.persist(&file).map_err(|e| io_error(&file, &e.error))?;
    Ok(())
}
