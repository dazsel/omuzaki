//! The OS input surface over the scripted hub (`omuzaki-input-mock`): the capture indicator in the status bar (ADR
//! 0060 task UI-1) and the Input panel (UI-2, #752's capture section).
//!
//! - The badge is drawn at Off with a fresh heartbeat, so the person's Start succeeds; it reads On within one frame;
//!   when the studio stops drawing it the heartbeat goes stale and capture pauses within 1.5 s, resuming when it is
//!   drawn again; Stop ends capture.
//! - Learning a chord stores it and shows it only in the panel; with capture off the editor says why instead; a
//!   conflict is shown, never overwritten; the ops the panel runs carry no chord; the protected list round-trips.
//!
//! The evidence test renders the panel off, waiting for a permission and on, with the status bar, at 1366x768 and
//! 1920x1080 (ignored; run by hand or by the visual job).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_input_api::capture::{CaptureCaps, CaptureState, PauseWhy, Support};
use omuzaki_input_api::feed::{ChordStore, InputControl, InputHub, ProtectList};
use omuzaki_input_mock::fake::FakeInput;
use omuzaki_ops_api::{
    Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, ServicesBuilder, TxId,
};
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::{all_strings, harness_state};
use omuzaki_ui::input::InputSurface;
use omuzaki_ui::input::panel::InputPanel;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::wiring::{Document, core_on};
use serde_json::Value;

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

/// The studio's runner, keeping every request it ran and every answer it gave, as JSON.
struct Recording {
    inner: Arc<dyn Runner>,
    seen: Mutex<Vec<String>>,
}

impl Recording {
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Runner for Recording {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        let line = format!("{} {}", req.op, req.args);
        let outcome = self.inner.run(req);
        let answer = match &outcome {
            Ok(outcome) => outcome.plan.output.clone().unwrap_or(Value::Null).to_string(),
            Err(error) => error.to_string(),
        };
        self.seen.lock().unwrap().push(format!("{line} -> {answer}"));
        outcome
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.rollback(tx, actor)
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.undo(actor, grants)
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.redo(actor, grants)
    }

    fn subscribe(&self, grants: &Grants) -> Result<std::sync::mpsc::Receiver<Event>, OpError> {
        self.inner.subscribe(grants)
    }

    fn get(&self, path: &omuzaki_doc_api::DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.inner.log(after_seq, limit, grants)
    }
}

/// The scripted hub, and the studio's core over its control, as the App wires them.
struct Rig {
    hub: FakeInput,
    clock: Arc<FakeClock>,
    runner: Arc<Recording>,
}

fn rig() -> Rig {
    omuzaki_ui::link();
    let clock = Arc::new(FakeClock::new(Seconds(10.0)));
    let hub = FakeInput::new(Arc::clone(&clock));
    // A desktop where every class works, as the hub reports it.
    let mut caps = CaptureCaps::default();
    (caps.typing, caps.pointer, caps.pen, caps.gamepad) =
        (Support::Available, Support::Available, Support::Available, Support::Available);
    hub.set_caps(caps);
    let services = ServicesBuilder::new().insert::<dyn InputControl>(hub.control()).build();
    let core = core_on(Document::empty(), services).unwrap();
    let runner = Arc::new(Recording { inner: core.runner, seen: Mutex::new(Vec::new()) });
    Rig { hub, clock, runner }
}

fn says<S>(h: &Harness<'_, S>, words: &str) -> bool {
    all_strings(h).iter().any(|s| s == words)
}

fn shows<S>(h: &Harness<'_, S>, words: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(words))
}

/// A binding's keys as the hub renders them, for the test's own eyes.
fn keys(hub: &FakeInput, id: omuzaki_input_api::binding::BindingId) -> Option<String> {
    let label = hub.label(id)?;
    let mut text = String::new();
    label.render(&mut |part| text.push_str(part));
    Some(text)
}

fn state(hub: &FakeInput) -> CaptureState {
    hub.report().state
}

/// The whole studio, with the hub's handles as a view: the badge sits in its status bar.
fn studio(rig: &Rig) -> (Harness<'static, Shell>, InputSurface) {
    let surface = InputSurface::install(&rig.hub);
    let mut shell = Shell::new(Arc::clone(&rig.runner) as Arc<dyn Runner>, None);
    shell.insert_view(surface.clone());
    let h = Harness::builder().with_size(vec2(1366.0, 768.0)).build_ui_state(
        |ui, shell: &mut Shell| {
            omuzaki_ui::ds::apply_settings(ui.ctx(), Settings::default());
            shell.ui(ui);
        },
        shell,
    );
    (h, surface)
}

