//! An in-memory [`Settings`] service for tests of other crates (spec v3 §39 rule 6; D12).
//!
//! The independent oracle for the file store: the same layering (default, base, active profile), the same events and
//! the same rules, with no files, no threads and no clock but the one it is given. Differences from the real store
//! are only the ones files cause: [`findings`](Settings::findings) is empty (nothing is read back), nothing is ever
//! `Storage`-failed, and nothing is dirty or delayed.
//!
//! Rules it implements, so a consumer's test can lean on them:
//! - A write is validated first (type, then `Setting::check`); a rejected write changes nothing and emits nothing.
//! - `Layer::Auto` writes into the active profile for a `Member` `User` key, else the base.
//! - One event per change set, only when some effective value changed. A profile switch is computed on a copy and
//!   committed once: one snapshot swap, one event (also when only the active profile changed), or nothing.
//! - The change log keeps the last [`CHANGE_LOG_CAPACITY`] changes, newest first.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::Actor;
use omuzaki_settings_api::error::{SettingError, SettingErrorKind};
use omuzaki_settings_api::event::{
    CHANGE_LOG_CAPACITY, Change, ChangeCause, Publisher, SettingChange, SettingsEvent, Subscription, SwitchReport,
};
use omuzaki_settings_api::meta::{Apply, InProfile, SettingScope};
use omuzaki_settings_api::profile::{ProfileId, ProfileInfo, ProfileSource, validate_name};
use omuzaki_settings_api::registry::{self, SettingRegistration};
use omuzaki_settings_api::service::{Layer, ListFilter, Resolved, Settings, Snapshot, Source};
use omuzaki_settings_api::session::{SessionState, SessionStore, WindowGeometry};

#[derive(Clone, Debug)]
struct Profile {
    name: String,
    values: BTreeMap<String, Value>,
}

#[derive(Debug)]
struct State {
    base: BTreeMap<String, Value>,
    profiles: BTreeMap<ProfileId, Profile>,
    active: Option<ProfileId>,
    snapshot: Arc<Snapshot>,
    log: VecDeque<SettingChange>,
    ids_made: u32,
}

/// Where a write goes.
enum Target {
    Base,
    Profile(ProfileId),
}

/// The in-memory service. `MemorySettings::default()` starts with every registered key at its default.
pub struct MemorySettings {
    state: Mutex<State>,
    events: Publisher<SettingsEvent>,
    session: MemorySession,
    clock: Arc<dyn Clock>,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self::with_clock(Arc::new(FakeClock::new(Seconds(0.0))))
    }
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // Every update leaves the state whole before it can panic, so a poisoned lock is still safe to use.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn member(reg: &SettingRegistration) -> bool {
    reg.meta.scope == SettingScope::User && reg.meta.profile == InProfile::Member
}

fn effective(base: &BTreeMap<String, Value>, active: Option<(&ProfileId, &Profile)>) -> Snapshot {
    let entries = registry::registrations()
        .map(|reg| {
            let key = reg.meta.key;
            let from_profile = active
                .filter(|_| member(reg))
                .and_then(|(id, p)| p.values.get(key).map(|v| (v.clone(), Source::Profile(id.clone()))));
            let entry = from_profile
                .or_else(|| base.get(key).map(|v| (v.clone(), Source::Base)))
                .unwrap_or_else(|| (reg.default_json(), Source::Default));
            (key.to_owned(), entry)
        })
        .collect();
    Snapshot::new(entries)
}

fn diff(before: &Snapshot, after: &Snapshot) -> Vec<Change> {
    after
        .iter()
        .filter_map(|(key, now, _)| {
            let was = before.value(key)?;
            let apply = registry::find(key).map_or(Apply::Live, |r| r.meta.apply);
            (was != now).then(|| Change::new(key, was.clone(), now.clone(), apply))
        })
        .collect()
}

impl State {
    fn rebuild(&self) -> Snapshot {
        effective(&self.base, self.active.as_ref().and_then(|id| self.profiles.get(id).map(|p| (id, p))))
    }

    fn target(&self, reg: &SettingRegistration, layer: Layer) -> Target {
        match (layer, &self.active) {
            (Layer::Auto, Some(id)) if member(reg) => Target::Profile(id.clone()),
            _ => Target::Base,
        }
    }

    fn layer_mut(&mut self, target: &Target) -> Option<&mut BTreeMap<String, Value>> {
        match target {
            Target::Base => Some(&mut self.base),
            Target::Profile(id) => self.profiles.get_mut(id).map(|p| &mut p.values),
        }
    }
}

