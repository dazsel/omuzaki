//! The Connectors page of the Settings window (#1446, CN-13) over the real `vts.*` ops and a loopback mock `VTube
//! Studio`: without a bridge the page says why; disconnected, it shows the setup steps, the parameters to map in VTS
//! (copied in one click) and the inputs Omuzaki can feed; a person connects with the keyboard alone, the popup's wait
//! and the connection show, Suggest proposes hotkeys for the character's expressions, and the person confirms one,
//! confirms the rest and removes one; an agent's mapping shows within a second; a revoked or denied bridge says what to
//! do in VTS. Every control is named, the page draws no image, the footer says Omuzaki is not affiliated with VTS's
//! makers, and every word goes through the message table.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, OutputCommand, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_anim_api::{Calibration, DeviceProfile};
use omuzaki_face_api::ExpressionSet;
use omuzaki_ops_api::{Actor, Budget, Grants, Limits, Mode, Request, ServicesBuilder, SessionSpec, Transport};
use omuzaki_stage::perform::host::{PerformError, PerformHost, PerformStatus, RetargetChange, RetargetReceipt};
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{all_strings, harness_state, press};
use omuzaki_ui::ds::{Settings, set_strings};
use omuzaki_ui::live::story::LiveState;
use omuzaki_ui::settings::{Page, SettingsWindow};
use omuzaki_ui::wiring::Core;
use omuzaki_vts::clock::SystemClock;
use omuzaki_vts::service::VtsService;
use omuzaki_vts::testing::{MockConfig, MockHotkey, MockModel, MockVts};
use serde_json::{Value, json};

/// The perform loop as `trigger.list` sees it: the starter expressions and no keys bound.
#[derive(Default)]
struct Performer(Mutex<()>);

impl PerformHost for Performer {
    fn calibrate(&self, _frames: usize) -> Result<(), PerformError> {
        Ok(())
    }

    fn set_retarget(&self, _change: RetargetChange) -> Result<RetargetReceipt, PerformError> {
        Err(PerformError::Unsupported)
    }

    fn set_calibration(&self, _calibration: Calibration) -> Result<(), PerformError> {
        Ok(())
    }

    fn calibration(&self) -> Calibration {
        let _held = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Calibration::identity(DeviceProfile::Webcam)
    }

    fn status(&self) -> PerformStatus {
        LiveState::Idle.status()
    }

    fn expressions(&self) -> ExpressionSet {
        ExpressionSet::starter()
    }
}

fn model() -> MockModel {
    MockModel {
        name: "Kit".into(),
        hotkeys: vec![
            MockHotkey { id: "h-happy".into(), name: "Happy face".into(), on_screen_button: None },
            MockHotkey { id: "h-angry".into(), name: "Angry".into(), on_screen_button: None },
            MockHotkey { id: "h-wave".into(), name: "Wave".into(), on_screen_button: Some(1) },
        ],
        expressions: vec!["Cry".into()],
        parameters: vec!["MouthOpen".into()],
    }
}

struct Rig {
    window: Option<SettingsWindow>,
    core: Core,
    person: Actor,
}

