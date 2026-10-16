//! An in-memory [`Settings`] service for tests of other crates (spec v3 §39 rule 6; D12).
//!
//! The independent oracle for the file store: the same layering (default, base, active profile), the same events and
//! the same rules, with no files, no threads and no clock but the one it is given. Differences from the real store
//! are only the ones files cause: nothing is ever `Storage`-failed, and nothing is dirty or delayed.
//! [`findings`](Settings::findings) starts empty and only a [`reload`](Settings::reload) of hand-edited "files"
//! fills it (an unknown key, a value the registry rejects, or a profile entry that is not a `Member` `User` key).
//!
//! Rules it implements, so a consumer's test can lean on them:
//! - A write is validated first (type, then `Setting::check`); a rejected write changes nothing and emits nothing.
//! - `Layer::Auto` writes into the active profile for a `Member` `User` key, else the base.
//! - One event per change set, only when some effective value changed. A profile switch is computed on a copy and
//!   committed once: one snapshot swap, one event (also when only the active profile changed), or nothing.
//! - The change log keeps the last [`CHANGE_LOG_CAPACITY`] changes, newest first, each stamped with the actor.
//! - **One path per method, in both modes.** Every mutating method edits a copy of the layers, builds the result from
//!   that copy, and installs the copy only for [`Mode::Apply`]. A [`Mode::DryRun`] therefore returns exactly what the
//!   apply would, and leaves the snapshot, the events, the change log, the profiles and the findings as they were.
//! - **The "files".** A write goes through to an in-memory copy of what the files would hold, and
//!   [`reload`](Settings::reload) re-reads that copy. [`hand_edit_base`](MemorySettings::hand_edit_base) and
//!   [`hand_edit_profile`](MemorySettings::hand_edit_profile) change the copy the way a person editing a file would,
//!   so a test can see a reload apply the difference, and a value that cannot be used become a finding.
//!   A write merges only its own changes into that copy, so hand edits and rejected values it did not touch survive;
//!   [`reset`](Settings::reset) also removes the stored entry it targets, even one that reload did not accept.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings_api::error::{SettingError, SettingErrorKind, SettingFinding};
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

/// The data a method edits: cloned for a plan, installed only when the plan is applied.
#[derive(Clone, Debug, Default)]
struct Layers {
    base: BTreeMap<String, Value>,
    profiles: BTreeMap<ProfileId, Profile>,
    active: Option<ProfileId>,
    /// Profile ids handed out so far, so a dry run does not use one up.
    ids_made: u32,
    /// What the edit in progress touched, so the "files" take exactly that. Empty outside a plan.
    touched: Vec<Touch>,
}

/// One thing an operation wrote. The "files" are per-key last-writer-wins, so a write lands on disk even when the live
/// value already equalled it (a hand edit not yet reloaded must not win over an explicit set).
#[derive(Clone, Debug)]
enum Touch {
    /// A key in a layer: set or removed on disk to match the live layer afterwards.
    Key(Target, String),
    /// A whole profile saved, created or deleted: the stored file becomes the live profile, or goes.
    Profile(ProfileId),
    /// A profile renamed: only its name.
    Name(ProfileId),
    /// The active profile changed.
    Active,
}

#[derive(Debug)]
struct State {
    live: Layers,
    /// What the files would hold. Writes go through to it; `hand_edit_*` and `reload` are about it.
    disk: Layers,
    snapshot: Arc<Snapshot>,
    log: VecDeque<SettingChange>,
    findings: Vec<SettingFinding>,
}

/// Where a write goes.
#[derive(Clone, Debug)]
enum Target {
    Base,
    Profile(ProfileId),
}

/// An edit worked out on a copy: what the method returns, what it changed, and the data that would replace the live
/// state. Built the same way in both modes; only [`MemorySettings::finish`] looks at the mode.
struct Planned<T> {
    out: T,
    changes: Vec<Change>,
    layers: Layers,
    snapshot: Arc<Snapshot>,
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

impl Layers {
    fn rebuild(&self) -> Snapshot {
        effective(&self.base, self.active.as_ref().and_then(|id| self.profiles.get(id).map(|p| (id, p))))
    }

