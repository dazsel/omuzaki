//! The OBS Studio half of the Settings window's Connectors page (#1030, BR-19) over the real `obs.*` and `settings.*`
//! ops, a loopback fake OBS and an in-memory keychain: without a bridge the page says why; disconnected, it shows the
//! steps, the port from the settings and the password field; the password is saved to the keychain with the keyboard,
//! never shown or exposed to assistive technology, the field empties and Forget deletes it; a person connects with
//! the keyboard alone, the port they typed is kept as a setting, and OBS's scenes show with the one on program, which
//! follows OBS; a missing password says what to do; a remote OBS sits behind its warning. Every control is named, the
//! page draws no image, the footer says Omuzaki is not affiliated with the OBS Project, and every word goes through
//! the message table.

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_obs::service::ObsService;
use omuzaki_obs::testing::{FakeConfig, FakeObs};
use omuzaki_ops_api::{Actor, ServicesBuilder};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
use omuzaki_secrets_mock::MemorySecrets;
use omuzaki_settings_api::service::{Settings as SettingsService, SettingsExt as _};
use omuzaki_settings_mock::MemorySettings;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{all_strings, harness_state, press, press_for};
use omuzaki_ui::ds::{Settings, set_strings};
use omuzaki_ui::settings::connectors::Keychain;
use omuzaki_ui::settings::{Page, SettingsWindow};
use omuzaki_ui::wiring::Core;

/// A password used only by these tests.
const PASSWORD: &str = "test-only-password";

struct Rig {
    window: Option<SettingsWindow>,
    core: Core,
    person: Actor,
    secrets: Arc<MemorySecrets>,
    settings: Arc<MemorySettings>,
}

fn label() -> SecretLabel {
    SecretLabel::new("obs/websocket").unwrap_or_else(|e| panic!("{e}"))
}

fn services(bridge: bool, secrets: &Arc<MemorySecrets>, settings: &Arc<MemorySettings>) -> omuzaki_ops_api::Services {
    let mut services = ServicesBuilder::new()
        .insert::<dyn SecretStore>(Arc::clone(secrets) as Arc<dyn SecretStore>)
        .insert::<dyn SettingsService>(Arc::clone(settings) as Arc<dyn SettingsService>);
    if bridge {
        services = services.insert::<ObsService>(Arc::new(ObsService::new()));
    }
    services.build()
}

fn state(bridge: bool) -> Rig {
    omuzaki_ui::link();
    let secrets = Arc::new(MemorySecrets::new());
    let settings = Arc::new(MemorySettings::with_clock(Arc::new(FakeClock::new(Seconds(0.0)))));
    let core =
        omuzaki_ui::wiring::core_on(omuzaki_ui::wiring::Document::empty(), services(bridge, &secrets, &settings))
            .unwrap_or_else(|e| panic!("{e}"));
    let keychain = Keychain(Arc::clone(&secrets) as Arc<dyn SecretStore>);
    let window = SettingsWindow::on(Page::Connectors).with_keychain(Some(keychain));
    Rig { window: Some(window), core, person: Actor::Person { name: "you".into() }, secrets, settings }
}

/// The Settings window open on Connectors (OBS Studio is the first app), with the bridge when `bridge`.
fn rig(bridge: bool) -> Harness<'static, Rig> {
    let mut h = harness_state(Settings::default(), vec2(900.0, 1800.0), state(bridge), draw);
    h.run_steps(4);
    h
}

fn draw(ui: &mut egui::Ui, r: &mut Rig) {
    let runner = Arc::clone(&r.core.runner);
    if let Some(window) = r.window.as_mut()
        && !window.show(ui.ctx(), &runner, &r.person)
    {
        r.window = None;
    }
}

fn shows(h: &Harness<'_, Rig>, text: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(text))
}

fn obs_state(h: &mut Harness<'_, Rig>) -> Option<String> {
    h.state_mut().window.as_mut().and_then(|w| w.connectors().obs().state().map(str::to_owned))
}

/// Runs frames (a second of egui's clock each round, so the page reads again) until `text` shows.
fn until_shows(h: &mut Harness<'_, Rig>, text: &str) {
    let started = Instant::now();
    while !shows(h, text) {
        assert!(started.elapsed() < Duration::from_secs(15), "{text:?} never showed: {:#?}", all_strings(h));
        std::thread::sleep(Duration::from_millis(20));
        h.run_steps(65);
    }
}

fn focused(h: &Harness<'_, Rig>) -> Option<String> {
    h.root().children_recursive().find(|n| n.accesskit_node().is_focused()).and_then(|n| n.accesskit_node().label())
}

