//! The Build Theatre (spec §13, `shell.md` section 11) over the studio's real executor and the Theatre's book: an agent
//! in a real session posts its plan and works, and the plan board, the steps and the stage show it and steer it.
//!
//! - TH-02, TH-03: the plan board lists each item with its status and step count, and choosing one filters the steps.
//! - TH-06: in "after each step" mode the agent's newest card offers Approve and Reject; Approve resumes the session,
//!   Reject sends the comment to the agent as a note on the step and resumes it. Pause, Step and Resume on the stage
//!   act on the real session; the pause mode is set through `theatre.pause_mode`.
//! - TH-08, TH-11: a reason shows under its card, Explain says when there is none, and a failed gate is a gate card.
//! - TH-10: the budget meter shows what the session spent against its limits.
//! - TH-09: drawing on the stage's viewport (on the software GPU) sends a note with its region, part and camera.
//!
//! The book takes the runner's events in the test's own thread, as its watcher does on its own, so every test is
//! ordered without a wait.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;
use std::sync::mpsc::Receiver;

use egui::{Event as UiEvent, Modifiers, PointerButton, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_char_api::{BuildServices, CharId, RegistryBuilder};
use omuzaki_ops_api::{
    Actor, Budget, Event, Grants, Limits, Mode, OpError, Outcome, Request, ServicesBuilder, SessionSpec, SessionState,
    Transport,
};
use omuzaki_theatre::{ItemStatus, TheatreBook};
use omuzaki_ui::ds::testing::{all_strings, unlabelled_interactive};
use omuzaki_ui::panels::agent::AgentFeed;
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::io::Io;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::theatre::Theatre;
use omuzaki_ui::theatre::plan::PlanBoard;
use omuzaki_ui::theatre::stage::{StagePanel, Views as StageViews};
use omuzaki_ui::theatre::steps::{StepsView, Views};
use omuzaki_ui::wiring::{Core, Document, core_on};
use serde_json::{Value, json};

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

/// The studio's core with the book in its services and one agent session working for the person.
struct Studio {
    core: Core,
    book: TheatreBook,
    agent: Actor,
    /// The book's own copy of the runner's events, taken by [`settle`](Self::settle).
    taken: Receiver<Event>,
}

fn studio() -> Studio {
    omuzaki_ui::link();
    let book = TheatreBook::new();
    let services = ServicesBuilder::new().insert::<TheatreBook>(Arc::new(book.clone())).build();
    let core = core_on(Document::empty(), services).unwrap();
    let spec = SessionSpec {
        transport: Transport::Stdio,
        client: "claude".into(),
        model: None,
        on_behalf_of: you(),
        grants: Grants::person(),
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: Vec::new(),
    };
    let agent = core.sessions.open(spec).unwrap().actor;
    let taken = core.runner.subscribe(&Grants::person()).unwrap();
    Studio { core, book, agent, taken }
}

impl Studio {
    fn run_as(&self, actor: &Actor, op: &str, args: Value) -> Result<Outcome, OpError> {
        let grants = if matches!(actor, Actor::Person { .. }) { Grants::person() } else { Grants::agent_default() };
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
        let out = self.core.runner.run(request);
        self.settle();
        out
    }

    /// Runs `op` as the agent.
    fn agent(&self, op: &str, args: Value) -> Outcome {
        self.run_as(&self.agent.clone(), op, args).unwrap()
    }

    /// The agent makes a character: one step that changes the project.
    fn step(&self, id: &str) {
        self.agent("character.create", json!({ "id": id, "name": id, "template": "soft-sd" }));
    }

    /// Hands the runner's events to the book, pausing what it says to pause, as its watcher does.
    fn settle(&self) {
        for event in self.taken.try_iter() {
            for agent in self.book.take(&event) {
                self.book.pause(self.core.sessions.as_ref(), &agent);
            }
        }
    }

    fn paused(&self) -> bool {
        let session = self.core.sessions.session_of(&self.agent).unwrap();
        matches!(session.state, SessionState::Paused { .. })
    }
}

/// The Theatre's steps beside its plan board, over the studio, with the shell's views as the app keeps them.
struct Rig {
    studio: Studio,
    theatre: Theatre,
    events: Receiver<Event>,
    feed: AgentFeed,
    plan: PlanBoard,
    steps: StepsView,
    stage: StagePanel,
    viewport: Option<ViewportServices>,
}

fn rig_on(studio: Studio, viewport: Option<ViewportServices>, focus: EditFocus) -> Rig {
    let theatre = Theatre::new(studio.book.clone(), None);
    let events = studio.core.runner.subscribe(&Grants::person()).unwrap();
    let feed = AgentFeed::load(studio.core.sessions.as_ref());
    Rig {
        studio,
        theatre,
        events,
        feed,
        plan: PlanBoard::default(),
        steps: StepsView::default(),
        stage: StagePanel::new(focus),
        viewport,
    }
}

fn frame(ui: &mut egui::Ui, rig: &mut Rig) {
    omuzaki_ui::ds::apply_settings(ui.ctx(), omuzaki_ui::ds::Settings::default());
    let events: Vec<Event> = rig.events.try_iter().collect();
    rig.feed.follow(&events);
    let actor = you();
    let io = Io::new(rig.studio.core.runner.as_ref(), &actor, &events);
    let sessions = Arc::clone(&rig.studio.core.sessions);
    let views = Views {
        sessions: Some(&sessions),
        feed: Some(&rig.feed),
        log: None,
        preview: None,
        pictures: None,
        requests: None,
    };
    let stage =
        StageViews { sessions: Some(&sessions), feed: Some(&rig.feed), services: rig.viewport.as_ref(), preview: None };
    egui::Panel::left("plan").exact_size(320.0).resizable(false).show(ui, |ui| rig.plan.ui(ui, &rig.theatre));
    egui::Panel::right("steps")
        .exact_size(460.0)
        .resizable(false)
        .show(ui, |ui| rig.steps.ui(ui, &rig.theatre, io, views));
    egui::CentralPanel::default().show(ui, |ui| rig.stage.show(ui, &rig.theatre, io, stage));
}

fn harness(studio: Studio) -> Harness<'static, Rig> {
    let rig = rig_on(studio, None, EditFocus::default());
    let mut h = Harness::builder().with_size(vec2(1600.0, 1000.0)).build_ui_state(frame, rig);
    h.run_steps(3);
    h
}

