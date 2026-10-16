//! `MockPlugins`: an in-memory [`Plugins`] service that runs [`MockGuest`]s, with no wasm and no engine.
//!
//! It is the independent oracle for the wasmtime host: the same lifecycle (design §6.2), the same ceiling (§3.2),
//! per-use queueing with `approval-resolved` (§2.2), faults, restart backoff and auto-disable (§4.4), and reload
//! draining a running call (§6.3), written again from the design rather than shared with the host. Time is the
//! runner's clock (a `FakeClock` in tests); nothing here sleeps or spawns.

use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_ops_api::{
    Actor, ApprovalId, Budget, EndReason, ErrorCode, ErrorDetail, Grants, Limits, Need, OpError, Scope, SessionId,
    SessionSpec, SessionState, Sessions, Transport,
};
use omuzaki_plugin_api::grants::{Approval, ceiling, needs_person_each_use};
use omuzaki_plugin_api::limits::{
    CallKind, FAULT_WINDOW_SECONDS, FAULTS_TO_DISABLE, LOG_BURST, LOG_LINE_MAX_BYTES, LOG_LINES_PER_SECOND,
    LOG_RING_LINES, RESTART_BACKOFF_SECONDS,
};
use omuzaki_plugin_api::manifest::Manifest;
use omuzaki_plugin_api::state::{DisabledReason, FaultKind, FaultRecord, LogLevel, LogLine, PluginInfo, PluginState};
use omuzaki_plugin_api::{PluginError, PluginId, Plugins};

use crate::guest::{GuestHost, MockGuest, Trap};

/// An op the mock runner answers: the scope it needs and what it returns.
#[derive(Clone, Debug, PartialEq)]
pub struct MockOp {
    /// The scope the op needs.
    pub scope: Scope,
    /// Its output (JSON), or its error.
    pub output: Result<String, OpError>,
}

/// One call a guest made, in order: what the plugin asked of the runner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRecord {
    /// Which plugin.
    pub plugin: PluginId,
    /// The session it called in.
    pub session: SessionId,
    /// The op.
    pub op: String,
    /// Its arguments.
    pub args: String,
}

struct Entry {
    manifest: Manifest,
    guest: Arc<MockGuest>,
    approval: Approval,
    state: PluginState,
    session: Option<SessionId>,
    enabled_by: Option<Actor>,
    faults: Vec<FaultRecord>,
    restart_at: Option<Seconds>,
    log: Vec<LogLine>,
    next_seq: u64,
    /// The log's token bucket, as the host keeps it: tokens, when they were counted, and lines refused since the last
    /// kept one.
    log_tokens: f64,
    log_at: Seconds,
    log_dropped: u64,
    /// An entry call is running outside the lock.
    busy: bool,
    /// Starts begun so far, and how the last one settled: a caller that waited out a start returns its outcome.
    attempt: u64,
    settled: Result<(), PluginError>,
}

struct Pending {
    plugin: PluginId,
    session: SessionId,
    output: Result<String, OpError>,
}

#[derive(Default)]
struct Inner {
    plugins: BTreeMap<PluginId, Entry>,
    ops: BTreeMap<String, MockOp>,
    pending: BTreeMap<u64, Pending>,
    next_approval: u64,
    calls: Vec<CallRecord>,
}

/// The in-memory [`Plugins`] service.
pub struct MockPlugins {
    clock: Arc<dyn Clock>,
    sessions: Arc<dyn Sessions>,
    person: Grants,
    inner: Mutex<Inner>,
    idle: Condvar,
}

fn is_person(actor: &Actor) -> bool {
    matches!(actor, Actor::Person { .. })
}

fn person_only(by: &Actor) -> Result<(), PluginError> {
    if is_person(by) { Ok(()) } else { Err(PluginError::denied(Scope::EditSettings)) }
}

