//! The `settings.*` ops over a real store in a temporary folder: the agent rule of each `AgentWrite` for an agent and a
//! person, a dry run that changes nothing, a list that is the registry, the inverse calls that undo a set and a reset,
//! near-miss keys, and a reload that only a person may run.

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, ErrorDetail, OpCtx, OpError, Plan, Services, ServicesBuilder, find};
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::registry::SettingRegistration;
use omuzaki_settings_api::service::Settings;
use omuzaki_settings_api::setting::Setting;
use omuzaki_settings_mock::MemorySettings;
use serde_json::{Value, json};

use crate::{Paths, Store};

macro_rules! setting {
    ($name:ident, $key:literal, $ty:ty, $default:expr, $agent:expr, $apply:expr) => {
        struct $name;
        impl Setting for $name {
            type Value = $ty;
            const META: SettingMeta =
                SettingMeta::new($key, "a test setting", SettingScope::User, $agent, InProfile::Member, $apply, 1);
            fn default() -> $ty {
                $default
            }
            fn check(value: &$ty) -> Result<(), SettingError> {
                check(Self::META.key, value)
            }
        }
    };
}

/// Only `ui.scale` has a rule: 75 to 200.
fn check<T: serde::Serialize>(key: &str, value: &T) -> Result<(), SettingError> {
    match (key, serde_json::to_value(value).ok().and_then(|v| v.as_u64())) {
        ("ui.scale", Some(n)) => SettingError::range(key, n, 75, 200),
        _ => Ok(()),
    }
}

setting!(Theme, "ui.theme", String, "dark".to_owned(), AgentWrite::Free, Apply::Live);
setting!(Scale, "ui.scale", u16, 100, AgentWrite::Free, Apply::Live);
setting!(Window, "output.window", String, "off".to_owned(), AgentWrite::Confirm, Apply::Live);
setting!(Telemetry, "privacy.telemetry", bool, false, AgentWrite::Never, Apply::NextStart);

static THEME: SettingRegistration = SettingRegistration::of::<Theme>("test");
static SCALE: SettingRegistration = SettingRegistration::of::<Scale>("test");
static WINDOW: SettingRegistration = SettingRegistration::of::<Window>("test");
static TELEMETRY: SettingRegistration = SettingRegistration::of::<Telemetry>("test");

struct Rig {
    store: Arc<Store>,
    services: Services,
    _folder: tempfile::TempDir,
}

fn rig() -> Rig {
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let paths = Paths::new(folder.path().join("config"), folder.path().join("data"));
    let store = Arc::new(Store::open_with(&paths, [&THEME, &SCALE, &WINDOW, &TELEMETRY]));
    let services = ServicesBuilder::new().insert::<dyn Settings>(Arc::clone(&store) as Arc<dyn Settings>).build();
    Rig { store, services, _folder: folder }
}

fn person() -> Actor {
    Actor::Person { name: "kit".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "designer".into(), model: None }
}

fn plan(services: &Services, actor: &Actor, name: &str, args: Value, dry_run: bool) -> Result<Plan, OpError> {
    let op = (find(name).unwrap_or_else(|| panic!("{name} is registered")).decode)(args)?;
    let (doc, clock) = (MemDoc::new(), FakeClock::new(Seconds(0.0)));
    let ctx = OpCtx::new(&doc, actor, &clock).with_services(services).with_dry_run(dry_run);
    op.plan(&ctx)
}

fn run(rig: &Rig, actor: &Actor, name: &str, args: Value) -> Result<Value, OpError> {
    plan(&rig.services, actor, name, args, false).map(|p| p.output.unwrap_or(Value::Null))
}

fn value(rig: &Rig, key: &str) -> Value {
    rig.store.resolve(key).map_or(Value::Null, |r| r.value)
}

fn detail(error: &OpError) -> Option<&ErrorDetail> {
    error.detail.as_deref()
}

#[test]
fn a_free_key_is_written_by_an_agent_and_a_person() {
    let rig = rig();
    run(&rig, &agent(), "settings.set", json!({"key": "ui.theme", "value": "light"})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "ui.theme"), json!("light"));
    run(&rig, &person(), "settings.set", json!({"key": "ui.theme", "value": "dark"})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "ui.theme"), json!("dark"));
}