impl MemorySettings {
    /// A service whose change log is stamped by `clock`.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        let snapshot = Arc::new(Snapshot::defaults());
        let state = State {
            base: BTreeMap::new(),
            profiles: BTreeMap::new(),
            active: None,
            snapshot,
            log: VecDeque::new(),
            ids_made: 0,
        };
        Self {
            state: Mutex::new(state),
            events: Publisher::default(),
            session: MemorySession::new(clock.clone()),
            clock,
        }
    }

    /// The active profile, for assertions.
    #[must_use]
    pub fn active_profile(&self) -> Option<ProfileId> {
        locked(&self.state).active.clone()
    }

    /// Swaps in the snapshot built from `st`, records and publishes the changes. Returns them.
    fn commit(&self, st: &mut State, cause: ChangeCause, by: &Actor) -> Vec<Change> {
        let next = Arc::new(st.rebuild());
        let changes = diff(&st.snapshot, &next);
        st.snapshot = next;
        let at = self.clock.now();
        for change in &changes {
            st.log.push_front(SettingChange::new(change.clone(), by.clone(), at));
        }
        st.log.truncate(CHANGE_LOG_CAPACITY);
        let switched = matches!(cause, ChangeCause::ProfileSwitched { .. });
        if switched || !changes.is_empty() {
            self.events.publish(&SettingsEvent::new(cause, changes.clone()));
        }
        changes
    }

    fn switch_locked(&self, st: &mut State, to: Option<&ProfileId>, by: &Actor) -> Result<SwitchReport, SettingError> {
        if let Some(id) = to.filter(|id| !st.profiles.contains_key(*id)) {
            return Err(unknown_profile(id));
        }
        if st.active.as_ref() == to {
            return Ok(SwitchReport::new(0, Vec::new()));
        }
        let from = std::mem::replace(&mut st.active, to.cloned());
        let changes = self.commit(st, ChangeCause::ProfileSwitched { from, to: to.cloned() }, by);
        let pending = changes.iter().filter(|c| c.apply == Apply::NextStart).map(|c| c.key.clone()).collect();
        Ok(SwitchReport::new(u32::try_from(changes.len()).unwrap_or(u32::MAX), pending))
    }

    fn info(st: &State, id: &ProfileId) -> Option<ProfileInfo> {
        let p = st.profiles.get(id)?;
        Some(ProfileInfo::new(
            id.clone(),
            p.name.clone(),
            u32::try_from(p.values.len()).unwrap_or(u32::MAX),
            st.active.as_ref() == Some(id),
        ))
    }

    fn current_values(st: &State) -> BTreeMap<String, Value> {
        registry::registrations()
            .filter(|reg| member(reg))
            .filter_map(|reg| {
                let key = reg.meta.key;
                let now = st.snapshot.value(key)?;
                let base_or_default = st.base.get(key).cloned().unwrap_or_else(|| reg.default_json());
                (*now != base_or_default).then(|| (key.to_owned(), now.clone()))
            })
            .collect()
    }
}

fn unknown_profile(id: &ProfileId) -> SettingError {
    SettingError::new("", SettingErrorKind::UnknownProfile { id: id.to_string() })
}

fn find(key: &str) -> Result<&'static SettingRegistration, SettingError> {
    registry::find(key).ok_or_else(|| SettingError::unknown_key(key))
}