    fn target(&self, reg: &SettingRegistration, layer: Layer) -> Target {
        match (layer, &self.active) {
            (Layer::Auto, Some(id)) if member(reg) => Target::Profile(id.clone()),
            _ => Target::Base,
        }
    }

    /// The map a write to `target` edits. A profile target that does not exist is an error, never a silent no-op.
    fn layer_mut(&mut self, target: &Target) -> Result<&mut BTreeMap<String, Value>, SettingError> {
        match target {
            Target::Base => Ok(&mut self.base),
            Target::Profile(id) => self.profiles.get_mut(id).map(|p| &mut p.values).ok_or_else(|| unknown_profile(id)),
        }
    }

    /// Carries what `touched` names over to `disk`, from `self` (the layers after the edit), leaving every other stored
    /// entry (a hand edit, a value reload rejected) as it was.
    fn merge_touched_into(&self, touched: &[Touch], disk: &mut Self) {
        for touch in touched {
            match touch {
                Touch::Key(target, key) => {
                    let now = match target {
                        Target::Base => self.base.get(key),
                        Target::Profile(id) => self.profiles.get(id).and_then(|p| p.values.get(key)),
                    };
                    let stored = match target {
                        Target::Base => Some(&mut disk.base),
                        Target::Profile(id) => disk.profiles.get_mut(id).map(|p| &mut p.values),
                    };
                    if let Some(stored) = stored {
                        edit_values(stored, key, now.cloned());
                    }
                }
                Touch::Profile(id) => match self.profiles.get(id) {
                    Some(now) => {
                        disk.profiles.insert(id.clone(), now.clone());
                    }
                    None => {
                        disk.profiles.remove(id);
                    }
                },
                Touch::Name(id) => {
                    if let (Some(now), Some(stored)) = (self.profiles.get(id), disk.profiles.get_mut(id)) {
                        stored.name.clone_from(&now.name);
                    }
                }
                Touch::Active => disk.active.clone_from(&self.active),
            }
        }
    }

    fn info(&self, id: &ProfileId) -> Option<ProfileInfo> {
        let p = self.profiles.get(id)?;
        Some(ProfileInfo::new(
            id.clone(),
            p.name.clone(),
            u32::try_from(p.values.len()).unwrap_or(u32::MAX),
            self.active.as_ref() == Some(id),
        ))
    }

    /// The layers with every value the registry cannot use left out, and a finding for each (D5: the file keeps the
    /// value, the key reads as its default).
    fn usable(&self) -> (Self, Vec<SettingFinding>) {
        let mut findings = Vec::new();
        let mut keep = |from: &str, in_profile: bool, values: &BTreeMap<String, Value>| -> BTreeMap<String, Value> {
            let mut kept = BTreeMap::new();
            for (key, value) in values {
                // A profile holds only `Member` `User` keys (D3, D8): any other well-typed key is ignored and reported.
                let usable = find(key).and_then(|reg| {
                    if in_profile && !member(reg) {
                        return Err(SettingError::new(key.as_str(), SettingErrorKind::NotInProfile));
                    }
                    reg.validate(value)
                });
                match usable {
                    Ok(()) => {
                        kept.insert(key.clone(), value.clone());
                    }
                    Err(error) => findings.push(SettingFinding::new(error, value.clone(), from)),
                }
            }
            kept
        };
        let mut usable = self.clone();
        usable.base = keep("base", false, &self.base);
        for (id, profile) in &mut usable.profiles {
            profile.values = keep(id.as_str(), true, &profile.values);
        }
        (usable, findings)
    }
}

impl State {
    /// Runs `edit` on a copy of the live layers and works out what it would change. Touches nothing.
    fn plan<T>(&self, edit: impl FnOnce(&mut Layers) -> Result<T, SettingError>) -> Result<Planned<T>, SettingError> {
        let mut layers = self.live.clone();
        let out = edit(&mut layers)?;
        let snapshot = Arc::new(layers.rebuild());
        let changes = diff(&self.snapshot, &snapshot);
        Ok(Planned { out, changes, layers, snapshot })
    }

