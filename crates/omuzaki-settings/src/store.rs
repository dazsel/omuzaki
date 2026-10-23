//! The [`Store`]: the `Settings` service over TOML files (D3, D5, D6).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, PoisonError, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omuzaki_core::time::{Clock, SystemClock};
use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings_api::error::{SettingError, SettingErrorKind, SettingFinding};
use omuzaki_settings_api::event::{
    CHANGE_LOG_CAPACITY, Change, ChangeCause, Publisher, SettingChange, SettingsEvent, Subscription, SwitchReport,
};
use omuzaki_settings_api::meta::{Apply, SettingScope};
use omuzaki_settings_api::profile::{ProfileId, ProfileInfo, ProfileSource};
use omuzaki_settings_api::registry::{self, SettingRegistration};
use omuzaki_settings_api::service::{Layer, ListFilter, Resolved, Settings, Snapshot, Source};
use omuzaki_settings_api::session::SessionStore;
use serde_json::Value;
use toml_edit::DocumentMut;

use crate::backend::{Backend, Edited, FileBackend, Quarantined, Updated};
use crate::document::{self, ApplyError, Edit};
#[cfg(test)]
use crate::ephemeral::Ephemeral;
use crate::locked;
use crate::paths::Paths;
use crate::profiles::{self, Profile, ProfileFiles, clean_name, member, name_edit, value_edit, whole};
use crate::session::SessionFile;

/// How long after the last change the writer waits before it writes (D5).
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// The longest a change waits under a burst of further changes, and the pause before a failed write is retried.
pub const MAX_DELAY: Duration = Duration::from_secs(2);

const SCOPES: [SettingScope; 2] = [SettingScope::User, SettingScope::Machine];

/// The key in `machine.toml` that names the active profile (profiles roam; which one this computer is in does not).
const ACTIVE_KEY: &str = "profiles.active";

/// How often a file that keeps changing under a quarantine is read again before it is left in place.
const QUARANTINE_ATTEMPTS: u32 = 3;

/// A registered key with its default, serialised once.
struct Key {
    reg: &'static SettingRegistration,
    default: Value,
}

/// A change not yet in the file.
#[derive(Clone, Debug, PartialEq)]
struct Dirty {
    scope: SettingScope,
    /// The new value, or `None` to remove the key.
    value: Option<Value>,
    version: u32,
    /// A key whose change must be written for this one to be: the removal of a retired name waits for the new name,
    /// so a refused write of the new name cannot lose the value.
    needs: Option<String>,
}

struct State {
    /// The usable values stored in the files and not yet overridden.
    stored: BTreeMap<String, Value>,
    dirty: BTreeMap<String, Dirty>,
    findings: Vec<SettingFinding>,
    write_error: Option<SettingError>,
    /// Pending changes the file's shape refuses, one finding per key; the rest of the file's changes still land.
    refused: Vec<SettingFinding>,
    log: VecDeque<SettingChange>,
    /// Scopes whose file holds a value that was migrated in memory: the next write copies the file first.
    needs_backup: Vec<SettingScope>,
    first_dirty: Option<Instant>,
    last_change: Instant,
    not_before: Instant,
    closing: bool,
    /// Every profile, by id (D7).
    profiles: BTreeMap<ProfileId, Profile>,
    /// The active profile, if one is.
    active: Option<ProfileId>,
    /// How many profile ids this store has made: varies the next one's suffix.
    ids_made: u64,
}

impl State {
    fn touch(&mut self) {
        let now = Instant::now();
        self.first_dirty.get_or_insert(now);
        self.last_change = now;
    }

    /// When the writer should write: [`DEBOUNCE`] after the last change, but never more than [`MAX_DELAY`] after the
    /// first unwritten one, and not before a failed write's pause is over.
    fn due(&self) -> Instant {
        let first = self.first_dirty.unwrap_or(self.last_change);
        (self.last_change + DEBOUNCE).min(first + MAX_DELAY).max(self.not_before)
    }
}

/// What reading the files produced.
#[derive(Default)]
struct Loaded {
    stored: BTreeMap<String, Value>,
    findings: Vec<SettingFinding>,
    /// Renamed keys: the new key is written and the old one removed by the next write.
    moved: BTreeMap<String, Dirty>,
    migrated: Vec<SettingScope>,
    /// The active profile's id as `machine.toml` names it.
    active: Option<String>,
}

struct Inner {
    backend: Box<dyn Backend>,
    keys: Vec<Key>,
    app_version: &'static str,
    state: Mutex<State>,
    wake: Condvar,
    snapshot: RwLock<Arc<Snapshot>>,
    hub: Publisher<SettingsEvent>,
    /// Serialises flushes, so the writer thread and an explicit [`Store::flush`] never write one file at once.
    flush_gate: Mutex<()>,
    session: Box<dyn SessionStore>,
    clock: SystemClock,
    profile_files: ProfileFiles,
}

