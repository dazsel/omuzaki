//! `MemorySettings`: layering, validation, atomic profile switches, events, the change log and session state.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

use std::path::Path;

use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings_api::error::SettingErrorKind;
use omuzaki_settings_api::event::{CHANGE_LOG_CAPACITY, ChangeCause};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::profile::{ProfileId, ProfileSource};
use omuzaki_settings_api::service::{Layer, ListFilter, Settings, SettingsExt, Snapshot, Source};
use omuzaki_settings_api::setting::Setting;
use omuzaki_settings_api::{check_crate, error::SettingError, register_setting};
use omuzaki_settings_mock::MemorySettings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// UI scale in percent. Appearance only, so agents may set it.
struct UiScale;
impl Setting for UiScale {
    type Value = u16;
    const META: SettingMeta = SettingMeta::new(
        "ui.scale",
        "UI scale in percent",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );
    fn default() -> u16 {
        100
    }
    fn check(v: &u16) -> Result<(), SettingError> {
        SettingError::range("ui.scale", *v, 75, 200)
    }
}
register_setting!(UiScale);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Tier {
    Live,
    High,
}

/// Quality tier. Taste, so agents may set it.
struct QualityTier;
impl Setting for QualityTier {
    type Value = Tier;
    const META: SettingMeta = SettingMeta::new(
        "quality.tier",
        "Render quality tier",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );
    fn default() -> Tier {
        Tier::Live
    }
}
register_setting!(QualityTier);

/// UI language: a person's choice, kept out of profiles, and it takes a restart in this test.
struct Language;
impl Setting for Language {
    type Value = String;
    const META: SettingMeta = SettingMeta::new(
        "ui.language",
        "Interface language",
        SettingScope::User,
        AgentWrite::Confirm,
        InProfile::Excluded,
        Apply::NextStart,
        1,
    );
    fn default() -> String {
        "en".into()
    }
}
register_setting!(Language);

/// Update channel: changes what code runs, so only a person sets it.
struct Channel;
impl Setting for Channel {
    type Value = String;
    const META: SettingMeta = SettingMeta::new(
        "update.channel",
        "Update channel",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::NextStart,
        1,
    );
    fn default() -> String {
        "stable".into()
    }
}
register_setting!(Channel);

/// Density: appearance only; the layout is rebuilt at start, so it waits for a restart.
struct Density;
impl Setting for Density {
    type Value = u8;
    const META: SettingMeta = SettingMeta::new(
        "ui.density",
        "UI density",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::NextStart,
        1,
    );
    fn default() -> u8 {
        1
    }
}
register_setting!(Density);

/// The keys above are registered by this test binary itself, so there is no other crate whose `link()` to call.
mod this_binary {
    pub fn link() {}
}

check_crate!(this_binary);

fn person() -> Actor {
    Actor::Person { name: "test".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "a1".into(), model: None }
}

#[test]
fn a_fresh_service_reads_defaults() {
    let s = MemorySettings::default();
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
    assert_eq!(s.get::<QualityTier>().unwrap(), Tier::Live);
    assert_eq!(s.resolve("ui.scale").unwrap().source, Source::Default);
    assert_eq!(s.list(&ListFilter::all()).len(), 5);
    assert_eq!(s.list(&ListFilter::all().with_area("ui")).len(), 3);
}

#[test]
fn a_wrong_type_or_range_is_rejected_with_the_key_named_and_nothing_changes() {
    let s = MemorySettings::default();
    let events = s.subscribe();
    let e = s.set_raw("ui.scale", json!("big"), Layer::Auto, Mode::Apply, &person()).unwrap_err();
    assert!(matches!(e.kind, SettingErrorKind::WrongType { .. }) && e.key == "ui.scale");
    let e = s.set::<UiScale>(&900, Layer::Auto, Mode::Apply, &person()).unwrap_err();
    assert!(e.to_string().contains("`ui.scale`") && matches!(e.kind, SettingErrorKind::OutOfRange { .. }));
    assert!(matches!(
        s.set_raw("nope.key", json!(1), Layer::Auto, Mode::Apply, &person()).unwrap_err().kind,
        SettingErrorKind::UnknownKey
    ));
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
    assert!(events.drain().is_empty() && s.changes(10).is_empty());
}

