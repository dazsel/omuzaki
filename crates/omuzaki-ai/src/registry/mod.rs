//! [`Registry`]: the in-process [`ModelRegistry`] for shipped models, over the verified cache (design
//! `docs/design/ai-api.md` sections 4, 6, 7 and 8).
//!
//! - `catalogue`: what the app registers, parsed and checked once.
//! - `state`: the person-level state file.
//! - `import`: `sync_catalogue`, models delivered between releases.
//! - `install`: install with its smoke test, rollback, removal.
//!
//! This file holds the registry itself, the choice chain (explicit model, selection, newest installed default) and
//! the read side of the trait. `open` returns a slot session (`crate::slot_session`) that follows the chain; user
//! models and the sandbox come later.

mod catalogue;
mod consent;
mod import;
mod install;
mod progress;
mod state;
#[cfg(test)]
mod tests;
mod user;

use std::borrow::Cow;
use std::ops::Deref;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock, Weak};

use omuzaki_ai_api::{
    CatalogueEntry, ConsentLookup, EvalReport, FileSource, ImportError, LoadError, ModelEntry, ModelManifest, ModelRef,
    ModelRegistry, ModelStatus, NoConsentRecords, NoProgress, Origin, Provider, RegistryError, Requester, Session,
    SessionOptions, SlotInfo, SlotMode, SlotRef, SlotStatus, Stream, SyncReport, Version, find_slot, slots,
};
use omuzaki_update_api::clock::WallClock;

use crate::sandbox::SandboxSettings;
use crate::slot_session::{self, Loader, SlotSession};
use catalogue::Registered;
pub use consent::IMPERSONATION_WARNING;
use consent::Imitation;
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
/// slot from the registry's loader thread after `open` returns.
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
    /// Counts every committed state change. Changed only with the state locked, so a chain read with the state
    /// locked knows which change it reflects; the loader publishes nothing chosen from an older one.
    generation: AtomicU64,
    /// Models switched off for this app session only, with the reason: a model that crashed. The state file never
    /// records them, so they run again after a restart or a reinstall.
    session_disabled: Mutex<Vec<(ModelRef, String)>>,
    /// The open slot sessions, oldest first. Each one's applied record says what answers in it; for `slots` and
    /// `models` the newest session of a slot speaks for it.
    sessions: Mutex<Vec<Arc<slot_session::Shared>>>,
    next_session: AtomicU64,
    /// The one loader thread every slot session shares, started on first need.
    loader: Arc<Loader>,
    /// Why the loader thread couldn't be started, while it can't. Also held while starting it.
    loader_failure: Mutex<Option<String>>,
    /// Models that failed to load as a slot session's replacement, with the runtime's words: `slots` says why the
    /// model it expected isn't the one running. Forgotten when the model loads.
    load_failures: Mutex<Vec<(ModelRef, String)>>,
    /// Where consent records for models that imitate a real person are found (SE-05). [`NoConsentRecords`] until the
    /// app wires its lookup.
    consent: RwLock<Arc<dyn ConsentLookup>>,
    /// The calendar time an acknowledgement records.
    clock: RwLock<Arc<dyn WallClock>>,
    /// How user models' worker processes are started.
    sandbox: RwLock<SandboxSettings>,
    /// The download fraction of each slot whose install is fetching files.
    downloads: Mutex<Vec<(SlotRef, f32)>>,
}

impl Drop for Shared {
    /// The registry is gone: so is its loader thread.
    fn drop(&mut self) {
        self.loader.close();
    }
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
                sessions: Mutex::new(Vec::new()),
                next_session: AtomicU64::new(0),
                loader: Arc::new(Loader::new()),
                loader_failure: Mutex::new(None),
                load_failures: Mutex::new(Vec::new()),
                consent: RwLock::new(Arc::new(NoConsentRecords)),
                clock: RwLock::new(Arc::new(user::SystemWallClock)),
                sandbox: RwLock::new(SandboxSettings::default()),
                downloads: Mutex::new(Vec::new()),
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

