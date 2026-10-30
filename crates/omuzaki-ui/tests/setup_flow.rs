//! The first-run panel over the setup machine (#1184, #989; `shell.md` section 14): driven by a fake host with chosen
//! probes, so every case of the design is a test. Every control is named, Tab follows the screen, Enter continues, Esc
//! skips setup, Back on the first step is off with its reason; one camera means no picker, a refused camera says how to
//! fix it and offers the other ways, a downloading tracker shows its progress and lets the person go on, skipping
//! calibration is recorded, and Output offers the system's recommendation and starts its test pattern.

use std::cell::RefCell;
use std::rc::Rc;

use egui::accesskit::Role;
use egui::{Key, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_ops_api::{Actor, Runner};
use omuzaki_setup::{
    Cameras, CharacterChoice, Current, Effect, Event, IPhone, LanguageOffer, OutputKind, Permission, Probes, Step,
    TrackerModel,
};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::CalibrationState;
use omuzaki_ui::ds::audit::plain_english;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{all_strings, harness_state};
use omuzaki_ui::ds::{Settings, set_strings};
use omuzaki_ui::panels::setup::{FirstRun, HostStatus, SetupHost, SetupPanel, Unavailable};
use omuzaki_ui::shell::requests::{ShellRequest, ShellRequests};
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui_api::panel::{Panel as _, PanelCtx, ViewModels};

/// A machine with one granted camera, the tracker ready, an output and no assistant.
fn probes() -> Probes {
    Probes {
        cameras: Cameras::One(Permission::Granted),
        tracker: TrackerModel::Ready,
        iphone: IPhone::NotPaired,
        recommended_output: Some(OutputKind::Spout),
        assistant: false,
        language: LanguageOffer::None,
        project_open: false,
        privacy_claim_true: true,
    }
}

/// A host that answers every request at once and keeps what it was asked.
struct Fake {
    probes: Rc<RefCell<Probes>>,
    asked: Rc<RefCell<Vec<Effect>>>,
}

impl SetupHost for Fake {
    fn refresh(&mut self, _: &dyn Runner, _: &Actor) {}

    fn probes(&self) -> Probes {
        *self.probes.borrow()
    }

    fn current(&self) -> Current {
        Current::default()
    }

    fn status(&self) -> HostStatus {
        HostStatus {
            camera: Some("Desk camera".into()),
            cameras: vec!["Desk camera".into(), "Capture card".into()],
            tracking: Some(Tracking::Tracking),
            calibration: Some(CalibrationState::Idle),
            ..HostStatus::default()
        }
    }

    fn character(&self, choice: CharacterChoice) -> Result<(), Unavailable> {
        if choice == CharacterChoice::FromSheetOrIdea { Err(Unavailable::NotInBuild) } else { Ok(()) }
    }

    fn pick_camera(&mut self, _: &dyn Runner, _: &Actor, _: usize) {}

    fn stage_import(&mut self, _: String) {}

    fn run_effect(&mut self, _: &dyn Runner, _: &Actor, effect: &Effect) -> Vec<Event> {
        self.asked.borrow_mut().push(effect.clone());
        match *effect {
            Effect::SelectInput { request, .. }
            | Effect::ShowTestPattern { request, .. }
            | Effect::ApplyCharacter { request, .. } => vec![Event::Applied { request }],
            Effect::StartCalibration { request } => vec![Event::CalibrationDone { request }],
            _ => Vec::new(),
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        Vec::new()
    }
}

struct Rig {
    panel: SetupPanel,
    views: ViewModels,
    shell: ShellRequests,
    probes: Rc<RefCell<Probes>>,
    asked: Rc<RefCell<Vec<Effect>>>,
    _folder: tempfile::TempDir,
}

fn rig(probes: Probes) -> Harness<'static, Rig> {
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let (shared, asked) = (Rc::new(RefCell::new(probes)), Rc::new(RefCell::new(Vec::new())));
    let host = Fake { probes: Rc::clone(&shared), asked: Rc::clone(&asked) };
    let shell = ShellRequests::default();
    let mut views = ViewModels::new();
    views.insert(shell.clone());
    views.insert(FirstRun::at(folder.path().join("setup-done")));
    let state =
        Rig { panel: SetupPanel::with_host(Box::new(host)), views, shell, probes: shared, asked, _folder: folder };
    let mut h = harness_state(Settings::default(), vec2(380.0, 900.0), state, |ui, r| {
        let actor = Actor::Person { name: "kit".into() };
        let ctx = PanelCtx::new(&NullRunner, &actor).with_views(&r.views);
        r.panel.ui(ui, &ctx);
    });
    h.run_steps(3);
    h
}

fn step(h: &Harness<'_, Rig>) -> Step {
    h.state().panel.step()
}

fn click(h: &mut Harness<'_, Rig>, role: Role, label: &str) {
    h.get_by_role_and_label(role, label).click();
    h.run_steps(3);
}

fn shows(h: &Harness<'_, Rig>, text: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(text))
}

/// To Input, through Welcome and the sample.
fn to_input(h: &mut Harness<'_, Rig>) {
    click(h, Role::Button, "Start");
    click(h, Role::RadioButton, "Use the sample");
    assert_eq!(step(h), Step::Input);
}

// ---- #1184: the chrome, Welcome, Character and Go-live ----

#[test]
fn every_control_is_named() {
    let mut h = rig(probes());
    for _ in 0..3 {
        for node in h.query_all_by_role(Role::Button).chain(h.query_all_by_role(Role::RadioButton)) {
            let label = node.accesskit_node().label().unwrap_or_default();
            assert!(!label.trim().is_empty(), "an unnamed control on {:?}", step(&h));
        }
        h.key_press(Key::Enter);
        h.run_steps(3);
    }
}

#[test]
fn tab_follows_the_screen_top_to_bottom() {
    let mut h = rig(probes());
    click(&mut h, Role::Button, "Start");
    let mut seen: Vec<(String, Pos2)> = Vec::new();
    for _ in 0..12 {
        h.key_press(Key::Tab);
        h.run_steps(2);
        let Some(node) = h
            .query_all_by_role(Role::Button)
            .chain(h.query_all_by_role(Role::RadioButton))
            .find(egui_kittest::Node::is_focused)
        else {
            continue;
        };
        let label = node.accesskit_node().label().unwrap_or_default();
        if seen.first().is_some_and(|(first, _)| *first == label) {
            break;
        }
        let rect = node.rect();
        seen.push((label, rect.min));
    }
    let names: Vec<&str> = seen.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["Skip setup", "Use the sample", "Import a VRM", "Back", "Continue"],
        "the card this build can't carry out takes no Tab stop"
    );
    // Below the header, every stop is lower on the screen than the one before, or to its right on the same line.
    for pair in seen[1..].windows(2) {
        let ((a, at), (b, bt)) = (&pair[0], &pair[1]);
        assert!(bt.y > at.y + 1.0 || (bt.y - at.y).abs() <= 1.0 && bt.x > at.x, "{a} then {b}");
    }
}