    fn current_values(&self) -> BTreeMap<String, Value> {
        registry::registrations()
            .filter(|reg| member(reg))
            .filter_map(|reg| {
                let key = reg.meta.key;
                let now = self.snapshot.value(key)?;
                let base_or_default = self.live.base.get(key).cloned().unwrap_or_else(|| reg.default_json());
                (*now != base_or_default).then(|| (key.to_owned(), now.clone()))
            })
            .collect()
    }
}

impl MemorySettings {
    /// A service whose change log is stamped by `clock`.
    #[must_use]
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        let state = State {
            live: Layers::default(),
            disk: Layers::default(),
            snapshot: Arc::new(Snapshot::defaults()),
            log: VecDeque::new(),
            findings: Vec::new(),
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
        locked(&self.state).live.active.clone()
    }

    /// Sets (`Some`) or removes (`None`) `key` in the base "file", as a person editing `settings.toml` would. Nothing
    /// is validated, and nothing changes until [`reload`](Settings::reload).
    pub fn hand_edit_base(&self, key: &str, value: Option<Value>) {
        edit_values(&mut locked(&self.state).disk.base, key, value);
    }

    /// As [`hand_edit_base`](Self::hand_edit_base), for a profile's "file".
    ///
    /// # Errors
    /// [`SettingErrorKind::UnknownProfile`] when no such profile has a "file".
    pub fn hand_edit_profile(&self, id: &ProfileId, key: &str, value: Option<Value>) -> Result<(), SettingError> {
        let mut st = locked(&self.state);
        let profile = st.disk.profiles.get_mut(id).ok_or_else(|| unknown_profile(id))?;
        edit_values(&mut profile.values, key, value);
        Ok(())
    }

    /// The one commit path, shared by every mutating method. [`Mode::DryRun`] stops here; [`Mode::Apply`] installs
    /// the plan, records the changes stamped with `by`, and publishes one event: when some effective value changed, or
    /// when `always_event`. It does not touch the "files": see `write` and `reload`.
    fn finish<T>(
        &self,
        st: &mut State,
        plan: Planned<T>,
        mode: Mode,
        cause: ChangeCause,
        always_event: bool,
        by: &Actor,
    ) -> (T, Vec<Change>) {
        let Planned { out, changes, layers, snapshot } = plan;
        if mode == Mode::Apply {
            st.live = layers;
            // No swap when nothing a reader can see differs (a base write under a profile override).
            if *snapshot != *st.snapshot {
                st.snapshot = snapshot;
            }
            let at = self.clock.now();
            for change in &changes {
                st.log.push_front(SettingChange::new(change.clone(), by.clone(), at));
            }
            st.log.truncate(CHANGE_LOG_CAPACITY);
            if always_event || !changes.is_empty() {
                self.events.publish(&SettingsEvent::new(cause, changes.clone()));
            }
        }
        (out, changes)
    }

    /// `finish` for a method that writes: the "files" take this operation's changes, and nothing else.
    fn write<T>(
        &self,
        st: &mut State,
        mut plan: Planned<T>,
        mode: Mode,
        cause: ChangeCause,
        always_event: bool,
        by: &Actor,
    ) -> (T, Vec<Change>) {
        let touched = std::mem::take(&mut plan.layers.touched);
        let after = plan.layers.clone();
        let done = self.finish(st, plan, mode, cause, always_event, by);
        if mode == Mode::Apply {
            after.merge_touched_into(&touched, &mut st.disk);
        }
        done
    }
}

fn edit_values(values: &mut BTreeMap<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(v) => values.insert(key.to_owned(), v),
        None => values.remove(key),
    };
}

fn report(changes: &[Change]) -> SwitchReport {
    let pending = changes.iter().filter(|c| c.apply == Apply::NextStart).map(|c| c.key.clone()).collect();
    SwitchReport::new(u32::try_from(changes.len()).unwrap_or(u32::MAX), pending)
}

fn unknown_profile(id: &ProfileId) -> SettingError {
    SettingError::new("", SettingErrorKind::UnknownProfile { id: id.to_string() })
}