#[test]
fn the_badge_beats_at_off_starts_capture_and_pauses_when_it_is_no_longer_drawn() {
    let rig = rig();
    let (mut h, surface) = studio(&rig);
    h.run_steps(2);
    // Off is shown, never hidden, and drawing it is the heartbeat the hub needs before capture can start.
    assert!(says(&h, "Input capture: off"), "{:#?}", all_strings(&h));
    assert!(surface.indicator().is_shown());

    h.get_by_label("Start").click();
    h.run_steps(1);
    assert!(state(&rig.hub).is_on(), "the person's Start ran: {:?}", rig.runner.seen());
    h.run_steps(1);
    assert!(says(&h, "Reading every device: zones only"), "On within a frame: {:#?}", all_strings(&h));

    // The studio stops drawing the badge (hidden, minimised, closed): the heartbeat ages and the hub pauses.
    rig.clock.advance(Seconds(1.0));
    rig.hub.tick();
    assert!(state(&rig.hub).is_on(), "1 s is inside the 1.5 s window");
    rig.clock.advance(Seconds(0.6));
    rig.hub.tick();
    assert!(
        matches!(state(&rig.hub), CaptureState::Paused { why: Some(PauseWhy::IndicatorLost), .. }),
        "{:?}",
        state(&rig.hub)
    );
    h.run_steps(1);
    assert!(says(&h, "Input capture: paused while Omuzaki was hidden"), "{:#?}", all_strings(&h));

    // Drawn again, it beats again, and the hub's own pause undoes itself.
    rig.hub.tick();
    assert!(state(&rig.hub).is_on(), "{:?}", state(&rig.hub));
    h.run_steps(1);
    h.get_by_label("Stop").click();
    h.run_steps(1);
    assert_eq!(state(&rig.hub), CaptureState::Off);
    h.run_steps(1);
    assert!(says(&h, "Input capture: off"));
}

#[test]
fn an_agent_cannot_start_capture_even_with_the_badge_shown() {
    let rig = rig();
    let (mut h, _surface) = studio(&rig);
    h.run_steps(2);
    let agent = Actor::Agent { id: "helper".into(), model: None };
    let request = Request {
        op: "input.capture.start".into(),
        args: serde_json::json!({}),
        mode: omuzaki_ops_api::Mode::Apply,
        actor: agent,
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    assert!(rig.runner.run(request).is_err());
    assert_eq!(state(&rig.hub), CaptureState::Off);
}

/// The Input panel alone over the rig, as the Perform workspace's tab draws it.
struct PanelRig {
    panel: InputPanel,
    surface: InputSurface,
    runner: Arc<Recording>,
}

fn panel(rig: &Rig, size: egui::Vec2) -> Harness<'static, PanelRig> {
    let surface = InputSurface::install(&rig.hub);
    let state = PanelRig { panel: InputPanel::default(), surface, runner: Arc::clone(&rig.runner) };
    let mut h = harness_state(Settings::default(), size, state, |ui, r: &mut PanelRig| {
        // The studio's status bar draws the badge every frame: its heartbeat.
        r.surface.indicator().beat();
        r.panel.ui(ui, Some(&r.surface), r.runner.as_ref(), &you());
    });
    h.run_steps(2);
    h
}

/// Runs frames, a second of egui's clock each round so the panel reads again, until `done`.
fn until<S>(h: &mut Harness<'_, S>, what: &str, done: impl Fn(&Harness<'_, S>) -> bool) {
    let started = Instant::now();
    while !done(h) {
        assert!(started.elapsed() < Duration::from_secs(20), "{what} never happened: {:#?}", all_strings(h));
        std::thread::yield_now();
        h.run_steps(65);
    }
}