#[test]
fn enter_continues_and_esc_skips_setup() {
    let mut h = rig(probes());
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(step(&h), Step::Character, "Enter is Start on Welcome");
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert_eq!(step(&h), Step::Input, "Enter takes the sample on Character");
    h.key_press(Key::Escape);
    h.run_steps(3);
    let progress = h.state().panel.progress().unwrap_or_default();
    assert!(progress.finished, "Esc ended setup");
    assert!(h.state().shell.take().contains(&ShellRequest::ClosePanel("setup")), "and closed the panel");
}

#[test]
fn back_on_the_first_step_is_off_with_its_reason() {
    let h = rig(probes());
    let back = h.get_by_role_and_label(Role::Button, "Back");
    assert!(back.accesskit_node().is_disabled());
    assert_eq!(back.accesskit_node().description().as_deref(), Some("This is the first step"));
}

#[test]
fn the_header_counts_the_shown_steps_and_says_how_long_each_takes() {
    let mut h = rig(probes());
    assert!(!shows(&h, "Setup \u{b7}"), "no count on Welcome");
    click(&mut h, Role::Button, "Start");
    assert!(shows(&h, "Setup \u{b7} 1 of 5"), "{:?}", all_strings(&h));
    assert!(shows(&h, "About 10 s"));
    assert!(shows(&h, "Step 2, Input: to do"), "each step in the list is named with its state");
}

