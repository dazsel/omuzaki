//! Install with a smoke test, rollback and removal: the parts of [`Registry`] that change what is installed.

use omuzaki_ai_api::{
    Check, EvalEnv, EvalError, EvalLevel, EvalReport, FetchError, FileSource, LoadError, LocalFile, ModelManifest,
    ModelRef, ModelSource, Progress, RegistryError, Session, SlotInfo, SlotRef, Version, find_runtime, find_slot,
};

use super::Registry;
use super::catalogue::Registered;
use super::state::{Health, Installed, State};

/// The evaluation environment of a smoke test: fixtures through the registry's cache, and another slot's default
/// opened the way `open` would.
pub(super) struct Env<'a> {
    pub(super) registry: &'a Registry,
}

impl EvalEnv for Env<'_> {
    fn files(&self) -> &dyn FileSource {
        &*self.registry.files
    }

    fn shipped(&self, slot: &SlotRef) -> Result<Box<dyn Session>, EvalError> {
        let needs = || EvalError::Needs(slot.clone());
        let info = find_slot(slot).map(|s| s.info()).ok_or_else(needs)?;
        let candidates: Vec<_> = {
            let state = self.registry.lock();
            self.registry.defaults(&state, slot).into_iter().cloned().collect()
        };
        candidates.iter().find_map(|installed| self.registry.load(installed, &info).ok()).ok_or_else(needs)
    }
}

/// The words for a failed smoke test: every check that missed its floor, with its number.
fn failure_reason(report: &EvalReport) -> String {
    let misses: Vec<String> = report.failures().map(describe).collect();
    if misses.is_empty() {
        return "its smoke test measured nothing".into();
    }
    format!("it failed its smoke test: {}", misses.join("; "))
}

fn describe(check: &Check) -> String {
    let m = &check.metric;
    match check.floor {
        Some(floor) => format!("{} was {} on {} (the floor is {floor})", m.name, m.value, m.set.id),
        None => format!("{} was {} on {}", m.name, m.value, m.set.id),
    }
}

fn from_fetch(e: FetchError) -> RegistryError {
    match e {
        FetchError::Cancelled => RegistryError::Cancelled,
        other => RegistryError::Fetch(other),
    }
}

/// The catalogued models whose files are all in the cache already (and verified), oldest version first: what a lost
/// or corrupted state file is rebuilt from. Bytes in the cache prove nothing about whether an install succeeded (a
/// smoke-failing version leaves its files behind), so the caller installs each one again, smoke test included.
/// `files` is the registry's own source, so the rebuild looks where the registry looks.
pub(super) fn recoverable(files: &dyn FileSource, catalogue: &[Registered], app_version: &Version) -> Vec<ModelRef> {
    let mut found: Vec<&Registered> = catalogue
        .iter()
        .filter(|r| r.problem.is_none() && &r.manifest.min_app_version <= app_version)
        .filter(|r| {
            r.manifest.files.iter().all(|f| match files.lookup(f.sha256) {
                Ok(found) => found.is_some(),
                // A failed read is not "absent": say so, or the model would silently drop out of the rebuild.
                Err(e) => {
                    tracing::warn!(model = %r.model, "couldn't check a cached file: {e}");
                    false
                }
            })
        })
        .collect();
    found.sort_by(|a, b| (&a.manifest.slot, a.manifest.version).cmp(&(&b.manifest.slot, b.manifest.version)));
    found.into_iter().map(|r| r.model.clone()).collect()
}

impl Registry {
    /// Installs each of `models` again, in order, with its smoke test: how a lost state file is rebuilt. A model that
    /// now fails is listed as disabled, as it would have been the first time.
    pub(super) fn reinstall(&self, models: &[ModelRef]) {
        for model in models {
            if let Err(e) = self.install_model(model, &omuzaki_ai_api::NoProgress) {
                tracing::warn!(%model, "not recovered: {e}");
            }
        }
    }

    /// Every file of `manifest` as a verified local path, downloading what the cache lacks.
    pub(super) fn fetch_files(
        &self,
        manifest: &ModelManifest,
        progress: &dyn Progress,
    ) -> Result<Vec<LocalFile>, RegistryError> {
        manifest
            .files
            .iter()
            .map(|file| {
                let pin = file.pin().ok_or_else(|| {
                    RegistryError::Manifest(vec![omuzaki_ai_api::ManifestError::NoUrl(file.name.clone())])
                })?;
                let path = self.files.get(&pin, progress).map_err(from_fetch)?;
                Ok(LocalFile { name: file.name.clone(), path })
            })
            .collect()
    }

    /// Loads verified files through the manifest's runtime, in this process.
    pub(super) fn load_files(
        &self,
        model: &ModelRef,
        manifest: &ModelManifest,
        info: &SlotInfo,
        files: &[LocalFile],
    ) -> Result<Box<dyn Session>, RegistryError> {
        let runtime = find_runtime(&manifest.runtime)
            .ok_or_else(|| LoadError::Runtime(format!("this app has no `{}` runtime", manifest.runtime)))?;
        let slot = find_slot(&info.slot).ok_or_else(|| RegistryError::UnknownSlot(info.slot.clone()))?;
        let source = ModelSource { model, files, stages: &manifest.stages, natives: &*self.files };
        let session = runtime.load(&source, &slot.signature(), &manifest.io, &self.options(manifest, info))?;
        Ok(session)
    }

