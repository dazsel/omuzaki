//! [`Registry`]: the in-process [`ModelRegistry`] for shipped models, over the verified cache (design
//! `docs/design/ai-api.md` sections 4, 6, 7 and 8).
//!
//! - `catalogue`: what the app registers, parsed and checked once.
//! - `state`: the person-level state file.
//! - `import`: `sync_catalogue`, models delivered between releases.
//! - `install`: install with its smoke test, rollback, removal.
//!
//! This file holds the registry itself, the choice chain (explicit model, selection, newest installed default) and
//! the read side of the trait. User models, the sandbox and the slot session that swaps and falls back come later.

mod catalogue;
mod import;
mod install;
mod state;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use omuzaki_ai_api::{
    CatalogueEntry, EvalReport, FileSource, ImportError, ModelEntry, ModelManifest, ModelRef, ModelRegistry,
    ModelStatus, NoProgress, Origin, Provider, RegistryError, Requester, Session, SessionOptions, SlotInfo, SlotMode,
    SlotRef, SlotStatus, Stream, SyncReport, Version, find_slot, slots,
};

use crate::slot_session::SlotSession;
use catalogue::Registered;
use state::{Health, Loaded, State};

/// The person's settings the registry reads. Only what shipped models need today.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistrySettings {
    /// Execution providers the person turned off (a GPU that misbehaves). The CPU can't be turned off.
    pub disabled_providers: Vec<Provider>,
}

/// The folder under the app's data directory where the verified cache lives (`<data dir>/cache`). The app builds its
/// [`FileSource`] over it; the registry asks that source (never the folder) to rebuild a lost state file and to
/// forget a removed model's files.
pub const CACHE_DIR: &str = "cache";

/// The registry for shipped models, in this process.
///
/// A cheap handle: the data lives in one shared [`Shared`], so the slot sessions it opens can keep following their
/// slot from a loader thread of their own after `open` returns.
pub struct Registry {
    shared: Arc<Shared>,
}

/// What every handle to one [`Registry`] shares.
pub struct Shared {
    data_dir: PathBuf,
    files: Arc<dyn FileSource>,
    app_version: Version,
    settings: RegistrySettings,
    catalogue: Vec<Registered>,
    state: Mutex<State>,
    /// Held for a whole install or rollback, so two of them never interleave their reads and writes of the state.
    changing: Mutex<()>,
    notes: Vec<String>,
    /// Counts every committed state change. Slot sessions compare it with the value they last acted on, without a
    /// lock, to learn that their slot's choice may have changed.
    generation: AtomicU64,
    /// Models switched off for this app session only, with the reason: a model that crashed. The state file never
    /// records them, so they run again after a restart or a reinstall.
    session_disabled: Mutex<Vec<(ModelRef, String)>>,
}

impl Deref for Registry {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.shared
    }
}

impl Registry {
    /// The registry for every model the linked crates registered with `register_model!`.
    ///
    /// `files` is the verified cache the models are downloaded through, at `<data_dir>/cache` ([`CACHE_DIR`]).
    ///
    /// # Errors
    /// [`RegistryError::Io`] when the state folder can't be read or written. A bad registration or a corrupted state
    /// file is not an error: see [`notes`](Self::notes).
    pub fn new(
        data_dir: impl Into<PathBuf>,
        files: Arc<dyn FileSource>,
        app_version: Version,
        settings: RegistrySettings,
    ) -> Result<Self, RegistryError> {
        let models = omuzaki_ai_api::shipped_models().map(|m| (m.id.to_owned(), m.manifest.to_owned()));
        Self::with_models(data_dir, files, app_version, settings, models)
    }

