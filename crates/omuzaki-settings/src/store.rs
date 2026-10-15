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

use crate::backend::{Backend, Edited, FileBackend};
use crate::document::{self, ApplyError, Edit};
use crate::ephemeral::Ephemeral;
use crate::locked;
use crate::paths::Paths;

/// How long after the last change the writer waits before it writes (D5).
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// The longest a change waits under a burst of further changes, and the pause before a failed write is retried.
pub const MAX_DELAY: Duration = Duration::from_secs(2);

const SCOPES: [SettingScope; 2] = [SettingScope::User, SettingScope::Machine];

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
}

struct State {
    /// The usable values stored in the files and not yet overridden.
    stored: BTreeMap<String, Value>,
    dirty: BTreeMap<String, Dirty>,
    findings: Vec<SettingFinding>,
    write_error: Option<SettingError>,
    log: VecDeque<SettingChange>,
    /// Scopes whose file holds a value that was migrated in memory: the next write copies the file first.
    needs_backup: Vec<SettingScope>,
    first_dirty: Option<Instant>,
    last_change: Instant,
    not_before: Instant,
    closing: bool,
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
    session: Ephemeral,
    clock: SystemClock,
}

/// The settings service over `settings.toml` (`User` keys) and `machine.toml` (`Machine` keys).
///
/// Opening never blocks and never fails: a file that cannot be read or parsed is reported by
/// [`findings`](Settings::findings) and its keys read as their defaults. Changes are written by a background thread
/// 250 ms after the last one (at most 2 s under a burst); dropping the store flushes what is left. Profiles are not
/// stored yet: no profile exists, and the session state is kept in memory.
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
        Self::with_backend(Box::new(FileBackend::new(paths.clone())), registrations)
    }

    pub(crate) fn with_backend(
        backend: Box<dyn Backend>,
        registrations: impl IntoIterator<Item = &'static SettingRegistration>,
    ) -> Self {
        let mut keys: Vec<Key> =
            registrations.into_iter().map(|reg| Key { reg, default: reg.default_json() }).collect();
        keys.sort_by_key(|k| k.reg.meta.key);
        let now = Instant::now();
        let inner = Arc::new(Inner {
            backend,
            keys,
            app_version: env!("CARGO_PKG_VERSION"),
            state: Mutex::new(State {
                stored: BTreeMap::new(),
                dirty: BTreeMap::new(),
                findings: Vec::new(),
                write_error: None,
                log: VecDeque::new(),
                needs_backup: Vec::new(),
                first_dirty: None,
                last_change: now,
                not_before: now,
                closing: false,
            }),
            wake: Condvar::new(),
            snapshot: RwLock::new(Arc::new(Snapshot::new(BTreeMap::new()))),
            hub: Publisher::default(),
            flush_gate: Mutex::new(()),
            session: Ephemeral::default(),
            clock: SystemClock::new(),
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
            // The writer flushes once more as it exits (a clean exit flushes).
            Some(handle) => drop(handle.join()),
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
        let mut st = locked(&self.state);
        st.stored = loaded.stored;
        st.findings = loaded.findings;
        st.needs_backup = loaded.migrated;
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

    /// Reads both files. `apply` moves an unparsable file aside; `strict` fails on a file that cannot be read, where
    /// start-up reports it and goes on.
    fn load(&self, apply: bool, strict: bool) -> Result<Loaded, SettingError> {
        let mut out = Loaded::default();
        for scope in SCOPES {
            let file = self.backend.file_name(scope);
            let text = match self.backend.read(scope) {
                Ok(text) => text,
                Err(e) if strict => return Err(e),
                Err(e) => {
                    out.findings.push(SettingFinding::new(e, Value::Null, file));
                    continue;
                }
            };
            let doc = text.and_then(|t| match document::parse(&t) {
                Ok(doc) => Some(doc),
                Err(e) => {
                    out.findings.push(self.broken_file(scope, &e, apply));
                    None
                }
            });
            let Some(doc) = doc else { continue };
            for key in self.keys.iter().filter(|k| k.reg.meta.scope == scope) {
                load_key(key, &doc, file, &mut out);
            }
        }
        Ok(out)
    }

    fn broken_file(&self, scope: SettingScope, parse_error: &str, apply: bool) -> SettingFinding {
        let file = self.backend.file_name(scope);
        let moved = if apply {
            match self.backend.quarantine(scope) {
                Ok(name) => format!("moved to {name}"),
                Err(e) => format!("could not be moved aside: {e}"),
            }
        } else {
            "left in place".to_owned()
        };
        let reason = format!("{file} does not parse ({parse_error}); using the defaults; {moved}");
        SettingFinding::new(SettingError::new("", SettingErrorKind::Storage { reason }), Value::Null, file)
    }

    fn effective(st: &State, key: &Key) -> (Value, Source) {
        match st.stored.get(key.reg.meta.key) {
            Some(v) => (v.clone(), Source::Base),
            None => (key.default.clone(), Source::Default),
        }
    }

    /// Builds the effective values and swaps them in as a whole.
    fn rebuild(&self, st: &State) {
        let entries = self.keys.iter().map(|k| (k.reg.meta.key.to_owned(), Self::effective(st, k))).collect();
        *self.snapshot.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(Snapshot::new(entries));
    }

    fn mark_dirty(&self, st: &mut State, key: &Key, value: Option<Value>) {
        let version = if value.is_some() { key.reg.meta.version } else { 1 };
        let dirty = Dirty { scope: key.reg.meta.scope, value, version };
        st.dirty.insert(key.reg.meta.key.to_owned(), dirty);
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

    fn flush(&self) -> Result<(), SettingError> {
        let _one_at_a_time = locked(&self.flush_gate);
        let (pending, backups) = {
            let st = locked(&self.state);
            (st.dirty.clone(), st.needs_backup.clone())
        };
        let mut failure = None;
        for scope in SCOPES {
            let edits: Vec<Edit> = pending
                .iter()
                .filter(|(_, d)| d.scope == scope)
                .map(|(key, d)| Edit { key: key.clone(), value: d.value.clone().map(|v| (v, d.version)) })
                .collect();
            if edits.is_empty() {
                continue;
            }
            let backup = backups.contains(&scope).then_some(self.app_version);
            match self.write_scope(scope, backup, &edits) {
                Ok(()) => self.settle(scope, &pending),
                Err(e) => failure = failure.or(Some(e)),
            }
        }
        let mut st = locked(&self.state);
        st.first_dirty = (!st.dirty.is_empty()).then(Instant::now);
        st.write_error.clone_from(&failure);
        if failure.is_some() {
            st.not_before = Instant::now() + MAX_DELAY;
        }
        failure.map_or(Ok(()), Err)
    }

    fn write_scope(&self, scope: SettingScope, backup: Option<&str>, edits: &[Edit]) -> Result<(), SettingError> {
        self.backend.update(scope, backup, &mut |text| match document::apply(text, edits) {
            Ok(new) if Some(new.as_str()) == text => Ok(Edited::Unchanged),
            Ok(new) => Ok(Edited::Write(new)),
            Err(ApplyError::Unparsable(_)) => Ok(Edited::Unparsable),
            Err(ApplyError::Conflict(reason)) => Err(reason),
        })
    }

    /// Forgets the changes a write took, except those changed again while it ran.
    fn settle(&self, scope: SettingScope, written: &BTreeMap<String, Dirty>) {
        let mut st = locked(&self.state);
        for (key, dirty) in written.iter().filter(|(_, d)| d.scope == scope) {
            if st.dirty.get(key) == Some(dirty) {
                st.dirty.remove(key);
            }
        }
        st.needs_backup.retain(|s| *s != scope);
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
        out.moved.insert(name.to_owned(), Dirty { scope, value: Some(value.clone()), version: reg.meta.version });
        out.moved.insert(old.to_owned(), Dirty { scope, value: None, version: 1 });
    }
    out.stored.insert(name.to_owned(), value);
}

fn unknown_profile(id: &ProfileId) -> SettingError {
    SettingError::new("", SettingErrorKind::UnknownProfile { id: id.as_str().to_owned() })
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

    fn set_raw(&self, key: &str, value: Value, _layer: Layer, mode: Mode, by: &Actor) -> Result<Change, SettingError> {
        let inner = &*self.inner;
        let k = inner.key(key)?;
        k.reg.validate(&value)?;
        document::json_to_toml(&value).map_err(|e| SettingError::invalid(key, e))?;
        let mut st = locked(&inner.state);
        let (before, _) = Inner::effective(&st, k);
        let change = Change::new(key, before, value.clone(), k.reg.meta.apply);
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

    fn reset(&self, key: &str, _layer: Layer, mode: Mode, by: &Actor) -> Result<Option<Change>, SettingError> {
        let inner = &*self.inner;
        let k = inner.key(key)?;
        let mut st = locked(&inner.state);
        let unusable = st.findings.iter().any(|f| f.error.key == key);
        let Some(before) = st.stored.get(key).cloned() else {
            // A value the file holds but the key cannot use is removed too, so the finding goes away.
            if mode == Mode::Apply && unusable {
                st.findings.retain(|f| f.error.key != key);
                inner.mark_dirty(&mut st, k, None);
            }
            return Ok(None);
        };
        let change = Change::new(key, before, k.default.clone(), k.reg.meta.apply);
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
        Vec::new()
    }

    fn save_profile(
        &self,
        _: &str,
        _: ProfileSource,
        _: bool,
        _: Mode,
        _: &Actor,
    ) -> Result<ProfileInfo, SettingError> {
        let reason = "profiles are not stored yet (docs/design/settings.md D7)".to_owned();
        Err(SettingError::new("", SettingErrorKind::Storage { reason }))
    }

    fn rename_profile(&self, id: &ProfileId, _: &str, _: Mode, _: &Actor) -> Result<ProfileInfo, SettingError> {
        Err(unknown_profile(id))
    }

    fn delete_profile(&self, id: &ProfileId, _: Mode, _: &Actor) -> Result<SwitchReport, SettingError> {
        Err(unknown_profile(id))
    }

    fn switch_profile(&self, to: Option<&ProfileId>, _: Mode, _: &Actor) -> Result<SwitchReport, SettingError> {
        to.map_or_else(|| Ok(SwitchReport::new(0, Vec::new())), |id| Err(unknown_profile(id)))
    }

    fn reload(&self, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError> {
        let inner = &*self.inner;
        let mut loaded = inner.load(mode == Mode::Apply, true)?;
        let mut st = locked(&inner.state);
        // A change this process has not written yet is newer than the file.
        for (key, dirty) in &st.dirty {
            match &dirty.value {
                Some(v) => loaded.stored.insert(key.clone(), v.clone()),
                None => loaded.stored.remove(key),
            };
        }
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
        st.findings.iter().cloned().chain(write).collect()
    }

    fn subscribe(&self) -> Subscription<SettingsEvent> {
        self.inner.hub.subscribe()
    }

    fn session(&self) -> &dyn SessionStore {
        &self.inner.session
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
            log: VecDeque::new(),
            needs_backup: Vec::new(),
            first_dirty: Some(now),
            last_change: now,
            not_before: now,
            closing: false,
        };
        assert_eq!(st.due(), now + DEBOUNCE);
        st.last_change = now + Duration::from_secs(10);
        assert_eq!(st.due(), now + MAX_DELAY, "a burst cannot push the write past the limit");
        st.not_before = now + Duration::from_secs(30);
        assert_eq!(st.due(), now + Duration::from_secs(30), "a failed write waits before it retries");
    }
}
