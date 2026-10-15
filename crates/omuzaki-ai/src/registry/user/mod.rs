//! Models the person adds (ADR 0016, design `docs/design/ai-api.md` sections 4, 5 and 7): `add` imports and checks
//! them, `acknowledge` records the person's acceptance of the disclaimer, `select` applies the requester rule and
//! `remove` forgets them.
//!
//! A user model is checked in the order the design gives; the first stage that finds problems lists all of them, and
//! the model is kept as disabled with those words. Probing and the smoke test run in this process for now; the
//! sandboxed worker takes them over later, and until then no user model is opened for use (`open` only runs
//! shipped models).

mod import;
mod manifest;
#[cfg(test)]
mod tests;

use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_ai_api::{
    AddModel, DISCLAIMER_VERSION, EvalLevel, EvalReport, LocalFile, ManifestError, ModelEntry, ModelManifest, ModelRef,
    ModelSource, ModelStatus, Origin, Progress, RegistryError, Slot, SlotInfo, SlotMode, find_runtime, find_slot,
};

use super::catalogue::Registered;
use super::install::Env;
use super::state::{Acknowledgement, State, UserModel};
use super::{CACHE_DIR, Registry};
use crate::model_ref;
use import::{Imported, blob_path, import};

/// The status of a user model that isn't disabled.
pub(super) fn status(state: &State, model: &ModelRef) -> ModelStatus {
    if state.acknowledged(model) { ModelStatus::Ready } else { ModelStatus::NeedsAcknowledgement }
}

impl Registry {
    /// The models the person added, as the registry lists them: a failed one carries its reason as `problem`.
    pub(super) fn user_models(state: &State) -> impl Iterator<Item = Registered> + '_ {
        state.user.iter().map(|u| Registered {
            manifest: u.manifest.clone(),
            model: u.model.clone(),
            problem: u.problem.clone(),
            origin: Origin::User,
        })
    }

    /// See [`ModelRegistry::add`](omuzaki_ai_api::ModelRegistry::add).
    pub(super) fn add_user_model(
        &self,
        request: AddModel,
        progress: &dyn Progress,
    ) -> Result<ModelEntry, RegistryError> {
        let _one_change_at_a_time = self.changing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let info = Self::slot_info(&request.slot)?;
        if request.files.is_empty() {
            return Err(RegistryError::Manifest(vec![ManifestError::NoFiles]));
        }
        let cache = self.data_dir.join(CACHE_DIR);
        let mut files = Vec::with_capacity(request.files.len());
        progress.stage("import", 0.0);
        for path in &request.files {
            files.push(import(&cache, path, progress)?);
        }
        progress.stage("import", 1.0);
        let given = request.manifest.is_some();
        let manifest = match request.manifest {
            Some(manifest) => manifest,
            None => self.written_manifest(&request.slot, &files, &cache)?,
        };
        let mut problems = if given { manifest::pin_problems(&manifest, &files) } else { Vec::new() };
        if manifest.slot != request.slot {
            problems.push(format!("its manifest is for {}, not {}", manifest.slot, request.slot));
        }
        let manifest = manifest::adopt(manifest, &files);
        let model = model_ref(&manifest);
        let locals: Vec<LocalFile> = manifest
            .files
            .iter()
            .map(|f| LocalFile { name: f.name.clone(), path: blob_path(&cache, f.sha256) })
            .collect();
        let outcome =
            if problems.is_empty() { self.check_user_model(&model, &manifest, &info, &locals) } else { Err(problems) };
        let (problem, eval) = match outcome {
            Ok(report) => (None, Some(report)),
            Err(reasons) => (Some(format!("it can't be used: {}", reasons.join("; "))), None),
        };
        let mut state = self.lock();
        let mut next = state.clone();
        next.user.retain(|u| u.model != model);
        next.user.push(UserModel { model: model.clone(), manifest: manifest.clone(), problem: problem.clone(), eval });
        if let Some(person) = request.acknowledged_by {
            record(&mut next, &model, &person);
        }
        self.commit(&mut state, next)?;
        let registered = Registered { manifest, model, problem, origin: Origin::User };
        Ok(self.entry(&state, &registered, None))
    }

    /// The manifest for files that came without one: the runtime from the main file, the id from its name.
    fn written_manifest(
        &self,
        slot: &omuzaki_ai_api::SlotRef,
        files: &[Imported],
        cache: &std::path::Path,
    ) -> Result<ModelManifest, RegistryError> {
        let main = &files[0];
        let head = std::fs::read(blob_path(cache, main.sha256))
            .map_err(|e| RegistryError::Io(format!("{}: {e}", main.name)))?;
        // An unknown format still gets a manifest, so the model is listed (disabled) with the reason the checks give.
        let runtime = manifest::runtime_of(&main.name, &head).unwrap_or_else(|| {
            omuzaki_ai_api::RuntimeId::new("unknown").unwrap_or_else(|_| unreachable!("a literal slug"))
        });
        let stem =
            std::path::Path::new(&main.name).file_stem().map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let state = self.lock();
        let id = manifest::unique_id(&stem, |id| {
            state.user.iter().any(|u| {
                &u.manifest.id == id && u.manifest.files.iter().map(|f| f.sha256).ne(files.iter().map(|f| f.sha256))
            })
        })
        .ok_or_else(|| RegistryError::Manifest(vec![ManifestError::UserId(stem)]))?;
        Ok(manifest::written(id, slot, runtime, files))
    }

    /// Every stage of the checks in order; the first with problems lists them all.
    fn check_user_model(
        &self,
        model: &ModelRef,
        manifest: &ModelManifest,
        info: &SlotInfo,
        locals: &[LocalFile],
    ) -> Result<EvalReport, Vec<String>> {
        let words = |errors: Vec<ManifestError>| errors.iter().map(ToString::to_string).collect::<Vec<_>>();
        manifest.validate(Origin::User).map_err(words)?;
        let registered =
            Registered { manifest: manifest.clone(), model: model.clone(), problem: None, origin: Origin::User };
        if let Some(reason) = self.disabled(&self.lock(), &registered) {
            return Err(vec![reason]);
        }
        let runtime = find_runtime(&manifest.runtime)
            .ok_or_else(|| vec![format!("this app has no `{}` runtime to read it with", manifest.runtime)])?;
        let slot = find_slot(&info.slot).ok_or_else(|| vec![format!("no slot {} is registered", info.slot)])?;
        let source = ModelSource { model, files: locals, stages: &manifest.stages, natives: &*self.files };
        let probe = runtime.probe(&source).map_err(|e| vec![e.to_string()])?;
        let mut misfits: Vec<String> = slot
            .signature()
            .accepts(&probe.signature.with_slot_names(&manifest.io))
            .err()
            .into_iter()
            .flatten()
            .map(|m| m.to_string())
            .collect();
        misfits.extend(stream_problems(&*slot, info, manifest));
        if !misfits.is_empty() {
            return Err(misfits);
        }
        let mut session = self.load_files(model, manifest, info, locals).map_err(|e| vec![e.to_string()])?;
        let report = slot
            .evaluate(&mut *session, &Env { registry: self }, EvalLevel::Smoke)
            .map_err(|e| vec![format!("its smoke test could not run: {e}")])?;
        if report.passed() { Ok(report) } else { Err(vec![smoke_failure(&report)]) }
    }
}