/// The settings service over `settings.toml` (`User` keys) and `machine.toml` (`Machine` keys).
///
/// Opening never blocks and never fails: a file that cannot be read or parsed is reported by
/// [`findings`](Settings::findings) and its keys read as their defaults. Changes are written by a background thread
/// 250 ms after the last one (at most 2 s under a burst); dropping the store flushes what is left. Profiles are not
/// stored yet: no profile exists. The session state is `session.toml` in the local data folder ([`SessionFile`]).
pub struct Store {
    inner: Arc<Inner>,
    writer: Option<JoinHandle<()>>,
}

impl Store {
    /// A store over every setting the linked crates registered.
    #[must_use]
    pub fn open(paths: &Paths) -> Self {
        Self::open_with(paths, registry::registrations())
    }

    /// A store over the given settings only: tests and benchmarks pass their own.
    #[must_use]
    pub fn open_with(paths: &Paths, registrations: impl IntoIterator<Item = &'static SettingRegistration>) -> Self {
        let session = Box::new(SessionFile::open(paths.data()));
        Self::with_parts(Box::new(FileBackend::new(paths.clone())), session, registrations)
    }

    #[cfg(test)]
    pub(crate) fn with_backend(
        backend: Box<dyn Backend>,
        registrations: impl IntoIterator<Item = &'static SettingRegistration>,
    ) -> Self {
        Self::with_parts(backend, Box::new(Ephemeral::default()), registrations)
    }

    fn with_parts(
        backend: Box<dyn Backend>,
        session: Box<dyn SessionStore>,
        registrations: impl IntoIterator<Item = &'static SettingRegistration>,
    ) -> Self {
        let mut keys: Vec<Key> =
            registrations.into_iter().map(|reg| Key { reg, default: reg.default_json() }).collect();
        keys.sort_by_key(|k| k.reg.meta.key);
        let now = Instant::now();
        let backend_profile_dir = backend.profile_dir();
        let inner = Arc::new(Inner {
            backend,
            keys,
            app_version: env!("CARGO_PKG_VERSION"),
            state: Mutex::new(State {
                stored: BTreeMap::new(),
                dirty: BTreeMap::new(),
                findings: Vec::new(),
                write_error: None,
                refused: Vec::new(),
                log: VecDeque::new(),
                needs_backup: Vec::new(),
                first_dirty: None,
                last_change: now,
                not_before: now,
                closing: false,
                profiles: BTreeMap::new(),
                active: None,
                ids_made: 0,
            }),
            wake: Condvar::new(),
            snapshot: RwLock::new(Arc::new(Snapshot::new(BTreeMap::new()))),
            hub: Publisher::default(),
            flush_gate: Mutex::new(()),
            session,
            clock: SystemClock::new(),
            profile_files: ProfileFiles::new(backend_profile_dir),
        });
        inner.start();
        let worker = Arc::clone(&inner);
        let writer = thread::Builder::new().name("settings-writer".to_owned()).spawn(move || worker.run_writer());
        let writer = match writer {
            Ok(handle) => Some(handle),
            Err(e) => {
                let reason = format!("the writer thread did not start ({e}); changes are written when the store drops");
                locked(&inner.state).write_error = Some(SettingError::new("", SettingErrorKind::Storage { reason }));
                None
            }
        };
        Self { inner, writer }
    }

    /// Writes every pending change now, waiting for another process's lock for up to two seconds.
    ///
    /// # Errors
    /// [`SettingErrorKind::Storage`] when a file cannot be locked, merged or replaced. The keys stay pending and the
    /// writer thread retries; the error is also listed by [`findings`](Settings::findings) until a write succeeds.
    pub fn flush(&self) -> Result<(), SettingError> {
        self.inner.flush()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        locked(&self.inner.state).closing = true;
        self.inner.wake.notify_all();
        match self.writer.take() {
            // The writer flushes once more as it exits (a clean exit flushes). One that panicked did not, so the
            // changes it left are written here.
            Some(handle) => {
                if handle.join().is_err() {
                    self.inner.flush_quietly();
                }
            }
            None => self.inner.flush_quietly(),
        }
    }
}

impl Inner {
    fn key(&self, name: &str) -> Result<&Key, SettingError> {
        self.keys
            .binary_search_by(|k| k.reg.meta.key.cmp(name))
            .map(|i| &self.keys[i])
            .map_err(|_| SettingError::unknown_key(name))
    }

    /// Reads the files into the state: the first and only load of a new store.
    fn start(&self) {
        let loaded = self.load(true, false).unwrap_or_else(|e| Loaded {
            findings: vec![SettingFinding::new(e, Value::Null, "")],
            ..Loaded::default()
        });
        let regs: Vec<&'static SettingRegistration> = self.keys.iter().map(|k| k.reg).collect();
        let (profiles, profile_findings) = self.profile_files.load(&regs);
        let mut st = locked(&self.state);
        st.stored = loaded.stored;
        st.findings = loaded.findings;
        st.findings.extend(profile_findings);
        st.needs_backup = loaded.migrated;
        st.active = loaded.active.and_then(|id| ProfileId::new(id).ok()).filter(|id| profiles.contains_key(id));
        st.profiles = profiles;
        self.adopt_moves(&mut st, loaded.moved);
        self.rebuild(&st);
    }

