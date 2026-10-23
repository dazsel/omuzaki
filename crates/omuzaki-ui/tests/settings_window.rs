//! The Settings window (#948): generated from the registry, with three keys this test registers beside the studio's
//! own. Every key gets a row in its area's section; a key that waits for a restart and one that stays on this computer
//! say so; picking an option runs `settings.set` and the reset runs `settings.reset`, so the store holds what the row
//! shows; the search narrows the rows; an agent's change shows its name on the row; every control is named; Esc
//! closes it; and every word goes through the message table (the pseudo-locale).

use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_ops_api::{Actor, Budget, Grants, Limits, Mode, Request, SessionSpec, Transport};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::service::Settings as SettingsService;
use omuzaki_settings_api::setting::Setting;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{all_strings, harness_state, press};
use omuzaki_ui::ds::{Settings, set_strings};
use omuzaki_ui::settings::SettingsWindow;
use omuzaki_ui::wiring::Core;
use serde_json::{Value, json};

/// A key that waits for the next start and stays on this computer.
struct Restart;
impl Setting for Restart {
    type Value = bool;
    const META: SettingMeta = SettingMeta::new(
        "zz_test.restart_flag",
        "A test key that applies at the next start",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::NextStart,
        1,
    );
    fn default() -> bool {
        false
    }
}
register_setting!(Restart);

/// A short enum: a segmented control.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Pace {
    /// Slow.
    Slow,
    /// Fast.
    Fast,
}

struct Mode2;
impl Setting for Mode2 {
    type Value = Pace;
    const META: SettingMeta = SettingMeta::new(
        "zz_test.pace",
        "A test key with two options",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );
    fn default() -> Pace {
        Pace::Slow
    }
}
register_setting!(Mode2);

/// A number with a range: a slider.
struct Amount;
impl Setting for Amount {
    type Value = u8;
    const META: SettingMeta = SettingMeta::new(
        "zz_test.amount",
        "A test key with a number",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );
    fn default() -> u8 {
        3
    }
}
register_setting!(Amount);

struct Rig {
    window: Option<SettingsWindow>,
    core: Core,
    store: Arc<dyn SettingsService>,
    person: Actor,
    _folder: tempfile::TempDir,
}

fn rig(settings: Settings) -> Harness<'static, Rig> {
    omuzaki_ui::link();
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let paths = omuzaki_settings::Paths::new(folder.path().join("config"), folder.path().join("data"));
    let store: Arc<dyn SettingsService> = Arc::new(omuzaki_settings::Store::open(&paths));
    let services = omuzaki_ops_api::ServicesBuilder::new().insert::<dyn SettingsService>(Arc::clone(&store)).build();
    let core =
        omuzaki_ui::wiring::core_on(omuzaki_ui::wiring::Document::empty(), services).unwrap_or_else(|e| panic!("{e}"));
    let state = Rig {
        window: Some(SettingsWindow::new()),
        core,
        store,
        person: Actor::Person { name: "you".into() },
        _folder: folder,
    };
    let mut h = harness_state(settings, vec2(900.0, 1400.0), state, |ui, r| {
        let runner = Arc::clone(&r.core.runner);
        if let Some(window) = r.window.as_mut()
            && !window.show(ui.ctx(), &runner, &r.person)
        {
            r.window = None;
        }
    });
    h.run_steps(4);
    h
}

fn value(h: &Harness<'_, Rig>, key: &str) -> Value {
    h.state().store.resolve(key).map_or(Value::Null, |r| r.value)
}

/// Searches for `query`, as a person looking for one setting does, so its row is on screen.
fn search(h: &mut Harness<'_, Rig>, query: &str) {
    h.get_by_role_and_label(Role::TextInput, "Search settings").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Search settings").type_text(query);
    h.run_steps(3);
}

fn shows(h: &Harness<'_, Rig>, text: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(text))
}