    /// Like [`new`](Self::new), for an explicit list of `(literal id, manifest TOML)` registrations: tests register
    /// extra versions this way.
    ///
    /// # Errors
    /// As [`new`](Self::new).
    pub fn with_models(
        data_dir: impl Into<PathBuf>,
        files: Arc<dyn FileSource>,
        app_version: Version,
        settings: RegistrySettings,
        models: impl IntoIterator<Item = (String, String)>,
    ) -> Result<Self, RegistryError> {
        let data_dir = data_dir.into();
        let (catalogue, mut notes) = catalogue::read(models);
        let mut to_reinstall = Vec::new();
        let state = match state::load(&data_dir)? {
            Loaded::Read(state) => state,
            Loaded::Missing => State::default(),
            Loaded::Corrupt { moved_to, problem } => {
                notes.push(format!(
                    "the model state file was unreadable ({problem}); it was kept as {} and the models whose files \
                     were already downloaded are being tested again",
                    moved_to.display()
                ));
                to_reinstall = install::recoverable(files.as_ref(), &catalogue, &app_version);
                State::default()
            }
        };
        for note in &notes {
            tracing::warn!("{note}");
        }
        let registry = Self {
            shared: Arc::new(Shared {
                data_dir,
                files,
                app_version,
                settings,
                catalogue,
                state: Mutex::new(state),
                changing: Mutex::new(()),
                notes,
                generation: AtomicU64::new(0),
                session_disabled: Mutex::new(Vec::new()),
            }),
        };
        registry.reinstall(&to_reinstall);
        Ok(registry)
    }

    /// What went wrong at startup without stopping it: registrations that were refused and a state file that had to
    /// be rebuilt. For the Models page and the log.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The state, locked. A panic elsewhere can't leave it half-written (writes happen on a copy), so a poisoned lock
    /// is still good to read.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Replaces the state with `next` once it is on disk, and counts the change for the slot sessions.
    fn commit(&self, state: &mut State, next: State) -> Result<(), RegistryError> {
        state::save(&self.data_dir, &self.persistent(&next))?;
        *state = next;
        self.generation.fetch_add(1, Ordering::Release);
        Ok(())
    }