    fn adopt_moves(&self, st: &mut State, moved: BTreeMap<String, Dirty>) {
        for (key, dirty) in moved {
            st.dirty.entry(key).or_insert(dirty);
            st.touch();
        }
        self.wake.notify_all();
    }

    /// Reads both files. `apply` moves an unparsable file aside, but only once both files have been read, so a failed
    /// read leaves the disk as it was; `strict` fails on a file that cannot be read, where start-up reports it and goes
    /// on.
    fn load(&self, apply: bool, strict: bool) -> Result<Loaded, SettingError> {
        let mut out = Loaded::default();
        let mut broken = Vec::new();
        let mut unread = false;
        for scope in SCOPES {
            let file = self.backend.file_name(scope);
            let text = match self.backend.read(scope) {
                Ok(Some(text)) => text,
                Ok(None) => continue,
                Err(e) if strict => return Err(e),
                Err(e) => {
                    out.findings.push(SettingFinding::new(e, Value::Null, file));
                    unread = true;
                    continue;
                }
            };
            match document::parse(&text) {
                Ok(doc) => {
                    if scope == SettingScope::Machine {
                        out.active = document::lookup(&doc, ACTIVE_KEY).and_then(|v| v.as_str().map(str::to_owned));
                    }
                    self.load_scope(scope, &doc, &mut out);
                }
                Err(error) => broken.push((scope, text, error)),
            }
        }
        for (scope, text, error) in broken {
            self.recover(scope, text, error, apply && !unread, &mut out);
        }
        Ok(out)
    }

    fn load_scope(&self, scope: SettingScope, doc: &DocumentMut, out: &mut Loaded) {
        let file = self.backend.file_name(scope);
        for key in self.keys.iter().filter(|k| k.reg.meta.scope == scope) {
            load_key(key, doc, file, out);
        }
    }

    /// Deals with a file that did not parse: with `apply`, moves it aside, but only while it is still the text that
    /// failed. If another process replaced it first, the new text is read instead (and, if broken too, tried again).
    fn recover(&self, scope: SettingScope, mut text: String, mut error: String, apply: bool, out: &mut Loaded) {
        let file = self.backend.file_name(scope);
        let mut finding = |what: &str, error: &str| {
            let reason = format!("{file} does not parse ({error}); using the defaults; {what}");
            let error = SettingError::new("", SettingErrorKind::Storage { reason });
            out.findings.push(SettingFinding::new(error, Value::Null, file));
        };
        if !apply {
            return finding("left in place", &error);
        }
        for _ in 0..QUARANTINE_ATTEMPTS {
            match self.backend.quarantine(scope, &text) {
                Ok(Quarantined::Moved(name)) => return finding(&format!("moved to {name}"), &error),
                Ok(Quarantined::Changed(None)) => return,
                Ok(Quarantined::Changed(Some(fresh))) => match document::parse(&fresh) {
                    Ok(doc) => return self.load_scope(scope, &doc, out),
                    Err(e) => (text, error) = (fresh, e),
                },
                Err(e) => return finding(&format!("could not be moved aside: {e}"), &error),
            }
        }
        finding("kept changing, so left in place", &error);
    }

    fn effective(st: &State, key: &Key) -> (Value, Source) {
        Self::effective_under(st, st.active.as_ref(), key)
    }

    /// The effective value with `active` as the active profile: its value for a key it may hold, else the base, else
    /// the default.
    fn effective_under(st: &State, active: Option<&ProfileId>, key: &Key) -> (Value, Source) {
        let name = key.reg.meta.key;
        if let Some(id) = active.filter(|_| member(key.reg))
            && let Some(value) = st.profiles.get(id).and_then(|p| p.values.get(name))
        {
            return (value.clone(), Source::Profile(id.clone()));
        }
        Self::base(st, key)
    }

    /// The value with no profile: the base, else the default.
    fn base(st: &State, key: &Key) -> (Value, Source) {
        match st.stored.get(key.reg.meta.key) {
            Some(v) => (v.clone(), Source::Base),
            None => (key.default.clone(), Source::Default),
        }
    }

    /// The changes from the effective values under the active profile to those under `to`.
    fn changes_to(&self, st: &State, to: Option<&ProfileId>) -> Vec<Change> {
        self.keys
            .iter()
            .filter_map(|k| {
                let (before, _) = Self::effective(st, k);
                let (after, _) = Self::effective_under(st, to, k);
                (before != after).then(|| Change::new(k.reg.meta.key, before, after, k.reg.meta.apply))
            })
            .collect()
    }