#[test]
fn a_confirm_key_waits_for_a_person_when_an_agent_writes_it() {
    let rig = rig();
    let refused = run(&rig, &agent(), "settings.set", json!({"key": "output.window", "value": "on"}));
    let error = refused.err().unwrap_or_else(|| panic!("an agent's write waits"));
    assert!(matches!(detail(&error), Some(ErrorDetail::NeedsPerson { .. })), "{error:?}");
    assert_eq!(value(&rig, "output.window"), json!("off"), "nothing was written while it waits");
    let dry = plan(&rig.services, &agent(), "settings.set", json!({"key": "output.window", "value": "on"}), true);
    assert!(dry.is_err(), "a dry run shows the same question");
    run(&rig, &person(), "settings.set", json!({"key": "output.window", "value": "on"}))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "output.window"), json!("on"));
}

#[test]
fn a_never_key_is_refused_to_an_agent_and_written_by_a_person() {
    let rig = rig();
    for name in ["settings.set", "settings.reset"] {
        let args = if name == "settings.set" {
            json!({"key": "privacy.telemetry", "value": true})
        } else {
            json!({"key": "privacy.telemetry"})
        };
        let error = run(&rig, &agent(), name, args).err().unwrap_or_else(|| panic!("{name} is refused"));
        assert_eq!(error.code, ErrorCode::Denied);
        assert!(matches!(detail(&error), Some(ErrorDetail::PersonOnly { .. })), "{error:?}");
    }
    let done = run(&rig, &person(), "settings.set", json!({"key": "privacy.telemetry", "value": true}))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((done["changed"].as_bool(), done["apply"].as_str()), (Some(true), Some("next_start")));
    assert_eq!(value(&rig, "privacy.telemetry"), json!(true));
}

#[test]
fn a_dry_run_validates_and_answers_but_changes_nothing() {
    let rig = rig();
    let dry = plan(&rig.services, &agent(), "settings.set", json!({"key": "ui.scale", "value": 150}), true)
        .unwrap_or_else(|e| panic!("{e}"));
    let answer = dry.output.unwrap_or_default();
    assert_eq!((answer["before"].clone(), answer["after"].clone()), (json!(100), json!(150)));
    assert_eq!(answer["applied"], json!(false));
    assert_eq!(value(&rig, "ui.scale"), json!(100), "the dry run wrote nothing");
    assert!(rig.store.changes(10).is_empty(), "and logged nothing");
    let bad = plan(&rig.services, &agent(), "settings.set", json!({"key": "ui.scale", "value": 900}), true);
    let error = bad.err().unwrap_or_else(|| panic!("900 is out of range"));
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("value")));
}

#[test]
fn a_wrong_type_names_the_value_and_an_unknown_key_suggests_the_nearest() {
    let rig = rig();
    let error = run(&rig, &person(), "settings.set", json!({"key": "ui.scale", "value": "big"}))
        .err()
        .unwrap_or_else(|| panic!("a string is not a scale"));
    assert_eq!(error.field.as_deref(), Some("value"));
    let error =
        run(&rig, &person(), "settings.get", json!({"key": "ui.scal"})).err().unwrap_or_else(|| panic!("no such key"));
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.hint.as_deref().is_some_and(|h| h.contains("`ui.scale`")), "{error:?}");
}