    /// Wires the app's consent lookup (SE-05) in place of [`NoConsentRecords`]. Call it again whenever the records it
    /// reads may have changed (a project was opened, a record was added): the open slot sessions look at their models
    /// again, as after any change.
    pub fn set_consent(&self, lookup: Arc<dyn ConsentLookup>) {
        *self.consent.write().unwrap_or_else(PoisonError::into_inner) = lookup;
        self.changed(&self.lock());
    }

    /// Sets the clock acknowledgements record their time by (the system's calendar clock by default).
    pub fn set_clock(&self, clock: Arc<dyn WallClock>) {
        *self.clock.write().unwrap_or_else(PoisonError::into_inner) = clock;
    }

    /// Sets how the worker processes that run user models are started (by default `omuzaki-model-worker` beside the
    /// running executable, with [`DEFAULT_MEMORY_MIB`](crate::sandbox::DEFAULT_MEMORY_MIB)).
    pub fn set_sandbox(&self, settings: SandboxSettings) {
        *self.sandbox.write().unwrap_or_else(PoisonError::into_inner) = settings;
    }

    fn sandbox(&self) -> SandboxSettings {
        self.sandbox.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn consent(&self) -> Arc<dyn ConsentLookup> {
        Arc::clone(&self.consent.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn clock(&self) -> Arc<dyn WallClock> {
        Arc::clone(&self.clock.read().unwrap_or_else(PoisonError::into_inner))
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
        // Only now, with the file written and the change taken, is a record that no longer applies safe to forget.
        self.session_off().retain(|(model, reason)| state.health(model).is_some_and(|h| &h.reason == reason));
        self.changed(state);
        Ok(())
    }

    /// Counts a change to `state` (still locked) and tells the open slot sessions: one whose model can't be used any
    /// more stops answering at once, and the loader looks at each of them.
    fn changed(&self, state: &State) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let sessions = self.session_list();
        for session in sessions.iter() {
            session.withdraw(generation, &|model| self.usable(state, model).is_some());
            self.loader.poke(session);
        }
        if !sessions.is_empty() {
            drop(sessions);
            self.start_loader();
        }
    }

    fn session_off(&self) -> MutexGuard<'_, Vec<(ModelRef, String)>> {
        self.session_disabled.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `state` as the file keeps it: without the models only this app session has switched off. A model that failed
    /// for another reason since is no longer ours to hide, so the reason must match.
    fn persistent<'a>(&self, state: &'a State) -> Cow<'a, State> {
        let off = self.session_off();
        if !state.health.iter().any(|h| off.iter().any(|(m, r)| m == &h.model && r == &h.reason)) {
            return Cow::Borrowed(state);
        }
        let mut kept = state.clone();
        kept.health.retain(|h| !off.iter().any(|(m, r)| m == &h.model && r == &h.reason));
        Cow::Owned(kept)
    }

    /// `state` as if nothing had crashed in this app session.
    fn without_session_off(&self, state: &State) -> State {
        let mut ideal = state.clone();
        let off = self.session_off();
        ideal.health.retain(|h| !off.iter().any(|(m, r)| m == &h.model && r == &h.reason));
        ideal
    }

    /// The model `slot` would run if nothing had crashed in this app session: what a fallback stands in for.
    fn ideal(&self, state: &State, slot: &SlotRef, explicit: Option<&ModelRef>) -> Option<ModelRef> {
        let ideal = self.without_session_off(state);
        self.chain(&ideal, slot, explicit).first().map(|i| i.model.clone())
    }

    /// True if the app is older than the model needs.
    fn too_new(&self, manifest: &ModelManifest) -> bool {
        manifest.min_app_version > self.app_version
    }

    /// The reason a known model can't be used, if there is one: its registration or check failed, the app is too old,
    /// it failed or crashed, or it claims to imitate a person in a slot whose models can't.
    fn disabled(&self, state: &State, entry: &Registered) -> Option<String> {
        if let Some(problem) = &entry.problem {
            return Some(problem.clone());
        }
        if self.too_new(&entry.manifest) {
            let needs = entry.manifest.min_app_version;
            return Some(format!("it needs Omuzaki {needs} or newer (this is {}); update the app", self.app_version));
        }
        if let Some(health) = state.health(&entry.model) {
            return Some(health.reason.clone());
        }
        consent::refusal(&entry.manifest)
    }

