//! `WasmPlugins`: the [`Plugins`] service of the wasmtime host (design §3.1, §4.4, §6.2, §6.3).
//!
//! Each running plugin has a worker thread and its own session, opened for the person who enabled it with the
//! ceiling of §3.2. A fault ends the session `Faulted`, records the fault and schedules a restart after 1 s, then
//! 10 s; the third fault in ten minutes disables the plugin until the person turns it on again. Disabling and
//! reloading wait for the running call, end the session `Closed` and run `shutdown`.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Actor, Budget, EndReason, Grants, Limits, Scope, SessionId, SessionSpec, Sessions, Transport};
use omuzaki_plugin_api::limits::{FAULT_WINDOW_SECONDS, FAULTS_TO_DISABLE, RESTART_BACKOFF_SECONDS};
use omuzaki_plugin_api::{
    Approval, DisabledReason, FaultRecord, LogLevel, LogLine, Manifest, PluginError, PluginId, PluginInfo, PluginState,
    Plugins, ceiling,
};
use wasmtime::component::Component;

use crate::bridge::{NoBridge, OpBridge};
use crate::engine::PluginEngine;
use crate::instance::{Fault, InstanceKind, InstanceSetup, insecure_seed};
use crate::log::{LogWriter, PluginLog};
use crate::worker::{Failed, Worker};

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

    /// Installs a plugin from its manifest and component bytes, off, with everything its manifest asks for approved.
    /// `hash` is the package's content hash, which seeds its `insecure` stream. The component is compiled and its
    /// imports and version checked now, so a refused plugin never reaches the list.
    ///
    /// # Errors
    /// [`PluginError::Manifest`] when the manifest's check has errors or the id is taken; the errors of
    /// [`load::compile`](crate::load::compile) and [`load::check`](crate::load::check).
    pub fn add(&self, manifest: Manifest, component: &[u8], hash: [u8; 32]) -> Result<PluginId, PluginError> {
        if let Some(error) = manifest.check().errors().next() {
            return Err(PluginError::Manifest(error.to_string()));
        }
        let compiled = crate::load::compile(&self.engine, component)?;
        crate::load::check(&self.engine, &compiled, &manifest)?;
        let id = manifest.id.clone();
        let mut plugins = self.lock();
        if plugins.contains_key(&id) {
            return Err(PluginError::Manifest(format!("a plugin `{id}` is already installed")));
        }
        let log = Arc::new(PluginLog::new(id.clone(), self.engine.clock().clone(), self.writer.as_ref()));
        let approval = Approval::all_of(&manifest.requests);
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

    /// Ends the session `Closed` and runs `shutdown`; a breach there only drops the instance, and is logged.
    fn stop(&self, entry: &mut Entry) {
        if let Some(session) = entry.session.take() {
            let _ended = self.sessions.end(session, EndReason::Closed);
        }
        if let Some(worker) = entry.worker.take() {
            let note = match worker.shutdown() {
                Ok(Ok(())) => None,
                Ok(Err(fault)) => Some(format!("shutdown stopped: {fault}")),
                Err(error) => Some(format!("shutdown did not run: {error}")),
            };
            if let Some(note) = note {
                // A line the bucket refuses here is counted in the next "lines dropped" line, like any other.
                let _kept = entry.log.push(LogLevel::Warn, &note, false);
            }
        }
        entry.log.flush();
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
        let mut plugins = self.drained(id);
        let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        self.stop(entry);
        entry.state = PluginState::disabled(DisabledReason::Person);
        entry.restart_at = None;
        Ok(info_of(id, entry))
    }

    fn set_approval(&self, id: &PluginId, approval: Approval, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        let mut plugins = self.lock();
        let entry = plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        let requests = &entry.manifest.requests;
        entry.approval = Approval {
            scopes: approval.scopes.into_iter().filter(|s| requests.scopes.contains(s)).collect(),
            hosts: approval.hosts.into_iter().filter(|h| requests.net_hosts.contains(h)).collect(),
            data: approval.data && requests.data,
        };
        if let Some(session) = entry.session {
            let grants = ceiling(requests, &entry.approval, &self.person);
            self.sessions.set_grants(session, grants, by).map_err(|e| PluginError::Instantiate(e.message))?;
        }
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
            self.stop(entry);
            entry.state = PluginState::Unloading;
        }
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

impl Drop for WasmPlugins {
    fn drop(&mut self) {
        // Stop every worker before the engine goes, so no thread outlives the service.
        let mut plugins = self.lock();
        let ids: Vec<PluginId> = plugins.keys().cloned().collect();
        for id in ids {
            if let Some(entry) = plugins.get_mut(&id) {
                self.stop(entry);
            }
        }
    }
}