#[test]
fn list_is_the_registry_and_get_says_where_a_value_came_from() {
    let rig = rig();
    let listed = run(&rig, &agent(), "settings.list", json!({})).unwrap_or_else(|e| panic!("{e}"));
    let keys: Vec<&str> =
        listed["settings"].as_array().into_iter().flatten().filter_map(|s| s["key"].as_str()).collect();
    assert_eq!(keys, ["output.window", "privacy.telemetry", "ui.scale", "ui.theme"]);
    run(&rig, &person(), "settings.set", json!({"key": "ui.scale", "value": 125})).unwrap_or_else(|e| panic!("{e}"));
    let ui =
        run(&rig, &agent(), "settings.list", json!({"area": "ui", "changed": true})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(ui["settings"].as_array().map(Vec::len), Some(1));
    let got = run(&rig, &agent(), "settings.get", json!({"key": "ui.scale"})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!((got["value"].clone(), got["default"].clone()), (json!(125), json!(100)));
    assert_eq!((got["modified"].as_bool(), got["agent"].as_str()), (Some(true), Some("free")));
    assert_eq!(got["source"], json!({"from": "base"}));
    let changes = run(&rig, &agent(), "settings.changes", json!({"limit": 5})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(changes["changes"][0]["change"]["key"], json!("ui.scale"));
    assert!(run(&rig, &agent(), "settings.changes", json!({"limit": 0})).is_err(), "a limit of 0 is refused");
}

#[test]
fn a_set_is_undone_by_a_reset_from_the_default_and_by_a_set_after_that() {
    let rig = rig();
    let first = plan(&rig.services, &person(), "settings.set", json!({"key": "ui.scale", "value": 125}), false)
        .unwrap_or_else(|e| panic!("{e}"));
    let effect = first.effect.unwrap_or_else(|| panic!("a change records its inverse"));
    assert_eq!((effect.undo.op.as_str(), effect.redo.op.as_str()), ("settings.reset", "settings.set"));
    let second = plan(&rig.services, &person(), "settings.set", json!({"key": "ui.scale", "value": 150}), false)
        .unwrap_or_else(|e| panic!("{e}"));
    let effect = second.effect.unwrap_or_else(|| panic!("a change records its inverse"));
    assert_eq!(effect.undo.op, "settings.set");
    assert_eq!(effect.undo.args["value"], json!(125));
    // Running the undo call puts it back.
    run(&rig, &person(), &effect.undo.op, effect.undo.args.clone()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "ui.scale"), json!(125));
    let reset = plan(&rig.services, &person(), "settings.reset", json!({"key": "ui.scale"}), false)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "ui.scale"), json!(100));
    let effect = reset.effect.unwrap_or_else(|| panic!("a reset records its inverse"));
    assert_eq!((effect.undo.op.as_str(), effect.undo.args["value"].clone()), ("settings.set", json!(125)));
    let same = plan(&rig.services, &person(), "settings.set", json!({"key": "ui.scale", "value": 100}), false)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(same.effect.is_none(), "no change, nothing to undo");
}