/// The Settings window open on Connectors, with the bridge (`vts`) when there is one.
fn rig(vts: Option<Arc<VtsService>>) -> Harness<'static, Rig> {
    omuzaki_ui::link();
    let mut services = ServicesBuilder::new().insert::<dyn PerformHost>(Arc::new(Performer::default()));
    if let Some(vts) = vts {
        services = services.insert::<VtsService>(vts);
    }
    let core = omuzaki_ui::wiring::core_on(omuzaki_ui::wiring::Document::empty(), services.build())
        .unwrap_or_else(|e| panic!("{e}"));
    let state =
        Rig { window: Some(SettingsWindow::on(Page::Connectors)), core, person: Actor::Person { name: "you".into() } };
    let mut h = harness_state(Settings::default(), vec2(900.0, 1800.0), state, |ui, r| {
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

fn bridge() -> Arc<VtsService> {
    Arc::new(VtsService::with(omuzaki_vts::inject::ChannelMap::builtin().unwrap_or_else(|e| panic!("{e}")), clock()))
}

fn clock() -> Arc<SystemClock> {
    Arc::new(SystemClock::new())
}

fn mock(approve: bool) -> MockVts {
    let config = MockConfig { model: Some(model()), clock: clock(), approve_token: approve, ..MockConfig::default() };
    MockVts::start(&config).unwrap_or_else(|e| panic!("{e}"))
}

fn shows(h: &Harness<'_, Rig>, text: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(text))
}

fn state(h: &mut Harness<'_, Rig>) -> Option<String> {
    h.state_mut().window.as_mut().and_then(|w| w.connectors().state().map(str::to_owned))
}

/// Runs frames (a second of egui's clock each round, so the page reads `vts.status` again) until `text` shows.
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

/// Focuses the button named `label` and presses Enter.
fn press_button(h: &mut Harness<'_, Rig>, label: &str) {
    h.get_by_role_and_label(Role::Button, label).focus();
    h.run();
    assert_eq!(focused(h).as_deref(), Some(label));
    press(h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
}

fn every_control_is_named_and_no_image_is_drawn(h: &Harness<'_, Rig>) {
    let controls = h.query_all_by_role(Role::Button).chain(h.query_all_by_role(Role::TextInput));
    for node in controls {
        let label = node.accesskit_node().label().unwrap_or_default();
        assert!(!label.trim().is_empty(), "an unnamed control: {node:?}");
    }
    assert_eq!(h.query_all_by_role(Role::Image).count(), 0, "no logo or image of VTS's");
    assert!(shows(h, "Not affiliated with or endorsed by DenchiSoft or Live2D Inc."), "the footer");
}

/// The bridge's mapping for the loaded model, as `vts.status` lists it.
fn mapping(h: &Harness<'_, Rig>) -> Vec<String> {
    let status = run(h, "vts.status", json!({}), &h.state().person.clone(), Grants::person());
    status["mapping"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r["omuzaki"].as_str().map(str::to_owned))
        .collect()
}

fn run(h: &Harness<'_, Rig>, op: &str, args: Value, actor: &Actor, grants: Grants) -> Value {
    let request = Request {
        op: op.into(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants,
        cause: None,
        tx: None,
        base: None,
    };
    let outcome = h.state().core.runner.run(request).unwrap_or_else(|e| panic!("{op}: {e}"));
    outcome.plan.output.unwrap_or(Value::Null)
}

#[test]
fn without_the_bridge_the_page_says_why() {
    let h = rig(None);
    assert!(shows(&h, "The VTube Studio bridge isn't in this build"), "{:#?}", all_strings(&h));
    every_control_is_named_and_no_image_is_drawn(&h);
}

#[test]
fn disconnected_it_shows_the_steps_and_the_parameters_to_copy() {
    let mut h = rig(Some(bridge()));
    assert_eq!(state(&mut h).as_deref(), Some("disconnected"));
    for text in [
        "Not connected",
        "1. Open VTube Studio and load your model",
        "2. In its settings, turn on Allow Plugin API access",
        "3. Click Connect here, then Allow in VTube Studio",
        "Parameters to map in VTube Studio",
    ] {
        assert!(shows(&h, text), "the page shows {text:?}: {:#?}", all_strings(&h));
    }
    // The names and the inputs are folded away until asked for.
    assert!(!shows(&h, "OmuzakiJawOpen"));
    let names = "The names, and the VTube Studio inputs";
    h.get_by_label(names).focus();
    h.run();
    assert_eq!(focused(&h).as_deref(), Some(names));
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert!(shows(&h, "OmuzakiJawOpen") && shows(&h, "MouthOpen: off"), "{:#?}", all_strings(&h));
    assert!(h.query_by_role_and_label(Role::TextInput, "Port").is_some());
    every_control_is_named_and_no_image_is_drawn(&h);

    h.get_by_role_and_label(Role::Button, "Copy the names").scroll_to_me();
    h.run();
    h.get_by_role_and_label(Role::Button, "Copy the names").click();
    let mut copied = None;
    for _ in 0..4 {
        h.step();
        copied = copied.or_else(|| {
            h.output().platform_output.commands.iter().find_map(|c| match c {
                OutputCommand::CopyText(text) => Some(text.clone()),
                _ => None,
            })
        });
    }
    let copied = copied.unwrap_or_else(|| panic!("nothing was copied"));
    let names: Vec<&str> = copied.lines().collect();
    assert!(names.contains(&"OmuzakiJawOpen") && names.iter().all(|n| n.starts_with("Omuzaki")), "{names:?}");
}

#[test]
fn a_person_connects_and_maps_with_the_keyboard_and_an_agents_mapping_shows() {
    let vts = mock(true);
    let mut h = rig(Some(bridge()));

    // The port, typed; Tab to Connect; Enter.
    h.get_by_role_and_label(Role::TextInput, "Port").focus();
    h.run();
    h.event(Event::Text(vts.port().to_string()));
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert_eq!(focused(&h).as_deref(), Some("Connect"), "Tab goes from the port to Connect");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    until_shows(&mut h, "Connected: VTube Studio");
    assert_eq!(state(&mut h).as_deref(), Some("connected"));
    assert!(shows(&h, "Model: Kit"), "{:#?}", all_strings(&h));
    assert!(!shows(&h, "1. Open VTube Studio"), "the steps go once connected");
    assert!(shows(&h, "Nothing mapped yet"), "{:#?}", all_strings(&h));

    // Suggest matches the character's expressions to the model's hotkeys by name; nothing is stored until confirmed.
    press_button(&mut h, "Suggest");
    assert!(shows(&h, "runs Happy face") && shows(&h, "runs Angry"), "{:#?}", all_strings(&h));
    assert_eq!(h.query_all_by_label("Suggested").count(), 2);
    assert!(mapping(&h).is_empty(), "a suggestion is not a mapping");

    press_button(&mut h, "Confirm happy");
    assert_eq!(mapping(&h), ["happy"]);
    assert!(h.query_by_role_and_label(Role::Button, "Remove happy").is_some());
    press_button(&mut h, "Confirm all");
    assert_eq!(mapping(&h), ["happy", "angry"]);
    press_button(&mut h, "Remove happy");
    assert_eq!(mapping(&h), ["angry"]);
    every_control_is_named_and_no_image_is_drawn(&h);

    // An agent maps a row over MCP's path (the same op): the page shows it within a second.
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
    let row = json!({ "omuzaki": "wave", "target": { "kind": "hotkey", "id": "h-wave", "name": "Wave" } });
    run(&h, "vts.map", json!({ "rows": [row], "merge": true }), &agent, Grants::agent_default());
    h.run_steps(70);
    assert!(shows(&h, "runs Wave"), "{:#?}", all_strings(&h));

    press_button(&mut h, "Disconnect");
    assert_eq!(state(&mut h).as_deref(), Some("disconnected"));
    drop(vts);
}

#[test]
fn a_revoked_or_denied_bridge_says_what_to_do_in_vts() {
    let vts = mock(true);
    let mut h = rig(Some(bridge()));
    connect_to(&mut h, &vts);
    until_shows(&mut h, "Connected: VTube Studio");
    vts.revoke();
    until_shows(&mut h, "Access revoked");
    assert!(shows(&h, "Allow access again in VTube Studio, then Connect"), "{:#?}", all_strings(&h));
    assert!(h.query_by_role_and_label(Role::Button, "Connect").is_some(), "the person connects again");

    let refused = mock(false);
    let mut h = rig(Some(bridge()));
    connect_to(&mut h, &refused);
    until_shows(&mut h, "Denied");
    assert!(shows(&h, "Allow access again in VTube Studio, then Connect"), "{:#?}", all_strings(&h));
}

#[test]
fn every_word_on_the_page_goes_through_the_message_table() {
    let mut h = rig(Some(bridge()));
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(4);
    for text in ["Not connected", "Parameters to map", "Open VTube Studio", "Not affiliated", "Copy the names"] {
        assert!(!shows(&h, text), "{text:?} is drawn in the pseudo-locale: {:#?}", all_strings(&h));
    }
}

/// Screenshots of the page for the pull request: each status at 1366x768 and 1920x1080, in English and the
/// pseudo-locale. Run with `OMUZAKI_EVIDENCE_OUT=<dir> cargo test -p omuzaki-ui --features omuzaki-vts/testing --test
/// connectors_page connectors_evidence -- --ignored` (software Vulkan).
#[test]
#[ignore = "writes screenshots; run by hand for a pull request"]
fn connectors_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for status in ["no-bridge", "disconnected", "connected", "revoked", "denied"] {
        for (w, h) in [(1366.0, 768.0), (1920.0, 1080.0)] {
            for pseudo in [false, true] {
                let vts = (status != "no-bridge" && status != "disconnected").then(|| mock(status != "denied"));
                let mut harness = evidence_rig(status != "no-bridge", vec2(w, h));
                if let Some(vts) = &vts {
                    connect_to(&mut harness, vts);
                    let shown = match status {
                        "connected" => "Connected: VTube Studio",
                        "revoked" => "Access revoked",
                        _ => "Denied",
                    };
                    if status == "revoked" {
                        until_shows(&mut harness, "Connected: VTube Studio");
                        vts.revoke();
                    }
                    until_shows(&mut harness, shown);
                    if status == "connected" {
                        press_button(&mut harness, "Suggest");
                        press_button(&mut harness, "Confirm happy");
                        harness.get_by_label("Advanced: an expression file").click();
                        harness.run_steps(3);
                    }
                }
                if pseudo {
                    set_strings(&harness.ctx, Arc::new(Pseudo(English)));
                }
                harness.run_steps(8);
                let image = harness.render().unwrap_or_else(|e| panic!("renders: {e}"));
                let locale = if pseudo { "-pseudo" } else { "" };
                let path = format!("{out}/connectors-{status}-{w}x{h}{locale}.png");
                image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
            }
        }
    }
}

/// [`rig`] at `size` on the software renderer, with the window as the app opens it.
fn evidence_rig(bridge_in: bool, size: egui::Vec2) -> Harness<'static, Rig> {
    omuzaki_ui::link();
    let mut services = ServicesBuilder::new().insert::<dyn PerformHost>(Arc::new(Performer::default()));
    if bridge_in {
        services = services.insert::<VtsService>(bridge());
    }
    let core = omuzaki_ui::wiring::core_on(omuzaki_ui::wiring::Document::empty(), services.build())
        .unwrap_or_else(|e| panic!("{e}"));
    let state =
        Rig { window: Some(SettingsWindow::on(Page::Connectors)), core, person: Actor::Person { name: "you".into() } };
    let mut h = Harness::builder().with_size(size).wgpu().build_ui_state(
        |ui, r: &mut Rig| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
            let runner = Arc::clone(&r.core.runner);
            if let Some(window) = r.window.as_mut() {
                window.show(ui.ctx(), &runner, &r.person);
            }
        },
        state,
    );
    h.run_steps(4);
    h
}

/// Types the mock's port and connects.
fn connect_to(h: &mut Harness<'_, Rig>, vts: &MockVts) {
    h.get_by_role_and_label(Role::TextInput, "Port").focus();
    h.run();
    h.event(Event::Text(vts.port().to_string()));
    h.run_steps(2);
    press_button(h, "Connect");
}