/// Records `person`'s acknowledgement of `model`'s digest, once.
fn record(state: &mut State, model: &ModelRef, person: &str) {
    if state.acknowledged(model) {
        return;
    }
    // The registry has no wall clock to inject, so this is the one place the OS clock is read.
    let at = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    state.acknowledgements.push(Acknowledgement {
        digest: model.digest,
        person: person.to_owned(),
        disclaimer_version: DISCLAIMER_VERSION,
        at,
    });
}

/// What is wrong with a model's `[stream]` table for its slot's mode.
fn stream_problems(slot: &dyn Slot, info: &SlotInfo, manifest: &ModelManifest) -> Vec<String> {
    match (info.mode, manifest.stream) {
        (SlotMode::Call { .. }, Some(_)) => {
            vec![format!("{} is a request/response slot, so the manifest must not have a [stream] table", info.slot)]
        }
        (SlotMode::Stream { .. }, None) => {
            vec![format!("{} is a streaming slot, so the manifest needs a [stream] table", info.slot)]
        }
        (SlotMode::Stream { .. }, Some(spec)) => slot.check_stream(&spec).err().into_iter().collect(),
        (SlotMode::Call { .. }, None) => Vec::new(),
    }
}

/// The words for a smoke test that missed: every check below its floor, with its number.
fn smoke_failure(report: &EvalReport) -> String {
    let misses: Vec<String> = report
        .failures()
        .map(|c| {
            let m = &c.metric;
            c.floor.map_or_else(
                || format!("{} was {} on {}", m.name, m.value, m.set.id),
                |floor| format!("{} was {} on {} (the floor is {floor})", m.name, m.value, m.set.id),
            )
        })
        .collect();
    if misses.is_empty() {
        "its smoke test measured nothing".into()
    } else {
        format!("it failed its smoke test: {}", misses.join("; "))
    }
}