#[test]
fn reload_is_a_persons_alone() {
    let rig = rig();
    let error = run(&rig, &agent(), "settings.reload", json!({})).err().unwrap_or_else(|| panic!("refused"));
    assert!(matches!(detail(&error), Some(ErrorDetail::PersonOnly { .. })), "{error:?}");
    let done = run(&rig, &person(), "settings.reload", json!({})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(done["changed"], json!(0));
}

#[test]
fn the_ops_work_over_the_mock_service_too() {
    let mock: Arc<dyn Settings> = Arc::new(MemorySettings::with_clock(Arc::new(FakeClock::new(Seconds(0.0)))));
    let services = ServicesBuilder::new().insert::<dyn Settings>(mock).build();
    let listed = plan(&services, &agent(), "settings.list", json!({}), false).unwrap_or_else(|e| panic!("{e}"));
    assert!(listed.output.is_some_and(|o| o["settings"].is_array()));
}

/// Two recent projects, `b` the newer and the last one open.
fn with_recent(rig: &Rig) {
    let session = rig.store.session();
    for (path, name) in [("/shows/a", "A"), ("/shows/b", "B")] {
        session.begin_open(std::path::Path::new(path)).unwrap_or_else(|e| panic!("{e}"));
        session.opened(std::path::Path::new(path), name).unwrap_or_else(|e| panic!("{e}"));
    }
}

#[test]
fn the_session_ops_list_recent_newest_first_and_read_the_whole_state() {
    let rig = rig();
    with_recent(&rig);
    let recent = run(&rig, &agent(), "session.recent", json!({})).unwrap_or_else(|e| panic!("{e}"));
    let names: Vec<&str> =
        recent["recent"].as_array().into_iter().flatten().filter_map(|r| r["name"].as_str()).collect();
    assert_eq!(names, ["B", "A"]);
    assert_eq!(recent["last_project"], json!("/shows/b"));
    let state = run(&rig, &agent(), "session.get", json!({})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(state["opening"], Value::Null, "an open that finished leaves no guard");
    assert_eq!(state["recent"].as_array().map(Vec::len), Some(2));
}

#[test]
fn forgetting_a_recent_project_is_a_persons_call_that_its_undo_puts_back_in_place() {
    let rig = rig();
    with_recent(&rig);
    let refused = run(&rig, &agent(), "session.forget", json!({"path": "/shows/b"}));
    assert_eq!(refused.err().map(|e| e.code), Some(ErrorCode::Denied));

    let dry = plan(&rig.services, &person(), "session.forget", json!({"path": "/shows/b"}), true)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(dry.effect.is_some());
    assert_eq!(rig.store.session().state().recent.len(), 2, "a dry run forgets nothing");

    let done = plan(&rig.services, &person(), "session.forget", json!({"path": "/shows/b"}), false)
        .unwrap_or_else(|e| panic!("{e}"));
    let state = rig.store.session().state();
    assert_eq!(state.recent.len(), 1);
    assert_eq!(state.last_project, None, "it was the last project");
    let undo = done.effect.unwrap_or_else(|| panic!("an undo")).undo;
    assert_eq!(undo.op, "session.restore_recent");
    run(&rig, &person(), &undo.op, undo.args).unwrap_or_else(|e| panic!("{e}"));
    let back = rig.store.session().state();
    assert_eq!(back.recent[0].name, "B", "back at the top");
    assert_eq!(back.last_project.as_deref(), Some(std::path::Path::new("/shows/b")));

    let unknown =
        run(&rig, &person(), "session.forget", json!({"path": "/shows/none"})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(unknown["forgotten"], json!(false));
}

#[test]
fn the_store_keeps_the_session_in_the_data_folder_across_a_restart() {
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let paths = Paths::new(folder.path().join("config"), folder.path().join("data"));
    let store = Store::open_with(&paths, [&THEME]);
    store.session().opened(std::path::Path::new("/shows/a"), "A").unwrap_or_else(|e| panic!("{e}"));
    drop(store);
    #[allow(clippy::disallowed_methods, reason = "the test checks the store's own file")]
    let kept = folder.path().join("data").join("session.toml").is_file();
    assert!(kept);
    let again = Store::open_with(&paths, [&THEME]);
    assert_eq!(again.session().state().recent[0].name, "A");
}

fn profile_id(rig: &Rig, name: &str) -> String {
    let listed = run(rig, &agent(), "settings.profile.list", json!({})).unwrap_or_else(|e| panic!("{e}"));
    listed["profiles"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| p["name"] == name)
        .and_then(|p| p["id"].as_str())
        .unwrap_or_else(|| panic!("no profile {name}: {listed}"))
        .to_owned()
}

#[test]
fn a_profile_saves_what_differs_switches_in_one_event_and_takes_the_tweaks_made_under_it() {
    let rig = rig();
    run(&rig, &person(), "settings.set", json!({"key": "ui.theme", "value": "light"}))
        .unwrap_or_else(|e| panic!("{e}"));
    // Saved from the base: nothing differs from what the base gives, so the profile starts empty.
    run(&rig, &person(), "settings.profile.save", json!({"name": "Stream"})).unwrap_or_else(|e| panic!("{e}"));
    let stream = profile_id(&rig, "Stream");
    assert!(stream.starts_with("stream-") && stream.len() == "stream-".len() + 4, "{stream}");

    let events = rig.store.subscribe();
    let switched = plan(&rig.services, &person(), "settings.profile.switch", json!({"profile": stream}), false)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(switched.output.as_ref().map(|o| o["changed"].clone()), Some(json!(0)));
    assert_eq!(events.drain().len(), 1, "a switch is one event, even when no value changes");

    // Under the profile a tweak goes into it; the base keeps its value.
    run(&rig, &person(), "settings.set", json!({"key": "ui.scale", "value": 150})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "ui.scale"), json!(150));
    let resolved = rig.store.resolve("ui.scale").unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(resolved.source, omuzaki_settings_api::service::Source::Profile(_)), "{:?}", resolved.source);

    drop(events.drain());
    // Switching back to none puts the base in effect, in one event listing only what changed; its undo switches back.
    let off = plan(&rig.services, &person(), "settings.profile.switch", json!({"profile": null}), false)
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "ui.scale"), json!(100));
    let batch = events.drain();
    assert_eq!(batch.len(), 1);
    assert_eq!(batch[0].changes.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["ui.scale"]);
    let undo = off.effect.unwrap_or_else(|| panic!("a switch is undoable")).undo;
    run(&rig, &person(), &undo.op, undo.args).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "ui.scale"), json!(150));

    // A reset under the profile removes its override: the key falls back to the base.
    run(&rig, &person(), "settings.reset", json!({"key": "ui.scale"})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(value(&rig, "ui.scale"), json!(100));
}