#[test]
fn welcome_says_what_happens_to_the_camera_and_offers_an_agent() {
    let h = rig(probes());
    assert!(shows(&h, "Your camera never leaves this computer. Telemetry is off."));
    assert!(h.query_by_label("Connect an agent").is_some());
}

#[test]
fn a_card_this_build_cant_carry_out_is_shown_off_with_its_reason() {
    let mut h = rig(probes());
    click(&mut h, Role::Button, "Start");
    let sheet = h.get_by_role_and_label(Role::RadioButton, "Describe a character");
    assert!(sheet.accesskit_node().is_disabled());
    assert!(sheet.accesskit_node().description().is_some_and(|d| d.contains("Not in this version yet")));
}

#[test]
fn go_live_ends_setup_where_the_person_chooses() {
    let mut h = rig(probes());
    to_input(&mut h);
    click(&mut h, Role::Button, "Continue");
    click(&mut h, Role::Button, "Use defaults");
    click(&mut h, Role::RadioButton, "Spout");
    click(&mut h, Role::Button, "Continue");
    assert_eq!(step(&h), Step::GoLive);
    // The character, the face and the layout; calibration took the defaults and this host's output isn't on.
    assert!(shows(&h, "3 of 5 ready"), "{:?}", all_strings(&h));
    let _ = h.state().shell.take();
    click(&mut h, Role::Button, "Customise the character");
    let asked = h.state().shell.take();
    assert!(asked.contains(&ShellRequest::Workspace("build")), "{asked:?}");
    assert!(h.state().panel.progress().is_some_and(|p| p.finished));
}

#[test]
fn every_word_goes_through_the_message_table() {
    let mut h = rig(probes());
    set_strings(&h.ctx, std::sync::Arc::new(Pseudo(English)));
    for _ in 0..5 {
        h.run_steps(3);
        // The camera names are the system's, not the studio's words.
        let english: Vec<String> = plain_english(all_strings(&h).iter().map(String::as_str))
            .into_iter()
            .filter(|s| !s.contains("Desk camera") && !s.contains("Capture card"))
            .collect();
        assert!(english.is_empty(), "plain English on {:?}: {english:?}", step(&h));
        h.key_press(Key::Enter);
    }
}

// ---- #989: Input, Calibrate and Output ----

#[test]
fn one_camera_has_no_picker_and_two_do() {
    let mut h = rig(probes());
    to_input(&mut h);
    assert!(h.query_by_label("Camera").is_none(), "one camera: nothing to pick (P6)");
    assert!(shows(&h, "Face found \u{b7} Desk camera"));

    let mut h = rig(Probes { cameras: Cameras::Several(2), ..probes() });
    to_input(&mut h);
    assert!(h.query_by_label("Camera").is_some());
}

#[test]
fn a_refused_camera_says_how_to_fix_it_and_offers_the_other_ways() {
    let mut h = rig(Probes { cameras: Cameras::One(Permission::Denied), ..probes() });
    to_input(&mut h);
    assert!(shows(&h, "Allow it in your system's privacy settings"), "{:?}", all_strings(&h));
    let webcam = h.get_by_role_and_label(Role::RadioButton, "Webcam");
    assert!(webcam.accesskit_node().is_disabled());
    for other in ["iPhone", "Skip for now"] {
        assert!(!h.get_by_role_and_label(Role::RadioButton, other).accesskit_node().is_disabled(), "{other}");
    }
}