#[test]
fn a_set_emits_one_event_and_is_logged_with_its_actor() {
    let s = MemorySettings::default();
    let events = s.subscribe();
    let change = s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &agent()).unwrap();
    assert_eq!((change.before, change.after), (json!(100), json!(125)));
    let got = events.drain();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].cause, ChangeCause::Set);
    assert_eq!(got[0].change_of("ui.scale").unwrap().after, json!(125));
    assert_eq!(s.changes(5)[0].actor, agent());
    // Writing the same value again is no change and no event.
    s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &agent()).unwrap();
    assert!(events.drain().is_empty());
}

#[test]
fn reset_falls_back_and_says_when_there_was_nothing_to_reset() {
    let s = MemorySettings::default();
    assert_eq!(s.reset("ui.scale", Layer::Auto, Mode::Apply, &person()).unwrap(), None);
    s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &person()).unwrap();
    let c = s.reset("ui.scale", Layer::Auto, Mode::Apply, &person()).unwrap().unwrap();
    assert_eq!((c.before, c.after), (json!(125), json!(100)));
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
}

#[test]
fn a_profile_overlays_the_base_and_writes_go_to_the_active_profile() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Auto, Mode::Apply, &person()).unwrap();
    // "Current" captures what differs from base-or-default: here both keys (base = effective, so only defaults differ).
    let stream = s.save_profile("Stream", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    assert_eq!(stream.keys, 0, "values equal to the base are omitted");
    s.switch_profile(Some(&stream.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&90, Layer::Auto, Mode::Apply, &person()).unwrap();
    assert_eq!(s.resolve("ui.scale").unwrap().source, Source::Profile(stream.id.clone()));
    // An explicit base write hidden by the override is no change, and the base keeps it.
    let events = s.subscribe();
    s.set::<UiScale>(&110, Layer::Base, Mode::Apply, &person()).unwrap();
    assert!(events.drain().is_empty());
    s.switch_profile(None, Mode::Apply, &person()).unwrap();
    assert_eq!(s.get::<UiScale>().unwrap(), 110);
    // A machine key never goes to a profile even while one is active.
    s.switch_profile(Some(&stream.id), Mode::Apply, &person()).unwrap();
    s.set_raw("update.channel", json!("beta"), Layer::Auto, Mode::Apply, &person()).unwrap();
    assert_eq!(s.resolve("update.channel").unwrap().source, Source::Base);
}

#[test]
fn a_profile_switch_is_one_event_listing_only_what_changed_and_restarts_pending() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &person()).unwrap();
    let stream = s.save_profile("Stream", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&stream.id), Mode::Apply, &person()).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.set::<Density>(&2, Layer::Auto, Mode::Apply, &person()).unwrap();
    assert_eq!(s.profiles()[0].keys, 2);

    let events = s.subscribe();
    let report = s.switch_profile(None, Mode::Apply, &agent()).unwrap();
    let got = events.drain();
    assert_eq!(got.len(), 1, "one event for the whole switch");
    assert_eq!(got[0].cause, ChangeCause::ProfileSwitched { from: Some(stream.id.clone()), to: None });
    let keys: Vec<_> = got[0].changes.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["quality.tier", "ui.density"], "ui.scale is the same under both, so it is not listed");
    assert_eq!(report.changed, 2);
    assert_eq!(report.pending_restart, ["ui.density"]);
    assert_eq!(s.changes(10).len(), 3 + 2, "three sets, then the switch logs once per changed key");

    // Switching to the profile we are in does nothing.
    s.switch_profile(Some(&stream.id), Mode::Apply, &person()).unwrap();
    let _ = events.drain();
    assert_eq!(s.switch_profile(Some(&stream.id), Mode::Apply, &person()).unwrap().changed, 0);
    assert!(events.drain().is_empty());
}

#[test]
fn a_switch_to_an_unknown_profile_changes_nothing() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &person()).unwrap();
    let before = s.snapshot();
    let events = s.subscribe();
    let ghost = omuzaki_settings_api::profile::ProfileId::new("ghost-0001").unwrap();
    let e = s.switch_profile(Some(&ghost), Mode::Apply, &person()).unwrap_err();
    assert!(matches!(e.kind, SettingErrorKind::UnknownProfile { .. }));
    assert_eq!(*s.snapshot(), *before);
    assert!(events.drain().is_empty() && s.active_profile().is_none());
}

