//! `WasmPlugins`: the [`Plugins`] service of the wasmtime host (design §3.1, §4.4, §6.2, §6.3).
//!
//! Each running plugin has a worker thread and its own session, opened for the person who enabled it with the
//! ceiling of §3.2. A fault ends the session `Faulted`, records the fault and schedules a restart after 1 s, then
//! 10 s; the third fault in ten minutes disables the plugin until the person turns it on again. Disabling and
//! reloading wait for the running call, end the session `Closed` and run `shutdown`.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Actor, Budget, EndReason, Grants, Limits, Scope, SessionId, SessionSpec, Sessions, Transport};
use omuzaki_plugin_api::grants::{Approval, ceiling};
use omuzaki_plugin_api::limits::{FAULT_WINDOW_SECONDS, FAULTS_TO_DISABLE, RESTART_BACKOFF_SECONDS};
use omuzaki_plugin_api::manifest::Manifest;
use omuzaki_plugin_api::state::{DisabledReason, FaultRecord, LogLevel, LogLine, PluginInfo, PluginState};
use omuzaki_plugin_api::{PluginError, PluginId, Plugins};
use wasmtime::component::Component;

use crate::bridge::{NoBridge, OpBridge};
use crate::cache::CodeCache;
use crate::engine::PluginEngine;
use crate::install::InstallRoot;
use crate::instance::{Fault, InstanceKind, InstanceSetup, insecure_seed};
use crate::log::{LogWriter, PluginLog};
use crate::package::{PackageLimits, PluginPackage, PublisherCheck};
use crate::worker::{Failed, Worker};

/// Where installs and compiled code go.
pub struct Storage {
    /// `<local data dir>/plugins/`.
    pub install: InstallRoot,
    /// `<cache dir>/plugins/`.
    pub cache: CodeCache,
}

struct Entry {
    manifest: Manifest,
    component: Component,
    hash: [u8; 32],
    approval: Approval,
    state: PluginState,
    session: Option<SessionId>,
    enabled_by: Option<Actor>,
    faults: Vec<FaultRecord>,
    restart_at: Option<Seconds>,
    log: Arc<PluginLog>,
    worker: Option<Worker>,
    busy: bool,
}

/// The wasmtime host's [`Plugins`] service.
pub struct WasmPlugins {
    engine: Arc<PluginEngine>,
    sessions: Arc<dyn Sessions>,
    person: Grants,
    bridge: Arc<dyn OpBridge>,
    writer: Option<LogWriter>,
    plugins: Mutex<BTreeMap<PluginId, Entry>>,
    idle: Condvar,
}

fn person_only(by: &Actor) -> Result<(), PluginError> {
    if matches!(by, Actor::Person { .. }) { Ok(()) } else { Err(PluginError::denied(Scope::EditSettings)) }
}

fn info_of(id: &PluginId, entry: &Entry) -> PluginInfo {
    let keep = entry.faults.len().saturating_sub(FAULTS_TO_DISABLE);
    PluginInfo {
        id: id.clone(),
        name: entry.manifest.name.clone(),
        version: entry.manifest.version.clone(),
        state: entry.state.clone(),
        requested: entry.manifest.requests.scopes.clone(),
        approved: entry.approval.scopes.clone(),
        session: entry.session,
        faults: entry.faults[keep..].to_vec(),
        restart_at: entry.restart_at,
    }
}

/// The plugin's settings as the JSON object `init` receives: each key with its default.
fn settings_json(manifest: &Manifest) -> String {
    let map: serde_json::Map<String, serde_json::Value> =
        manifest.settings.iter().map(|s| (s.key.clone(), s.default.clone())).collect();
    serde_json::Value::Object(map).to_string()
}

impl WasmPlugins {
    /// A service on `engine` that opens plugin sessions in `sessions` for a person holding `person`, writing every
    /// plugin's log through `writer` when given.
    pub fn new(
        engine: Arc<PluginEngine>,
        sessions: Arc<dyn Sessions>,
        person: Grants,
        writer: Option<LogWriter>,
    ) -> Self {
        Self {
            engine,
            sessions,
            person,
            bridge: Arc::new(NoBridge),
            writer,
            plugins: Mutex::new(BTreeMap::new()),
            idle: Condvar::new(),
        }
    }