/// Focuses the control named `label` and presses `key`.
fn press_on(h: &mut Harness<'_, Rig>, label: &str, key: Key) {
    h.get_by_label(label).focus();
    h.run();
    assert_eq!(focused(h).as_deref(), Some(label));
    // A fixed number of frames: while a connect runs the page repaints until it answers.
    press_for(h, Modifiers::NONE, key, 4);
}

/// Focuses the field named `label`, selects what it holds and types `text` over it.
fn type_into(h: &mut Harness<'_, Rig>, label: &str, text: &str) {
    h.get_by_label(label).focus();
    h.run();
    press(h, Modifiers::COMMAND, Key::A);
    h.event(Event::Text(text.to_owned()));
    h.run_steps(2);
}

fn every_control_is_named_and_no_image_is_drawn(h: &Harness<'_, Rig>) {
    let roles = [Role::Button, Role::TextInput, Role::PasswordInput, Role::Switch, Role::CheckBox];
    for node in roles.into_iter().flat_map(|role| h.query_all_by_role(role)) {
        let label = node.accesskit_node().label().unwrap_or_default();
        assert!(!label.trim().is_empty(), "an unnamed control: {node:?}");
    }
    assert_eq!(h.query_all_by_role(Role::Image).count(), 0, "no logo or image of OBS's");
    assert!(shows(h, "Not affiliated with or endorsed by the OBS Project."), "the footer");
}

/// Types the password and saves it with the keyboard.
fn save_password(h: &mut Harness<'_, Rig>) {
    type_into(h, "Password", PASSWORD);
    press_on(h, "Save password", Key::Enter);
}

#[test]
fn without_the_bridge_the_page_says_why() {
    let h = rig(false);
    assert!(shows(&h, "The OBS bridge isn't in this build"), "{:#?}", all_strings(&h));
    every_control_is_named_and_no_image_is_drawn(&h);
}

#[test]
fn disconnected_it_shows_the_steps_the_port_and_the_password_field() {
    let mut h = rig(true);
    assert_eq!(obs_state(&mut h).as_deref(), Some("disconnected"));
    for text in [
        "Not connected",
        "1. In OBS, open Tools, then WebSocket Server Settings",
        "2. Turn on Enable WebSocket server",
        "3. Click Show Connect Info for the port and the password",
        "4. Enter them here, then Connect",
    ] {
        assert!(shows(&h, text), "the page shows {text:?}: {:#?}", all_strings(&h));
    }
    assert!(shows(&h, "4455"), "the port comes from obs.port's default");
    assert!(h.query_by_role_and_label(Role::PasswordInput, "Password").is_some(), "a masked field");
    assert!(!shows(&h, "Password saved"));
    every_control_is_named_and_no_image_is_drawn(&h);
}