#[test]
fn switching_there_and_back_restores_the_exact_snapshot() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &person()).unwrap();
    let p = s.save_profile("Alt", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&80, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Auto, Mode::Apply, &person()).unwrap();
    let in_profile = s.snapshot();
    s.switch_profile(None, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    assert_eq!(*s.snapshot(), *in_profile);
}

#[test]
fn profile_names_ids_and_overwrite() {
    let s = MemorySettings::default();
    let a = s.save_profile("  Stream ", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    assert_eq!(a.name, "Stream");
    assert!(a.id.as_str().starts_with("stream-"));
    let e = s.save_profile("Stream", ProfileSource::Current, false, Mode::Apply, &person()).unwrap_err();
    assert!(matches!(e.kind, SettingErrorKind::ProfileExists { .. }));
    assert_eq!(s.save_profile("Stream", ProfileSource::Current, true, Mode::Apply, &person()).unwrap().id, a.id);
    assert!(s.save_profile("", ProfileSource::Current, false, Mode::Apply, &person()).is_err());
    let jp = s.save_profile("配信", ProfileSource::Profile(a.id.clone()), false, Mode::Apply, &person()).unwrap();
    assert!(jp.id.as_str().starts_with("profile-"));
    let renamed = s.rename_profile(&jp.id, "Live", Mode::Apply, &person()).unwrap();
    assert_eq!((renamed.id, renamed.name.as_str()), (jp.id.clone(), "Live"));
    assert!(s.rename_profile(&jp.id, "Stream", Mode::Apply, &person()).is_err());
    assert_eq!(s.profiles().len(), 2);
}

#[test]
fn deleting_the_active_profile_switches_to_none_first() {
    let s = MemorySettings::default();
    let p = s.save_profile("Alt", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&80, Layer::Auto, Mode::Apply, &person()).unwrap();
    let events = s.subscribe();
    s.delete_profile(&p.id, Mode::Apply, &agent()).unwrap();
    assert_eq!(s.active_profile(), None);
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
    assert_eq!(events.drain().len(), 1);
    assert!(s.profiles().is_empty());
    assert!(s.delete_profile(&p.id, Mode::Apply, &agent()).is_err());
}

#[test]
fn the_change_log_is_newest_first_and_capped() {
    let s = MemorySettings::default();
    for i in 0..u16::try_from(CHANGE_LOG_CAPACITY + 50).unwrap() {
        s.set::<UiScale>(&(75 + i % 100 + 1), Layer::Auto, Mode::Apply, &person()).unwrap();
    }
    let log = s.changes(1000);
    assert_eq!(log.len(), CHANGE_LOG_CAPACITY);
    assert_eq!(log[0].change.after, json!(75 + (CHANGE_LOG_CAPACITY + 49) % 100 + 1));
    assert_eq!(s.changes(3).len(), 3);
}

#[test]
fn session_state_is_kept_and_the_opening_guard_survives_until_opened() {
    let s = MemorySettings::default();
    let session = s.session();
    session.begin_open(Path::new("/p/a")).unwrap();
    assert_eq!(session.state().opening.as_deref(), Some(Path::new("/p/a")));
    session.opened(Path::new("/p/a"), "A").unwrap();
    session.flush().unwrap();
    session.mark_version_seen("0.9.0");
    session.mark_version_seen("0.9.0-beta.1");
    assert_eq!(session.state().last_seen_version.as_deref(), Some("0.9.0"), "a beta never replaces its release");
    session.set_window("main", omuzaki_settings_api::session::WindowGeometry::new(0, 0, 800, 600));
    let state = session.state();
    assert!(state.opening.is_none() && state.recent.len() == 1 && state.windows.contains_key("main"));
    session.forget(Path::new("/p/a"));
    assert!(session.state().recent.is_empty());
}

// --- dry runs, actors and reload ------------------------------------------------------------------------------

/// Everything a caller can observe of a service that a dry run must leave alone.
#[derive(Debug, PartialEq)]
struct Observed {
    snapshot: Snapshot,
    changes: Vec<omuzaki_settings_api::event::SettingChange>,
    profiles: Vec<omuzaki_settings_api::profile::ProfileInfo>,
    findings: Vec<omuzaki_settings_api::error::SettingFinding>,
    active: Option<omuzaki_settings_api::profile::ProfileId>,
}

fn observed(s: &MemorySettings) -> Observed {
    Observed {
        snapshot: (*s.snapshot()).clone(),
        changes: s.changes(CHANGE_LOG_CAPACITY),
        profiles: s.profiles(),
        findings: s.findings(),
        active: s.active_profile(),
    }
}

/// Runs `op` as a dry run and then as an apply on the same store, and checks the two returned the same thing, that the
/// dry run changed nothing anyone can see (no snapshot swap, no log entry, no event, no profile), and that the apply
/// then did what it said. Returns the apply's result for further checks.
fn dry_run_then_apply<R: PartialEq + std::fmt::Debug>(
    s: &MemorySettings,
    op: impl Fn(&MemorySettings, Mode) -> Result<R, SettingError>,
) -> Result<R, SettingError> {
    let events = s.subscribe();
    let before = observed(s);
    let snapshot_before = s.snapshot();
    let dry = op(s, Mode::DryRun);
    assert_eq!(observed(s), before, "a dry run changed the store");
    assert!(std::sync::Arc::ptr_eq(&snapshot_before, &s.snapshot()), "a dry run swapped the snapshot");
    assert!(events.drain().is_empty(), "a dry run emitted an event");
    let applied = op(s, Mode::Apply);
    assert_eq!(dry, applied, "a dry run returns what the apply returns");
    applied
}

/// A store where profile `P` is active and holds `ui.scale = 100` over a base of `125`.
fn hidden_base() -> (MemorySettings, omuzaki_settings_api::profile::ProfileId) {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Base, Mode::Apply, &person()).unwrap();
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&100, Layer::Auto, Mode::Apply, &person()).unwrap();
    assert_eq!(s.resolve("ui.scale").unwrap().value, json!(100), "resolve shows only the override");
    (s, p.id)
}

