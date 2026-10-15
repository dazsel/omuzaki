//! The [`Settings`] service (D4, D6): what ops, the UI and consumers talk to.
//!
//! `omuzaki-settings` implements it over TOML files; `omuzaki-settings-mock` in memory. The app wires one with
//! `ServicesBuilder::insert::<dyn Settings>` and ops reach it with `ctx.service::<dyn Settings>()`. The service holds
//! the state; settings are never in the document, so no journal, undo or branch ever captures one person's taste.
//!
//! Who may write which key is **not** decided here. The service stores what it is given and records the actor; the
//! `settings.*` ops apply [`AgentWrite`](crate::meta::AgentWrite) before calling it, so the rule lives in one place.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Mode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{SettingError, SettingFinding};
use crate::event::{Change, SettingChange, SettingsEvent, Subscription, SwitchReport};
use crate::meta::SettingMeta;
use crate::profile::{ProfileId, ProfileInfo, ProfileSource};
use crate::registry;
use crate::session::SessionStore;
use crate::setting::Setting;

/// Where an effective value came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "from", content = "profile", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Source {
    /// The setting's default.
    Default,
    /// The base file (`settings.toml` or `machine.toml`).
    Base,
    /// The active profile's override.
    Profile(ProfileId),
}

/// Which layer a write or reset targets.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Layer {
    /// The active profile for a `Member` key while one is active, else the base (so a tweak made under "Stream" does
    /// not vanish at the next switch).
    Auto,
    /// The base, explicitly.
    Base,
}

/// One setting as a reader sees it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Resolved {
    /// The key.
    pub key: String,
    /// The effective value.
    pub value: Value,
    /// The default.
    pub default: Value,
    /// Where `value` came from.
    pub source: Source,
    /// Static facts.
    pub meta: SettingMeta,
}

impl Resolved {
    /// A resolved setting.
    #[must_use]
    pub fn new(key: impl Into<String>, value: Value, default: Value, source: Source, meta: SettingMeta) -> Self {
        Self { key: key.into(), value, default, source, meta }
    }

    /// Whether the value differs from the default (the Settings page's "modified" dot).
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.value != self.default
    }
}

/// What `Settings::list` returns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ListFilter {
    /// Only keys of this area (`ui`).
    pub area: Option<String>,
    /// Only keys whose value differs from the default.
    pub changed: bool,
}

impl ListFilter {
    /// Everything.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Only this area.
    #[must_use]
    pub fn with_area(mut self, area: impl Into<String>) -> Self {
        self.area = Some(area.into());
        self
    }

    /// Only modified keys.
    #[must_use]
    pub fn only_changed(mut self) -> Self {
        self.changed = true;
        self
    }
}

/// The effective values at one instant: immutable, shared by `Arc`, replaced as a whole on every change (D6).
///
/// Taking the snapshot is an `Arc` clone with no lock beyond the pointer swap in the service, and [`value`](Self::value)
/// is a map lookup. The typed [`get`](Self::get) clones and deserialises, so it allocates for strings and lists: a
/// real-time consumer copies the value it needs when its key's event arrives and never calls it per frame (D6).
/// A snapshot built by [`defaults`](Snapshot::defaults), [`Default`] or a service holds every registered key, so a
/// lookup of a registered key always succeeds; [`new`](Snapshot::new) keeps exactly the entries it is given.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    entries: BTreeMap<String, (Value, Source)>,
}

impl Default for Snapshot {
    /// Every registered key at its default, the same as [`defaults`](Snapshot::defaults): an empty snapshot would
    /// break the rule that a registered key is always found.
    fn default() -> Self {
        Self::defaults()
    }
}

impl Snapshot {
    /// A snapshot of `entries`, which a service builds from the registry, the layers and the active profile. It holds
    /// what it is given, so a service passes every registered key; a key left out reads as unknown from
    /// [`get`](Self::get).
    #[must_use]
    pub fn new(entries: BTreeMap<String, (Value, Source)>) -> Self {
        Self { entries }
    }

    /// Every registered key at its default: the state before any file is read.
    #[must_use]
    pub fn defaults() -> Self {
        Self::new(
            registry::registrations().map(|r| (r.meta.key.to_owned(), (r.default_json(), Source::Default))).collect(),
        )
    }