    /// Records the active profile for `machine.toml`; the writer writes it with the next flush.
    fn mark_active(&self, st: &mut State) {
        let value = st.active.as_ref().map(|id| Value::String(id.to_string()));
        st.dirty.insert(ACTIVE_KEY.to_owned(), Dirty { scope: SettingScope::Machine, value, version: 1, needs: None });
        st.touch();
        self.wake.notify_all();
    }

    /// Builds the effective values and swaps them in as a whole.
    fn rebuild(&self, st: &State) {
        let entries = self.keys.iter().map(|k| (k.reg.meta.key.to_owned(), Self::effective(st, k))).collect();
        *self.snapshot.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(Snapshot::new(entries));
    }

    fn mark_dirty(&self, st: &mut State, key: &Key, value: Option<Value>) {
        let version = if value.is_some() { key.reg.meta.version } else { 1 };
        let scope = key.reg.meta.scope;
        if value.is_none() {
            // A removed key must not come back from a name it was renamed from, so those go too.
            for old in key.reg.renamed_from {
                st.dirty.insert(
                    (*old).to_owned(),
                    Dirty { scope, value: None, version: 1, needs: Some(key.reg.meta.key.to_owned()) },
                );
            }
        }
        st.dirty.insert(key.reg.meta.key.to_owned(), Dirty { scope, value, version, needs: None });
        st.touch();
        self.wake.notify_all();
    }

    /// Logs the changes and tells the subscribers: one event per change set.
    fn announce(&self, st: &mut State, cause: ChangeCause, changes: Vec<Change>, by: &Actor) {
        if changes.is_empty() {
            return;
        }
        let at = self.clock.now();
        for change in &changes {
            if st.log.len() == CHANGE_LOG_CAPACITY {
                st.log.pop_front();
            }
            st.log.push_back(SettingChange::new(change.clone(), by.clone(), at));
        }
        self.hub.publish(&SettingsEvent::new(cause, changes));
    }

    /// Logs a profile switch and tells the subscribers with one event, even when no value changed: the active profile
    /// did.
    fn switched(
        &self,
        st: &mut State,
        from: Option<ProfileId>,
        to: Option<ProfileId>,
        changes: Vec<Change>,
        by: &Actor,
    ) {
        let cause = ChangeCause::ProfileSwitched { from, to };
        if changes.is_empty() {
            self.hub.publish(&SettingsEvent::new(cause, changes));
        } else {
            self.announce(st, cause, changes, by);
        }
    }

    fn flush(&self) -> Result<(), SettingError> {
        let _one_at_a_time = locked(&self.flush_gate);
        let (pending, backups) = {
            let st = locked(&self.state);
            (st.dirty.clone(), st.needs_backup.clone())
        };
        let mut failure = None;
        let mut recovered = Vec::new();
        let mut refusals = Vec::new();
        for scope in SCOPES {
            let edits: Vec<Edit> = pending
                .iter()
                .filter(|(_, d)| d.scope == scope)
                .map(|(key, d)| Edit { key: key.clone(), value: d.value.clone().map(|v| (v, d.version)) })
                .collect();
            let needs: BTreeMap<String, String> = pending
                .iter()
                .filter(|(_, d)| d.scope == scope)
                .filter_map(|(key, d)| d.needs.clone().map(|n| (key.clone(), n)))
                .collect();
            if edits.is_empty() {
                continue;
            }
            let backup = backups.contains(&scope).then_some(self.app_version);
            let (Updated { moved_aside, outcome }, refused) = self.write_scope(scope, backup, &edits, &needs);
            if let Some(name) = moved_aside {
                recovered.push(self.moved_while_writing(scope, &name, outcome.is_ok()));
            }
            let landed: BTreeMap<String, Dirty> = pending
                .iter()
                .filter(|(key, _)| !refused.iter().any(|(r, _)| r == *key))
                .map(|(k, d)| (k.clone(), d.clone()))
                .collect();
            for (key, reason) in &refused {
                let stored = pending.get(key).and_then(|d| d.value.clone()).unwrap_or(Value::Null);
                refusals.push(SettingFinding::new(refusal_error(key, reason), stored, "write"));
            }
            match outcome {
                Ok(wrote) => self.settle(scope, &landed, wrote),
                Err(e) => failure = failure.or(Some(e)),
            }
        }
        let refusal = refusals.first().map(|f| f.error.clone());
        let mut st = locked(&self.state);
        st.refused = refusals;
        st.findings.extend(recovered);
        st.first_dirty = (!st.dirty.is_empty()).then(Instant::now);
        st.write_error.clone_from(&failure);
        if failure.is_some() || refusal.is_some() {
            st.not_before = Instant::now() + MAX_DELAY;
        }
        failure.or(refusal).map_or(Ok(()), Err)
    }

