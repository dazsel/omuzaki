//! Models the person adds (ADR 0016, design `docs/design/ai-api.md` sections 4, 5 and 7): `add` imports and checks
//! them, `acknowledge` records the person's acceptance of the disclaimer, `select` applies the requester rule
//! (`Registry::select`) and `remove` forgets them and frees their files.
//!
//! A user model is checked in the order the design gives; the first stage that finds problems lists all of them, and
//! the model is kept as disabled with those words. Its runtime only ever sees it inside a sandboxed worker
//! (`crate::sandbox`): the probe, the smoke test's session and every session `open` runs.

mod import;
mod manifest;
#[cfg(test)]
mod tests;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omuzaki_ai_api::{
    AddModel, DISCLAIMER_VERSION, EvalLevel, EvalReport, LoadError, LocalFile, ManifestError, ModelEntry,
    ModelManifest, ModelRef, Origin, Progress, RegistryError, Session, Slot, SlotInfo, SlotMode, find_slot,
};
use omuzaki_update_api::clock::{Timestamp, WallClock};

use super::catalogue::Registered;
use super::install::{Env, failure_reason};
use super::state::{Acknowledgement, State, UserModel};
use super::{CACHE_DIR, Registry};
use crate::model_ref;
use crate::sandbox::{self, LoadSpec};
use import::{Imported, blob_path, import};

/// Why a user model the person hasn't accepted can't be selected or opened yet.
pub(super) const NEEDS_ACKNOWLEDGEMENT: &str =
    "the person hasn't accepted its licence notice yet; they acknowledge it on the Models page (model.acknowledge)";

/// The calendar clock acknowledgements are stamped with unless the app sets another: the operating system's.
pub(super) struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> Timestamp {
        // A clock set before 1970 records 0: the time is for people to read, nothing is decided by it.
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        Timestamp(i64::try_from(seconds).unwrap_or(i64::MAX))
    }
}

impl Registered {
    /// A model the person added, as the registry lists it: a failed one carries its reason as `problem`.
    pub(super) fn user(u: &UserModel) -> Self {
        Self { manifest: u.manifest.clone(), model: u.model.clone(), problem: u.problem.clone(), origin: Origin::User }
    }
}

