//! The keyboard as a trigger device, end to end (#2144): a key bound in the Live panel, pressed in the app, shows its
//! expression on the running performer, and a hold binding lets go when the key does.
//!
//! The pipeline is the app's: `LiveApp` (the perform thread, the real op runner with the trigger ops), the Live panel
//! binding keys through its trigger buttons, and `LiveApp::keys` called after the panel as `main.rs` calls it after the
//! shell.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_anim_api::HumanoidPose;
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner};
use omuzaki_ui::aux_window::Transparency;
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::{harness_state, press_for};
use omuzaki_ui::live::panel::LivePanel;
use omuzaki_ui::live::wiring::{LiveApp, NoTracker, RealClock};

fn request(op: &str, pairs: &[(&str, Value)]) -> Request {
    Request {
        op: op.to_owned(),
        args: Value::Object(pairs.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect()),
        mode: Mode::Apply,
        actor: Actor::Person { name: "performer".into() },
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    }
}

fn run(runner: &Arc<dyn Runner>, op: &str, pairs: &[(&str, Value)]) -> Value {
    runner.run(request(op, pairs)).unwrap_or_else(|e| panic!("{op}: {e:?}")).plan.output.unwrap_or(Value::Null)
}

/// Whether the binding on `key` is showing its expression now.
fn active(runner: &Arc<dyn Runner>, key: &str) -> bool {
    let listing = run(runner, "trigger.list", &[]);
    let found = listing["triggers"].as_array().and_then(|all| all.iter().find(|t| t["binding"]["key"] == key));
    found.unwrap_or_else(|| panic!("{key} is not bound: {listing}"))["active"] == true
}

/// Waits up to a generous limit for `check` to hold, looking every few milliseconds. The limit only bounds a failure;
/// what is waited for is the perform thread taking a queued command, which takes a step.
fn eventually(mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        // Paces the poll; what it waits for is the state `check` reads, not the time.
        std::thread::sleep(Duration::from_millis(5));
    }
    check()
}

struct World {
    app: LiveApp,
    runner: Arc<dyn Runner>,
    panel: LivePanel,
}

/// The app's live slice with the Live panel over it, drawn the way `main.rs` draws a frame: the panel (the shell), then
/// the keyboard.
fn workspace() -> Harness<'static, World> {
    omuzaki_ui::link();
    let app =
        LiveApp::start(Box::new(NoTracker), Box::new(RealClock::new()), Transparency::Fallback, HumanoidPose::REST)
            .expect("the live slice starts");
    let runner = app.runner();
    let panel = LivePanel::new(&*runner);
    harness_state(Settings::default(), vec2(1366.0, 1200.0), World { app, runner, panel }, |ui, world| {
        let actor = Actor::Person { name: "performer".into() };
        world.panel.ui(ui, &*world.runner, &actor);
        world.app.keys(ui.ctx());
    })
}

fn key_event(h: &Harness<'_, World>, key: Key, pressed: bool, modifiers: Modifiers) {
    h.event_modifiers(Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }, modifiers);
}

fn bind(runner: &Arc<dyn Runner>, key: &str, expression: &str, mode: &str) {
    let pairs = [("key", Value::from(key)), ("expression", Value::from(expression)), ("mode", Value::from(mode))];
    run(runner, "trigger.bind", &pairs);
}

#[test]
fn a_bound_key_pressed_in_the_app_shows_its_expression_and_the_next_press_hides_it() {
    let mut h = workspace();
    let runner = Arc::clone(&h.state().runner);
    bind(&runner, "F1", "happy", "toggle");
    bind(&runner, "Ctrl+F2", "angry", "toggle");
    assert!(!active(&runner, "F1") && !active(&runner, "Ctrl+F2"), "nothing shows before a key is pressed");

    press_for(&mut h, Modifiers::NONE, Key::F1, 2);
    assert!(eventually(|| active(&runner, "F1")), "F1 pressed in the app shows Happy");
    assert!(!active(&runner, "Ctrl+F2"), "only the key that was pressed");

    press_for(&mut h, Modifiers::NONE, Key::F1, 2);
    assert!(eventually(|| !active(&runner, "F1")), "a toggle hides it on the next press");

    // A chord fires its own binding.
    press_for(&mut h, Modifiers::CTRL, Key::F2, 2);
    assert!(eventually(|| active(&runner, "Ctrl+F2")), "Ctrl+F2 pressed in the app shows Angry");
    h.state_mut().app.shutdown().expect("a clean shutdown");
}

#[test]
fn a_hold_binding_shows_while_its_key_is_down_and_fades_when_the_key_comes_up() {
    let mut h = workspace();
    let runner = Arc::clone(&h.state().runner);
    bind(&runner, "F3", "surprised", "hold");

    key_event(&h, Key::F3, true, Modifiers::NONE);
    h.run_steps(2);
    assert!(eventually(|| active(&runner, "F3")), "held down, it shows");
    h.run_steps(2);
    assert!(active(&runner, "F3"), "and keeps showing while the key stays down");

    key_event(&h, Key::F3, false, Modifiers::NONE);
    h.run_steps(2);
    assert!(eventually(|| !active(&runner, "F3")), "released, it lets go");
    h.state_mut().app.shutdown().expect("a clean shutdown");
}

#[test]
fn a_hold_is_let_go_when_the_window_loses_focus_with_the_key_down() {
    let mut h = workspace();
    let runner = Arc::clone(&h.state().runner);
    bind(&runner, "F4", "sad", "hold");
    key_event(&h, Key::F4, true, Modifiers::NONE);
    h.run_steps(2);
    assert!(eventually(|| active(&runner, "F4")));
    // The key comes up while another window has the focus, so the app never sees the release.
    h.event(Event::WindowFocused(false));
    h.run_steps(2);
    assert!(eventually(|| !active(&runner, "F4")), "a key that can't be released is not held forever");
    h.state_mut().app.shutdown().expect("a clean shutdown");
}

#[test]
fn a_key_nothing_is_bound_to_does_nothing() {
    let mut h = workspace();
    let runner = Arc::clone(&h.state().runner);
    bind(&runner, "F1", "happy", "toggle");
    press_for(&mut h, Modifiers::NONE, Key::F9, 2);
    press_for(&mut h, Modifiers::NONE, Key::F1, 2);
    assert!(eventually(|| active(&runner, "F1")), "the bound key still works after an unbound one");
    h.state_mut().app.shutdown().expect("a clean shutdown");
}

#[test]
fn the_key_that_binds_a_hotkey_does_not_also_fire_it() {
    let mut h = workspace();
    let runner = Arc::clone(&h.state().runner);
    bind(&runner, "F1", "happy", "toggle");
    // Angry's trigger button waits for a key; F5 is the one it gets.
    h.get_by_label("Angry trigger: Not bound").click();
    h.run_steps(2);
    press_for(&mut h, Modifiers::NONE, Key::F5, 2);
    assert!(h.query_by_label("Angry trigger: F5").is_some(), "F5 is bound to Angry");
    // Commands reach the performer in order: once F1's press has shown, F5's would have too.
    press_for(&mut h, Modifiers::NONE, Key::F1, 2);
    assert!(eventually(|| active(&runner, "F1")));
    assert!(!active(&runner, "F5"), "binding a key is not pressing it");
    h.state_mut().app.shutdown().expect("a clean shutdown");
}