    /// Installs a plugin from its manifest and component bytes, off, with everything its manifest asks for approved:
    /// for embedders and tests that hold the person's approval already. A package a person installs goes through
    /// [`install`](Self::install), which approves nothing. `hash` is the package's content hash, which seeds its
    /// `insecure` stream. The component is compiled and its imports and version checked now, so a refused plugin never
    /// reaches the list.
    ///
    /// # Errors
    /// [`PluginError::Manifest`] when the manifest's check has errors, the id is taken or names an op family; the
    /// errors of [`load::compile`](crate::load::compile) and [`load::check`](crate::load::check).
    pub fn add(&self, manifest: Manifest, component: &[u8], hash: [u8; 32]) -> Result<PluginId, PluginError> {
        let compiled = crate::load::compile(&self.engine, component)?;
        self.admissible(&manifest, &compiled)?;
        let approval = Approval::all_of(&manifest.requests);
        self.register(manifest, compiled, hash, approval)
    }

    /// Installs a package from `path` (a folder or a `.zip`) for the person `by`: reads it as hostile input, checks its
    /// signature against its publisher, compiles it through the compiled-code cache and checks it, and only then copies
    /// its files into `storage`'s install folder (keeping the last two versions) and adds it, off and with nothing
    /// approved: the person approves what the card shows through [`Plugins::set_approval`]. A refused package changes
    /// nothing in the install folder.
    ///
    /// # Errors
    /// [`PluginError::Denied`] when `by` is not a person; [`PluginError::Package`] for a refused package;
    /// [`PluginError::Signature`] when its signature does not hold; the errors of [`add`](Self::add).
    pub fn install(&self, path: &Path, storage: &Storage, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        let limits = PackageLimits::default();
        let package = if path.is_dir() {
            PluginPackage::open_folder(path, &limits)?
        } else {
            let bytes = std::fs::read(path).map_err(|e| PluginError::Io(e.to_string()))?;
            PluginPackage::open_zip(&bytes, &limits)?
        };
        if package.publisher() == PublisherCheck::Mismatch {
            return Err(PluginError::Signature("the signature does not match the package or its publisher".into()));
        }
        let (component, _) = storage.cache.load(&self.engine, &package.hash, package.component())?;
        self.admissible(&package.manifest, &component)?;
        let installed = storage.install.install(&package)?;
        let id = self.register(package.manifest, component, installed.hash.0, Approval::default())?;
        self.info(&id)
    }

    /// Everything that can refuse a new plugin, run before anything is kept: the manifest's check, an id that names
    /// an op family, the component's imports and version, an id already installed.
    fn admissible(&self, manifest: &Manifest, compiled: &Component) -> Result<(), PluginError> {
        if let Some(error) = manifest.check().errors().next() {
            return Err(PluginError::Manifest(error.to_string()));
        }
        let ops = omuzaki_ops_api::registrations().map(|r| (r.meta)().name);
        if let Some(op) = op_of_family(&manifest.id, ops) {
            let id = &manifest.id;
            return Err(PluginError::Manifest(format!("`{id}` is reserved: it is the family of the op `{op}`")));
        }
        crate::load::check(&self.engine, compiled, manifest)?;
        not_taken(&self.lock(), &manifest.id)
    }