impl Registry {
    /// The models the person added, failed ones included.
    pub(super) fn user_models(state: &State) -> impl Iterator<Item = Registered> + '_ {
        state.user.iter().map(Registered::user)
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
            record(&mut next, &model, &person, &*self.clock());
        }
        self.commit(&mut state, next)?;
        let registered = Registered { manifest, model, problem, origin: Origin::User };
        Ok(self.entry(&state, &registered, None))
    }

    /// See [`ModelRegistry::acknowledge`](omuzaki_ai_api::ModelRegistry::acknowledge). Only models the person added
    /// need it; a shipped model has nothing to acknowledge, so that is a no-op.
    pub(super) fn acknowledge_user_model(&self, model: &ModelRef, person: &str) -> Result<(), RegistryError> {
        let _one_change_at_a_time = self.changing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self.lock();
        if state.user(model).is_none() {
            let known = self.known(&state).iter().any(|r| &r.model == model);
            return if known { Ok(()) } else { Err(RegistryError::UnknownModel(model.to_string())) };
        }
        if state.acknowledged(model) {
            return Ok(());
        }
        let mut next = state.clone();
        record(&mut next, model, person, &*self.clock());
        self.commit(&mut state, next)
    }

    /// See [`ModelRegistry::remove`](omuzaki_ai_api::ModelRegistry::remove), for a model the person added: it is
    /// forgotten with its acknowledgement (adding the same file again asks again), a slot that selected it goes back to
    /// its shipped default, and its copies in the cache are deleted unless another model uses the same bytes. The
    /// person's originals were never touched.
    pub(super) fn remove_user_model(&self, model: &ModelRef) -> Result<(), RegistryError> {
        let _one_change_at_a_time = self.changing.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut state = self.lock();
        let Some(removed) = state.user(model).cloned() else {
            return Err(RegistryError::UnknownModel(model.to_string()));
        };
        let mut next = state.clone();
        next.user.retain(|u| &u.model != model);
        // Another model the person added may have the same files, hence the same digest, under another manifest.
        if !next.user.iter().any(|u| u.model.digest == model.digest) {
            next.acknowledgements.retain(|a| a.digest != model.digest);
        }
        for record in &mut next.slots {
            if record.selected.as_ref() == Some(model) {
                record.selected = None;
            }
        }
        self.commit(&mut state, next.clone())?;
        drop(state);
        self.delete_blobs(&removed.manifest, &next);
        Ok(())
    }

    /// Opens a user model the person added, from the files `add` imported. A file the cache no longer holds (or holds
    /// changed) can't be had again from anywhere, so the model must be added again.
    pub(super) fn load_user(
        &self,
        model: &ModelRef,
        manifest: &ModelManifest,
        info: &SlotInfo,
    ) -> Result<Box<dyn Session>, RegistryError> {
        let locals = self.user_files(manifest)?;
        Ok(self.load_sandboxed(model, manifest, info, &locals)?)
    }

    /// Loads a user model in a worker of its own, with the slot's run deadline.
    fn load_sandboxed(
        &self,
        model: &ModelRef,
        manifest: &ModelManifest,
        info: &SlotInfo,
        locals: &[LocalFile],
    ) -> Result<Box<dyn Session>, LoadError> {
        let slot = find_slot(&info.slot)
            .ok_or_else(|| LoadError::Runtime(format!("no slot {} is registered", info.slot)))?
            .signature();
        let spec =
            LoadSpec { slot: &slot, io: &manifest.io, options: self.options(manifest, info), stall: stall(info) };
        let session = sandbox::load(&self.sandbox(), &sandboxed(model, manifest, locals), spec, &*self.files)?;
        Ok(Box::new(session))
    }

    /// The verified cached copy of every file of a user model.
    fn user_files(&self, manifest: &ModelManifest) -> Result<Vec<LocalFile>, RegistryError> {
        manifest
            .files
            .iter()
            .map(|file| match self.files.lookup(file.sha256) {
                Ok(Some(path)) => Ok(LocalFile { name: file.name.clone(), path }),
                Ok(None) => Err(RegistryError::Io(format!(
                    "{:?} is no longer in the model cache (or changed there); add the model again",
                    file.name
                ))),
                Err(e) => Err(RegistryError::Fetch(e)),
            })
            .collect()
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
        let slot = find_slot(&info.slot).ok_or_else(|| vec![format!("no slot {} is registered", info.slot)])?;
        let sandboxed = sandboxed(model, manifest, locals);
        let probe = sandbox::probe(&self.sandbox(), &sandboxed, &*self.files).map_err(|e| vec![e.to_string()])?;
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
        let mut session = self.load_sandboxed(model, manifest, info, locals).map_err(|e| vec![e.to_string()])?;
        let report = slot
            .evaluate(&mut *session, &Env { registry: self }, EvalLevel::Smoke)
            .map_err(|e| vec![format!("its smoke test could not run: {e}")])?;
        if report.passed() { Ok(report) } else { Err(vec![failure_reason(&report)]) }
    }
}

/// A user model as the worker gets it.
fn sandboxed<'a>(model: &'a ModelRef, manifest: &'a ModelManifest, locals: &[LocalFile]) -> sandbox::Model<'a> {
    sandbox::Model {
        model,
        runtime: &manifest.runtime,
        files: locals.iter().map(|f| (f.name.clone(), f.path.clone())).collect(),
        stages: &manifest.stages,
    }
}

/// How long one run of a model in this slot may take before its worker counts as stalled: the slot's own deadline for
/// a request/response slot. The stream host (C10) sets its block deadline; until then a stream slot's smoke test gets
/// [`STREAM_RUN_LIMIT`] per block.
fn stall(info: &SlotInfo) -> Duration {
    match info.mode {
        SlotMode::Call { stall_ms } => Duration::from_millis(u64::from(stall_ms)),
        SlotMode::Stream { .. } => STREAM_RUN_LIMIT,
    }
}

/// See [`stall`].
const STREAM_RUN_LIMIT: Duration = Duration::from_secs(2);

/// Records `person`'s acknowledgement of `model`'s digest, once: the first acceptance of a digest is the record.
fn record(state: &mut State, model: &ModelRef, person: &str, clock: &dyn WallClock) {
    if state.acknowledged(model) {
        return;
    }
    state.acknowledgements.push(Acknowledgement {
        digest: model.digest,
        person: person.to_owned(),
        disclaimer_version: DISCLAIMER_VERSION,
        at: u64::try_from(clock.now().0).unwrap_or(0),
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