#[test]
fn a_downloading_tracker_shows_its_progress_and_continue_goes_on() {
    let mut h = rig(Probes { tracker: TrackerModel::Downloading { percent: 42 }, ..probes() });
    to_input(&mut h);
    assert!(shows(&h, "Getting the face tracker: 42%"), "{:?}", all_strings(&h));
    click(&mut h, Role::Button, "Continue");
    assert_ne!(step(&h), Step::Input, "the person goes on while it downloads");
}

#[test]
fn the_camera_permission_is_asked_on_input_with_its_reason() {
    let mut h = rig(Probes { cameras: Cameras::One(Permission::Undecided), ..probes() });
    to_input(&mut h);
    assert!(shows(&h, "Continue asks your system for the camera"));
    click(&mut h, Role::Button, "Continue");
    assert!(h.state().asked.borrow().contains(&Effect::RequestCameraPermission));
    // The system answers: the camera is the person's to use.
    *h.state().probes.borrow_mut() = Probes { cameras: Cameras::One(Permission::Granted), ..probes() };
    h.run_steps(3);
    click(&mut h, Role::Button, "Continue");
    assert_eq!(step(&h), Step::Calibrate);
}

#[test]
fn skipping_calibration_is_recorded() {
    let mut h = rig(probes());
    to_input(&mut h);
    click(&mut h, Role::Button, "Continue");
    assert_eq!(step(&h), Step::Calibrate);
    click(&mut h, Role::Button, "Use defaults");
    assert_eq!(step(&h), Step::Output);
    let progress = h.state().panel.progress().unwrap_or_default();
    assert!(progress.skipped.contains(&Step::Calibrate), "{progress:?}");
}

#[test]
fn skip_for_now_leaves_the_life_layer_on_and_is_recorded() {
    let mut h = rig(probes());
    to_input(&mut h);
    click(&mut h, Role::RadioButton, "Skip for now");
    assert!(h.state().panel.progress().is_some_and(|p| p.skipped.contains(&Step::Input)));
    let asked = h.state().asked.borrow().clone();
    assert!(
        asked
            .iter()
            .any(|e| matches!(e, Effect::SelectInput { source: omuzaki_setup::InputChoice::LifeLayerOnly, .. }))
    );
}

#[test]
fn output_offers_the_systems_recommendation_and_starts_its_test_pattern() {
    let mut h = rig(Probes { recommended_output: Some(OutputKind::Syphon), ..probes() });
    to_input(&mut h);
    click(&mut h, Role::Button, "Continue");
    click(&mut h, Role::Button, "Use defaults");
    assert_eq!(step(&h), Step::Output);
    assert!(h.query_by_role_and_label(Role::RadioButton, "Spout").is_none());
    click(&mut h, Role::RadioButton, "Syphon");
    let asked = h.state().asked.borrow().clone();
    assert!(asked.iter().any(|e| matches!(e, Effect::ShowTestPattern { output: OutputKind::Syphon, .. })), "{asked:?}");
}

#[test]
fn calibrating_runs_through_the_host_and_moves_on() {
    let mut h = rig(probes());
    to_input(&mut h);
    click(&mut h, Role::Button, "Continue");
    click(&mut h, Role::Button, "Continue");
    assert!(h.state().asked.borrow().iter().any(|e| matches!(e, Effect::StartCalibration { .. })));
    assert_eq!(step(&h), Step::Output, "the run came back done");
    assert!(!h.state().panel.progress().unwrap_or_default().skipped.contains(&Step::Calibrate));
}

// ---- #2578: Describe a character, through the studio's own core ----

/// The Setup panel on a real core with the kit and the brief linked, so the Describe card runs
/// `character.create_from_description` for real; the edit focus is shared, as the shell shares it.
struct Real {
    panel: SetupPanel,
    views: ViewModels,
    focus: omuzaki_ui::panels::edit::EditFocus,
    core: omuzaki_ui::wiring::Core,
    _folder: tempfile::TempDir,
}