#[test]
fn saving_current_captures_an_override_equal_to_the_default_when_the_base_differs() {
    let rig = rig();
    run(&rig, &person(), "settings.set", json!({"key": "ui.scale", "value": 125})).unwrap_or_else(|e| panic!("{e}"));
    run(&rig, &person(), "settings.profile.save", json!({"name": "Base"})).unwrap_or_else(|e| panic!("{e}"));
    let base = profile_id(&rig, "Base");
    run(&rig, &person(), "settings.profile.switch", json!({"profile": base})).unwrap_or_else(|e| panic!("{e}"));
    run(&rig, &person(), "settings.set", json!({"key": "ui.scale", "value": 100})).unwrap_or_else(|e| panic!("{e}"));
    // Default 100, base 125, override 100: saving current keeps `scale = 100`.
    run(&rig, &person(), "settings.profile.save", json!({"name": "Small"})).unwrap_or_else(|e| panic!("{e}"));
    let small = rig.store.profiles().into_iter().find(|p| p.name == "Small").unwrap_or_else(|| panic!("saved"));
    assert_eq!(small.keys, 1);
    let taken = run(&rig, &person(), "settings.profile.save", json!({"name": "Small"}));
    assert_eq!(taken.err().map(|e| e.code), Some(ErrorCode::Conflict));
    run(&rig, &person(), "settings.profile.save", json!({"name": "Small", "overwrite": true}))
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn an_agents_profile_change_waits_for_a_person_and_a_dry_run_changes_nothing() {
    let rig = rig();
    run(&rig, &person(), "settings.profile.save", json!({"name": "Stream"})).unwrap_or_else(|e| panic!("{e}"));
    let stream = profile_id(&rig, "Stream");
    let waits = run(&rig, &agent(), "settings.profile.switch", json!({"profile": stream}));
    let error = waits.err().unwrap_or_else(|| panic!("an agent's switch waits"));
    assert!(matches!(detail(&error), Some(ErrorDetail::NeedsPerson { .. })), "{error:?}");
    plan(&rig.services, &person(), "settings.profile.switch", json!({"profile": stream}), true)
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(rig.store.profiles().iter().all(|p| !p.active), "a dry run switches nothing");
    let unknown = run(&rig, &person(), "settings.profile.switch", json!({"profile": "nope-0000"}));
    assert_eq!(unknown.err().map(|e| e.code), Some(ErrorCode::NotFound));
}

#[test]
fn profiles_and_the_active_one_survive_a_restart_and_deleting_the_active_one_switches_to_none() {
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let paths = Paths::new(folder.path().join("config"), folder.path().join("data"));
    let open = || {
        let store = Arc::new(Store::open_with(&paths, [&THEME, &SCALE, &WINDOW, &TELEMETRY]));
        let services = ServicesBuilder::new().insert::<dyn Settings>(Arc::clone(&store) as Arc<dyn Settings>).build();
        (store, services)
    };
    let (store, services) = open();
    let r = Rig { store, services, _folder: tempfile::tempdir().unwrap_or_else(|e| panic!("{e}")) };
    run(&r, &person(), "settings.profile.save", json!({"name": "Stream"})).unwrap_or_else(|e| panic!("{e}"));
    let stream = profile_id(&r, "Stream");
    run(&r, &person(), "settings.profile.switch", json!({"profile": stream})).unwrap_or_else(|e| panic!("{e}"));
    run(&r, &person(), "settings.set", json!({"key": "ui.scale", "value": 175})).unwrap_or_else(|e| panic!("{e}"));
    run(&r, &person(), "settings.profile.rename", json!({"profile": stream, "name": "配信"}))
        .unwrap_or_else(|e| panic!("{e}"));
    drop(r);

    #[allow(clippy::disallowed_methods, reason = "the test reads the store's own profile file")]
    let file = std::fs::read_to_string(folder.path().join("config/profiles").join(format!("{stream}.toml")))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(file.contains("配信") && file.contains("scale = 175"), "{file}");
    let (store, services) = open();
    let r = Rig { store, services, _folder: tempfile::tempdir().unwrap_or_else(|e| panic!("{e}")) };
    assert_eq!(value(&r, "ui.scale"), json!(175), "the active profile is still active");
    let deleted =
        run(&r, &person(), "settings.profile.delete", json!({"profile": stream})).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(deleted["changed"], json!(1));
    assert_eq!(value(&r, "ui.scale"), json!(100));
    assert_eq!(r.store.profiles().len(), 0);
}