    /// The finding for a file that stopped parsing between the last read and a write, and was moved aside by it.
    fn moved_while_writing(&self, scope: SettingScope, name: &str, written: bool) -> SettingFinding {
        let file = self.backend.file_name(scope);
        let after = if written {
            "a new file holds the changes"
        } else {
            "writing the new file failed, so the changes are still pending"
        };
        let reason = format!("{file} did not parse when it was written; moved to {name}; {after}");
        SettingFinding::new(SettingError::new("", SettingErrorKind::Storage { reason }), Value::Null, file)
    }

    /// Writes the edits of one file; also returns the key and reason of each edit the file's shape refused.
    fn write_scope(
        &self,
        scope: SettingScope,
        backup: Option<&str>,
        edits: &[Edit],
        needs: &BTreeMap<String, String>,
    ) -> (Updated, Vec<(String, String)>) {
        let mut refused = Vec::new();
        let updated = self.backend.update(scope, backup, &mut |text| match document::apply_each(text, edits, needs) {
            Ok(applied) => {
                refused = applied.refused;
                Ok(if Some(applied.text.as_str()) == text { Edited::Unchanged } else { Edited::Write(applied.text) })
            }
            Err(ApplyError::Unparsable(_)) => Ok(Edited::Unparsable),
            Err(ApplyError::Conflict(reason)) => Err(reason),
        });
        (updated, refused)
    }

    /// Forgets the changes a write took, except those changed again while it ran.
    /// `wrote`: the file was replaced, so a pre-migration backup asked for was made (or was already there). A write
    /// that changed nothing leaves the backup still owed.
    fn settle(&self, scope: SettingScope, written: &BTreeMap<String, Dirty>, wrote: bool) {
        let mut st = locked(&self.state);
        for (key, dirty) in written.iter().filter(|(_, d)| d.scope == scope) {
            if st.dirty.get(key) == Some(dirty) {
                st.dirty.remove(key);
            }
        }
        if wrote {
            st.needs_backup.retain(|s| *s != scope);
        }
    }

    fn flush_quietly(&self) {
        // The failure is kept in the state and listed by `findings`.
        drop(self.flush());
    }

    fn run_writer(&self) {
        let mut st = locked(&self.state);
        loop {
            while st.dirty.is_empty() && !st.closing {
                st = self.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
            if st.closing {
                break;
            }
            let wait = st.due().saturating_duration_since(Instant::now());
            if wait > Duration::ZERO {
                st = self.wake.wait_timeout(st, wait).unwrap_or_else(PoisonError::into_inner).0;
                continue;
            }
            drop(st);
            self.flush_quietly();
            st = locked(&self.state);
        }
        drop(st);
        self.flush_quietly();
    }
}

/// Reads one registered key out of its file: migrates, checks and moves it as D5 says.
fn load_key(key: &Key, doc: &DocumentMut, file: &str, out: &mut Loaded) {
    let reg = key.reg;
    let name = reg.meta.key;
    let found = document::lookup(doc, name).map(|v| (v, document::stored_version(doc, name), None)).or_else(|| {
        reg.renamed_from
            .iter()
            .find_map(|old| document::lookup(doc, old).map(|v| (v, document::stored_version(doc, old), Some(*old))))
    });
    let Some((stored, version, renamed_from)) = found else { return };
    let unusable = |out: &mut Loaded, error: SettingError| {
        out.findings.push(SettingFinding::new(error, stored.clone(), file));
    };
    if version > reg.meta.version {
        let reason = format!("stored at version {version}, newer than this build's {}; kept as is", reg.meta.version);
        return unusable(out, SettingError::invalid(name, reason));
    }
    let migrated = version < reg.meta.version;
    let value = if migrated {
        match reg.migrate(version, stored.clone()) {
            Ok(v) => v,
            Err(e) => return unusable(out, e),
        }
    } else {
        stored.clone()
    };
    if let Err(e) = reg.validate(&value) {
        return unusable(out, e);
    }
    let scope = reg.meta.scope;
    if migrated && !out.migrated.contains(&scope) {
        out.migrated.push(scope);
    }
    if let Some(old) = renamed_from {
        out.moved.insert(
            name.to_owned(),
            Dirty { scope, value: Some(value.clone()), version: reg.meta.version, needs: None },
        );
        out.moved.insert(old.to_owned(), Dirty { scope, value: None, version: 1, needs: Some(name.to_owned()) });
    }
    out.stored.insert(name.to_owned(), value);
}

/// A change the file's shape refuses is a storage failure, not an invalid value: the value itself was accepted.
fn refusal_error(key: &str, reason: &str) -> SettingError {
    SettingError::new(key, SettingErrorKind::Storage { reason: reason.to_owned() })
}

fn unknown_profile(id: &ProfileId) -> SettingError {
    SettingError::new("", SettingErrorKind::UnknownProfile { id: id.as_str().to_owned() })
}

fn info(st: &State, id: &ProfileId) -> Option<ProfileInfo> {
    let profile = st.profiles.get(id)?;
    let keys = u32::try_from(profile.values.len()).unwrap_or(u32::MAX);
    Some(ProfileInfo::new(id.clone(), profile.name.clone(), keys, st.active.as_ref() == Some(id)))
}

fn report(changes: &[Change]) -> SwitchReport {
    let pending = changes.iter().filter(|c| c.apply == Apply::NextStart).map(|c| c.key.clone()).collect();
    SwitchReport::new(u32::try_from(changes.len()).unwrap_or(u32::MAX), pending)
}

/// The changes from `shown` (the effective values in key order, taken earlier) to the effective values now.
fn changed_since(inner: &Inner, st: &State, shown: &[Value]) -> Vec<Change> {
    inner
        .keys
        .iter()
        .zip(shown)
        .filter_map(|(k, before)| {
            let (after, _) = Inner::effective(st, k);
            (*before != after).then(|| Change::new(k.reg.meta.key, before.clone(), after, k.reg.meta.apply))
        })
        .collect()
}

impl Settings for Store {
    fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.inner.snapshot.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn resolve(&self, key: &str) -> Result<Resolved, SettingError> {
        let k = self.inner.key(key)?;
        let snapshot = self.snapshot();
        let (value, source) = match (snapshot.value(key), snapshot.source(key)) {
            (Some(v), Some(s)) => (v.clone(), s.clone()),
            _ => (k.default.clone(), Source::Default),
        };
        Ok(Resolved::new(key, value, k.default.clone(), source, k.reg.meta))
    }