    /// Where a known model stands, first match wins (`docs/design/ai-api.md` section 7).
    fn status(&self, state: &State, entry: &Registered) -> ModelStatus {
        if state.revoked(&entry.model) {
            return ModelStatus::Revoked;
        }
        if let Some(reason) = self.disabled(state, entry) {
            return ModelStatus::Disabled { reason };
        }
        if entry.origin == Origin::User {
            if !state.acknowledged(&entry.model, &entry.manifest) {
                return ModelStatus::NeedsAcknowledgement;
            }
        } else if state.installed(&entry.model).is_none() {
            return ModelStatus::NotInstalled;
        }
        match consent::judge(&entry.manifest, &*self.consent()) {
            Imitation::NeedsConsent { .. } => ModelStatus::NeedsConsent,
            Imitation::Fine => ModelStatus::Ready,
        }
    }

    /// Why a known model that isn't `Ready` can't be selected or opened, in words; `None` if it is ready.
    fn not_ready_because(&self, state: &State, entry: &Registered) -> Option<String> {
        match self.status(state, entry) {
            ModelStatus::Ready => None,
            ModelStatus::Revoked => Some("it was withdrawn by the model catalogue".into()),
            ModelStatus::Disabled { reason } => Some(reason),
            ModelStatus::NotInstalled => Some("it isn't installed yet; install it first".into()),
            ModelStatus::NeedsAcknowledgement => Some(user::NEEDS_ACKNOWLEDGEMENT.into()),
            ModelStatus::NeedsConsent => match consent::judge(&entry.manifest, &*self.consent()) {
                Imitation::NeedsConsent { person, likeness } => {
                    Some(Imitation::needs_consent_reason(&person, likeness))
                }
                Imitation::Fine => None,
            },
            other => Some(format!("its status is {other:?}")),
        }
    }

    /// `model` as the registry knows it in `state`: the person's model, the app's registration, or a version only the
    /// state file still knows. `None` if it is none of them.
    fn registered(&self, state: &State, model: &ModelRef) -> Option<Registered> {
        if let Some(user) = state.user(model) {
            return Some(Registered::user(user));
        }
        if let Some(entry) = self.catalogue.iter().find(|r| &r.model == model) {
            return Some(entry.clone());
        }
        self.known(state).into_iter().find(|r| &r.model == model)
    }