#[test]
fn every_registered_key_but_a_page_owned_one_has_a_row_in_its_areas_section() {
    let h = rig(Settings::default());
    let listed = h.state().window.as_ref().map(SettingsWindow::listed).unwrap_or_default();
    // The Connectors page edits `connectors.entries` in words; the list does not show it as a raw map.
    let mut registered: Vec<&str> = omuzaki_settings_api::registry::registrations()
        .map(|r| r.meta.key)
        .filter(|key| *key != omuzaki_connect::settings::KEY)
        .collect();
    registered.sort_unstable();
    let mut shown = listed.clone();
    shown.sort_unstable();
    assert_eq!(shown, registered, "one row per registered key");
    for key in ["ui.theme", "ui.scale", "zz_test.pace", "zz_test.amount", "zz_test.restart_flag"] {
        assert!(listed.contains(&key), "{key} is listed");
    }
    assert!(shows(&h, "Appearance"), "the ui area is titled from its message");
    assert!(shows(&h, "Zz test"), "an area with no message reads as its name");
    assert!(shows(&h, "Restart flag"), "a row's label is its key's last segment in words");
    assert!(shows(&h, "A test key with two options"), "the summary is under the row");
}

#[test]
fn a_key_that_waits_for_a_restart_and_a_machine_key_say_so() {
    let h = rig(Settings::default());
    assert!(shows(&h, "Applies after restart"));
    assert!(shows(&h, "This computer"));
}

#[test]
fn picking_an_option_sets_it_through_the_op_and_reset_puts_the_default_back() {
    let mut h = rig(Settings::default());
    search(&mut h, "pace");
    h.get_by_role_and_label(Role::RadioButton, "Fast").click();
    h.run_steps(3);
    assert_eq!(value(&h, "zz_test.pace"), json!("fast"), "the store holds what the row shows");
    let records = h.state().core.runner.log(0, 100, &Grants::person()).unwrap_or_default();
    assert!(records.iter().any(|r| r.op == "settings.set"), "the change is an op in history");
    // The row's reset, beside its control.
    assert!(h.query_by_label_contains("Reset Pace").is_some(), "a modified row has a reset");
    h.get_by_role_and_label(Role::RadioButton, "Slow").click();
    h.run_steps(3);
    assert_eq!(value(&h, "zz_test.pace"), json!("slow"));
    let records = h.state().core.runner.log(0, 100, &Grants::person()).unwrap_or_default();
    assert!(records.iter().any(|r| r.op == "settings.reset"), "back to the default is a reset");
}

#[test]
fn the_theme_is_a_choice_of_three_named_in_words() {
    let mut h = rig(Settings::default());
    search(&mut h, "theme");
    for option in ["Dark", "Light", "High contrast"] {
        assert!(h.query_by_role_and_label(Role::RadioButton, option).is_some(), "{option} is an option");
    }
    h.get_by_role_and_label(Role::RadioButton, "Light").click();
    h.run_steps(3);
    assert_eq!(value(&h, "ui.theme"), json!("light"));
}

#[test]
fn the_search_narrows_the_rows() {
    let mut h = rig(Settings::default());
    search(&mut h, "restart");
    let listed = h.state().window.as_ref().map(SettingsWindow::listed).unwrap_or_default();
    assert_eq!(listed, ["zz_test.restart_flag"]);
    h.get_by_role_and_label(Role::TextInput, "Search settings").type_text("zzzz");
    h.run_steps(3);
    assert!(shows(&h, "No setting matches"), "an empty search says so");
}