    /// The raw effective value of `key`.
    #[must_use]
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.entries.get(key).map(|(v, _)| v)
    }

    /// Where the effective value of `key` came from.
    #[must_use]
    pub fn source(&self, key: &str) -> Option<&Source> {
        self.entries.get(key).map(|(_, s)| s)
    }

    /// Every key and its effective value, sorted by key.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value, &Source)> {
        self.entries.iter().map(|(k, (v, s))| (k.as_str(), v, s))
    }

    /// The typed value of `S`.
    ///
    /// # Errors
    /// [`SettingError`] if the key is not in the snapshot (nothing registered it) or the stored JSON no longer fits
    /// the type, which a validating service never lets happen.
    pub fn get<S: Setting>(&self) -> Result<S::Value, SettingError> {
        let key = S::META.key;
        let value = self.value(key).ok_or_else(|| SettingError::unknown_key(key))?;
        serde_json::from_value(value.clone()).map_err(|e| SettingError::wrong_type(key, e))
    }
}

/// The settings service. Raw: a string key and a JSON value, so ops and the generated page need no type per key;
/// [`SettingsExt`] adds the typed `get::<K>()` and `set::<K>()`. Every method takes `&self`: the service is shared.
///
/// # Mutating methods
///
/// Every method that can change an effective value, a profile or the change log takes **a [`Mode`] and the
/// [`Actor`]** who asked, in that order: [`set_raw`](Self::set_raw), [`reset`](Self::reset),
/// [`save_profile`](Self::save_profile), [`rename_profile`](Self::rename_profile),
/// [`delete_profile`](Self::delete_profile), [`switch_profile`](Self::switch_profile) and [`reload`](Self::reload).
/// The change log stamps each change with that actor, so a method that changes values as a side effect (deleting the
/// active profile switches to none) is attributed to the person who asked, never to the system. Session state is
/// written by the app as a side effect and has no actor: see [`SessionStore`].
///
/// # Dry runs
///
/// `Mode::Apply` validates, writes, swaps the snapshot, records the changes and emits the event. `Mode::DryRun` runs
/// **the same validation** and computes **the same return value** on an identical store, then stops: it writes
/// nothing, swaps no snapshot, records no change, emits no event and leaves the profile list, the change log and the
/// findings as they were. Errors are identical in both modes. A caller therefore previews a change by calling the
/// method with `DryRun`, and never has to work the result out from [`resolve`](Self::resolve), which cannot see a
/// value a profile override hides (profile `ui.scale = 100` over base `125`: `resolve` shows `100`, and only the
/// service knows that a reset leads to `125`). Permissions are the ops layer's business and are checked before the
/// service is called in either mode.
///
/// One exception, for a store that draws random ids: a dry-run [`save_profile`](Self::save_profile) returns the
/// `ProfileInfo` an apply would, except that the random suffix of the new id is only an example of its shape.
/// Everything else in the result is exact.
pub trait Settings: Send + Sync {
    /// The effective values. Cheap; swapped as a whole on every change.
    fn snapshot(&self) -> Arc<Snapshot>;

    /// One setting with its default and source.
    ///
    /// # Errors
    /// [`SettingError::unknown_key`] when nothing registered `key`.
    fn resolve(&self, key: &str) -> Result<Resolved, SettingError>;

    /// The settings that pass `filter`, sorted by key.
    fn list(&self, filter: &ListFilter) -> Vec<Resolved>;

    /// Validates (type, `Setting::check`), then writes to the active profile or the base (see [`Layer`]). Emits one
    /// event when the effective value changed, and none when it did not (a base write hidden by a profile override).
    /// Returns the change either way, with `before` and `after` the effective values; see [Dry runs](Settings#dry-runs).
    ///
    /// # Errors
    /// Unknown key, wrong type, or a failed `Setting::check`.
    fn set_raw(&self, key: &str, value: Value, layer: Layer, mode: Mode, by: &Actor) -> Result<Change, SettingError>;

    /// Removes the key from the layer, so it falls back (profile to base to default). `None` when the layer did not
    /// hold the key. Emits one event when the effective value changed. The returned change carries the fallback as
    /// `after`, also under `DryRun`; see [Dry runs](Settings#dry-runs).
    ///
    /// # Errors
    /// Unknown key.
    fn reset(&self, key: &str, layer: Layer, mode: Mode, by: &Actor) -> Result<Option<Change>, SettingError>;