    /// `state` as the file keeps it: without the models only this app session has switched off.
    fn persistent<'a>(&self, state: &'a State) -> Cow<'a, State> {
        let mut off = self.session_disabled.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        // A model that was reinstalled, or failed for another reason since, is no longer ours to hide.
        off.retain(|(model, reason)| state.health(model).is_some_and(|h| &h.reason == reason));
        if off.is_empty() {
            return Cow::Borrowed(state);
        }
        let mut kept = state.clone();
        kept.health.retain(|h| !off.iter().any(|(model, _)| model == &h.model));
        Cow::Owned(kept)
    }

    /// True if the app is older than the model needs.
    fn too_new(&self, manifest: &ModelManifest) -> bool {
        manifest.min_app_version > self.app_version
    }

    /// The reason a catalogued model can't be used, if there is one.
    fn disabled(&self, state: &State, entry: &Registered) -> Option<String> {
        if let Some(problem) = &entry.problem {
            return Some(problem.clone());
        }
        if self.too_new(&entry.manifest) {
            let needs = entry.manifest.min_app_version;
            return Some(format!("it needs Omuzaki {needs} or newer (this is {}); update the app", self.app_version));
        }
        state.health(&entry.model).map(|h| h.reason.clone())
    }

    /// The installed model that can run now, if `model` is one. The app's current registration decides when there
    /// is one (a raised `min_app_version` or a refused manifest disables an installed model, the same as `models`
    /// reports); a version the app no longer registers is judged by the manifest kept in the state file.
    fn usable<'a>(&self, state: &'a State, model: &ModelRef) -> Option<&'a state::Installed> {
        let installed = state.installed(model)?;
        if state.revoked(model) {
            return None;
        }
        let disabled = match self.catalogue.iter().find(|r| &r.model == model) {
            Some(entry) => self.disabled(state, entry).is_some(),
            None => state.health(model).is_some() || self.too_new(&installed.manifest),
        };
        (!disabled).then_some(installed)
    }

    /// The slot's shipped defaults, best first: the one it records (so a rollback holds), then the newest installed
    /// model the app can run. What `open` falls back to and what an evaluation that needs "the shipped model" uses;
    /// the person's selection is no part of it.
    fn defaults<'a>(&self, state: &'a State, slot: &SlotRef) -> Vec<&'a state::Installed> {
        let recorded = state.slot(slot).and_then(|r| r.default.as_ref()).and_then(|m| self.usable(state, m));
        let newest = state
            .installed
            .iter()
            // Only this slot's models may run in it: another slot's model could fit the same tensors and skip this
            // slot's evaluation. Filtered before picking the newest, or a newer model elsewhere would hide it.
            .filter(|i| &i.manifest.slot == slot && self.usable(state, &i.model).is_some())
            .max_by_key(|i| i.manifest.version);
        let mut out: Vec<&state::Installed> = Vec::new();
        for installed in recorded.into_iter().chain(newest).filter(|i| &i.manifest.slot == slot) {
            if !out.iter().any(|o| o.model == installed.model) {
                out.push(installed);
            }
        }
        out
    }

    /// The models to try for `slot`, best first: the explicit one, the selection, then the [`defaults`](Self::defaults).
    /// Models that can't run, or belong to another slot, are left out.
    fn chain<'a>(&self, state: &'a State, slot: &SlotRef, explicit: Option<&ModelRef>) -> Vec<&'a state::Installed> {
        let selected = state.slot(slot).and_then(|r| r.selected.as_ref());
        let named = [explicit, selected].into_iter().flatten().filter_map(|m| self.usable(state, m));
        let mut out: Vec<&state::Installed> = Vec::new();
        for installed in named.filter(|i| &i.manifest.slot == slot).chain(self.defaults(state, slot)) {
            if !out.iter().any(|o| o.model == installed.model) {
                out.push(installed);
            }
        }
        out
    }

    /// Another handle to this registry, for a slot session's loader thread.
    pub(crate) fn handle(&self) -> Self {
        Self { shared: Arc::clone(&self.shared) }
    }

    /// How many state changes were committed so far (see [`Shared`]'s `generation`).
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Loads the first model of `slot`'s choice chain that opens (see [`chain`](Self::chain)), skipping the ones that
    /// don't. If the chain reaches `current` first, that model is already the best that can run: nothing is loaded.
    ///
    /// # Errors
    /// The last load error, or `NoModel` when the chain is empty.
    pub(crate) fn load_chain(
        &self,
        slot: &SlotRef,
        explicit: Option<&ModelRef>,
        current: Option<&ModelRef>,
    ) -> Result<Chosen, RegistryError> {
        let info = Self::slot_info(slot)?;
        let (candidates, wanted) = {
            let state = self.lock();
            let candidates: Vec<state::Installed> = self.chain(&state, slot, explicit).into_iter().cloned().collect();
            let selected = state.slot(slot).and_then(|r| r.selected.clone());
            (candidates, explicit.cloned().or(selected))
        };
        let mut last = None;
        for installed in &candidates {
            if current == Some(&installed.model) {
                return Ok(Chosen::Keep);
            }
            match self.load(installed, &info) {
                Ok(session) => return Ok(Chosen::Loaded { session, model: installed.model.clone(), wanted }),
                Err(e) => {
                    tracing::warn!(model = %installed.model, "couldn't open: {e}");
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or(RegistryError::NoModel { slot: slot.clone(), without_model: info.without_model }))
    }

    /// Switches `model` off for this app session, with `reason`: it drops out of the choice chains and shows as
    /// disabled in `models`. The state file doesn't keep it (see [`persistent`](Self::persistent)).
    pub(crate) fn disable_for_session(&self, model: &ModelRef, reason: &str) {
        let mut state = self.lock();
        let mut next = state.clone();
        next.set_health(Health { reason: reason.to_owned(), model: model.clone(), eval: None });
        self.session_disabled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((model.clone(), reason.to_owned()));
        if let Err(e) = self.commit(&mut state, next.clone()) {
            // The file isn't written for these anyway, so a failing disk must not keep a crashing model running.
            tracing::warn!(%model, "couldn't save the model state: {e}");
            *state = next;
            self.generation.fetch_add(1, Ordering::Release);
        }
    }

    /// The slot's info, or `UnknownSlot`.
    fn slot_info(slot: &SlotRef) -> Result<SlotInfo, RegistryError> {
        find_slot(slot).map(|s| s.info()).ok_or_else(|| RegistryError::UnknownSlot(slot.clone()))
    }

    /// Loads `installed` in this process with the manifest's providers, filtered by the person's settings, and the
    /// slot's thread count.
    fn load(&self, installed: &state::Installed, info: &SlotInfo) -> Result<Box<dyn Session>, RegistryError> {
        let locals = self.fetch_files(&installed.manifest, &NoProgress)?;
        self.load_files(&installed.model, &installed.manifest, info, &locals)
    }

    fn options(&self, manifest: &ModelManifest, info: &SlotInfo) -> SessionOptions {
        let providers = manifest
            .providers
            .iter()
            .copied()
            .filter(|p| *p == Provider::Cpu || !self.settings.disabled_providers.contains(p))
            .collect();
        SessionOptions::new(providers, info.threads)
    }

    fn entry(&self, state: &State, entry: &Registered, active: Option<&ModelRef>) -> ModelEntry {
        let status = match self.disabled(state, entry) {
            _ if state.revoked(&entry.model) => ModelStatus::Revoked,
            Some(reason) => ModelStatus::Disabled { reason },
            None if state.installed(&entry.model).is_some() => ModelStatus::Ready,
            None => ModelStatus::NotInstalled,
        };
        let eval = state
            .installed(&entry.model)
            .and_then(|i| i.eval.clone())
            .or_else(|| state.health(&entry.model).and_then(|h| h.eval.clone()));
        entry_of(&entry.manifest, &entry.model, entry.origin, status, active == Some(&entry.model), eval)
    }

    /// Every model the registry knows: the catalogue, then the catalogue imports (live ones first, so a revoked record
    /// of the same model never hides them), then installed versions nothing else lists.
    fn known(&self, state: &State) -> Vec<Registered> {
        let mut out: Vec<Registered> = self.catalogue.clone();
        let imports = state.imports.iter().filter(|i| !i.revoked).chain(state.imports.iter().filter(|i| i.revoked));
        for import in imports {
            if !out.iter().any(|r| r.model == import.model) {
                out.push(Registered {
                    manifest: import.manifest.clone(),
                    model: import.model.clone(),
                    problem: None,
                    origin: Origin::Catalogue,
                });
            }
        }
        for installed in &state.installed {
            if !out.iter().any(|r| r.model == installed.model) {
                out.push(Registered {
                    manifest: installed.manifest.clone(),
                    model: installed.model.clone(),
                    problem: None,
                    origin: Origin::Shipped,
                });
            }
        }
        out
    }
}

/// What [`Registry::load_chain`] found.
pub(crate) enum Chosen {
    /// The model the caller already runs is the best the chain offers.
    Keep,
    /// A freshly loaded session.
    Loaded {
        /// The session.
        session: Box<dyn Session>,
        /// The model it runs.
        model: ModelRef,
        /// The model that was asked for (explicit, else selected), which `model` replaces if they differ.
        wanted: Option<ModelRef>,
    },
}

fn entry_of(
    manifest: &ModelManifest,
    model: &ModelRef,
    origin: Origin,
    status: ModelStatus,
    active: bool,
    eval: Option<EvalReport>,
) -> ModelEntry {
    ModelEntry {
        model: model.clone(),
        name: manifest.name.clone(),
        slot: manifest.slot.clone(),
        runtime: manifest.runtime.clone(),
        origin,
        licence: manifest.card.licence.clone(),
        size: manifest.size(),
        status,
        active,
        eval,
    }
}

impl ModelRegistry for Registry {
    fn slots(&self) -> Vec<SlotStatus> {
        let state = self.lock();
        let mut out: Vec<SlotStatus> = slots()
            .map(|slot| {
                let info = slot.info();
                let selected = state.slot(&info.slot).and_then(|r| r.selected.clone());
                let active = self.chain(&state, &info.slot, None).first().map(|i| i.model.clone());
                let fallback = selected
                    .as_ref()
                    .filter(|s| active.as_ref() != Some(*s))
                    .map(|s| format!("{s} can't run, so the slot runs its default"));
                SlotStatus { info, selected, active, fallback }
            })
            .collect();
        out.sort_by(|a, b| a.info.slot.cmp(&b.info.slot));
        out
    }

    fn models(&self, slot: Option<&SlotRef>) -> Vec<ModelEntry> {
        let state = self.lock();
        let mut out: Vec<ModelEntry> = self
            .known(&state)
            .iter()
            .filter(|r| slot.is_none_or(|s| &r.manifest.slot == s))
            .map(|r| {
                let active = self.chain(&state, &r.manifest.slot, None).first().map(|i| i.model.clone());
                self.entry(&state, r, active.as_ref())
            })
            .collect();
        out.sort_by(|a, b| (&a.slot, &a.model.id, a.model.version).cmp(&(&b.slot, &b.model.id, b.model.version)));
        out
    }

    fn open(&self, slot: &SlotRef, model: Option<&ModelRef>) -> Result<Box<dyn Session>, RegistryError> {
        let info = Self::slot_info(slot)?;
        if matches!(info.mode, SlotMode::Stream { .. }) {
            return Err(RegistryError::WrongMode(slot.clone()));
        }
        if let Some(wanted) = model {
            let state = self.lock();
            let elsewhere = self.known(&state).into_iter().find(|r| &r.model == wanted && &r.manifest.slot != slot);
            if elsewhere.is_some() {
                return Err(RegistryError::WrongSlot { model: wanted.clone(), wanted: slot.clone() });
            }
        }
        // Read before the chain, so a change that lands meanwhile is noticed by the session's first run.
        let generation = self.generation();
        match self.load_chain(slot, model, None)? {
            Chosen::Loaded { session, wanted, .. } => {
                Ok(Box::new(SlotSession::start(self.handle(), slot, model.cloned(), generation, session, wanted)?))
            }
            Chosen::Keep => Err(RegistryError::NoModel { slot: slot.clone(), without_model: info.without_model }),
        }
    }

    fn open_stream(&self, slot: &SlotRef, _model: Option<&ModelRef>) -> Result<Stream, RegistryError> {
        let info = Self::slot_info(slot)?;
        match info.mode {
            SlotMode::Call { .. } => Err(RegistryError::WrongMode(slot.clone())),
            SlotMode::Stream { .. } => {
                Err(RegistryError::NoModel { slot: slot.clone(), without_model: info.without_model })
            }
        }
    }

    fn install(&self, model: &ModelRef, progress: &dyn omuzaki_ai_api::Progress) -> Result<(), RegistryError> {
        self.install_model(model, progress)
    }

    fn rollback(&self, slot: &SlotRef) -> Result<ModelRef, RegistryError> {
        self.rollback_slot(slot)
    }

    fn add(
        &self,
        request: omuzaki_ai_api::AddModel,
        _progress: &dyn omuzaki_ai_api::Progress,
    ) -> Result<ModelEntry, RegistryError> {
        let what = request.manifest.map_or_else(|| request.slot.to_string(), |m| m.id.to_string());
        Err(RegistryError::UnknownModel(format!("{what}: adding your own models isn't available yet")))
    }

    fn acknowledge(&self, model: &ModelRef, _person: &str) -> Result<(), RegistryError> {
        Err(RegistryError::UnknownModel(model.to_string()))
    }

    fn select(&self, slot: &SlotRef, model: Option<&ModelRef>, _by: Requester) -> Result<(), RegistryError> {
        Self::slot_info(slot)?;
        let mut state = self.lock();
        if let Some(model) = model {
            let known = self.known(&state);
            let found = known.iter().find(|r| &r.model == model);
            let found = found.ok_or_else(|| RegistryError::UnknownModel(model.to_string()))?;
            if &found.manifest.slot != slot {
                return Err(RegistryError::WrongSlot { model: model.clone(), wanted: slot.clone() });
            }
            if self.usable(&state, model).is_none() {
                let reason = if state.revoked(model) {
                    "it was withdrawn by the model catalogue".into()
                } else {
                    self.disabled(&state, found).unwrap_or_else(|| "it isn't installed yet; install it first".into())
                };
                return Err(RegistryError::NotUsable { model: model.clone(), reason });
            }
        }
        let mut next = state.clone();
        next.slot_mut(slot).selected = model.cloned();
        self.commit(&mut state, next)
    }

    fn remove(&self, model: &ModelRef) -> Result<(), RegistryError> {
        self.remove_model(model)
    }

    fn sync_catalogue(&self, epoch: u32, sequence: u64, entries: &[CatalogueEntry]) -> Result<SyncReport, ImportError> {
        self.sync(epoch, sequence, entries)
    }
}