#[test]
fn an_agents_change_shows_its_name_on_the_row() {
    let mut h = rig(Settings::default());
    let spec = SessionSpec {
        transport: Transport::Stdio,
        client: "designer".into(),
        model: None,
        on_behalf_of: Actor::Person { name: "you".into() },
        grants: Grants::agent_default(),
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: Vec::new(),
    };
    let agent = h.state().core.sessions.open(spec).unwrap_or_else(|e| panic!("{e}")).actor;
    let Actor::Agent { id, .. } = &agent else { panic!("an agent") };
    let id = id.clone();
    let request = Request {
        op: "settings.set".to_owned(),
        args: json!({"key": "zz_test.amount", "value": 7}),
        mode: Mode::Apply,
        actor: agent,
        grants: Grants::agent_default(),
        cause: None,
        tx: None,
        base: None,
    };
    h.state().core.runner.run(request).unwrap_or_else(|e| panic!("{e}"));
    // The window reads again once a second while it is open.
    h.run_steps(70);
    assert!(shows(&h, "Last changed by"), "the row says an agent changed it");
    assert!(shows(&h, &id), "and which one");
}

#[test]
fn every_control_is_named_and_esc_closes_the_window() {
    let mut h = rig(Settings::default());
    assert!(h.query_by_role_and_label(Role::TextInput, "Search settings").is_some());
    for node in h.query_all_by_role(Role::Button).chain(h.query_all_by_role(Role::RadioButton)) {
        let label = node.accesskit_node().label().unwrap_or_default();
        assert!(!label.trim().is_empty(), "an unnamed control: {node:?}");
    }
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().window.is_none(), "Esc closed it");
}

#[test]
fn every_word_goes_through_the_message_table() {
    let mut h = rig(Settings::default());
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(70);
    let english = [
        "Settings",
        "Search settings",
        "Appearance",
        "Applies after restart",
        "This computer",
        "Restart flag",
        "A test key with two options",
    ];
    let all = all_strings(&h);
    for word in english {
        assert!(!all.iter().any(|s| s == word), "{word} was drawn in English under the pseudo-locale");
    }
}

/// Screenshots of the window for the pull request: dark, light and high contrast at 100%, dark at 75% and 200%, and
/// the pseudo-locale. Run with `OMUZAKI_EVIDENCE_OUT=<dir> cargo test -p omuzaki-ui --test settings_window
/// settings_evidence -- --ignored` (software Vulkan).
#[test]
#[ignore = "writes screenshots; run by hand for a pull request"]
fn settings_evidence() {
    use omuzaki_ui::ds::{Scale, ThemeKind, apply_settings};
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let shots = [
        ("dark", Settings::default(), false),
        ("light", Settings { theme: ThemeKind::Light, ..Settings::default() }, false),
        ("high-contrast", Settings { theme: ThemeKind::HighContrast, ..Settings::default() }, false),
        ("dark-75", Settings { scale: Scale::nearest(75), ..Settings::default() }, false),
        ("dark-200", Settings { scale: Scale::nearest(200), ..Settings::default() }, false),
        ("pseudo", Settings::default(), true),
    ];
    for (name, settings, pseudo) in shots {
        omuzaki_ui::link();
        let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let paths = omuzaki_settings::Paths::new(folder.path().join("config"), folder.path().join("data"));
        let store: Arc<dyn SettingsService> = Arc::new(omuzaki_settings::Store::open(&paths));
        let services =
            omuzaki_ops_api::ServicesBuilder::new().insert::<dyn SettingsService>(Arc::clone(&store)).build();
        let core = omuzaki_ui::wiring::core_on(omuzaki_ui::wiring::Document::empty(), services)
            .unwrap_or_else(|e| panic!("{e}"));
        let person = Actor::Person { name: "you".into() };
        let mut window = SettingsWindow::new();
        let mut h = Harness::builder().with_size(vec2(900.0, 720.0)).wgpu().build_ui(move |ui| {
            apply_settings(ui.ctx(), settings);
            egui::CentralPanel::default().show(ui, |_| {});
            window.show(ui.ctx(), &core.runner, &person);
        });
        if pseudo {
            set_strings(&h.ctx, Arc::new(Pseudo(English)));
        }
        h.run_steps(8);
        let image = h.render().unwrap_or_else(|e| panic!("renders: {e}"));
        let path = format!("{out}/settings-{name}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
        drop(store);
    }
}