    /// The profiles, sorted by id.
    fn profiles(&self) -> Vec<ProfileInfo>;

    /// Creates a profile from `from`. A name that exists is replaced only with `overwrite`. Saving over the active
    /// profile changes effective values: one event with [`ChangeCause::Reloaded`](crate::event::ChangeCause), changes
    /// stamped with `by`. See [Dry runs](Settings#dry-runs) for the id of a new profile.
    ///
    /// # Errors
    /// A bad name, an unknown source profile, or [`ProfileExists`](crate::error::SettingErrorKind::ProfileExists).
    fn save_profile(
        &self,
        name: &str,
        from: ProfileSource,
        overwrite: bool,
        mode: Mode,
        by: &Actor,
    ) -> Result<ProfileInfo, SettingError>;

    /// Renames the display name; the id and file name stay. No value changes, so there is no event and no log entry;
    /// `by` is carried for the store's own audit log. See [Dry runs](Settings#dry-runs).
    ///
    /// # Errors
    /// A bad name, a name another profile has, or an unknown profile.
    fn rename_profile(&self, id: &ProfileId, name: &str, mode: Mode, by: &Actor) -> Result<ProfileInfo, SettingError>;

    /// Deletes a profile. Deleting the active one switches to none in the same step: one snapshot swap, one event
    /// (`ProfileSwitched` to none), changes stamped with `by`. The report says what the switch changed (zero for a
    /// profile that was not active), so a dry run previews it. See [Dry runs](Settings#dry-runs).
    ///
    /// # Errors
    /// An unknown profile.
    fn delete_profile(&self, id: &ProfileId, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError>;

    /// Atomic: all of the new effective values appear in one snapshot swap and one event, or nothing changes. See
    /// [Dry runs](Settings#dry-runs).
    ///
    /// # Errors
    /// An unknown profile.
    fn switch_profile(&self, to: Option<&ProfileId>, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError>;

    /// Re-reads the settings files and applies what differs from the running state (`settings.reload`; hand edits are
    /// otherwise read at the next start). Atomic like [`switch_profile`](Self::switch_profile): the new effective
    /// values appear in one snapshot swap, one event with [`ChangeCause::Reloaded`](crate::event::ChangeCause) and one
    /// log entry per changed key stamped with `by`; when no effective value differs there is no swap and no event.
    /// Stored values that cannot be used do not fail the reload: they fall back and are listed by
    /// [`findings`](Self::findings). A store with no files reloads its in-memory copy. See
    /// [Dry runs](Settings#dry-runs).
    ///
    /// # Errors
    /// [`SettingErrorKind::Storage`](crate::error::SettingErrorKind::Storage) when a file exists but cannot be read;
    /// nothing changes then.
    fn reload(&self, mode: Mode, by: &Actor) -> Result<SwitchReport, SettingError>;

    /// The last `limit` changes, newest first (at most [`CHANGE_LOG_CAPACITY`](crate::event::CHANGE_LOG_CAPACITY)).
    fn changes(&self, limit: usize) -> Vec<SettingChange>;

    /// Stored values that were not used (D5): shown on the Settings page and logged. Empty for a store with no files.
    fn findings(&self) -> Vec<SettingFinding> {
        Vec::new()
    }

    /// A new queue of change events.
    fn subscribe(&self) -> Subscription<SettingsEvent>;

    /// Session state: recent projects, window geometry, the crash-loop guard.
    fn session(&self) -> &dyn SessionStore;
}

/// Typed access, for any [`Settings`] including `dyn Settings`: `settings.get::<UiScale>()`.
pub trait SettingsExt: Settings {
    /// The effective value of `S`.
    ///
    /// # Errors
    /// See [`Snapshot::get`].
    fn get<S: Setting>(&self) -> Result<S::Value, SettingError> {
        self.snapshot().get::<S>()
    }

    /// Writes `value` to `S`.
    ///
    /// # Errors
    /// As [`Settings::set_raw`].
    fn set<S: Setting>(&self, value: &S::Value, layer: Layer, mode: Mode, by: &Actor) -> Result<Change, SettingError> {
        let json = serde_json::to_value(value).map_err(|e| SettingError::wrong_type(S::META.key, e))?;
        self.set_raw(S::META.key, json, layer, mode, by)
    }
}

impl<T: Settings + ?Sized> SettingsExt for T {}