#[test]
fn the_password_goes_to_the_keychain_and_is_never_shown_or_read_back() {
    let mut h = rig(true);
    type_into(&mut h, "Password", PASSWORD);
    assert!(!shows(&h, PASSWORD), "the typed password is not in the AccessKit tree: {:#?}", all_strings(&h));
    press_on(&mut h, "Save password", Key::Enter);
    let stored = h.state().secrets.get(&label()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(stored.as_ref().map(SecretValue::expose), Some(PASSWORD), "kept in the keychain under obs/websocket");
    assert!(shows(&h, "Password saved"), "{:#?}", all_strings(&h));
    assert!(!shows(&h, PASSWORD));
    let field = h.get_by_role_and_label(Role::PasswordInput, "Password");
    assert!(field.accesskit_node().value().unwrap_or_default().is_empty(), "the field is empty after Save");

    press_on(&mut h, "Forget", Key::Enter);
    assert!(h.state().secrets.get(&label()).unwrap_or_else(|e| panic!("{e}")).is_none(), "Forget deletes it");
    assert!(!shows(&h, "Password saved"));
}

#[test]
fn a_person_connects_with_the_keyboard_and_sees_the_scenes() {
    let mut fake = FakeObs::start(FakeConfig { password: Some(PASSWORD.into()), ..FakeConfig::default() });
    let mut h = rig(true);
    save_password(&mut h);
    type_into(&mut h, "Port", &fake.port().to_string());
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert_eq!(focused(&h).as_deref(), Some("Connect"), "Tab goes from the port to Connect");
    press_for(&mut h, Modifiers::NONE, Key::Enter, 4);
    until_shows(&mut h, "Connected: OBS WebSocket");
    assert_eq!(obs_state(&mut h).as_deref(), Some("connected"));
    let kept = h.state().settings.get::<omuzaki_obs::settings::Port>().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(kept, fake.port(), "the port the person typed is kept as obs.port");
    for scene in ["Main", "Starting soon", "BRB", "On program"] {
        assert!(shows(&h, scene), "{scene:?}: {:#?}", all_strings(&h));
    }
    assert!(!shows(&h, "1. In OBS"), "the steps go once connected");
    every_control_is_named_and_no_image_is_drawn(&h);

    // The program scene follows OBS (read only here).
    fake.set_program_scene("BRB");
    h.run_steps(70);
    let row = h.get_by_label("BRB").rect();
    let chip = h.get_by_label_contains("On program").rect();
    assert!((row.center().y - chip.center().y).abs() < 8.0, "the chip sits on BRB's row: {row:?} {chip:?}");

    press_on(&mut h, "Disconnect", Key::Enter);
    assert_eq!(obs_state(&mut h).as_deref(), Some("disconnected"));
    fake.down();
}

#[test]
fn a_missing_password_says_what_to_do() {
    let fake = FakeObs::start(FakeConfig { password: Some(PASSWORD.into()), ..FakeConfig::default() });
    let mut h = rig(true);
    type_into(&mut h, "Port", &fake.port().to_string());
    press_on(&mut h, "Connect", Key::Enter);
    until_shows(&mut h, "Wrong or missing password");
    assert!(shows(&h, "Enter OBS's WebSocket password, Save it, then Connect"), "{:#?}", all_strings(&h));
    assert!(h.query_by_role_and_label(Role::Button, "Connect").is_some(), "the person connects again");
}

#[test]
fn a_remote_obs_sits_behind_its_warning() {
    let mut h = rig(true);
    assert!(!shows(&h, "not encrypted"), "folded away");
    press_on(&mut h, "OBS on another computer", Key::Enter);
    assert!(shows(&h, "The link to another computer is not encrypted."), "{:#?}", all_strings(&h));
    assert!(h.query_by_label("Host").is_none(), "no host until remote links are allowed");
    press_on(&mut h, "Allow OBS on another computer", Key::Space);
    let allowed = h.state().settings.get::<omuzaki_obs::settings::AllowRemote>().unwrap_or_else(|e| panic!("{e}"));
    assert!(allowed, "obs.allow_remote, set as the person");
    h.run_steps(3);
    assert!(h.query_by_label("Host").is_some(), "{:#?}", all_strings(&h));
}

#[test]
fn every_word_on_the_page_goes_through_the_message_table() {
    let mut h = rig(true);
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(4);
    for text in ["Not connected", "In OBS, open Tools", "Not affiliated", "Save password", "OBS on another computer"] {
        assert!(!shows(&h, text), "{text:?} is drawn in the pseudo-locale: {:#?}", all_strings(&h));
    }
}

/// Screenshots of the page for the pull request: each status at 1366x768 and 1920x1080, in English and the
/// pseudo-locale. Run with `OMUZAKI_EVIDENCE_OUT=<dir> cargo test -p omuzaki-ui --test obs_page obs_evidence --
/// --ignored` (software Vulkan).
#[test]
#[ignore = "writes screenshots; run by hand for a pull request"]
fn obs_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for status in ["no-bridge", "disconnected", "connected", "auth-failed", "remote"] {
        for (w, h) in [(1366.0, 768.0), (1920.0, 1080.0)] {
            for pseudo in [false, true] {
                let fake = matches!(status, "connected" | "auth-failed")
                    .then(|| FakeObs::start(FakeConfig { password: Some(PASSWORD.into()), ..FakeConfig::default() }));
                let state = state(status != "no-bridge");
                let mut harness = Harness::builder().with_size(vec2(w, h)).wgpu().build_ui_state(
                    |ui, r: &mut Rig| {
                        omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
                        draw(ui, r);
                    },
                    state,
                );
                harness.run_steps(4);
                if let Some(fake) = &fake {
                    if status == "connected" {
                        save_password(&mut harness);
                    }
                    type_into(&mut harness, "Port", &fake.port().to_string());
                    press_on(&mut harness, "Connect", Key::Enter);
                    let shown = if status == "connected" { "Connected: OBS WebSocket" } else { "Wrong or missing" };
                    until_shows(&mut harness, shown);
                }
                if status == "remote" {
                    press_on(&mut harness, "OBS on another computer", Key::Enter);
                    press_on(&mut harness, "Allow OBS on another computer", Key::Space);
                }
                if pseudo {
                    set_strings(&harness.ctx, Arc::new(Pseudo(English)));
                }
                harness.run_steps(8);
                let image = harness.render().unwrap_or_else(|e| panic!("renders: {e}"));
                let locale = if pseudo { "-pseudo" } else { "" };
                let path = format!("{out}/obs-{status}-{w}x{h}{locale}.png");
                image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
            }
        }
    }
}