#[test]
fn a_dry_run_set_returns_the_change_and_changes_nothing() {
    let s = MemorySettings::default();
    let c = dry_run_then_apply(&s, |s, m| s.set::<UiScale>(&125, Layer::Auto, m, &agent())).unwrap();
    assert_eq!((c.before, c.after), (json!(100), json!(125)));
    assert_eq!(s.get::<UiScale>().unwrap(), 125);
    // A rejected value is rejected the same way in both modes, and the key is named.
    let e = dry_run_then_apply(&s, |s, m| s.set::<UiScale>(&900, Layer::Auto, m, &agent())).unwrap_err();
    assert!(e.to_string().contains("`ui.scale`"));
    // A base write hidden by an override is a change with no effective difference, in both modes.
    let (s, _) = hidden_base();
    let hidden = dry_run_then_apply(&s, |s, m| s.set::<UiScale>(&110, Layer::Base, m, &person())).unwrap();
    assert_eq!((hidden.before, hidden.after), (json!(100), json!(100)));
}

#[test]
fn a_dry_run_reset_previews_the_fallback_a_resolve_cannot_see() {
    let (s, _) = hidden_base();
    let c = dry_run_then_apply(&s, |s, m| s.reset("ui.scale", Layer::Auto, m, &person())).unwrap().unwrap();
    assert_eq!((c.before, c.after), (json!(100), json!(125)), "profile 100 over base 125 falls back to 125");
    assert_eq!(s.get::<UiScale>().unwrap(), 125);
    // Nothing to reset is nothing in both modes.
    assert_eq!(dry_run_then_apply(&s, |s, m| s.reset("ui.scale", Layer::Auto, m, &person())).unwrap(), None);
    assert!(dry_run_then_apply(&s, |s, m| s.reset("nope.key", Layer::Auto, m, &person())).is_err());
}