#[test]
fn a_preset_adds_its_bindings_and_with_capture_off_the_editor_says_why() {
    let rig = rig();
    let mut h = panel(&rig, vec2(360.0, 1600.0));
    assert!(says(&h, "No bindings yet"), "{:#?}", all_strings(&h));
    h.get_by_label("Add the locomotion preset").click();
    h.run_steps(2);
    for (name, _, _) in omuzaki_input_mock::LOCOMOTION_PRESET {
        assert!(says(&h, name), "{name}: {:#?}", all_strings(&h));
    }
    assert!(says(&h, "Jump") && says(&h, "Move"), "what each does");
    // The preset's keys are the hub's, shown here and only here.
    assert!(says(&h, "Left Shift"));
    // Capture is off, so keys can't be learnt: the panel says so, under the one Start.
    assert!(shows(&h, "it needs input capture on"), "{:#?}", all_strings(&h));
    let set = h.get_by_label("Set keys for jump");
    assert!(set.accesskit_node().is_disabled(), "Set keys waits for capture");
}

#[test]
fn learning_a_chord_stores_it_shows_it_here_and_no_op_carries_it() {
    let rig = rig();
    let mut h = panel(&rig, vec2(360.0, 1600.0));
    h.get_by_label("Add the locomotion preset").click();
    h.run_steps(2);
    h.get_by_label("Start").click();
    h.run_steps(2);
    assert!(state(&rig.hub).is_on(), "{:?}", rig.runner.seen());

    rig.hub.script_chord("Right Alt+J");
    h.get_by_label("Set keys for jump").click();
    h.run_steps(1);
    until(&mut h, "the learnt chord", |h| says(h, "Right Alt+J"));
    assert!(!h.state().panel.is_learning());
    let jump = rig.hub.bindings().into_iter().find(|b| b.name == "jump").unwrap();
    assert_eq!(keys(&rig.hub, jump.id), Some("Right Alt+J".to_owned()));

    // The keys reach the panel straight from the hub: no op asked for them, carried them or answered with them, and
    // the panel keeps none of them.
    for line in rig.runner.seen() {
        for (_, _, chord) in omuzaki_input_mock::LOCOMOTION_PRESET {
            assert!(!line.contains(&format!("\"{chord}\"")), "{chord} in {line}");
        }
        assert!(!line.contains("Right Alt"), "{line}");
    }
    let debug = format!("{:?} {:?}", h.state().panel, h.state().surface);
    assert!(!debug.contains("Right Alt") && !debug.contains("Space"), "{debug}");

    // Clear drops the keys; Remove removes the binding through its op.
    h.get_by_label("Clear keys for jump").click();
    h.run_steps(2);
    assert!(rig.hub.label(jump.id).is_none());
    h.get_by_label("Remove jump").click();
    h.run_steps(2);
    assert!(!rig.hub.bindings().iter().any(|b| b.name == "jump"));
    assert!(rig.runner.seen().iter().any(|l| l.starts_with("input.binding.remove")));
}

#[test]
fn a_conflicting_chord_is_shown_never_overwritten() {
    let rig = rig();
    let mut h = panel(&rig, vec2(360.0, 1600.0));
    h.get_by_label("Add the locomotion preset").click();
    h.run_steps(2);
    h.get_by_label("Start").click();
    h.run_steps(2);
    // "sit" learns jump's Space: both keep their keys, and both say they share them.
    rig.hub.script_chord("Space");
    h.get_by_label("Set keys for sit").click();
    h.run_steps(1);
    until(&mut h, "the conflict", |h| shows(h, "Shares its keys with another binding"));
    let conflicts = h.query_all_by_label_contains("Shares its keys").count();
    assert_eq!(conflicts, 2, "{:#?}", all_strings(&h));
    let bindings = rig.hub.bindings();
    let id = |name: &str| bindings.iter().find(|b| b.name == name).unwrap().id;
    assert_eq!(keys(&rig.hub, id("jump")).as_deref(), Some("Space"), "jump kept its keys");
    assert_eq!(keys(&rig.hub, id("sit")).as_deref(), Some("Space"));
}

#[test]
fn the_protected_list_round_trips_through_the_hub() {
    let rig = rig();
    let mut h = panel(&rig, vec2(360.0, 1200.0));
    assert!(says(&h, "No programs yet"));
    h.get_by_label("Program name").click();
    h.run_steps(1);
    h.get_by_label("Program name").type_text("vault");
    h.run_steps(1);
    h.get_by_label("Add").click();
    h.run_steps(2);
    assert_eq!(rig.hub.get(), vec!["vault".to_owned()]);
    assert!(says(&h, "vault"));

    // A protected program running pauses capture and the panel names it; the hub's own pause lifts when it closes.
    h.get_by_label("Start").click();
    h.run_steps(1);
    rig.hub.set_running(Some("vault"));
    rig.hub.tick();
    h.run_steps(1);
    assert!(says(&h, "Input capture: paused while vault runs"), "{:#?}", all_strings(&h));
    rig.hub.set_running(None);
    rig.hub.tick();
    assert!(state(&rig.hub).is_on());

    h.get_by_label("Remove vault").click();
    h.run_steps(2);
    assert_eq!(rig.hub.get(), Vec::<String>::new());
    // The list never went through an op, so no agent could read it.
    assert!(rig.runner.seen().iter().all(|l| !l.contains("vault")), "{:?}", rig.runner.seen());
}