fn real(size: egui::Vec2, settings: Settings) -> Harness<'static, Real> {
    omuzaki_ui::link();
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let core = omuzaki_ui::wiring::core_on(
        omuzaki_ui::wiring::Document::empty(),
        omuzaki_ops_api::ServicesBuilder::new().build(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let focus = omuzaki_ui::panels::edit::EditFocus::default();
    let mut views = ViewModels::new();
    views.insert(ShellRequests::default());
    views.insert(FirstRun::at(folder.path().join("setup-done")));
    views.insert(focus.clone());
    let state = Real { panel: SetupPanel::default(), views, focus, core, _folder: folder };
    let mut h = Harness::builder().with_size(size).wgpu().build_ui_state(
        move |ui, r: &mut Real| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), settings);
            egui::CentralPanel::default().show(ui, |ui| {
                let actor = Actor::Person { name: "you".into() };
                let ctx = PanelCtx::new(r.core.runner.as_ref(), &actor).with_views(&r.views);
                r.panel.ui(ui, &ctx);
            });
        },
        state,
    );
    h.run_steps(3);
    h
}

/// Start, then the Describe card, then the words typed into its box.
fn describe(h: &mut Harness<'_, Real>, words: &str) {
    h.get_by_role_and_label(Role::Button, "Start").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::RadioButton, "Describe a character").click();
    h.run_steps(3);
    h.get_by_label("Describe your character").click();
    h.run_steps(2);
    h.get_by_label("Describe your character").type_text(words);
    h.run_steps(2);
}

#[test]
fn describing_a_character_makes_it_through_the_op_and_opens_it_for_editing() {
    let mut h = real(vec2(420.0, 900.0), Settings::default());
    describe(&mut h, "a sleepy librarian with silver twin tails and round glasses");
    h.get_by_role_and_label(Role::Button, "Make it").click();
    h.run_steps(4);
    let opened = h.state().focus.character();
    assert_eq!(
        opened.as_ref().map(omuzaki_char_api::CharId::as_str),
        Some("sleepy-librarian"),
        "{:?}",
        all_strings(&h)
    );
    // The character is in the document, made by one op the person can undo, and setup went on to Input.
    let stored = h
        .state()
        .core
        .runner
        .get(
            &omuzaki_doc_api::DocPath::parse("characters/sleepy-librarian").unwrap_or_else(|e| panic!("{e}")),
            &omuzaki_ops_api::Grants::person(),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(stored.is_some(), "the character is in the document");
    assert_eq!(h.state().panel.step(), Step::Input);
}

#[test]
fn the_make_button_waits_for_words() {
    let mut h = real(vec2(420.0, 900.0), Settings::default());
    describe(&mut h, "");
    let make = h.get_by_role_and_label(Role::Button, "Make it");
    assert!(make.accesskit_node().is_disabled());
    assert!(make.accesskit_node().description().is_some_and(|d| d.contains("Write a few words first")));
}

/// Screenshots of the Describe card for the pull request: the box with words in it, dark and light. Run with
/// `OMUZAKI_EVIDENCE_OUT=<dir> cargo test -p omuzaki-ui --test setup_flow describe_evidence -- --ignored` (software
/// Vulkan).
#[test]
#[ignore = "writes screenshots; run by hand for a pull request"]
fn describe_evidence() {
    use omuzaki_ui::ds::ThemeKind;
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    for (name, settings) in
        [("dark", Settings::default()), ("light", Settings { theme: ThemeKind::Light, ..Settings::default() })]
    {
        let mut h = real(vec2(560.0, 760.0), settings);
        describe(&mut h, "a sleepy librarian with silver twin tails, round glasses and a zorbly scarf");
        let image = h.render().unwrap_or_else(|e| panic!("renders: {e}"));
        let path = format!("{out}/setup-describe-{name}.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
        h.get_by_role_and_label(Role::Button, "Make it").click();
        h.run_steps(4);
    }
}