    fn list(&self, filter: &ListFilter) -> Vec<Resolved> {
        self.inner
            .keys
            .iter()
            .filter(|k| filter.area.as_deref().is_none_or(|a| k.reg.meta.area() == a))
            .filter_map(|k| self.resolve(k.reg.meta.key).ok())
            .filter(|r| !filter.changed || r.is_modified())
            .collect()
    }

    fn set_raw(&self, key: &str, value: Value, layer: Layer, mode: Mode, by: &Actor) -> Result<Change, SettingError> {
        let inner = &*self.inner;
        let k = inner.key(key)?;
        k.reg.validate(&value)?;
        document::json_to_toml(&value).map_err(|e| SettingError::invalid(key, e))?;
        let mut st = locked(&inner.state);
        let (before, source) = Inner::effective(&st, k);
        // Under a profile, a key it may hold is written into it (D7), so a tweak made under "Stream" stays there.
        if let Some(id) = st.active.clone().filter(|_| layer == Layer::Auto && member(k.reg)) {
            let change = Change::new(key, before, value.clone(), k.reg.meta.apply);
            let held = st.profiles.get(&id).and_then(|p| p.values.get(key));
            if mode == Mode::Apply && held != Some(&value) {
                inner.profile_files.write(&id, &[value_edit(key, Some(&value))])?;
                if let Some(profile) = st.profiles.get_mut(&id) {
                    profile.values.insert(key.to_owned(), value);
                }
                inner.rebuild(&st);
                if change.before != change.after {
                    inner.announce(&mut st, ChangeCause::Set, vec![change.clone()], by);
                }
            }
            return Ok(change);
        }
        // A base write under a profile that holds the key changes the base, not what is in effect.
        let after = if matches!(source, Source::Profile(_)) { before.clone() } else { value.clone() };
        let change = Change::new(key, before, after, k.reg.meta.apply);
        if mode == Mode::Apply {
            st.findings.retain(|f| f.error.key != key);
            if st.stored.get(key) != Some(&value) {
                st.stored.insert(key.to_owned(), value.clone());
                inner.mark_dirty(&mut st, k, Some(value));
                inner.rebuild(&st);
            }
            if change.before != change.after {
                inner.announce(&mut st, ChangeCause::Set, vec![change.clone()], by);
            }
        }
        Ok(change)
    }

    fn reset(&self, key: &str, layer: Layer, mode: Mode, by: &Actor) -> Result<Option<Change>, SettingError> {
        let inner = &*self.inner;
        let k = inner.key(key)?;
        let mut st = locked(&inner.state);
        let (shown, source) = Inner::effective(&st, k);
        // Under a profile, a reset removes the profile's override: the key falls back to the base (D7).
        if let Some(id) = st.active.clone().filter(|_| layer == Layer::Auto && member(k.reg)) {
            if !st.profiles.get(&id).is_some_and(|p| p.values.contains_key(key)) {
                return Ok(None);
            }
            let (after, _) = Inner::base(&st, k);
            let change = Change::new(key, shown, after, k.reg.meta.apply);
            if mode == Mode::Apply {
                inner.profile_files.write(&id, &[value_edit(key, None)])?;
                if let Some(profile) = st.profiles.get_mut(&id) {
                    profile.values.remove(key);
                }
                inner.rebuild(&st);
                if change.before != change.after {
                    inner.announce(&mut st, ChangeCause::Reset, vec![change.clone()], by);
                }
            }
            return Ok(Some(change));
        }
        let masked = matches!(source, Source::Profile(_));
        let unusable = st.findings.iter().any(|f| f.error.key == key);
        let Some(before) = st.stored.get(key).cloned() else {
            // A value the file holds but the key cannot use is removed too, so the finding goes away.
            if mode == Mode::Apply && unusable {
                st.findings.retain(|f| f.error.key != key);
                inner.mark_dirty(&mut st, k, None);
            }
            return Ok(None);
        };
        // A base reset under a profile that holds the key changes the base, not what is in effect.
        let change = if masked {
            Change::new(key, shown.clone(), shown, k.reg.meta.apply)
        } else {
            Change::new(key, before, k.default.clone(), k.reg.meta.apply)
        };
        if mode == Mode::Apply {
            st.stored.remove(key);
            st.findings.retain(|f| f.error.key != key);
            inner.mark_dirty(&mut st, k, None);
            inner.rebuild(&st);
            if change.before != change.after {
                inner.announce(&mut st, ChangeCause::Reset, vec![change.clone()], by);
            }
        }
        Ok(Some(change))
    }

