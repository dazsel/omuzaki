//! `MemorySettings`: layering, validation, atomic profile switches, events, the change log and session state.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

use std::path::Path;

use omuzaki_ops_api::Actor;
use omuzaki_settings_api::error::SettingErrorKind;
use omuzaki_settings_api::event::ChangeCause;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::profile::ProfileSource;
use omuzaki_settings_api::service::{Layer, ListFilter, Settings, SettingsExt, Source};
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

check_crate!();

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
    let e = s.set_raw("ui.scale", json!("big"), Layer::Auto, &person()).unwrap_err();
    assert!(matches!(e.kind, SettingErrorKind::WrongType { .. }) && e.key == "ui.scale");
    let e = s.set::<UiScale>(&900, Layer::Auto, &person()).unwrap_err();
    assert!(e.to_string().contains("`ui.scale`") && matches!(e.kind, SettingErrorKind::OutOfRange { .. }));
    assert!(matches!(
        s.set_raw("nope.key", json!(1), Layer::Auto, &person()).unwrap_err().kind,
        SettingErrorKind::UnknownKey
    ));
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
    assert!(events.drain().is_empty() && s.changes(10).is_empty());
}

#[test]
fn a_set_emits_one_event_and_is_logged_with_its_actor() {
    let s = MemorySettings::default();
    let events = s.subscribe();
    let change = s.set::<UiScale>(&125, Layer::Auto, &agent()).unwrap();
    assert_eq!((change.before, change.after), (json!(100), json!(125)));
    let got = events.drain();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].cause, ChangeCause::Set);
    assert_eq!(got[0].change_of("ui.scale").unwrap().after, json!(125));
    assert_eq!(s.changes(5)[0].actor, agent());
    // Writing the same value again is no change and no event.
    s.set::<UiScale>(&125, Layer::Auto, &agent()).unwrap();
    assert!(events.drain().is_empty());
}

#[test]
fn reset_falls_back_and_says_when_there_was_nothing_to_reset() {
    let s = MemorySettings::default();
    assert_eq!(s.reset("ui.scale", Layer::Auto, &person()).unwrap(), None);
    s.set::<UiScale>(&125, Layer::Auto, &person()).unwrap();
    let c = s.reset("ui.scale", Layer::Auto, &person()).unwrap().unwrap();
    assert_eq!((c.before, c.after), (json!(125), json!(100)));
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
}

#[test]
fn a_profile_overlays_the_base_and_writes_go_to_the_active_profile() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, &person()).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Auto, &person()).unwrap();
    // "Current" captures what differs from base-or-default: here both keys (base = effective, so only defaults differ).
    let stream = s.save_profile("Stream", ProfileSource::Current, false, &person()).unwrap();
    assert_eq!(stream.keys, 0, "values equal to the base are omitted");
    s.switch_profile(Some(&stream.id), &person()).unwrap();
    s.set::<UiScale>(&90, Layer::Auto, &person()).unwrap();
    assert_eq!(s.resolve("ui.scale").unwrap().source, Source::Profile(stream.id.clone()));
    // An explicit base write hidden by the override is no change, and the base keeps it.
    let events = s.subscribe();
    s.set::<UiScale>(&110, Layer::Base, &person()).unwrap();
    assert!(events.drain().is_empty());
    s.switch_profile(None, &person()).unwrap();
    assert_eq!(s.get::<UiScale>().unwrap(), 110);
    // A machine key never goes to a profile even while one is active.
    s.switch_profile(Some(&stream.id), &person()).unwrap();
    s.set_raw("update.channel", json!("beta"), Layer::Auto, &person()).unwrap();
    assert_eq!(s.resolve("update.channel").unwrap().source, Source::Base);
}

#[test]
fn a_profile_switch_is_one_event_listing_only_what_changed_and_restarts_pending() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, &person()).unwrap();
    let stream = s.save_profile("Stream", ProfileSource::Current, false, &person()).unwrap();
    s.switch_profile(Some(&stream.id), &person()).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Auto, &person()).unwrap();
    s.set::<Density>(&2, Layer::Auto, &person()).unwrap();
    assert_eq!(s.profiles()[0].keys, 2);

    let events = s.subscribe();
    let report = s.switch_profile(None, &agent()).unwrap();
    let got = events.drain();
    assert_eq!(got.len(), 1, "one event for the whole switch");
    assert_eq!(got[0].cause, ChangeCause::ProfileSwitched { from: Some(stream.id.clone()), to: None });
    let keys: Vec<_> = got[0].changes.iter().map(|c| c.key.as_str()).collect();
    assert_eq!(keys, ["quality.tier", "ui.density"], "ui.scale is the same under both, so it is not listed");
    assert_eq!(report.changed, 2);
    assert_eq!(report.pending_restart, ["ui.density"]);
    assert_eq!(s.changes(10).len(), 3 + 2, "three sets, then the switch logs once per changed key");

    // Switching to the profile we are in does nothing.
    s.switch_profile(Some(&stream.id), &person()).unwrap();
    let _ = events.drain();
    assert_eq!(s.switch_profile(Some(&stream.id), &person()).unwrap().changed, 0);
    assert!(events.drain().is_empty());
}

