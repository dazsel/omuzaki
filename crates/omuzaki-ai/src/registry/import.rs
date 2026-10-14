//! `sync_catalogue`: models delivered between releases (`docs/design/updates.md` section 8).
//!
//! One call per verified catalogue, all or nothing: the whole change is planned on a copy of the state and written
//! with one atomic save, so an error leaves the registry as it was. The caller (`omuzaki-update`) has verified the
//! signature, each manifest's hash and `min_app_version`, and decided on replay; the registry decides only what the
//! catalogue means for the models it holds.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_ai_api::{CatalogueEntry, ImportError, ModelId, ModelRef, Origin, Sha256, SyncReport, Version, find_slot};

use super::Registry;
use super::state::{Import, State};
use crate::model_ref;

impl Registry {
    /// See [`ModelRegistry::sync_catalogue`](omuzaki_ai_api::ModelRegistry::sync_catalogue).
    pub(super) fn sync(
        &self,
        epoch: u32,
        sequence: u64,
        entries: &[CatalogueEntry],
    ) -> Result<SyncReport, ImportError> {
        let _one_change_at_a_time = self.changing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self.lock();
        let (next, report) = self.plan(&state, epoch, entries)?;
        self.commit(&mut state, next).map_err(|e| ImportError::Io(e.to_string()))?;
        tracing::info!(
            epoch,
            sequence,
            imported = report.imported.len(),
            unchanged = report.unchanged.len(),
            superseded = report.superseded.len(),
            revoked = report.revoked.len(),
            hidden = report.hidden.len(),
            "model catalogue synced"
        );
        Ok(report)
    }

    /// The state after the catalogue and what it did, or the first reason to refuse it. `current` is not touched.
    fn plan(
        &self,
        current: &State,
        epoch: u32,
        entries: &[CatalogueEntry],
    ) -> Result<(State, SyncReport), ImportError> {
        let mut next = current.clone();
        let mut report = SyncReport::default();
        let Triage { usable, listed, hidden } = triage(entries, &mut report)?;

        for entry in hidden {
            self.merge_hidden(&mut next, epoch, entry, &mut report)?;
        }
        if epoch > current.catalogue_epoch {
            revoke_unlisted(&mut next, epoch, &listed, &mut report);
        }
        // `triage` has refused differing hashes for one version, so entries of one version are identical here.
        let mut previous: Option<&CatalogueEntry> = None;
        for entry in usable {
            let key = |e: &CatalogueEntry| (e.manifest.id.clone(), e.manifest.version);
            if previous.is_some_and(|p| key(p) == key(entry)) {
                continue;
            }
            previous = Some(entry);
            self.merge(&mut next, epoch, entry, &mut report)?;
        }
        next.catalogue_epoch = next.catalogue_epoch.max(epoch);
        let revoked: Vec<ModelRef> = next.imports.iter().map(|i| i.model.clone()).filter(|m| next.revoked(m)).collect();
        forget_installations(&mut next, &revoked);
        Ok((next, report))
    }

    /// Applies one entry: confirm it, supersede an older-epoch import, or import it as a new version.
    fn merge(
        &self,
        next: &mut State,
        epoch: u32,
        entry: &CatalogueEntry,
        report: &mut SyncReport,
    ) -> Result<(), ImportError> {
        let manifest = &entry.manifest;
        let model = model_ref(manifest);
        let same = |m: &ModelRef| (&m.id, m.version) == (&manifest.id, manifest.version);

        // A shipped registration of the same version is never replaced; the same manifest is simply confirmed.
        if let Some(shipped) = self.catalogue.iter().find(|r| same(&r.model)) {
            if shipped.model != model {
                return Err(ImportError::Conflict { model });
            }
            report.unchanged.push(model);
            return Ok(());
        }
        if let Some(at) = next.imports.iter().position(|i| !i.revoked && same(&i.model)) {
            let known = &mut next.imports[at];
            if known.manifest_sha256 == entry.manifest_sha256 {
                known.epoch = known.epoch.max(epoch);
                report.unchanged.push(known.model.clone());
                return Ok(());
            }
            // Within one epoch a version is never replaced; a higher epoch is the root's recovery from a poisoned import.
            if known.epoch >= epoch {
                return Err(ImportError::Conflict { model });
            }
            known.revoked = true;
            report.superseded.push(known.model.clone());
            // The replacement takes the version's place, so the ordering rule (which a newer sibling would fail)
            // doesn't apply; only the slot must stay the same.
            if known.manifest.slot != manifest.slot {
                return Err(ImportError::SlotMismatch { model });
            }
        } else {
            self.check_version_order(next, entry, &model)?;
        }
        // Same files, other manifest (a replacement, or a re-import after a revocation): what is installed ran under
        // the old manifest (its io, providers, `min_app_version`), so it is dropped and the person installs the new
        // one. The files are cached.
        if next.installed.iter().any(|i| i.model == model && i.manifest != *manifest) {
            next.installed.retain(|i| i.model != model);
            forget_installations(next, std::slice::from_ref(&model));
        }
        next.imports.push(Import {
            model: model.clone(),
            manifest: manifest.clone(),
            manifest_sha256: entry.manifest_sha256,
            epoch,
            revoked: false,
        });
        report.imported.push(model);
        Ok(())
    }