    fn profiles(&self) -> Vec<ProfileInfo> {
        let st = locked(&self.inner.state);
        st.profiles.keys().filter_map(|id| info(&st, id)).collect()
    }

    fn save_profile(
        &self,
        name: &str,
        from: ProfileSource,
        overwrite: bool,
        mode: Mode,
        by: &Actor,
    ) -> Result<ProfileInfo, SettingError> {
        let inner = &*self.inner;
        let name = clean_name(name)?;
        let mut st = locked(&inner.state);
        let values = match from {
            ProfileSource::Profile(id) => st.profiles.get(&id).ok_or_else(|| unknown_profile(&id))?.values.clone(),
            // Every key a profile may hold whose value now differs from what the base would give (D7), so the
            // profile reproduces today's setup exactly, an override equal to the default included.
            _ => inner
                .keys
                .iter()
                .filter(|k| member(k.reg))
                .filter_map(|k| {
                    let (now, base) = (Inner::effective(&st, k).0, Inner::base(&st, k).0);
                    (now != base).then(|| (k.reg.meta.key.to_owned(), now))
                })
                .collect(),
        };
        let existing = st.profiles.iter().find(|(_, p)| p.name == name).map(|(id, _)| id.clone());
        let id = match existing {
            Some(_) if !overwrite => {
                return Err(SettingError::new("", SettingErrorKind::ProfileExists { name }));
            }
            Some(id) => id,
            None => {
                let mut tries = st.ids_made;
                loop {
                    tries += 1;
                    let id = ProfileId::from_name(&name, &profiles::suffix(tries))?;
                    if !st.profiles.contains_key(&id) {
                        if mode == Mode::Apply {
                            st.ids_made = tries;
                        }
                        break id;
                    }
                }
            }
        };
        let profile = Profile { name, values };
        let active = st.active.as_ref() == Some(&id);
        let keys = u32::try_from(profile.values.len()).unwrap_or(u32::MAX);
        let saved = ProfileInfo::new(id.clone(), profile.name.clone(), keys, active);
        if mode == Mode::Apply {
            let before = st.profiles.get(&id).cloned();
            inner.profile_files.write(&id, &whole(&profile, before.as_ref()))?;
            let shown: Vec<Value> = inner.keys.iter().map(|k| Inner::effective(&st, k).0).collect();
            st.profiles.insert(id, profile);
            if active {
                // Saving over the active profile changes what is in effect: that is a reload (the cause's docs).
                let changes = changed_since(inner, &st, &shown);
                inner.rebuild(&st);
                inner.announce(&mut st, ChangeCause::Reloaded, changes, by);
            }
        }
        Ok(saved)
    }

    fn rename_profile(&self, id: &ProfileId, name: &str, mode: Mode, _: &Actor) -> Result<ProfileInfo, SettingError> {
        let inner = &*self.inner;
        let name = clean_name(name)?;
        let mut st = locked(&inner.state);
        if !st.profiles.contains_key(id) {
            return Err(unknown_profile(id));
        }
        if st.profiles.iter().any(|(other, p)| other != id && p.name == name) {
            return Err(SettingError::new("", SettingErrorKind::ProfileExists { name }));
        }
        if mode == Mode::Apply {
            inner.profile_files.write(id, &[name_edit(&name)])?;
            if let Some(profile) = st.profiles.get_mut(id) {
                profile.name.clone_from(&name);
            }
        }
        let mut renamed = info(&st, id).ok_or_else(|| unknown_profile(id))?;
        renamed.name = name;
        Ok(renamed)
    }