fn resolve_in(snap: &Snapshot, reg: &SettingRegistration) -> Option<Resolved> {
    let key = reg.meta.key;
    let (value, source) = (snap.value(key)?, snap.source(key)?);
    Some(Resolved::new(key, value.clone(), reg.default_json(), source.clone(), reg.meta))
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
        resolve_in(&self.snapshot(), reg).ok_or_else(|| SettingError::unknown_key(key))
    }

    fn list(&self, filter: &ListFilter) -> Vec<Resolved> {
        // One snapshot for the whole listing, so a concurrent profile switch cannot mix two profiles.
        let snap = self.snapshot();
        registry::registrations()
            .filter(|reg| filter.area.as_deref().is_none_or(|a| reg.meta.area() == a))
            .filter_map(|reg| resolve_in(&snap, reg))
            .filter(|r| !filter.changed || r.is_modified())
            .collect()
    }

    fn set_raw(&self, key: &str, value: Value, layer: Layer, mode: Mode, by: &Actor) -> Result<Change, SettingError> {
        let reg = find(key)?;
        reg.validate(&value)?;
        let mut st = locked(&self.state);
        let plan = st.plan(|layers| {
            let target = layers.target(reg, layer);
            layers.layer_mut(&target)?.insert(key.to_owned(), value);
            layers.touched.push(Touch::Key(target, key.to_owned()));
            Ok(())
        })?;
        let before = st.snapshot.value(key).cloned().unwrap_or(Value::Null);
        let after = plan.snapshot.value(key).cloned().unwrap_or(Value::Null);
        self.write(&mut st, plan, mode, ChangeCause::Set, false, by);
        Ok(Change::new(key, before, after, reg.meta.apply))
    }

    fn reset(&self, key: &str, layer: Layer, mode: Mode, by: &Actor) -> Result<Option<Change>, SettingError> {
        let reg = find(key)?;
        let mut st = locked(&self.state);
        let target = st.live.target(reg, layer);
        // A value reload rejected is stored but not live: reset must still remove it, or the next reload reports it again.
        let stored = st.disk.layer_mut(&target)?.contains_key(key);
        let plan = st.plan(|layers| {
            let removed = layers.layer_mut(&target)?.remove(key).is_some();
            layers.touched.push(Touch::Key(target.clone(), key.to_owned()));
            Ok(removed)
        })?;
        let removed = plan.out || stored;
        let before = st.snapshot.value(key).cloned().unwrap_or(Value::Null);
        let after = plan.snapshot.value(key).cloned().unwrap_or(Value::Null);
        if removed {
            self.write(&mut st, plan, mode, ChangeCause::Reset, false, by);
            if mode == Mode::Apply {
                let from = match &target {
                    Target::Base => "base".to_owned(),
                    Target::Profile(id) => id.to_string(),
                };
                st.findings.retain(|f| !(f.error.key == key && f.from == from));
            }
        }
        Ok(removed.then(|| Change::new(key, before, after, reg.meta.apply)))
    }

    fn profiles(&self) -> Vec<ProfileInfo> {
        let st = locked(&self.state);
        st.live.profiles.keys().filter_map(|id| st.live.info(id)).collect()
    }

    fn save_profile(
        &self,
        name: &str,
        from: ProfileSource,
        overwrite: bool,
        mode: Mode,
        by: &Actor,
    ) -> Result<ProfileInfo, SettingError> {
        let name = validate_name(name)?;
        let mut st = locked(&self.state);
        let values = match from {
            ProfileSource::Profile(id) => st.live.profiles.get(&id).ok_or_else(|| unknown_profile(&id))?.values.clone(),
            _ => st.current_values(),
        };
        let plan = st.plan(|layers| {
            let existing = layers.profiles.iter().find(|(_, p)| p.name == name).map(|(id, _)| id.clone());
            let id = match existing {
                Some(_) if !overwrite => {
                    return Err(SettingError::new("", SettingErrorKind::ProfileExists { name: name.clone() }));
                }
                Some(id) => id,
                None => loop {
                    layers.ids_made += 1;
                    let id = ProfileId::from_name(&name, &format!("{:04x}", layers.ids_made % 0x1_0000))?;
                    if !layers.profiles.contains_key(&id) {
                        break id;
                    }
                },
            };
            layers.profiles.insert(id.clone(), Profile { name: name.clone(), values });
            layers.touched.push(Touch::Profile(id.clone()));
            layers.info(&id).ok_or_else(|| unknown_profile(&id))
        })?;
        let info = plan.out.clone();
        // Saving over the active profile changes effective values: that is a `Reloaded` (see the cause's docs).
        self.write(&mut st, plan, mode, ChangeCause::Reloaded, false, by);
        Ok(info)
    }

    fn rename_profile(&self, id: &ProfileId, name: &str, mode: Mode, by: &Actor) -> Result<ProfileInfo, SettingError> {
        let name = validate_name(name)?;
        let mut st = locked(&self.state);
        let plan = st.plan(|layers| {
            if layers.profiles.iter().any(|(other, p)| other != id && p.name == name) {
                return Err(SettingError::new("", SettingErrorKind::ProfileExists { name: name.clone() }));
            }
            layers.profiles.get_mut(id).ok_or_else(|| unknown_profile(id))?.name = name;
            layers.touched.push(Touch::Name(id.clone()));
            layers.info(id).ok_or_else(|| unknown_profile(id))
        })?;
        let info = plan.out.clone();
        self.write(&mut st, plan, mode, ChangeCause::Reloaded, false, by);
        Ok(info)
    }

    fn delete_profile(&self, id: &ProfileId, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError> {
        let mut st = locked(&self.state);
        let plan = st.plan(|layers| {
            if !layers.profiles.contains_key(id) {
                return Err(unknown_profile(id));
            }
            let was_active = layers.active.as_ref() == Some(id);
            if was_active {
                layers.active = None;
            }
            layers.profiles.remove(id);
            layers.touched.extend([Touch::Profile(id.clone()), Touch::Active]);
            Ok(was_active)
        })?;
        let was_active = plan.out;
        let cause = ChangeCause::ProfileSwitched { from: was_active.then(|| id.clone()), to: None };
        let (_, changes) = self.write(&mut st, plan, mode, cause, was_active, by);
        Ok(report(&changes))
    }

    fn switch_profile(&self, to: Option<&ProfileId>, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError> {
        let mut st = locked(&self.state);
        let plan = st.plan(|layers| {
            if let Some(id) = to.filter(|id| !layers.profiles.contains_key(*id)) {
                return Err(unknown_profile(id));
            }
            layers.touched.push(Touch::Active);
            Ok(std::mem::replace(&mut layers.active, to.cloned()))
        })?;
        let from = plan.out.clone();
        if from.as_ref() == to {
            return Ok(report(&[]));
        }
        let (_, changes) =
            self.write(&mut st, plan, mode, ChangeCause::ProfileSwitched { from, to: to.cloned() }, true, by);
        Ok(report(&changes))
    }

    fn reload(&self, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError> {
        let mut st = locked(&self.state);
        let disk = st.disk.clone();
        let plan = st.plan(|layers| {
            let ids_made = layers.ids_made;
            let (usable, findings) = disk.usable();
            *layers = Layers { ids_made, ..usable };
            Ok(findings)
        })?;
        let findings = plan.out.clone();
        // The "files" are the source here: they stay as read, unusable values included.
        let (_, changes) = self.finish(&mut st, plan, mode, ChangeCause::Reloaded, false, by);
        if mode == Mode::Apply {
            st.findings = findings;
        }
        Ok(report(&changes))
    }

    fn changes(&self, limit: usize) -> Vec<SettingChange> {
        locked(&self.state).log.iter().take(limit).cloned().collect()
    }

    fn findings(&self) -> Vec<SettingFinding> {
        locked(&self.state).findings.clone()
    }

    fn subscribe(&self) -> Subscription<SettingsEvent> {
        self.events.subscribe()
    }

    fn session(&self) -> &dyn SessionStore {
        &self.session
    }
}

/// The in-memory [`SessionStore`]: [`SessionState`]'s own transitions behind a lock. `begin_open`, `opened` and `flush` never fail (there is no file).
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

    fn opened(&self, project: &Path, name: &str) -> Result<(), SettingError> {
        // Truncation is intended: a fake clock's whole seconds are the timestamp.
        #[allow(clippy::cast_possible_truncation)]
        let at = self.clock.now().0 as i64;
        locked(&self.state).opened(project, name, at);
        Ok(())
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

    fn flush(&self) -> Result<(), SettingError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_target_layer_is_an_error_not_a_silent_success() {
        let id = ProfileId::from_name("gone", "0000").expect("a valid id");
        let mut layers = Layers { active: Some(id.clone()), ..Layers::default() };
        let err = layers.layer_mut(&Target::Profile(id)).expect_err("no such profile");
        assert!(matches!(err.kind, SettingErrorKind::UnknownProfile { .. }));
    }
}