    /// Adds a checked plugin to the list, off, under `approval`.
    fn register(
        &self,
        manifest: Manifest,
        compiled: Component,
        hash: [u8; 32],
        approval: Approval,
    ) -> Result<PluginId, PluginError> {
        let id = manifest.id.clone();
        let mut plugins = self.lock();
        not_taken(&plugins, &id)?;
        let log = Arc::new(PluginLog::new(id.clone(), self.engine.clock().clone(), self.writer.as_ref()));
        plugins.insert(
            id.clone(),
            Entry {
                manifest,
                component: compiled,
                hash,
                approval,
                state: PluginState::Installed,
                session: None,
                enabled_by: None,
                faults: Vec::new(),
                restart_at: None,
                log,
                worker: None,
                busy: false,
            },
        );
        Ok(id)
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<PluginId, Entry>> {
        self.plugins.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until no entry call of `id` runs, and returns the lock.
    fn drained(&self, id: &PluginId) -> MutexGuard<'_, BTreeMap<PluginId, Entry>> {
        let mut plugins = self.lock();
        while plugins.get(id).is_some_and(|e| e.busy) {
            plugins = self.idle.wait(plugins).unwrap_or_else(PoisonError::into_inner);
        }
        plugins
    }

    fn record_fault(&self, entry: &mut Entry, fault: &Fault) {
        entry.worker = None;
        entry.log.flush();
        if let Some(session) = entry.session.take() {
            // The session may already have ended (revoked); the fault is still recorded.
            let _ended = self.sessions.end(session, EndReason::faulted(fault.detail.clone()));
        }
        let now = self.engine.clock().now();
        entry.faults.retain(|f| now.0 - f.at.0 < FAULT_WINDOW_SECONDS);
        entry.faults.push(FaultRecord { fault: fault.kind, detail: fault.detail.clone(), at: now });
        let count = entry.faults.len();
        if count >= FAULTS_TO_DISABLE {
            entry.state = PluginState::disabled(DisabledReason::Faults);
            entry.restart_at = None;
        } else {
            entry.state = PluginState::faulted(fault.kind);
            let wait = RESTART_BACKOFF_SECONDS[(count - 1).min(RESTART_BACKOFF_SECONDS.len() - 1)];
            entry.restart_at = Some(Seconds(now.0 + wait));
        }
    }

    /// Stops `id`: under the lock it ends the session `Closed` and takes the worker, marking the plugin busy; off the
    /// lock `shutdown` runs (a breach there only drops the instance, and is logged), so other plugins' calls never
    /// wait for it; then the plugin is left in `after`.
    fn stop(&self, id: &PluginId, after: PluginState) -> Result<PluginInfo, PluginError> {
        let (worker, log) = {
            let mut plugins = self.drained(id);
            let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
            if let Some(session) = entry.session.take() {
                let _ended = self.sessions.end(session, EndReason::Closed);
            }
            entry.state = PluginState::Unloading;
            entry.busy = true;
            (entry.worker.take(), entry.log.clone())
        };
        if let Some(worker) = worker {
            let note = match worker.shutdown() {
                Ok(Ok(())) => None,
                Ok(Err(fault)) => Some(format!("shutdown stopped: {fault}")),
                Err(error) => Some(format!("shutdown did not run: {error}")),
            };
            if let Some(note) = note {
                // A line the bucket refuses here is counted in the next "lines dropped" line, like any other.
                let _kept = log.push(LogLevel::Warn, &note, false);
            }
        }
        log.flush();
        let mut plugins = self.lock();
        let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        entry.busy = false;
        entry.state = after;
        entry.restart_at = None;
        self.idle.notify_all();
        Ok(info_of(id, entry))
    }

    /// Starts `id` for `by`: opens its session, starts its worker and runs `init` there, off the lock.
    fn start(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        let mut plugins = self.lock();
        let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        let spec = SessionSpec {
            transport: Transport::Plugin,
            client: format!("plugin:{}@{}", entry.manifest.id, entry.manifest.version),
            model: None,
            on_behalf_of: by.clone(),
            grants: ceiling(&entry.manifest.requests, &entry.approval, &self.person),
            budget: Budget::default(),
            limits: Limits::default(),
            parent: None,
            role: None,
            fence: Vec::new(),
        };
        let session = self.sessions.open(spec).map_err(|e| PluginError::Instantiate(e.message))?;
        entry.session = Some(session.id);
        entry.enabled_by = Some(by.clone());
        entry.restart_at = None;
        entry.state = PluginState::Loading;
        entry.busy = true;
        let setup = InstanceSetup {
            kind: InstanceKind::Worker,
            log: entry.log.clone(),
            bridge: self.bridge.clone(),
            insecure_seed: insecure_seed(&entry.hash, session.id.0),
            secure_seed: None,
            data_requested: entry.manifest.requests.data,
            net_requested: entry.manifest.requests.scopes.contains(&Scope::Net),
        };
        let (component, config) = (entry.component.clone(), settings_json(&entry.manifest));
        drop(plugins);
        let started = Worker::start(id.as_str(), self.engine.clone(), component, setup, config);
        let mut plugins = self.lock();
        let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        entry.busy = false;
        self.idle.notify_all();
        match started {
            Ok(worker) => {
                entry.worker = Some(worker);
                entry.state = PluginState::Running;
                Ok(info_of(id, entry))
            }
            Err(Failed::Faulted(fault)) => {
                self.record_fault(entry, &fault);
                Err(PluginError::Fault(fault.kind))
            }
            Err(Failed::Refused(error)) => {
                if let Some(session) = entry.session.take() {
                    let _ended = self.sessions.end(session, EndReason::Closed);
                }
                entry.state = PluginState::Installed;
                Err(error)
            }
        }
    }
}

impl Plugins for WasmPlugins {
    fn list(&self) -> Vec<PluginInfo> {
        self.lock().iter().map(|(id, entry)| info_of(id, entry)).collect()
    }