    fn delete_profile(&self, id: &ProfileId, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError> {
        let inner = &*self.inner;
        let mut st = locked(&inner.state);
        if !st.profiles.contains_key(id) {
            return Err(unknown_profile(id));
        }
        let was_active = st.active.as_ref() == Some(id);
        let changes = if was_active { inner.changes_to(&st, None) } else { Vec::new() };
        let report = report(&changes);
        if mode == Mode::Apply {
            inner.profile_files.remove(id)?;
            st.profiles.remove(id);
            if was_active {
                // Deleting the active profile switches to none first, in the same step (D7).
                st.active = None;
                inner.mark_active(&mut st);
                inner.rebuild(&st);
                inner.switched(&mut st, Some(id.clone()), None, changes, by);
            }
        }
        Ok(report)
    }

    fn switch_profile(&self, to: Option<&ProfileId>, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError> {
        let inner = &*self.inner;
        let mut st = locked(&inner.state);
        if let Some(id) = to.filter(|id| !st.profiles.contains_key(*id)) {
            return Err(unknown_profile(id));
        }
        if st.active.as_ref() == to {
            return Ok(report(&[]));
        }
        let changes = inner.changes_to(&st, to);
        let report = report(&changes);
        if mode == Mode::Apply {
            let from = std::mem::replace(&mut st.active, to.cloned());
            inner.mark_active(&mut st);
            inner.rebuild(&st);
            inner.switched(&mut st, from, to.cloned(), changes, by);
        }
        Ok(report)
    }

    fn reload(&self, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError> {
        let inner = &*self.inner;
        // Held from the read to the swap: a flush that finished in between would leave the read text older than the
        // file, and the swap would put that old value back.
        let _one_at_a_time = locked(&inner.flush_gate);
        let mut loaded = inner.load(mode == Mode::Apply, true)?;
        let mut st = locked(&inner.state);
        // A change this process has not written yet is newer than the file, and so is any complaint about the value it
        // will replace.
        for (key, dirty) in &st.dirty {
            match &dirty.value {
                Some(v) => loaded.stored.insert(key.clone(), v.clone()),
                None => loaded.stored.remove(key),
            };
        }
        loaded.findings.retain(|f| !st.dirty.contains_key(&f.error.key));
        let changes: Vec<Change> = inner
            .keys
            .iter()
            .filter_map(|k| {
                let name = k.reg.meta.key;
                let before = st.stored.get(name).unwrap_or(&k.default);
                let after = loaded.stored.get(name).unwrap_or(&k.default);
                (before != after).then(|| Change::new(name, before.clone(), after.clone(), k.reg.meta.apply))
            })
            .collect();
        let pending_restart = changes.iter().filter(|c| c.apply == Apply::NextStart).map(|c| c.key.clone()).collect();
        let report = SwitchReport::new(u32::try_from(changes.len()).unwrap_or(u32::MAX), pending_restart);
        if mode == Mode::Apply {
            st.stored = loaded.stored;
            st.findings = loaded.findings;
            for scope in loaded.migrated {
                if !st.needs_backup.contains(&scope) {
                    st.needs_backup.push(scope);
                }
            }
            inner.adopt_moves(&mut st, loaded.moved);
            if !changes.is_empty() {
                inner.rebuild(&st);
            }
            inner.announce(&mut st, ChangeCause::Reloaded, changes, by);
        }
        Ok(report)
    }

    fn changes(&self, limit: usize) -> Vec<SettingChange> {
        locked(&self.inner.state).log.iter().rev().take(limit).cloned().collect()
    }

    fn findings(&self) -> Vec<SettingFinding> {
        let st = locked(&self.inner.state);
        let write = st.write_error.iter().map(|e| SettingFinding::new(e.clone(), Value::Null, "write"));
        // A writer that ended while the store is open has panicked; its changes wait for `flush` and for the drop.
        let stopped = self.writer.as_ref().is_some_and(JoinHandle::is_finished).then(|| {
            let reason = "the writer thread stopped; pending changes are written by `flush` and when the store drops";
            let error = SettingError::new("", SettingErrorKind::Storage { reason: reason.to_owned() });
            SettingFinding::new(error, Value::Null, "write")
        });
        st.findings.iter().cloned().chain(st.refused.iter().cloned()).chain(write).chain(stopped).collect()
    }

    fn subscribe(&self) -> Subscription<SettingsEvent> {
        self.inner.hub.subscribe()
    }

    fn session(&self) -> &dyn SessionStore {
        &*self.inner.session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_writer_waits_the_debounce_after_the_last_change_but_never_more_than_the_burst_limit() {
        let now = Instant::now();
        let mut st = State {
            stored: BTreeMap::new(),
            dirty: BTreeMap::new(),
            findings: Vec::new(),
            write_error: None,
            refused: Vec::new(),
            log: VecDeque::new(),
            needs_backup: Vec::new(),
            first_dirty: Some(now),
            last_change: now,
            not_before: now,
            closing: false,
            profiles: BTreeMap::new(),
            active: None,
            ids_made: 0,
        };
        assert_eq!(st.due(), now + DEBOUNCE);
        st.last_change = now + Duration::from_secs(10);
        assert_eq!(st.due(), now + MAX_DELAY, "a burst cannot push the write past the limit");
        st.not_before = now + Duration::from_secs(30);
        assert_eq!(st.due(), now + Duration::from_secs(30), "a failed write waits before it retries");
    }
}