/// `text` cut to at most [`LOG_LINE_MAX_BYTES`], at a character boundary.
fn cut(text: &str) -> &str {
    let mut end = text.len().min(LOG_LINE_MAX_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl MockPlugins {
    /// A service on `clock` that opens plugin sessions in `sessions` for a person holding `person`.
    pub fn new(clock: Arc<dyn Clock>, sessions: Arc<dyn Sessions>, person: Grants) -> Self {
        Self { clock, sessions, person, inner: Mutex::new(Inner::default()), idle: Condvar::new() }
    }

    /// Installs a plugin, off, with everything its manifest asks for approved.
    ///
    /// # Errors
    /// [`PluginError::Manifest`] when the manifest's check has errors or the id is taken.
    pub fn add(&self, manifest: Manifest, guest: MockGuest) -> Result<PluginId, PluginError> {
        if let Some(error) = manifest.check().errors().next() {
            return Err(PluginError::Manifest(error.to_string()));
        }
        let mut inner = self.lock();
        let id = manifest.id.clone();
        if inner.plugins.contains_key(&id) {
            return Err(PluginError::Manifest(format!("a plugin `{id}` is already installed")));
        }
        let approval = Approval::all_of(&manifest.requests);
        inner.plugins.insert(
            id.clone(),
            Entry {
                manifest,
                guest: Arc::new(guest),
                approval,
                state: PluginState::Installed,
                session: None,
                enabled_by: None,
                faults: Vec::new(),
                restart_at: None,
                log: Vec::new(),
                next_seq: 1,
                log_tokens: f64::from(LOG_BURST),
                log_at: Seconds(0.0),
                log_dropped: 0,
                busy: false,
                attempt: 0,
                settled: Ok(()),
            },
        );
        Ok(id)
    }

    /// Registers an op the guests may call.
    pub fn add_op(&self, name: impl Into<String>, op: MockOp) {
        self.lock().ops.insert(name.into(), op);
    }

    /// Every op call the guests made, in order.
    pub fn calls(&self) -> Vec<CallRecord> {
        self.lock().calls.clone()
    }

    /// Moves a running plugin to `Faulted`, as a trap would: its session ends `Faulted`, the fault is recorded and the
    /// restart is scheduled (or the plugin disabled at the third fault in the window).
    ///
    /// # Errors
    /// [`PluginError::NotFound`] for an unknown id.
    pub fn fault(&self, id: &PluginId, fault: FaultKind) -> Result<PluginInfo, PluginError> {
        let mut inner = self.lock();
        let entry = inner.plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        self.record_fault(entry, fault, format!("{} injected", fault_name(fault)));
        Ok(info_of(id, entry))
    }

    /// Decides a request a plugin's call queued: on approval it runs and its output is the outcome; on refusal the
    /// outcome is `denied`. The plugin hears it through `approval-resolved`, unless its session has ended, in which
    /// case the request was withdrawn and nothing runs.
    ///
    /// # Errors
    /// [`PluginError::Denied`] when `by` is not a person; [`PluginError::NotFound`] when no such request waits.
    pub fn resolve(&self, approval: u64, approve: bool, by: &Actor) -> Result<(), PluginError> {
        person_only(by)?;
        let mut inner = self.lock();
        let pending = inner.pending.remove(&approval).ok_or(PluginError::NotFound)?;
        let live = self.session_is_active(pending.session);
        let Some(entry) = inner.plugins.get_mut(&pending.plugin) else { return Err(PluginError::NotFound) };
        if !live || entry.session != Some(pending.session) {
            return Ok(());
        }
        let outcome = if approve { pending.output } else { Err(OpError::denied("a person refused this request")) };
        let id = pending.plugin.clone();
        drop(inner);
        self.run_entry(&id, CallKind::ApprovalResolved, |guest, host| {
            guest.approval_resolved(host, approval, outcome).map(Ok)
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn session_is_active(&self, session: SessionId) -> bool {
        self.sessions.session(session).is_some_and(|s| s.state == SessionState::Active)
    }

    /// Waits until no entry call of `id` runs, and returns the lock.
    fn drained(&self, id: &PluginId) -> MutexGuard<'_, Inner> {
        let mut inner = self.lock();
        while inner.plugins.get(id).is_some_and(|e| e.busy) {
            inner = self.idle.wait(inner).unwrap_or_else(PoisonError::into_inner);
        }
        inner
    }

    fn record_fault(&self, entry: &mut Entry, fault: FaultKind, detail: String) {
        flush_dropped(entry, self.clock.now());
        if let Some(session) = entry.session.take() {
            // The session may already have ended (revoked); the fault is still recorded.
            let _ended = self.sessions.end(session, EndReason::faulted(detail.clone()));
        }
        let now = self.clock.now();
        entry.faults.retain(|f| now.0 - f.at.0 < FAULT_WINDOW_SECONDS);
        entry.faults.push(FaultRecord { fault, detail, at: now });
        let count = entry.faults.len();
        if count >= FAULTS_TO_DISABLE {
            entry.state = PluginState::disabled(DisabledReason::Faults);
            entry.restart_at = None;
        } else {
            entry.state = PluginState::faulted(fault);
            let wait = RESTART_BACKOFF_SECONDS[(count - 1).min(RESTART_BACKOFF_SECONDS.len() - 1)];
            entry.restart_at = Some(Seconds(now.0 + wait));
        }
    }

    /// Starts `id`: opens its session and runs `init`. Called with the plugin idle.
    fn start(&self, id: &PluginId, by: &Actor, why: Start) -> Result<PluginInfo, PluginError> {
        let mut inner = self.lock();
        // A start already under way when this one arrives is the one it waits for, and its outcome is the answer.
        let joined = inner.plugins.get(id).filter(|e| e.state == PluginState::Loading).map(|e| e.attempt);
        while inner.plugins.get(id).is_some_and(|e| e.busy) {
            inner = self.idle.wait(inner).unwrap_or_else(PoisonError::into_inner);
        }
        let entry = inner.plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        if joined == Some(entry.attempt) {
            return entry.settled.clone().map(|()| info_of(id, entry));
        }
        if let Some(answer) = may_start(id, entry, why) {
            return answer;
        }
        entry.attempt += 1;
        let grants = ceiling(&entry.manifest.requests, &entry.approval, &self.person);
        let spec = SessionSpec {
            transport: Transport::Plugin,
            client: format!("plugin:{}@{}", entry.manifest.id, entry.manifest.version),
            model: None,
            on_behalf_of: by.clone(),
            grants,
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
        // Busy from this lock hold on, so no other start, stop or call of this plugin runs until `init` settles.
        entry.busy = true;
        let config = settings_json(&entry.manifest);
        drop(inner);
        self.run_entry(id, CallKind::Init, |guest, host| guest.init(host, &config).map(|r| r.map_err(Some)))?;
        self.info(id)
    }

    /// Runs one entry call outside the lock, then settles its outcome: a trap or an `init` error faults the plugin.
    fn run_entry<F>(&self, id: &PluginId, kind: CallKind, call: F) -> Result<(), PluginError>
    where
        F: FnOnce(&MockGuest, &mut dyn GuestHost) -> Result<Result<(), Option<String>>, Trap>,
    {
        let (guest, session) = {
            let mut inner = self.lock();
            let entry = inner.plugins.get_mut(id).ok_or(PluginError::NotFound)?;
            entry.busy = true;
            (entry.guest.clone(), entry.session)
        };
        let mut host = Host { plugins: self, id: id.clone(), session, lines: Vec::new() };
        let outcome = call(&guest, &mut host);
        let lines = std::mem::take(&mut host.lines);
        let mut inner = self.lock();
        let entry = inner.plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        for (level, text) in lines {
            push_line(entry, self.clock.now(), level, &text);
        }
        entry.busy = false;
        self.idle.notify_all();
        let settled = match outcome {
            Ok(Ok(())) => {
                if kind == CallKind::Init {
                    entry.state = PluginState::Running;
                }
                Ok(())
            }
            Ok(Err(text)) => {
                let detail = format!("init failed: {}", text.unwrap_or_default());
                self.record_fault(entry, FaultKind::InitError, detail);
                Err(PluginError::Fault(FaultKind::InitError))
            }
            Err(Trap(fault)) => {
                self.record_fault(entry, fault, format!("{} in {}", fault_name(fault), kind.entry()));
                Err(PluginError::Fault(fault))
            }
        };
        if kind == CallKind::Init {
            entry.settled.clone_from(&settled);
        }
        settled
    }

    fn stop(&self, entry: &mut Entry) {
        let guest = entry.guest.clone();
        if let Some(session) = entry.session.take() {
            let _ended = self.sessions.end(session, EndReason::Closed);
            // `shutdown` runs after the session has ended, so every effectful import is refused; a trap only drops
            // the instance.
            let mut host = Host { plugins: self, id: entry.manifest.id.clone(), session: None, lines: Vec::new() };
            let stopped = guest.shutdown(&mut host);
            let now = self.clock.now();
            for (level, text) in std::mem::take(&mut host.lines) {
                push_line(entry, now, level, &text);
            }
            if let Err(Trap(fault)) = stopped {
                // Logged as the host logs it; a line the bucket refuses is counted in the notice below.
                push_line(entry, now, LogLevel::Warn, &format!("shutdown stopped: {} in shutdown", fault_name(fault)));
            }
        }
        flush_dropped(entry, self.clock.now());
    }
}

/// The plugin's settings as the JSON object `init` receives: each key with its default.
fn settings_json(manifest: &Manifest) -> String {
    let map: serde_json::Map<String, serde_json::Value> =
        manifest.settings.iter().map(|s| (s.key.clone(), s.default.clone())).collect();
    serde_json::Value::Object(map).to_string()
}

/// Why a plugin is being started; `start`'s check of its state, made in the same lock hold that marks it busy, so two
/// starts of one plugin never both run.
#[derive(Clone, Copy)]
enum Start {
    /// The person turned it on: a running plugin is left as it is; the fault history is cleared.
    Enable,
    /// A scheduled restart: only a faulted plugin whose backoff has passed by `now`.
    Restart(Seconds),
    /// A reload's second half: only a plugin its first half left unloading.
    Reload,
}

/// What `start` does with a plugin in `entry`'s state for `why`: go on, or return the answer now.
fn may_start(id: &PluginId, entry: &mut Entry, why: Start) -> Option<Result<PluginInfo, PluginError>> {
    match (why, &entry.state) {
        (Start::Enable | Start::Reload, PluginState::Running) => Some(Ok(info_of(id, entry))),
        (Start::Enable, _) => {
            // The person turning it on clears the fault history, so the next fault starts a fresh count.
            entry.faults.clear();
            None
        }
        (Start::Restart(now), PluginState::Faulted { .. }) if entry.restart_at.is_some_and(|t| t <= now) => None,
        (Start::Restart(_), _) => Some(Err(PluginError::Instantiate(format!("`{id}` is not due for a restart")))),
        (Start::Reload, PluginState::Unloading) => None,
        (Start::Reload, _) => Some(Err(PluginError::Instantiate(format!("`{id}` is not running")))),
    }
}

fn fault_name(fault: FaultKind) -> &'static str {
    match fault {
        FaultKind::Trap => "trap",
        FaultKind::OutOfFuel => "out of fuel",
        FaultKind::OutOfMemory => "out of memory",
        FaultKind::Timeout => "deadline",
        FaultKind::HostCallFlood => "host-call flood",
        FaultKind::BadReturn => "bad return value",
        FaultKind::InitError => "init failed",
        FaultKind::Exited => "exited",
        _ => "fault",
    }
}

/// Keeps one line under the host's rule (design §7): [`LOG_LINES_PER_SECOND`] sustained, [`LOG_BURST`] at once; a
/// refused line is counted, and the next kept one is preceded by "N lines dropped".
fn push_line(entry: &mut Entry, at: Seconds, level: LogLevel, text: &str) {
    let elapsed = (at.0 - entry.log_at.0).max(0.0);
    entry.log_tokens = (entry.log_tokens + elapsed * f64::from(LOG_LINES_PER_SECOND)).min(f64::from(LOG_BURST));
    entry.log_at = Seconds(entry.log_at.0.max(at.0));
    if entry.log_tokens < 1.0 {
        entry.log_dropped += 1;
        return;
    }
    entry.log_tokens -= 1.0;
    flush_dropped(entry, at);
    keep_line(entry, at, level, cut(text).to_owned());
}

/// Writes the "N lines dropped" line now, if lines were dropped: when the plugin stops or faults, as the host's
/// `PluginLog::flush` does, so a flood that ended its run still says how much it lost.
fn flush_dropped(entry: &mut Entry, at: Seconds) {
    if entry.log_dropped > 0 {
        let notice = format!("{} lines dropped", entry.log_dropped);
        entry.log_dropped = 0;
        keep_line(entry, at, LogLevel::Warn, notice);
    }
}

fn keep_line(entry: &mut Entry, at: Seconds, level: LogLevel, text: String) {
    entry.log.push(LogLine { seq: entry.next_seq, at, level, text, dry_run: false });
    entry.next_seq += 1;
    if entry.log.len() > LOG_RING_LINES {
        entry.log.remove(0);
    }
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

/// The guest's view of the host during one entry call.
struct Host<'a> {
    plugins: &'a MockPlugins,
    id: PluginId,
    session: Option<SessionId>,
    lines: Vec<(LogLevel, String)>,
}

impl GuestHost for Host<'_> {
    fn call(&mut self, op: &str, args: &str) -> Result<String, OpError> {
        let refused = |lines: &mut Vec<(LogLevel, String)>, error: OpError| {
            lines.push((LogLevel::Warn, format!("{op} {}: {}", error.code, error.message)));
            Err(error)
        };
        let Some(session) = self.session.filter(|s| self.plugins.session_is_active(*s)) else {
            return refused(&mut self.lines, OpError::denied("the plugin has no open session"));
        };
        let Some(info) = self.plugins.sessions.session(session) else {
            return refused(&mut self.lines, OpError::denied("the plugin has no open session"));
        };
        let mut inner = self.plugins.lock();
        inner.calls.push(CallRecord { plugin: self.id.clone(), session, op: op.into(), args: args.into() });
        let Some(found) = inner.ops.get(op).cloned() else {
            drop(inner);
            return refused(&mut self.lines, OpError::not_found(format!("no op `{op}`")));
        };
        if !info.grants.allows(found.scope) {
            drop(inner);
            let error = OpError::denied(format!("{op} needs {}", found.scope))
                .with_detail(ErrorDetail::MissingScope { scope: found.scope });
            return refused(&mut self.lines, error);
        }
        if needs_person_each_use(found.scope) {
            inner.next_approval += 1;
            let approval = inner.next_approval;
            inner.pending.insert(approval, Pending { plugin: self.id.clone(), session, output: found.output });
            drop(inner);
            self.lines.push((LogLevel::Info, format!("waiting for a person (approval {approval})")));
            let need = Need::Person { reason: format!("{op} needs {} on every use", found.scope) };
            let detail = ErrorDetail::Queued { approval: ApprovalId(approval), need };
            return Err(OpError {
                code: ErrorCode::Waiting,
                message: format!("{op} waits for a person"),
                field: None,
                hint: None,
                detail: Some(Box::new(detail)),
            });
        }
        found.output
    }

    fn log(&mut self, level: LogLevel, message: &str) {
        self.lines.push((level, message.to_owned()));
    }

    fn monotonic_seconds(&mut self) -> f64 {
        self.plugins.clock.now().0
    }
}

impl Plugins for MockPlugins {
    fn list(&self) -> Vec<PluginInfo> {
        self.lock().plugins.iter().map(|(id, entry)| info_of(id, entry)).collect()
    }

    fn info(&self, id: &PluginId) -> Result<PluginInfo, PluginError> {
        self.lock().plugins.get(id).map(|entry| info_of(id, entry)).ok_or(PluginError::NotFound)
    }

    fn enable(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        self.start(id, by, Start::Enable)
    }

    fn disable(&self, id: &PluginId, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        let mut inner = self.drained(id);
        let entry = inner.plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        self.stop(entry);
        entry.state = PluginState::disabled(DisabledReason::Person);
        entry.restart_at = None;
        Ok(info_of(id, entry))
    }

    fn set_approval(&self, id: &PluginId, approval: Approval, by: &Actor) -> Result<PluginInfo, PluginError> {
        person_only(by)?;
        let mut inner = self.lock();
        let entry = inner.plugins.get_mut(id).ok_or(PluginError::NotFound)?;
        let requests = &entry.manifest.requests;
        let approval = approval.within(requests);
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
            let mut inner = self.drained(id);
            let entry = inner.plugins.get_mut(id).ok_or(PluginError::NotFound)?;
            if entry.state != PluginState::Running {
                return Err(PluginError::Instantiate(format!("`{id}` is not running")));
            }
            self.stop(entry);
            entry.state = PluginState::Unloading;
        }
        self.start(id, by, Start::Reload)
    }

    fn run_due(&self) -> Vec<PluginId> {
        let now = self.clock.now();
        let due: Vec<(PluginId, Actor)> = self
            .lock()
            .plugins
            .iter()
            .filter(|(_, e)| matches!(e.state, PluginState::Faulted { .. }) && e.restart_at.is_some_and(|t| t <= now))
            .filter_map(|(id, e)| e.enabled_by.clone().map(|by| (id.clone(), by)))
            .collect();
        for (id, by) in &due {
            // A restart that faults again is recorded on the plugin; the next backoff applies.
            let _restarted = self.start(id, by, Start::Restart(now));
        }
        due.into_iter().map(|(id, _)| id).collect()
    }

    fn log(&self, id: &PluginId, after: u64, limit: usize) -> Result<Vec<LogLine>, PluginError> {
        let inner = self.lock();
        let entry = inner.plugins.get(id).ok_or(PluginError::NotFound)?;
        Ok(entry.log.iter().filter(|l| l.seq > after).take(limit).cloned().collect())
    }
}