fn click(h: &mut Harness<'_, Rig>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(3);
}

fn shows(h: &Harness<'_, Rig>, text: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(text))
}

fn plan(studio: &Studio) {
    let items = json!({ "items": [
        { "id": "face", "text": "Make the face more oval", "status": "running" },
        { "id": "hair", "text": "Add a fringe" }
    ] });
    studio.agent("theatre.plan", items);
}

#[test]
fn the_plan_board_lists_each_item_and_choosing_one_filters_the_steps() {
    let studio = studio();
    plan(&studio);
    studio.step("kit");
    studio.step("kit_two");
    studio.agent("theatre.item", json!({ "id": "face", "status": "done" }));
    studio.agent("theatre.item", json!({ "id": "hair", "status": "running" }));
    studio.step("kit_three");
    let mut h = harness(studio);
    assert!(h.query_by_label("Make the face more oval, Done: show its steps").is_some(), "{:?}", all_strings(&h));
    assert!(h.query_by_label("Add a fringe, Running: show its steps").is_some());
    assert!(shows(&h, "Done \u{b7} 2 steps"));
    assert!(shows(&h, "Running \u{b7} 1 step"));
    let cards = |h: &Harness<'_, Rig>| h.query_all_by_label_contains("Rewind: ").count();
    assert_eq!(cards(&h), 3, "every step has a card");

    click(&mut h, "Make the face more oval, Done: show its steps");
    assert!(shows(&h, "Steps of: Make the face more oval"));
    assert_eq!(cards(&h), 2, "only the item's steps");
    click(&mut h, "Show all steps");
    assert_eq!(cards(&h), 3);
    assert!(unlabelled_interactive(&h).is_empty(), "{:?}", unlabelled_interactive(&h));
}

#[test]
fn after_each_step_the_newest_card_asks_for_review_and_the_comment_reaches_the_agent() {
    let studio = studio();
    studio.run_as(&you(), "theatre.pause_mode", json!({ "agent": "claude#1", "mode": "every_step" })).unwrap();
    studio.step("kit");
    assert!(studio.paused(), "the agent pauses after its step");
    let label = studio.book.snapshot().steps[0].label.clone();
    let mut h = harness(studio);
    click(&mut h, &format!("Approve: {label}"));
    assert!(!h.state().studio.paused(), "Approve resumes the agent");

    h.state().studio.step("kit_two");
    h.run_steps(3);
    assert!(h.state().studio.paused());
    let second = h.state().studio.book.snapshot().steps[1].label.clone();
    click(&mut h, &format!("Reject: {second}"));
    h.get_by_label("What should change?").click();
    h.run_steps(1);
    h.event(UiEvent::Text("keep the first name".into()));
    h.run_steps(2);
    click(&mut h, "Send to the agent");
    let studio = &h.state().studio;
    assert!(!studio.paused(), "the agent resumes to act on the comment");
    let notes = studio.agent("theatre.notes", json!({})).plan.output.unwrap();
    let note = &notes["notes"][0];
    assert_eq!(note["text"], json!(format!("Rejected the step {second}: keep the first name")));
    assert_eq!(note["step"], json!(studio.book.snapshot().steps[1].tx));
}

#[test]
fn a_reason_shows_under_its_card_and_a_failed_gate_is_a_gate_card() {
    let studio = studio();
    plan(&studio);
    studio.step("kit");
    studio.agent("theatre.explain", json!({ "text": "The sheet's face is narrower." }));
    studio.step("kit_two");
    let gate = json!({ "gate": "likeness", "message": "The eyes sit 8% lower than the sheet's." });
    studio.agent("theatre.gate", gate);
    let label = studio.book.snapshot().steps[1].label.clone();
    let mut h = harness(studio);
    assert!(shows(&h, "Why: The sheet's face is narrower."));
    assert!(shows(&h, "likeness: The eyes sit 8% lower than the sheet's."));
    assert!(shows(&h, "Gate failed"), "the item and the card say so: {:?}", all_strings(&h));
    assert_eq!(h.state().studio.book.snapshot().plans[0].items[0].status, ItemStatus::FailedGate);
    assert!(!shows(&h, "No reason given yet"));
    // Both steps share their op's label; the newest card comes first.
    h.get_all_by_label(&format!("Explain: {label}")).next().unwrap().click();
    h.run_steps(3);
    assert!(shows(&h, "No reason given yet"), "Explain says when there is none");
}