#[test]
fn a_backend_waiting_for_a_permission_says_how_and_what_isnt_here() {
    let rig = rig();
    let mut caps = rig.hub.report().caps;
    caps.typing = Support::NeedsPermission;
    caps.pen = Support::Unavailable;
    rig.hub.set_caps(caps);
    let h = panel(&rig, vec2(360.0, 1200.0));
    assert!(says(&h, "keyboard: needs a permission"), "{:#?}", all_strings(&h));
    assert!(says(&h, "pen: not on this system"));
    assert!(!shows(&h, "mouse:"), "what works needs no words");
}

#[test]
fn without_the_hub_the_panel_says_input_is_off() {
    omuzaki_ui::link();
    let runner = omuzaki_ui::shell::stub::NullRunner;
    let mut panel = InputPanel::default();
    let h = harness_state(Settings::default(), vec2(360.0, 400.0), (), move |ui, ()| {
        panel.ui(ui, None, &runner, &you());
    });
    assert!(says(&h, "OS input isn't available"), "{:#?}", all_strings(&h));
}

/// The panel off with no bindings, waiting for a permission with the preset added, and on, side by side.
/// `OMUZAKI_EVIDENCE_OUT=<folder> cargo test -p omuzaki-ui --test input_surface -- --ignored`.
#[test]
#[ignore = "renders on the GPU (lavapipe in CI) and writes evidence images"]
fn input_surface_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    for (w, hgt) in [(1366.0, 768.0), (1920.0, 1080.0)] {
        // Off, nothing bound.
        let off = rig();
        // Waiting: the keyboard needs a permission here; the preset is added.
        let waiting = rig();
        let mut caps = waiting.hub.report().caps;
        caps.typing = Support::NeedsPermission;
        caps.gamepad = Support::Unavailable;
        waiting.hub.set_caps(caps);
        waiting.hub.apply_preset("locomotion").unwrap();
        // On, with the preset, and jump learnt onto walk forward's W: both say they share it.
        let on = rig();
        on.hub.apply_preset("locomotion").unwrap();
        let rigs = [off, waiting, on];
        let mut columns: Vec<_> = rigs
            .iter()
            .map(|r| PanelRig {
                panel: InputPanel::default(),
                surface: InputSurface::install(&r.hub),
                runner: Arc::clone(&r.runner),
            })
            .collect();
        for column in &columns {
            column.surface.indicator().beat();
        }
        rigs[2].hub.start(omuzaki_input_api::event::DeviceMask::ALL).unwrap();
        rigs[2].hub.set_detail(omuzaki_input_api::capture::Detail::HandOnly);
        rigs[2].hub.tick();
        let jump = rigs[2].hub.bindings().into_iter().find(|b| b.name == "jump").unwrap();
        rigs[2].hub.script_chord("W");
        rigs[2].hub.learn(jump.id).unwrap();
        let surfaces: Vec<_> = columns.iter().map(|c| c.surface.clone()).collect();
        let mut h = harness_state(Settings::default(), vec2(w, hgt), (), move |ui, ()| {
            ui.set_min_size(ui.available_size());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 24.0;
                for (i, column) in columns.iter_mut().enumerate() {
                    ui.vertical(|ui| {
                        ui.set_width(340.0);
                        // The status bar's badge over the panel, as the studio shows both.
                        ui.horizontal(|ui| {
                            omuzaki_ui::input::indicator::badge(ui, &surfaces[i], column.runner.as_ref(), &you());
                        });
                        ui.separator();
                        ui.push_id(i, |ui| column.panel.ui(ui, Some(&column.surface), column.runner.as_ref(), &you()));
                    });
                }
            });
        });
        h.run_steps(4);
        h.render().unwrap().save(out.join(format!("input-surface-{w}x{hgt}.png"))).unwrap();
    }
}