    fn info(&self, id: &PluginId) -> Result<PluginInfo, PluginError> {
        self.lock().get(id).map(|entry| info_of(id, entry)).ok_or(PluginError::NotFound)
    }

    fn enable(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        {
            let mut plugins = self.drained(id);
            let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
            if entry.state == PluginState::Running {
                return Ok(info_of(id, entry));
            }
            // The person turning it on clears the fault history, so the next fault starts a fresh count.
            entry.faults.clear();
        }
        self.start(id, by)
    }

    fn disable(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        self.stop(id, PluginState::disabled(DisabledReason::Person))
    }

    fn set_approval(&self, id: &PluginId, approval: Approval, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        let mut plugins = self.lock();
        let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        let requests = &entry.manifest.requests;
        let approval = Approval {
            scopes: approval.scopes.into_iter().filter(|s| requests.scopes.contains(s)).collect(),
            hosts: approval.hosts.into_iter().filter(|h| requests.net_hosts.contains(h)).collect(),
            data: approval.data && requests.data,
        };
        // The live session's grants change first: when that is refused, the approval stays as it was.
        if let Some(session) = entry.session {
            let grants = ceiling(requests, &approval, &self.person);
            self.sessions.set_grants(session, grants, by).map_err(|e| PluginError::Instantiate(e.message))?;
        }
        entry.approval = approval;
        Ok(info_of(id, entry))
    }

    fn reload(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        {
            let mut plugins = self.drained(id);
            let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
            if entry.state != PluginState::Running {
                return Err(PluginError::Instantiate(format!("`{id}` is not running")));
            }
        }
        self.stop(id, PluginState::Unloading)?;
        self.start(id, by)
    }

    fn run_due(&self) -> Vec<PluginId> {
        let now = self.engine.clock().now();
        let due: Vec<(PluginId, Actor)> = self
            .lock()
            .iter()
            .filter(|(_, e)| matches!(e.state, PluginState::Faulted { .. }) && e.restart_at.is_some_and(|t| t <= now))
            .filter_map(|(id, e)| e.enabled_by.clone().map(|by| (id.clone(), by)))
            .collect();
        for (id, by) in &due {
            // A restart that faults again is recorded on the plugin; the next backoff applies.
            let _restarted = self.start(id, by);
        }
        due.into_iter().map(|(id, _)| id).collect()
    }

    fn log(&self, id: &PluginId, after: u64, limit: usize) -> Result<Vec<LogLine>, PluginError> {
        let plugins = self.lock();
        let entry = plugins.get(id).ok_or(PluginError::NotFound)?;
        Ok(entry.log.page(after, limit))
    }
}

fn not_taken(plugins: &BTreeMap<PluginId, Entry>, id: &PluginId) -> Result<(), PluginError> {
    if plugins.contains_key(id) {
        return Err(PluginError::Manifest(format!("a plugin `{id}` is already installed")));
    }
    Ok(())
}

/// The first of `ops` whose family (the name before its first `.`) is `id`: a plugin with that id would collide with
/// it (design §7: every first-party op family is reserved).
fn op_of_family<'a>(id: &PluginId, ops: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    ops.into_iter().find(|op| op.split('.').next() == Some(id.as_str()))
}

impl Drop for WasmPlugins {
    fn drop(&mut self) {
        // Stop every worker before the engine goes, so no thread outlives the service.
        let ids: Vec<PluginId> = self.lock().keys().cloned().collect();
        for id in ids {
            // Every id was just listed and nothing removes one, so this can only stop the plugin.
            let _stopped = self.stop(&id, PluginState::Installed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_naming_an_op_family_is_found_and_a_prefix_is_not() {
        let ops = ["face.blink", "session.open", "io.read"];
        let id = |s: &str| PluginId::new(s).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(op_of_family(&id("session"), ops), Some("session.open"));
        assert_eq!(op_of_family(&id("faces"), ops), None);
        assert_eq!(op_of_family(&id("example"), ops), None);
    }
}