#[test]
fn the_stage_pauses_steps_and_resumes_the_real_session_and_shows_its_limits() {
    let studio = studio();
    let session = studio.core.sessions.session_of(&studio.agent).unwrap();
    let budget = Budget { usd_micros: 2_000_000, tokens: Some(20_000), seconds: Some(600) };
    studio.core.sessions.set_budget(session.id, budget, &you()).unwrap();
    studio.step("kit");
    let mut h = harness(studio);
    assert!(shows(&h, "$0.00 of $2.00"), "{:?}", all_strings(&h));
    assert!(shows(&h, "0 of 20000 tokens"));
    assert!(shows(&h, "0:00 of 10:00"));
    click(&mut h, "Pause");
    assert!(h.state().studio.paused());
    click(&mut h, "Step");
    assert!(!h.state().studio.paused(), "Step lets the agent go on");
    h.state().studio.step("kit_two");
    h.run_steps(3);
    assert!(h.state().studio.paused(), "and pauses it after one step");
    click(&mut h, "Resume");
    assert!(!h.state().studio.paused());
    h.state().studio.step("kit_three");
    assert!(!h.state().studio.paused(), "a resumed agent in the default mode runs on");

    // The limits open with the session's own, and setting them sets the session's budget.
    click(&mut h, "Limits");
    click(&mut h, "Set limits");
    let studio = &h.state().studio;
    assert_eq!(studio.core.sessions.session_of(&studio.agent).unwrap().budget, budget);
    assert!(unlabelled_interactive(&h).is_empty(), "{:?}", unlabelled_interactive(&h));
}

/// TH-09 on the software GPU: a drag over the character circles a region, and Send hands the note to the agents with
/// the region, the part under it and the camera. Without a Vulkan adapter it says so and passes.
#[test]
fn drawing_on_the_stage_sends_a_note_with_its_region_part_and_camera() {
    let state = std::panic::catch_unwind(support::render_state).ok();
    let Some(state) = state.filter(|s| s.adapter.get_info().backend == wgpu::Backend::Vulkan) else {
        eprintln!("no Vulkan adapter (install mesa-vulkan-drivers): the drawing test skips");
        return;
    };
    let studio = studio();
    studio.run_as(&you(), "character.create", json!({ "id": "kit", "name": "Kit", "template": "soft-sd" })).unwrap();
    let services = ViewportServices {
        gpu: Some(state.clone()),
        builder: Arc::new(RegistryBuilder),
        build: BuildServices::default(),
    };
    let focus = EditFocus::new(CharId::new("kit").unwrap());
    let rig = rig_on(studio, Some(services), focus);
    let mut h = Harness::builder()
        .with_size(vec2(1600.0, 1000.0))
        .renderer(egui_kittest::wgpu::WgpuTestRenderer::from_render_state(state))
        .build_ui_state(frame, rig);
    h.run_steps(4);
    click(&mut h, "Draw to direct");
    let rect = h.state().stage_rect().expect("the viewport is drawn");
    let (from, to) = (rect.center() + vec2(-40.0, -160.0), rect.center() + vec2(40.0, -80.0));
    h.event(UiEvent::PointerMoved(from));
    h.event(UiEvent::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(1);
    for step in 1..=4_u8 {
        let at = from + (to - from) * (f32::from(step) / 4.0);
        h.event(UiEvent::PointerMoved(at));
        h.run_steps(1);
    }
    h.event(UiEvent::PointerButton {
        pos: to,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(2);
    h.get_by_label("What should change here?").click();
    h.run_steps(1);
    h.event(UiEvent::Text("make this rounder".into()));
    h.run_steps(2);
    click(&mut h, "Send note");
    assert!(shows(&h, "Sent 1 notes to the agents"), "{:?}", all_strings(&h));
    let notes = h.state().studio.agent("theatre.notes", json!({})).plan.output.unwrap();
    let note = &notes["notes"][0];
    assert_eq!(note["text"], json!("make this rounder"));
    let region = &note["region"];
    let share = |v: &Value| v.as_f64().unwrap();
    assert!((share(&region["w"]) * f64::from(rect.width()) - 80.0).abs() < 2.0, "{region}");
    assert!((share(&region["h"]) * f64::from(rect.height()) - 80.0).abs() < 2.0, "{region}");
    assert!(note["camera"]["distance"].as_f64().unwrap() > 0.0);
    assert!(note["part"].is_string(), "the part under the region: {note}");
}

impl Rig {
    fn stage_rect(&self) -> Option<egui::Rect> {
        self.stage.viewport_rect()
    }
}