impl Settings for MemorySettings {
    fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&locked(&self.state).snapshot)
    }

    fn resolve(&self, key: &str) -> Result<Resolved, SettingError> {
        let reg = find(key)?;
        let snap = self.snapshot();
        let (Some(value), Some(source)) = (snap.value(key), snap.source(key)) else {
            return Err(SettingError::unknown_key(key));
        };
        Ok(Resolved::new(key, value.clone(), reg.default_json(), source.clone(), reg.meta))
    }

    fn list(&self, filter: &ListFilter) -> Vec<Resolved> {
        registry::registrations()
            .filter(|reg| filter.area.as_deref().is_none_or(|a| reg.meta.area() == a))
            .filter_map(|reg| self.resolve(reg.meta.key).ok())
            .filter(|r| !filter.changed || r.is_modified())
            .collect()
    }

    fn set_raw(&self, key: &str, value: Value, layer: Layer, by: &Actor) -> Result<Change, SettingError> {
        let reg = find(key)?;
        reg.validate(&value)?;
        let mut st = locked(&self.state);
        let before = st.snapshot.value(key).cloned().unwrap_or(Value::Null);
        let target = st.target(reg, layer);
        if let Some(map) = st.layer_mut(&target) {
            map.insert(key.to_owned(), value);
        }
        self.commit(&mut st, ChangeCause::Set, by);
        let after = st.snapshot.value(key).cloned().unwrap_or(Value::Null);
        Ok(Change::new(key, before, after, reg.meta.apply))
    }

    fn reset(&self, key: &str, layer: Layer, by: &Actor) -> Result<Option<Change>, SettingError> {
        let reg = find(key)?;
        let mut st = locked(&self.state);
        let before = st.snapshot.value(key).cloned().unwrap_or(Value::Null);
        let target = st.target(reg, layer);
        if st.layer_mut(&target).and_then(|map| map.remove(key)).is_none() {
            return Ok(None);
        }
        self.commit(&mut st, ChangeCause::Reset, by);
        let after = st.snapshot.value(key).cloned().unwrap_or(Value::Null);
        Ok(Some(Change::new(key, before, after, reg.meta.apply)))
    }

    fn profiles(&self) -> Vec<ProfileInfo> {
        let st = locked(&self.state);
        st.profiles.keys().filter_map(|id| Self::info(&st, id)).collect()
    }

    fn save_profile(
        &self,
        name: &str,
        from: ProfileSource,
        overwrite: bool,
        by: &Actor,
    ) -> Result<ProfileInfo, SettingError> {
        let name = validate_name(name)?;
        let mut st = locked(&self.state);
        let values = match from {
            ProfileSource::Profile(id) => st.profiles.get(&id).ok_or_else(|| unknown_profile(&id))?.values.clone(),
            _ => Self::current_values(&st),
        };
        let existing = st.profiles.iter().find(|(_, p)| p.name == name).map(|(id, _)| id.clone());
        let id = match existing {
            Some(_) if !overwrite => return Err(SettingError::new("", SettingErrorKind::ProfileExists { name })),
            Some(id) => id,
            None => loop {
                st.ids_made += 1;
                let id = ProfileId::from_name(&name, &format!("{:04x}", st.ids_made % 0x1_0000))?;
                if !st.profiles.contains_key(&id) {
                    break id;
                }
            },
        };
        st.profiles.insert(id.clone(), Profile { name, values });
        if st.active.as_ref() == Some(&id) {
            self.commit(&mut st, ChangeCause::Reloaded, by);
        }
        Self::info(&st, &id).ok_or_else(|| unknown_profile(&id))
    }

    fn rename_profile(&self, id: &ProfileId, name: &str) -> Result<ProfileInfo, SettingError> {
        let name = validate_name(name)?;
        let mut st = locked(&self.state);
        if st.profiles.iter().any(|(other, p)| other != id && p.name == name) {
            return Err(SettingError::new("", SettingErrorKind::ProfileExists { name }));
        }
        st.profiles.get_mut(id).ok_or_else(|| unknown_profile(id))?.name = name;
        Self::info(&st, id).ok_or_else(|| unknown_profile(id))
    }

    fn delete_profile(&self, id: &ProfileId) -> Result<(), SettingError> {
        let mut st = locked(&self.state);
        if !st.profiles.contains_key(id) {
            return Err(unknown_profile(id));
        }
        if st.active.as_ref() == Some(id) {
            self.switch_locked(&mut st, None, &Actor::System)?;
        }
        st.profiles.remove(id);
        Ok(())
    }

    fn switch_profile(&self, to: Option<&ProfileId>, by: &Actor) -> Result<SwitchReport, SettingError> {
        self.switch_locked(&mut locked(&self.state), to, by)
    }

    fn changes(&self, limit: usize) -> Vec<SettingChange> {
        locked(&self.state).log.iter().take(limit).cloned().collect()
    }

    fn subscribe(&self) -> Subscription<SettingsEvent> {
        self.events.subscribe()
    }

    fn session(&self) -> &dyn SessionStore {
        &self.session
    }
}

/// The in-memory [`SessionStore`]: [`SessionState`]'s own transitions behind a lock. `begin_open` never fails.
pub struct MemorySession {
    state: Mutex<SessionState>,
    clock: Arc<dyn Clock>,
}

impl MemorySession {
    /// A session store that stamps `opened` with the whole seconds of `clock`.
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self { state: Mutex::new(SessionState::default()), clock }
    }
}

impl SessionStore for MemorySession {
    fn state(&self) -> SessionState {
        locked(&self.state).clone()
    }

    fn begin_open(&self, project: &Path) -> Result<(), SettingError> {
        locked(&self.state).begin_open(project);
        Ok(())
    }

    fn opened(&self, project: &Path, name: &str) {
        // Truncation is intended: a fake clock's whole seconds are the timestamp.
        #[allow(clippy::cast_possible_truncation)]
        let at = self.clock.now().0 as i64;
        locked(&self.state).opened(project, name, at);
    }

    fn forget(&self, project: &Path) {
        locked(&self.state).forget(project);
    }

    fn set_window(&self, id: &str, geometry: WindowGeometry) {
        locked(&self.state).set_window(id, geometry);
    }

    fn mark_version_seen(&self, version: &str) {
        locked(&self.state).mark_version_seen(version);
    }
}