#[test]
fn a_dry_run_save_returns_the_profile_an_apply_creates_without_using_up_its_id() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &person()).unwrap();
    let a =
        dry_run_then_apply(&s, |s, m| s.save_profile("Stream", ProfileSource::Current, false, m, &person())).unwrap();
    assert!(a.id.as_str().starts_with("stream-") && s.profiles().len() == 1);
    // The same name without overwrite fails identically; with it, the same profile is replaced.
    let e = dry_run_then_apply(&s, |s, m| s.save_profile("Stream", ProfileSource::Current, false, m, &person()));
    assert!(matches!(e.unwrap_err().kind, SettingErrorKind::ProfileExists { .. }));
    let again =
        dry_run_then_apply(&s, |s, m| s.save_profile("Stream", ProfileSource::Current, true, m, &person())).unwrap();
    assert_eq!(again.id, a.id);
    // Saving over the active profile changes effective values: previewed by the dry run, then one event on apply.
    let other = s.save_profile("Other", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&other.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&90, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&a.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&80, Layer::Auto, Mode::Apply, &person()).unwrap();
    let events = s.subscribe();
    dry_run_then_apply(&s, |s, m| {
        s.save_profile("Stream", ProfileSource::Profile(other.id.clone()), true, m, &agent())
    })
    .unwrap();
    assert_eq!(s.get::<UiScale>().unwrap(), 90);
    assert_eq!(s.changes(1)[0].actor, agent(), "the change is stamped with the actor who saved");
    assert_eq!(events.drain().len(), 1);
}

#[test]
fn a_dry_run_rename_and_delete_and_switch_return_what_the_apply_returns() {
    let (s, p) = hidden_base();
    let renamed = dry_run_then_apply(&s, |s, m| s.rename_profile(&p, "Renamed", m, &agent())).unwrap();
    assert_eq!(renamed.name, "Renamed");
    assert!(dry_run_then_apply(&s, |s, m| s.rename_profile(&p, "", m, &agent())).is_err());

    // Switching away from the profile: ui.scale 100 -> 125.
    let switched = dry_run_then_apply(&s, |s, m| s.switch_profile(None, m, &agent())).unwrap();
    assert_eq!(switched.changed, 1);
    let ghost = omuzaki_settings_api::profile::ProfileId::new("ghost-0001").unwrap();
    assert!(dry_run_then_apply(&s, |s, m| s.switch_profile(Some(&ghost), m, &agent())).is_err());

    // Deleting a profile that is not active reports no change; deleting the active one reports the switch.
    let other = s.save_profile("Other", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    assert_eq!(dry_run_then_apply(&s, |s, m| s.delete_profile(&other.id, m, &agent())).unwrap().changed, 0);
    s.switch_profile(Some(&p), Mode::Apply, &person()).unwrap();
    let deleted = dry_run_then_apply(&s, |s, m| s.delete_profile(&p, m, &agent())).unwrap();
    assert_eq!(deleted.changed, 1, "deleting the active profile lets ui.scale fall back from 100 to 125");
    assert!(dry_run_then_apply(&s, |s, m| s.delete_profile(&p, m, &agent())).is_err());
}

#[test]
fn deleting_the_active_profile_is_one_event_attributed_to_the_actor_who_asked() {
    let (s, p) = hidden_base();
    let events = s.subscribe();
    s.delete_profile(&p, Mode::Apply, &agent()).unwrap();
    let got = events.drain();
    assert_eq!(got.len(), 1, "one swap and one event for the switch and the delete together");
    assert_eq!(got[0].cause, ChangeCause::ProfileSwitched { from: Some(p), to: None });
    assert!(s.changes(5).iter().take(1).all(|c| c.actor == agent()), "never attributed to the system");
}

#[test]
fn reload_applies_hand_edits_as_one_event_with_the_reloaded_cause() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.hand_edit_base("ui.scale", Some(json!(150)));
    s.hand_edit_base("quality.tier", Some(json!("high")));
    let events = s.subscribe();
    let report = dry_run_then_apply(&s, |s, m| s.reload(m, &agent())).unwrap();
    assert_eq!(report.changed, 2);
    let got = events.drain();
    assert_eq!(got.len(), 1, "both keys arrive in one event");
    assert_eq!(got[0].cause, ChangeCause::Reloaded);
    let keys: Vec<_> = got[0].changes.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["quality.tier", "ui.scale"]);
    assert_eq!((s.get::<UiScale>().unwrap(), s.get::<QualityTier>().unwrap()), (150, Tier::High));
    assert!(s.changes(2).iter().all(|c| c.actor == agent()));
    // Reloading again, with the files as they were read, changes nothing: no swap, no event.
    let snapshot = s.snapshot();
    assert_eq!(s.reload(Mode::Apply, &agent()).unwrap().changed, 0);
    assert!(std::sync::Arc::ptr_eq(&snapshot, &s.snapshot()) && events.drain().is_empty());
}