    /// Applies an entry for a slot this app doesn't register. It confirms an import of the same manifest, and a
    /// different one is judged as `merge` would judge it, except that nothing is imported: in the same epoch it is a
    /// `Conflict`, in a higher one the older import it replaces is revoked.
    fn merge_hidden(
        &self,
        next: &mut State,
        epoch: u32,
        entry: &CatalogueEntry,
        report: &mut SyncReport,
    ) -> Result<(), ImportError> {
        let manifest = &entry.manifest;
        let model = model_ref(manifest);
        let same = |m: &ModelRef| (&m.id, m.version) == (&manifest.id, manifest.version);
        if self.catalogue.iter().any(|r| same(&r.model) && r.model != model) {
            return Err(ImportError::Conflict { model });
        }
        for known in next.imports.iter_mut().filter(|i| !i.revoked && same(&i.model)) {
            if known.manifest_sha256 == entry.manifest_sha256 {
                known.epoch = known.epoch.max(epoch);
            } else if known.epoch >= epoch {
                return Err(ImportError::Conflict { model });
            } else {
                known.revoked = true;
                report.superseded.push(known.model.clone());
            }
        }
        Ok(())
    }

    /// A new version of a known id must keep its slot and be strictly above every non-revoked version.
    fn check_version_order(&self, state: &State, entry: &CatalogueEntry, model: &ModelRef) -> Result<(), ImportError> {
        let manifest = &entry.manifest;
        let shipped = self.catalogue.iter().map(|r| (&r.manifest.id, &r.manifest));
        let imported = state.imports.iter().filter(|i| !i.revoked).map(|i| (&i.manifest.id, &i.manifest));
        let installed =
            state.installed.iter().filter(|i| !state.revoked(&i.model)).map(|i| (&i.manifest.id, &i.manifest));
        let newest = shipped
            .chain(imported)
            .chain(installed)
            .filter(|(id, _)| **id == manifest.id)
            .map(|(_, m)| m)
            .max_by_key(|m| m.version);
        let Some(newest) = newest else { return Ok(()) };
        if newest.slot != manifest.slot {
            return Err(ImportError::SlotMismatch { model: model.clone() });
        }
        if manifest.version <= newest.version {
            return Err(ImportError::NotNewer { model: model.clone(), have: newest.version });
        }
        Ok(())
    }
}

/// What a catalogue holds, sorted out.
struct Triage<'a> {
    /// Entries to apply, sorted by id and version, so a catalogue's order never matters.
    usable: Vec<&'a CatalogueEntry>,
    /// The `(id, version)` pairs of `usable`: whatever hash they carry, `merge` confirms or supersedes the import.
    listed: BTreeSet<(ModelId, Version)>,
    /// Entries for a slot this app doesn't register. They can confirm or revoke an import, never add one.
    hidden: Vec<&'a CatalogueEntry>,
}

/// Sorts the entries and refuses a catalogue that contradicts itself (two hashes for one version, hidden entries
/// included), lists a `user-*` id or carries a manifest the shipped rules refuse.
fn triage<'a>(entries: &'a [CatalogueEntry], report: &mut SyncReport) -> Result<Triage<'a>, ImportError> {
    let mut out = Triage { usable: Vec::new(), listed: BTreeSet::new(), hidden: Vec::new() };
    let mut hashes: BTreeMap<(&ModelId, Version), Sha256> = BTreeMap::new();
    for entry in entries {
        let manifest = &entry.manifest;
        if manifest.id.as_str().starts_with("user-") {
            return Err(ImportError::UserId(manifest.id.clone()));
        }
        if hashes
            .insert((&manifest.id, manifest.version), entry.manifest_sha256)
            .is_some_and(|h| h != entry.manifest_sha256)
        {
            return Err(ImportError::Conflict { model: model_ref(manifest) });
        }
        if find_slot(&manifest.slot).is_none() {
            out.hidden.push(entry);
            if !report.hidden.contains(&manifest.id) {
                report.hidden.push(manifest.id.clone());
            }
            continue;
        }
        manifest.validate(Origin::Catalogue).map_err(ImportError::Invalid)?;
        out.listed.insert((manifest.id.clone(), manifest.version));
        out.usable.push(entry);
    }
    out.usable.sort_by(|a, b| (&a.manifest.id, a.manifest.version).cmp(&(&b.manifest.id, b.manifest.version)));
    Ok(out)
}

/// Poisoning recovery: a catalogue of a higher epoch revokes every older-epoch import it doesn't list again.
/// Imports a hidden entry confirmed (`merge_hidden`) already carry this epoch.
fn revoke_unlisted(state: &mut State, epoch: u32, listed: &BTreeSet<(ModelId, Version)>, report: &mut SyncReport) {
    for import in &mut state.imports {
        let key = (import.manifest.id.clone(), import.manifest.version);
        if !import.revoked && import.epoch < epoch && !listed.contains(&key) {
            import.revoked = true;
            report.revoked.push(import.model.clone());
        }
    }
}

/// Takes `gone` out of the slots' records, so selection falls back along the chain: the previous version if it is
/// still there, then the newest installed one (`Registry::defaults`), then the shipped default.
fn forget_installations(state: &mut State, gone: &[ModelRef]) {
    for record in &mut state.slots {
        let is_gone = |m: &Option<ModelRef>| m.as_ref().is_some_and(|m| gone.contains(m));
        if is_gone(&record.previous) {
            record.previous = None;
        }
        if is_gone(&record.selected) {
            record.selected = None;
        }
        if is_gone(&record.default) {
            record.default = record.previous.take();
        }
    }
}