#[test]
fn a_switch_to_an_unknown_profile_changes_nothing() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, &person()).unwrap();
    let before = s.snapshot();
    let events = s.subscribe();
    let ghost = omuzaki_settings_api::profile::ProfileId::new("ghost-0001").unwrap();
    let e = s.switch_profile(Some(&ghost), &person()).unwrap_err();
    assert!(matches!(e.kind, SettingErrorKind::UnknownProfile { .. }));
    assert_eq!(*s.snapshot(), *before);
    assert!(events.drain().is_empty() && s.active_profile().is_none());
}

#[test]
fn switching_there_and_back_restores_the_exact_snapshot() {
    let s = MemorySettings::default();
    s.set::<UiScale>(&125, Layer::Auto, &person()).unwrap();
    let p = s.save_profile("Alt", ProfileSource::Current, false, &person()).unwrap();
    s.switch_profile(Some(&p.id), &person()).unwrap();
    s.set::<UiScale>(&80, Layer::Auto, &person()).unwrap();
    s.set::<QualityTier>(&Tier::High, Layer::Auto, &person()).unwrap();
    let in_profile = s.snapshot();
    s.switch_profile(None, &person()).unwrap();
    s.switch_profile(Some(&p.id), &person()).unwrap();
    assert_eq!(*s.snapshot(), *in_profile);
}

#[test]
fn profile_names_ids_and_overwrite() {
    let s = MemorySettings::default();
    let a = s.save_profile("  Stream ", ProfileSource::Current, false, &person()).unwrap();
    assert_eq!(a.name, "Stream");
    assert!(a.id.as_str().starts_with("stream-"));
    let e = s.save_profile("Stream", ProfileSource::Current, false, &person()).unwrap_err();
    assert!(matches!(e.kind, SettingErrorKind::ProfileExists { .. }));
    assert_eq!(s.save_profile("Stream", ProfileSource::Current, true, &person()).unwrap().id, a.id);
    assert!(s.save_profile("", ProfileSource::Current, false, &person()).is_err());
    let jp = s.save_profile("配信", ProfileSource::Profile(a.id.clone()), false, &person()).unwrap();
    assert!(jp.id.as_str().starts_with("profile-"));
    let renamed = s.rename_profile(&jp.id, "Live").unwrap();
    assert_eq!((renamed.id, renamed.name.as_str()), (jp.id.clone(), "Live"));
    assert!(s.rename_profile(&jp.id, "Stream").is_err());
    assert_eq!(s.profiles().len(), 2);
}

#[test]
fn deleting_the_active_profile_switches_to_none_first() {
    let s = MemorySettings::default();
    let p = s.save_profile("Alt", ProfileSource::Current, false, &person()).unwrap();
    s.switch_profile(Some(&p.id), &person()).unwrap();
    s.set::<UiScale>(&80, Layer::Auto, &person()).unwrap();
    let events = s.subscribe();
    s.delete_profile(&p.id).unwrap();
    assert_eq!(s.active_profile(), None);
    assert_eq!(s.get::<UiScale>().unwrap(), 100);
    assert_eq!(events.drain().len(), 1);
    assert!(s.profiles().is_empty());
    assert!(s.delete_profile(&p.id).is_err());
}

#[test]
fn the_change_log_is_newest_first_and_capped() {
    let s = MemorySettings::default();
    for i in 0..250u16 {
        s.set::<UiScale>(&(75 + i % 100 + 1), Layer::Auto, &person()).unwrap();
    }
    let log = s.changes(1000);
    assert_eq!(log.len(), 200);
    assert_eq!(log[0].change.after, json!(75 + 249 % 100 + 1));
    assert_eq!(s.changes(3).len(), 3);
}

#[test]
fn session_state_is_kept_and_the_opening_guard_survives_until_opened() {
    let s = MemorySettings::default();
    let session = s.session();
    session.begin_open(Path::new("/p/a")).unwrap();
    assert_eq!(session.state().opening.as_deref(), Some(Path::new("/p/a")));
    session.opened(Path::new("/p/a"), "A");
    session.mark_version_seen("0.9.0");
    session.set_window("main", omuzaki_settings_api::session::WindowGeometry::new(0, 0, 800, 600));
    let state = session.state();
    assert!(state.opening.is_none() && state.recent.len() == 1 && state.windows.contains_key("main"));
    session.forget(Path::new("/p/a"));
    assert!(session.state().recent.is_empty());
}