#[test]
fn reload_falls_back_on_values_it_cannot_use_and_reports_them_as_findings() {
    let s = MemorySettings::default();
    s.hand_edit_base("ui.scale", Some(json!(900)));
    s.hand_edit_base("quality.tier", Some(json!("ultra")));
    s.hand_edit_base("removed.key", Some(json!(1)));
    let report = dry_run_then_apply(&s, |s, m| s.reload(m, &person())).unwrap();
    assert_eq!(report.changed, 0, "nothing usable differs, so the values stay at their defaults");
    assert_eq!((s.get::<UiScale>().unwrap(), s.get::<QualityTier>().unwrap()), (100, Tier::Live));
    let found = s.findings();
    let mut keys: Vec<_> = found.iter().map(|f| f.error.key.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["quality.tier", "removed.key", "ui.scale"]);
    assert!(found.iter().all(|f| f.from == "base"));
    // A profile's "file" is read too, and the active profile's usable values apply.
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    s.hand_edit_profile(&p.id, "ui.scale", Some(json!(90))).unwrap();
    assert_eq!(dry_run_then_apply(&s, |s, m| s.reload(m, &person())).unwrap().changed, 1);
    assert_eq!(s.resolve("ui.scale").unwrap().source, Source::Profile(p.id));
}

#[test]
fn the_session_store_reports_the_durable_clear_and_flushes() {
    let s = MemorySettings::default();
    let session = s.session();
    session.begin_open(Path::new("/p/a")).unwrap();
    session.opened(Path::new("/p/a"), "A").unwrap();
    assert!(session.state().opening.is_none());
    session.flush().unwrap();
}

#[test]
fn a_write_keeps_hand_edits_and_rejected_values_it_did_not_touch() {
    let s = MemorySettings::default();
    s.hand_edit_base("ui.scale", Some(json!(150)));
    s.hand_edit_base("quality.tier", Some(json!("ultra")));
    s.set::<Density>(&2, Layer::Base, Mode::Apply, &person()).unwrap();
    // The hand edit was never reloaded, so it is not live, but the write must not erase it.
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
    let report = dry_run_then_apply(&s, |s, m| s.reload(m, &person())).unwrap();
    assert_eq!(report.changed, 1);
    assert_eq!(s.get::<UiScale>().unwrap(), 150, "the hand edit survived the unrelated write");
    assert_eq!(s.findings().len(), 1, "the rejected quality.tier is still reported");
    assert_eq!(s.get::<Density>().unwrap(), 2);
}

#[test]
fn profile_writes_keep_hand_edits_in_the_profile_file() {
    let s = MemorySettings::default();
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    s.hand_edit_profile(&p.id, "ui.scale", Some(json!(90))).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!((s.get::<UiScale>().unwrap(), s.get::<QualityTier>().unwrap()), (90, Tier::High));
    assert!(s.hand_edit_profile(&ProfileId::from_name("gone", "0000").unwrap(), "ui.scale", None).is_err());
}

#[test]
fn reset_removes_a_stored_value_reload_rejected_apply_and_dry_run() {
    let s = MemorySettings::default();
    s.hand_edit_base("ui.scale", Some(json!(900)));
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.findings().len(), 1);
    let change = dry_run_then_apply(&s, |s, m| s.reset("ui.scale", Layer::Auto, m, &person())).unwrap();
    assert!(change.is_some(), "there was a stored value to reset");
    assert!(s.findings().is_empty(), "the finding is gone with its value");
    s.reload(Mode::Apply, &person()).unwrap();
    assert!(s.findings().is_empty(), "the next reload does not report it again");
    assert!(s.reset("ui.scale", Layer::Auto, Mode::Apply, &person()).unwrap().is_none());
}