    /// The model, if it can run now: a user model the person added and acknowledged, or an installed shipped one,
    /// either `Ready`. The app's current registration decides when there is one (a raised `min_app_version` or a
    /// refused manifest disables an installed model, the same as `models` reports); a version the app no longer
    /// registers is judged by the manifest kept in the state file.
    fn usable<'a>(&self, state: &'a State, model: &ModelRef) -> Option<Runnable<'a>> {
        let (runnable, registered) = if let Some(user) = state.user(model) {
            (Runnable { model: &user.model, manifest: &user.manifest, origin: Origin::User }, Registered::user(user))
        } else {
            let installed = state.installed(model)?;
            let registered = self.catalogue.iter().find(|r| &r.model == model).cloned().unwrap_or_else(|| Registered {
                manifest: installed.manifest.clone(),
                model: model.clone(),
                problem: None,
                origin: Origin::Shipped,
            });
            (Runnable { model: &installed.model, manifest: &installed.manifest, origin: Origin::Shipped }, registered)
        };
        (self.status(state, &registered) == ModelStatus::Ready).then_some(runnable)
    }

    /// The slot's shipped defaults, best first: the one it records (so a rollback holds), then the newest installed
    /// model the app can run. What `open` falls back to and what an evaluation that needs "the shipped model" uses;
    /// the person's selection is no part of it, and neither is any model the person added.
    fn defaults<'a>(&self, state: &'a State, slot: &SlotRef) -> Vec<Runnable<'a>> {
        let recorded = state
            .slot(slot)
            .and_then(|r| r.default.as_ref())
            .and_then(|m| self.usable(state, m))
            .filter(|r| r.origin != Origin::User);
        let newest = state
            .installed
            .iter()
            // Only this slot's models may run in it: another slot's model could fit the same tensors and skip this
            // slot's evaluation. Filtered before picking the newest, or a newer model elsewhere would hide it.
            .filter_map(|i| self.usable(state, &i.model).filter(|r| &r.manifest.slot == slot))
            .max_by_key(|r| r.manifest.version);
        let mut out: Vec<Runnable<'a>> = Vec::new();
        for runnable in recorded.into_iter().chain(newest).filter(|r| &r.manifest.slot == slot) {
            if !out.iter().any(|o| o.model == runnable.model) {
                out.push(runnable);
            }
        }
        out
    }

    /// The models to try for `slot`, best first: the explicit one, the selection, then the [`defaults`](Self::defaults).
    /// Models that can't run, or belong to another slot, are left out.
    fn chain<'a>(&self, state: &'a State, slot: &SlotRef, explicit: Option<&ModelRef>) -> Vec<Runnable<'a>> {
        let selected = state.slot(slot).and_then(|r| r.selected.as_ref());
        let named = [explicit, selected].into_iter().flatten().filter_map(|m| self.usable(state, m));
        let mut out: Vec<Runnable<'a>> = Vec::new();
        for runnable in named.filter(|r| &r.manifest.slot == slot).chain(self.defaults(state, slot)) {
            if !out.iter().any(|o| o.model == runnable.model) {
                out.push(runnable);
            }
        }
        out
    }

    /// Another handle to this registry, for a slot session it opens.
    pub(crate) fn handle(&self) -> Self {
        Self { shared: Arc::clone(&self.shared) }
    }

    /// How many state changes were committed so far (see [`Shared`]'s `generation`).
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Loads the first model of `slot`'s choice chain that opens (see [`chain`](Self::chain)), skipping the ones that
    /// don't and the ones in `failed` (the caller couldn't prepare what this pass loaded of them; their failure is
    /// recorded). If the chain reaches a model in `held` first, that one is already the best that can run: nothing is
    /// loaded. Returns the generation of the state the chain was read from.
    ///
    /// # Errors
    /// The last load error, or `NoModel` when nothing else is left. Either way nothing in `held` is in the chain.
    pub(crate) fn load_chain(
        &self,
        slot: &SlotRef,
        explicit: Option<&ModelRef>,
        held: &[ModelRef],
        failed: &[ModelRef],
    ) -> (u64, Result<Chosen, RegistryError>) {
        let (generation, candidates) = {
            let state = self.lock();
            let candidates: Vec<Candidate> =
                self.chain(&state, slot, explicit).into_iter().map(Candidate::from).collect();
            (self.generation(), candidates)
        };
        let info = match Self::slot_info(slot) {
            Ok(info) => info,
            Err(e) => return (generation, Err(e)),
        };
        let mut last = None;
        for installed in candidates.iter().filter(|i| !failed.contains(&i.model)) {
            if held.contains(&installed.model) {
                return (generation, Ok(Chosen::Keep(installed.model.clone())));
            }
            match self.load_caught(installed, &info) {
                Ok(session) => {
                    self.load_failures_list().retain(|(m, _)| m != &installed.model);
                    return (generation, Ok(Chosen::Loaded { session, model: installed.model.clone() }));
                }
                Err(e) => {
                    tracing::warn!(model = %installed.model, "couldn't open: {e}");
                    self.record_load_failure(&installed.model, e.to_string());
                    last = Some(e);
                }
            }
        }
        let none = || RegistryError::NoModel { slot: slot.clone(), without_model: info.without_model.clone() };
        (generation, Err(last.unwrap_or_else(none)))
    }

    /// Records why a slot session couldn't load `model`, for `slots`, until a load of it succeeds.
    pub(crate) fn record_load_failure(&self, model: &ModelRef, reason: String) {
        let mut failures = self.load_failures_list();
        failures.retain(|(m, _)| m != model);
        failures.push((model.clone(), reason));
    }

    /// [`load`](Self::load), with a runtime that panics recorded as a load failure like any other: one broken runtime
    /// must not take the loader thread (and every slot session) or the caller with it.
    fn load_caught(&self, candidate: &Candidate, info: &SlotInfo) -> Result<Box<dyn Session>, RegistryError> {
        catch_unwind(AssertUnwindSafe(|| self.load(candidate, info)))
            .unwrap_or_else(|_| Err(RegistryError::Load(LoadError::Runtime(LOAD_PANICKED.to_owned()))))
    }

    /// Switches `model` off for this app session, with `reason`: it drops out of the choice chains and shows as
    /// disabled in `models`. The state file doesn't keep it (see [`persistent`](Self::persistent)).
    pub(crate) fn disable_for_session(&self, model: &ModelRef, reason: &str) {
        let mut state = self.lock();
        let mut next = state.clone();
        next.set_health(Health { reason: reason.to_owned(), model: model.clone(), eval: None });
        self.session_off().push((model.clone(), reason.to_owned()));
        if let Err(e) = self.commit(&mut state, next.clone()) {
            // The file isn't written for these anyway, so a failing disk must not keep a crashing model running.
            tracing::warn!(%model, "couldn't save the model state: {e}");
            *state = next;
            self.changed(&state);
        }
    }

    /// A fresh id for a slot session.
    pub(crate) fn next_session_id(&self) -> u64 {
        self.next_session.fetch_add(1, Ordering::Relaxed)
    }

    /// Notes that `session` is open: commits tell it about changes, and `slots` reads what answers in it.
    pub(crate) fn add_session(&self, session: Arc<slot_session::Shared>) {
        self.session_list().push(session);
    }

    /// Notes that session `id` is gone.
    pub(crate) fn forget_session(&self, id: u64) {
        self.session_list().retain(|s| s.id != id);
    }

    /// The open sessions that asked the loader for something, for the loader thread.
    pub(crate) fn sessions_with_work(&self) -> Vec<Arc<slot_session::Shared>> {
        self.session_list().iter().filter(|s| s.take_work()).cloned().collect()
    }

    fn session_list(&self) -> MutexGuard<'_, Vec<Arc<slot_session::Shared>>> {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// True if `model` can run now.
    pub(crate) fn is_usable(&self, model: &ModelRef) -> bool {
        self.usable(&self.lock(), model).is_some()
    }

    /// The model `slot` would run if nothing had crashed or failed to load in this app session: what a slot session
    /// that runs another one stands in for.
    pub(crate) fn wanted(&self, slot: &SlotRef, explicit: Option<&ModelRef>) -> Option<ModelRef> {
        self.ideal(&self.lock(), slot, explicit)
    }

    /// The loader thread's handle. Every slot session of this registry shares it.
    pub(crate) fn loader(&self) -> Arc<Loader> {
        Arc::clone(&self.loader)
    }

    /// Starts the loader thread if it isn't running. If the OS refuses it, the reason shows in `slots` and the next
    /// `open`, `reset` or commit tries again; meanwhile every slot session keeps what it runs.
    pub(crate) fn start_loader(&self) {
        let mut failure = self.loader_failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.loader.started() {
            return;
        }
        match self.loader.start(Arc::downgrade(&self.shared)) {
            Ok(()) => *failure = None,
            Err(e) => {
                tracing::error!("couldn't start the thread that loads models: {e}");
                *failure = Some(format!("Omuzaki couldn't start the thread that loads models ({e})"));
            }
        }
    }

    /// A handle to the registry `shared` points at, if it still exists.
    pub(crate) fn upgrade(shared: &Weak<Shared>) -> Option<Self> {
        shared.upgrade().map(|shared| Self { shared })
    }

    fn load_failures_list(&self) -> MutexGuard<'_, Vec<(ModelRef, String)>> {
        self.load_failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Why `model` isn't running when it should be: it crashed in this app session, a slot session couldn't load it,
    /// it can't be used, or there is no loader to load it. `None`: nothing is known against it (it is loading).
    fn trouble_with(&self, state: &State, model: &ModelRef) -> Option<String> {
        let crashed = self.session_off().iter().find(|(m, _)| m == model).map(|(_, reason)| reason.clone());
        crashed
            .or_else(|| self.load_failures_list().iter().find(|(m, _)| m == model).map(|(_, e)| e.clone()))
            .or_else(|| self.unusable_because(state, model))
            .or_else(|| self.loader_failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())
    }

    /// Why `model` can't be used, if it can't.
    fn unusable_because(&self, state: &State, model: &ModelRef) -> Option<String> {
        if self.usable(state, model).is_some() {
            return None;
        }
        let entry =
            self.registered(state, model).filter(|r| r.origin == Origin::User || state.installed(model).is_some());
        let Some(entry) = entry else { return Some("it isn't installed".to_owned()) };
        Some(
            self.not_ready_because(state, &entry)
                .unwrap_or_else(|| format!("it needs a newer Omuzaki than {}", self.app_version)),
        )
    }

    /// What answers in `slot` now: what its newest open slot session runs, or with none open, what `open` would load.
    fn active_in(&self, state: &State, slot: &SlotRef) -> Option<ModelRef> {
        let newest = self.session_list().iter().rev().find(|s| &s.slot == slot).cloned();
        match newest {
            Some(session) => session.answering(),
            None => self.chain(state, slot, None).first().map(|i| i.model.clone()),
        }
    }

    /// The `fallback` line of `slots`: `expected` should run, and `active` does instead.
    fn fallback_text(&self, state: &State, expected: &ModelRef, active: Option<&ModelRef>, info: &SlotInfo) -> String {
        let then = match active {
            Some(active) => format!("so the slot runs {active}"),
            None => info.without_model.clone(),
        };
        match self.trouble_with(state, expected) {
            Some(why) => format!("{expected} can't run ({why}), {then}"),
            None => format!("{expected} is loading, {then} until it is ready"),
        }
    }

    /// The slot's info, or `UnknownSlot`.
    fn slot_info(slot: &SlotRef) -> Result<SlotInfo, RegistryError> {
        find_slot(slot).map(|s| s.info()).ok_or_else(|| RegistryError::UnknownSlot(slot.clone()))
    }

    /// Loads `candidate` with the manifest's providers, filtered by the person's settings, and the slot's thread
    /// count: a shipped model in this process, from files the cache verifies (downloading what it lacks); a user model
    /// from the files `add` imported.
    fn load(&self, candidate: &Candidate, info: &SlotInfo) -> Result<Box<dyn Session>, RegistryError> {
        if candidate.origin == Origin::User {
            return self.load_user(&candidate.model, &candidate.manifest, info);
        }
        let locals = self.fetch_files(&candidate.manifest, &NoProgress)?;
        self.load_files(&candidate.model, &candidate.manifest, info, &locals)
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
        let status = self.status(state, entry);
        let eval = state
            .installed(&entry.model)
            .and_then(|i| i.eval.clone())
            .or_else(|| state.user(&entry.model).and_then(|u| u.eval.clone()))
            .or_else(|| state.health(&entry.model).and_then(|h| h.eval.clone()));
        entry_of(&entry.manifest, &entry.model, entry.origin, status, active == Some(&entry.model), eval)
    }

    /// Every model the registry knows: the catalogue, then the catalogue imports (live ones first, so a revoked record
    /// of the same model never hides them), then installed versions nothing else lists.
    fn known(&self, state: &State) -> Vec<Registered> {
        let mut out: Vec<Registered> = self.catalogue.clone();
        out.extend(Self::user_models(state));
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

/// A model the choice chain can run, as the state file holds it.
#[derive(Clone, Copy)]
pub(super) struct Runnable<'a> {
    pub model: &'a ModelRef,
    pub manifest: &'a ModelManifest,
    pub origin: Origin,
}