    /// See [`ModelRegistry::install`](omuzaki_ai_api::ModelRegistry::install).
    pub(super) fn install_model(&self, model: &ModelRef, progress: &dyn Progress) -> Result<(), RegistryError> {
        let _one_change_at_a_time = self.changing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let (entry, already_default) = {
            let state = self.lock();
            let entry = self
                .known(&state)
                .into_iter()
                .find(|r| &r.model == model)
                .ok_or_else(|| RegistryError::UnknownModel(model.to_string()))?;
            if state.revoked(model) {
                let reason = "it was withdrawn by the model catalogue".to_owned();
                return Err(RegistryError::NotUsable { model: model.clone(), reason });
            }
            let default = state.slot(&entry.manifest.slot).and_then(|r| r.default.as_ref());
            let already = default == Some(model) && self.usable(&state, model).is_some();
            if let Some(reason) = entry.problem.clone().or_else(|| self.too_new_reason(&entry.manifest)) {
                return Err(RegistryError::NotUsable { model: model.clone(), reason });
            }
            (entry, already)
        };
        if already_default {
            return Ok(());
        }
        let manifest = &entry.manifest;
        let info = Self::slot_info(&manifest.slot)?;
        let files = self.fetch_files(manifest, progress)?;
        let mut session = match self.load_files(model, manifest, &info, &files) {
            Ok(session) => session,
            Err(e @ RegistryError::Load(_)) => return self.refuse(model, e.to_string(), None, e),
            Err(e) => return Err(e),
        };
        let slot = find_slot(&info.slot).ok_or_else(|| RegistryError::UnknownSlot(info.slot.clone()))?;
        let report = slot.evaluate(&mut *session, &Env { registry: self }, EvalLevel::Smoke)?;
        if progress.is_cancelled() {
            return Err(RegistryError::Cancelled);
        }
        if !report.passed() {
            let reason = failure_reason(&report);
            return self.refuse(model, reason, Some(report.clone()), RegistryError::BelowFloor(report));
        }
        let mut state = self.lock();
        let mut next = state.clone();
        next.set_installed(Installed { model: model.clone(), manifest: manifest.clone(), eval: Some(report) });
        let record = next.slot_mut(&manifest.slot);
        if record.default.as_ref() != Some(model) {
            record.previous = record.default.replace(model.clone());
        }
        self.commit(&mut state, next)
    }

    /// Records that `model` failed its install, so it is listed as disabled with `reason`, and returns `error`.
    /// What was installed before stays as it was.
    fn refuse(
        &self,
        model: &ModelRef,
        reason: String,
        eval: Option<EvalReport>,
        error: RegistryError,
    ) -> Result<(), RegistryError> {
        let mut state = self.lock();
        let mut next = state.clone();
        next.set_health(Health { reason, model: model.clone(), eval });
        self.commit(&mut state, next)?;
        Err(error)
    }

    fn too_new_reason(&self, manifest: &ModelManifest) -> Option<String> {
        self.too_new(manifest).then(|| {
            format!(
                "it needs Omuzaki {} or newer (this is {}); update the app",
                manifest.min_app_version, self.app_version
            )
        })
    }

    /// See [`ModelRegistry::rollback`](omuzaki_ai_api::ModelRegistry::rollback).
    pub(super) fn rollback_slot(&self, slot: &SlotRef) -> Result<ModelRef, RegistryError> {
        let _one_change_at_a_time = self.changing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::slot_info(slot)?;
        let mut state = self.lock();
        let target = state.slot(slot).and_then(|r| r.previous.clone());
        let target = target
            .filter(|m| self.usable(&state, m).is_some())
            .ok_or_else(|| RegistryError::NoRollback(slot.clone()))?;
        let mut next = state.clone();
        let record = next.slot_mut(slot);
        // The version we leave stays installed; "previous" is spent, so a second rollback has nowhere to go.
        record.default = Some(target.clone());
        record.previous = None;
        self.commit(&mut state, next)?;
        Ok(target)
    }

    /// See [`ModelRegistry::remove`](omuzaki_ai_api::ModelRegistry::remove).
    pub(super) fn remove_model(&self, model: &ModelRef) -> Result<(), RegistryError> {
        let _one_change_at_a_time = self.changing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self.lock();
        if !self.known(&state).iter().any(|r| &r.model == model) {
            return Err(RegistryError::UnknownModel(model.to_string()));
        }
        let Some(removed) = state.installed(model).cloned() else { return Ok(()) };
        let mut next = state.clone();
        next.installed.retain(|i| &i.model != model);
        for record in &mut next.slots {
            if record.selected.as_ref() == Some(model) {
                record.selected = None;
            }
            if record.previous.as_ref() == Some(model) {
                record.previous = None;
            }
            if record.default.as_ref() == Some(model) {
                record.default = record.previous.take();
            }
        }
        self.commit(&mut state, next.clone())?;
        self.delete_blobs(&removed.manifest, &next);
        Ok(())
    }

    /// Forgets the cached files of a removed model that no installed model still uses. Best effort: a file that
    /// can't be deleted only costs disk, and the cache checks files again before it hands them out.
    fn delete_blobs(&self, manifest: &ModelManifest, state: &State) {
        for file in &manifest.files {
            let shared = state.installed.iter().any(|i| i.manifest.files.iter().any(|f| f.sha256 == file.sha256));
            if shared {
                continue;
            }
            if let Err(e) = self.files.forget(file.sha256) {
                tracing::warn!(sha256 = %file.sha256, "couldn't delete a removed model's file: {e}");
            }
        }
    }
}