#[test]
fn a_listing_comes_from_one_snapshot() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&150, Layer::Base, Mode::Apply, &person()).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Base, Mode::Apply, &person()).unwrap();
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&100, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.set::<QualityTier>(&Tier::Live, Layer::Auto, Mode::Apply, &person()).unwrap();
    // Profile P reads (100, Live); the base reads (150, High). A listing must never mix the two.
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for i in 0..2000 {
                let to = (i % 2 == 0).then_some(&p.id);
                s.switch_profile(to, Mode::Apply, &person()).unwrap();
            }
        });
        for _ in 0..2000 {
            let got = s.list(&ListFilter::default());
            let value = |k: &str| got.iter().find(|r| r.key == k).unwrap().value.clone();
            let pair = (value("ui.scale"), value("quality.tier"));
            assert!(
                pair == (json!(100), json!("live")) || pair == (json!(150), json!("high")),
                "mixed listing {pair:?}"
            );
        }
    });
}

#[test]
fn reload_leaves_non_member_keys_out_of_a_profile_and_reports_them() {
    let s = MemorySettings::default();
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.hand_edit_profile(&p.id, "ui.language", Some(json!("de"))).unwrap();
    s.hand_edit_profile(&p.id, "update.channel", Some(json!("beta"))).unwrap();
    s.hand_edit_profile(&p.id, "ui.scale", Some(json!(90))).unwrap();
    dry_run_then_apply(&s, |s, m| s.reload(m, &person())).unwrap();
    assert_eq!(s.profiles()[0].keys, 1, "only the Member key counts");
    let found = s.findings();
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|f| f.error.kind == SettingErrorKind::NotInProfile && f.from == p.id.as_str()));
    let copy = s.save_profile("Copy", ProfileSource::Profile(p.id), false, Mode::Apply, &person()).unwrap();
    assert_eq!(copy.keys, 1, "duplication does not propagate them");
}

#[test]
fn an_explicit_set_back_to_the_live_value_beats_an_unreloaded_hand_edit() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Base, Mode::Apply, &person()).unwrap();
    s.hand_edit_base("ui.scale", Some(json!(150)));
    // The live value is still 125, so this set changes nothing live, but it is the last writer.
    dry_run_then_apply(&s, |s, m| s.set::<UiScale>(&125, Layer::Base, m, &person()).map(|_| ())).unwrap();
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.get::<UiScale>().unwrap(), 125, "the explicit set persisted over the hand edit");
}

#[test]
fn an_explicit_set_in_a_profile_beats_an_unreloaded_hand_edit_of_that_profile() {
    let s = MemorySettings::default();
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&110, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.hand_edit_profile(&p.id, "ui.scale", Some(json!(90))).unwrap();
    s.set::<UiScale>(&110, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.get::<UiScale>().unwrap(), 110);
}

#[test]
fn reset_of_a_key_that_is_unchanged_live_still_clears_a_hand_edit() {
    let s = MemorySettings::default();
    s.hand_edit_base("ui.scale", Some(json!(150)));
    // Live holds nothing for the key, but the file does: reset clears the file, so a reload keeps the default.
    let change = dry_run_then_apply(&s, |s, m| s.reset("ui.scale", Layer::Base, m, &person())).unwrap();
    assert!(change.is_some());
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
}

#[test]
fn a_profile_switch_keeps_the_files_hand_edits_and_the_active_profile_across_a_reload() {
    let s = MemorySettings::default();
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.hand_edit_base("ui.scale", Some(json!(150)));
    s.hand_edit_profile(&p.id, "quality.tier", Some(json!("high"))).unwrap();
    dry_run_then_apply(&s, |s, m| s.switch_profile(Some(&p.id), m, &person())).unwrap();
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.active_profile(), Some(p.id));
    assert_eq!((s.get::<UiScale>().unwrap(), s.get::<QualityTier>().unwrap()), (150, Tier::High));
}

/// A store whose base file holds a rejected `ui.scale = 900`, found by a reload.
fn with_rejected_scale() -> MemorySettings {
    let s = MemorySettings::default();
    s.hand_edit_base("ui.scale", Some(json!(900)));
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.findings().len(), 1);
    s
}