/// A [`Runnable`] the loader keeps after the state is unlocked.
pub(super) struct Candidate {
    model: ModelRef,
    manifest: ModelManifest,
    origin: Origin,
}

impl From<Runnable<'_>> for Candidate {
    fn from(r: Runnable<'_>) -> Self {
        Self { model: r.model.clone(), manifest: r.manifest.clone(), origin: r.origin }
    }
}

/// What [`Registry::load_chain`] found.
pub(crate) enum Chosen {
    /// This model, which the caller holds, is the best the chain offers.
    Keep(ModelRef),
    /// A freshly loaded session.
    Loaded {
        /// The session.
        session: Box<dyn Session>,
        /// The model it runs.
        model: ModelRef,
    },
}

/// What a runtime that panicked while loading is recorded as.
const LOAD_PANICKED: &str = "its runtime failed while loading it";

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
        imitates: manifest.card.imitates.clone(),
    }
}

impl ModelRegistry for Registry {
    fn slots(&self) -> Vec<SlotStatus> {
        let state = self.lock();
        let mut out: Vec<SlotStatus> = slots()
            .map(|slot| {
                let info = slot.info();
                let selected = state.slot(&info.slot).and_then(|r| r.selected.clone());
                let active = self.active_in(&state, &info.slot);
                // What the person asked for (else the shipped default): whenever something else answers, say why.
                let expected = selected.clone().or_else(|| self.ideal(&state, &info.slot, None));
                let fallback = expected
                    .filter(|e| Some(e) != active.as_ref())
                    .map(|e| self.fallback_text(&state, &e, active.as_ref(), &info));
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
                let active = self.active_in(&state, &r.manifest.slot);
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
            if let Some(entry) = self.registered(&state, wanted) {
                if &entry.manifest.slot != slot {
                    return Err(RegistryError::WrongSlot { model: wanted.clone(), wanted: slot.clone() });
                }
                // Waiting for the person is not a failure to fall back from: the caller named this model, so say why
                // it can't run rather than quietly running another.
                let status = self.status(&state, &entry);
                if matches!(status, ModelStatus::NeedsAcknowledgement | ModelStatus::NeedsConsent) {
                    let reason = self.not_ready_because(&state, &entry).unwrap_or_default();
                    return Err(RegistryError::NotUsable { model: wanted.clone(), reason });
                }
            }
        }
        let (generation, chosen) = self.load_chain(slot, model, &[], &[]);
        let Chosen::Loaded { session, model: loaded } = chosen? else {
            // Nothing is held, so the chain can't reach a held model.
            return Err(RegistryError::NoModel { slot: slot.clone(), without_model: info.without_model });
        };
        self.start_loader();
        Ok(Box::new(SlotSession::start(self.handle(), slot, model.cloned(), generation, session, loaded)?))
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

    fn downloading(&self, slot: &SlotRef) -> Option<f32> {
        self.download_fraction(slot)
    }

    fn rollback(&self, slot: &SlotRef) -> Result<ModelRef, RegistryError> {
        self.rollback_slot(slot)
    }

    fn add(
        &self,
        request: omuzaki_ai_api::AddModel,
        progress: &dyn omuzaki_ai_api::Progress,
    ) -> Result<ModelEntry, RegistryError> {
        self.add_user_model(request, progress)
    }

    fn acknowledge(&self, model: &ModelRef, person: &str) -> Result<(), RegistryError> {
        self.acknowledge_user_model(model, person)
    }

    fn select(&self, slot: &SlotRef, model: Option<&ModelRef>, by: Requester) -> Result<(), RegistryError> {
        Self::slot_info(slot)?;
        let mut state = self.lock();
        if let Some(model) = model {
            let found = self.registered(&state, model).ok_or_else(|| RegistryError::UnknownModel(model.to_string()))?;
            if &found.manifest.slot != slot {
                return Err(RegistryError::WrongSlot { model: model.clone(), wanted: slot.clone() });
            }
            // ADR 0016 rule 5: an agent never switches to a model the person hasn't accepted; it asks them instead.
            let status = self.status(&state, &found);
            if by == Requester::Agent && status == ModelStatus::NeedsAcknowledgement {
                return Err(RegistryError::NeedsPerson(model.clone()));
            }
            if let Some(reason) = self.not_ready_because(&state, &found) {
                return Err(RegistryError::NotUsable { model: model.clone(), reason });
            }
        }
        let mut next = state.clone();
        next.slot_mut(slot).selected = model.cloned();
        self.commit(&mut state, next)
    }

    fn remove(&self, model: &ModelRef) -> Result<(), RegistryError> {
        if self.lock().user(model).is_some() {
            return self.remove_user_model(model);
        }
        self.remove_model(model)
    }

    fn sync_catalogue(&self, epoch: u32, sequence: u64, entries: &[CatalogueEntry]) -> Result<SyncReport, ImportError> {
        self.sync(epoch, sequence, entries)
    }
}