#[test]
fn a_set_clears_the_finding_for_the_value_it_replaced_and_a_dry_run_or_refusal_does_not() {
    let s = with_rejected_scale();
    s.hand_edit_base("quality.tier", Some(json!("ultra")));
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.findings().len(), 2);
    s.set::<UiScale>(&125, Layer::Base, Mode::DryRun, &person()).unwrap();
    assert_eq!(s.findings().len(), 2, "a dry run leaves findings alone");
    assert!(s.set_raw("ui.scale", json!(900), Layer::Base, Mode::Apply, &person()).is_err());
    assert_eq!(s.findings().len(), 2, "a refused apply leaves findings alone");
    s.set::<UiScale>(&125, Layer::Base, Mode::Apply, &person()).unwrap();
    let left = s.findings();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].error.key, "quality.tier", "only the replaced key's finding is gone");
}

#[test]
fn a_reset_clears_its_finding_and_a_refused_reset_leaves_it() {
    let s = with_rejected_scale();
    assert!(s.reset("no.such.key", Layer::Base, Mode::Apply, &person()).is_err());
    assert_eq!(s.findings().len(), 1);
    s.reset("ui.scale", Layer::Base, Mode::DryRun, &person()).unwrap();
    assert_eq!(s.findings().len(), 1);
    s.reset("ui.scale", Layer::Base, Mode::Apply, &person()).unwrap();
    assert!(s.findings().is_empty());
}

#[test]
fn overwriting_a_profile_merges_per_key_and_deleting_it_clears_its_findings() {
    let s = MemorySettings::default();
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.hand_edit_profile(&p.id, "ui.scale", Some(json!(900))).unwrap();
    s.hand_edit_profile(&p.id, "removed.key", Some(json!(1))).unwrap();
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.findings().len(), 2);
    s.rename_profile(&p.id, "Q", Mode::Apply, &person()).unwrap();
    assert_eq!(s.findings().len(), 2, "a rename leaves the stored values, so their findings");
    // Overwrite with a copy that carries a good ui.scale and quality.tier: the keys it writes replace the rejected
    // value; the unknown key it never touched stays in the file and in the findings.
    let src = s.save_profile("Src", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.switch_profile(Some(&src.id), Mode::Apply, &person()).unwrap();
    s.set::<UiScale>(&110, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Auto, Mode::Apply, &person()).unwrap();
    s.switch_profile(None, Mode::Apply, &person()).unwrap();
    let from = || ProfileSource::Profile(src.id.clone());
    s.save_profile("Q", from(), true, Mode::DryRun, &person()).unwrap();
    assert_eq!(s.findings().len(), 2, "a dry run leaves findings alone");
    let info = s.save_profile("Q", from(), true, Mode::Apply, &person()).unwrap();
    assert_eq!(info.keys, 2, "the profile reads the new values");
    let left = s.findings();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].error.key, "removed.key");
    s.reload(Mode::Apply, &person()).unwrap();
    let keys: Vec<_> = s.findings().iter().map(|f| f.error.key.clone()).collect();
    assert_eq!(keys, ["removed.key"], "the unknown key survived the overwrite");
    s.switch_profile(Some(&p.id), Mode::Apply, &person()).unwrap();
    assert_eq!((s.get::<UiScale>().unwrap(), s.get::<QualityTier>().unwrap()), (110, Tier::High));

    s.delete_profile(&p.id, Mode::DryRun, &person()).unwrap();
    assert_eq!(s.findings().len(), 1);
    s.delete_profile(&p.id, Mode::Apply, &person()).unwrap();
    assert!(s.findings().is_empty(), "the deleted file has no findings");
}

#[test]
fn overwriting_a_profile_keeps_a_rejected_value_it_did_not_write() {
    let s = MemorySettings::default();
    let p = s.save_profile("P", ProfileSource::Current, false, Mode::Apply, &person()).unwrap();
    s.hand_edit_profile(&p.id, "ui.scale", Some(json!(900))).unwrap();
    s.reload(Mode::Apply, &person()).unwrap();
    s.save_profile("P", ProfileSource::Current, true, Mode::Apply, &person()).unwrap();
    assert_eq!(s.findings().len(), 1);
    s.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(s.findings().len(), 1, "the rejected raw entry is still in the profile's file");
}
