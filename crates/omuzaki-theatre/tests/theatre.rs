//! The Theatre's book over the real executor: an agent's plan with its steps nested under the running item, reasons
//! and gate cards on the steps, the person's notes, the three pause modes pausing the agent's real session, the book
//! saved beside the project and read back, and dry runs that change nothing.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::needless_pass_by_value)]

use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, Budget, Event, Grants, Limits, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Outcome, Plan, Request, Runner,
    Scope, ServicesBuilder, SessionSpec, SessionState, Sessions, Transport, TxId, Undo, register_op,
};
use omuzaki_theatre::{ItemStatus, PauseMode, TheatreBook};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Sets a value at a path: the stand-in for a character op.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Set {
    path: DocPath,
    value: Value,
}

impl Op for Set {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.set",
            summary: "Set the value at a path.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = ctx.doc.get(&self.path).cloned();
        Ok(Plan::edit(Patch::new().with(self.path.clone(), before, Some(self.value.clone()))))
    }
}
register_op!(Set);

fn person() -> Actor {
    Actor::Person { name: "you".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "claude#1".into(), model: None }
}

/// The executor with the book in its services, the agent's session open, and the events the book will take.
struct Rig {
    exec: Arc<Executor>,
    book: TheatreBook,
    events: Receiver<Event>,
}

fn rig_with(book: TheatreBook) -> Rig {
    omuzaki_theatre::link();
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let doc = MemDoc::from_value(json!({ "face": {} }));
    let services = ServicesBuilder::new().insert::<TheatreBook>(Arc::new(book.clone())).build();
    let exec = Arc::new(Executor::new(Box::new(doc), clock).unwrap().with_services(services));
    let spec = SessionSpec {
        transport: Transport::Stdio,
        client: "claude".into(),
        model: None,
        on_behalf_of: person(),
        grants: Grants::person(),
        budget: Budget::default(),
        limits: Limits::default(),
        parent: None,
        role: None,
        fence: Vec::new(),
    };
    assert_eq!(exec.open(spec).unwrap().actor, agent());
    let events = exec.subscribe(&Grants::person()).unwrap();
    Rig { exec, book, events }
}

fn rig() -> Rig {
    rig_with(TheatreBook::new())
}

impl Rig {
    fn run_as(&self, actor: &Actor, op: &str, args: Value, mode: Mode) -> Result<Outcome, OpError> {
        let grants = if matches!(actor, Actor::Person { .. }) { Grants::person() } else { Grants::agent_default() };
        let request =
            Request { op: op.into(), args, mode, actor: actor.clone(), grants, cause: None, tx: None, base: None };
        self.exec.run(request)
    }

    /// Runs as the agent, then hands every event to the book as its watcher does, pausing what it says to pause.
    fn agent(&self, op: &str, args: Value) -> Result<Outcome, OpError> {
        let out = self.run_as(&agent(), op, args, Mode::Apply);
        self.settle();
        out
    }

    fn person(&self, op: &str, args: Value) -> Result<Outcome, OpError> {
        let out = self.run_as(&person(), op, args, Mode::Apply);
        self.settle();
        out
    }

    fn settle(&self) {
        for event in self.events.try_iter() {
            for who in self.book.take(&event) {
                self.book.pause(self.exec.as_ref(), &who);
            }
        }
    }

    fn edit(&self, at: &str, value: Value) {
        self.agent("test.set", json!({ "path": at, "value": value })).unwrap();
    }

    fn paused(&self) -> bool {
        matches!(self.exec.session_of(&agent()).unwrap().state, SessionState::Paused { .. })
    }

    fn resume(&self) {
        let session = self.exec.session_of(&agent()).unwrap();
        self.exec.resume(session.id, &person()).unwrap();
    }
}

fn answer(outcome: Outcome) -> Value {
    outcome.plan.output.unwrap_or(Value::Null)
}

#[test]
fn an_agents_steps_nest_under_its_running_item_with_their_reasons() {
    let rig = rig();
    let plan = json!({ "items": [
        { "id": "face", "text": "Make the face   more\u{202E} oval" },
        { "id": "hair", "text": "Add a fringe" }
    ] });
    rig.agent("theatre.plan", plan).unwrap();
    rig.agent("theatre.item", json!({ "id": "face", "status": "running" })).unwrap();
    rig.edit("face/shape", json!("oval"));
    rig.agent("theatre.explain", json!({ "text": "The reference's chin is narrower." })).unwrap();
    rig.edit("face/jaw", json!(0.4));
    // A query and a Theatre op are not steps.
    rig.agent("theatre.status", json!({})).unwrap();

    let book = rig.book.snapshot();
    let plan = book.plan(&agent()).unwrap();
    assert_eq!(plan.items[0].text, "Make the face more oval", "text is cleaned on the way in");
    assert_eq!(plan.running().map(|i| i.id.as_str()), Some("face"));
    assert_eq!(book.steps.len(), 2);
    assert_eq!(plan.items[0].steps, book.steps.iter().map(|s| s.tx).collect::<Vec<_>>());
    assert!(book.steps.iter().all(|s| s.item.as_deref() == Some("face") && s.actor == agent() && s.ops == 1));
    assert_eq!(book.steps[0].rationale.as_deref(), Some("The reference's chin is narrower."));
    assert_eq!(book.steps[1].rationale, None);

    // Starting the next item stops the first; its steps stay with it when the plan is posted again.
    rig.agent("theatre.item", json!({ "id": "hair", "status": "running" })).unwrap();
    rig.edit("face/fringe", json!(9));
    let replan = json!({ "items": [
        { "id": "face", "text": "Make the face more oval", "status": "done" },
        { "id": "hair", "text": "Add a fringe" },
        { "id": "eyes", "text": "Brighter eyes" }
    ] });
    rig.agent("theatre.plan", replan).unwrap();
    let book = rig.book.snapshot();
    let plan = book.plan(&agent()).unwrap();
    let ids: Vec<_> = plan.items.iter().map(|i| (i.id.as_str(), i.status, i.steps.len())).collect();
    assert_eq!(
        ids,
        [("face", ItemStatus::Done, 2), ("hair", ItemStatus::Running, 1), ("eyes", ItemStatus::Planned, 0)]
    );
}

#[test]
fn undo_and_redo_mark_the_card() {
    let rig = rig();
    rig.edit("face/shape", json!("oval"));
    rig.exec.undo(&agent(), &Grants::agent_default()).unwrap();
    rig.settle();
    assert!(rig.book.snapshot().steps[0].undone);
    rig.exec.redo(&agent(), &Grants::agent_default()).unwrap();
    rig.settle();
    assert!(!rig.book.snapshot().steps[0].undone);
}

#[test]
fn every_step_mode_pauses_the_agents_session_after_each_step_and_resume_lets_it_go_on() {
    let rig = rig();
    rig.person("theatre.pause_mode", json!({ "agent": "claude#1", "mode": "every_step" })).unwrap();
    assert!(!rig.paused());
    rig.edit("face/shape", json!("oval"));
    assert!(rig.paused());
    let refused = rig.agent("test.set", json!({ "path": "face/jaw", "value": 1 }));
    assert!(refused.is_err(), "a paused agent's next edit is refused");
    rig.resume();
    rig.edit("face/jaw", json!(1));
    assert!(rig.paused());
    rig.resume();

    // Off again: the agent runs free.
    rig.person("theatre.pause_mode", json!({ "agent": "claude#1", "mode": "off" })).unwrap();
    rig.edit("face/jaw", json!(2));
    assert!(!rig.paused());
}

#[test]
fn each_item_mode_pauses_when_an_item_finishes_and_step_pauses_once() {
    let rig = rig();
    rig.person("theatre.pause_mode", json!({ "agent": "claude#1", "mode": "each_item" })).unwrap();
    rig.agent("theatre.plan", json!({ "items": [{ "id": "face", "text": "Oval face", "status": "running" }] }))
        .unwrap();
    rig.edit("face/shape", json!("oval"));
    assert!(!rig.paused(), "a step inside an item does not pause");
    let done = answer(rig.agent("theatre.item", json!({ "id": "face", "status": "needs_review" })).unwrap());
    assert_eq!(done["paused"], json!(true));
    assert!(rig.paused());
    rig.resume();

    // The person's Step: let it go on for one step, then pause again.
    rig.book.pause_after_next(&agent());
    rig.edit("face/jaw", json!(1));
    assert!(rig.paused());
    rig.resume();
    rig.edit("face/jaw", json!(2));
    assert!(!rig.paused());
}

#[test]
fn a_failed_gate_becomes_a_card_fails_the_item_and_pauses_under_the_gates_mode() {
    let rig = rig();
    rig.person("theatre.pause_mode", json!({ "agent": "claude#1", "mode": "gates" })).unwrap();
    rig.agent("theatre.plan", json!({ "items": [{ "id": "eyes", "text": "Match the eyes", "status": "running" }] }))
        .unwrap();
    rig.edit("face/eyes", json!(0.3));
    assert!(!rig.paused());
    let gate = json!({
        "gate": "likeness",
        "message": "The eyes sit 8% lower than the reference's.",
        "evidence": "omuzaki://render/5f2c.png"
    });
    rig.agent("theatre.gate", gate).unwrap();
    assert!(rig.paused());
    let book = rig.book.snapshot();
    let failure = book.steps[0].gate.clone().unwrap();
    assert_eq!((failure.gate.as_str(), failure.evidence.as_deref()), ("likeness", Some("omuzaki://render/5f2c.png")));
    assert_eq!(book.plan(&agent()).unwrap().items[0].status, ItemStatus::FailedGate);
}

#[test]
fn only_the_person_directs_and_sets_pause_modes_and_notes_reach_the_agent_as_data() {
    let rig = rig();
    let note = json!({
        "text": "make this rounder",
        "region": { "x": 0.4, "y": 0.2, "w": 0.2, "h": 0.15 },
        "part": "head",
        "camera": { "yaw": 15.0, "pitch": 0.0, "distance": 1.2, "target": [0.0, 1.5, 0.0] }
    });
    assert!(rig.agent("theatre.direct", note.clone()).is_err(), "an agent cannot write the person's notes");
    assert!(rig.agent("theatre.pause_mode", json!({ "agent": "claude#1", "mode": "off" })).is_err());
    let outside = json!({ "text": "here", "region": { "x": 0.9, "y": 0.2, "w": 0.2, "h": 0.1 } });
    assert!(rig.person("theatre.direct", outside).is_err(), "a region outside the viewport is refused");

    assert_eq!(answer(rig.person("theatre.direct", note).unwrap())["note"], json!(1));
    let notes = answer(rig.agent("theatre.notes", json!({})).unwrap());
    assert_eq!(notes["about"], json!(omuzaki_theatre::ops::NOTES_ARE_DATA));
    assert_eq!(notes["notes"][0]["text"], json!("make this rounder"));
    assert_eq!(notes["notes"][0]["part"], json!("head"));
    assert_eq!(notes["notes"][0]["by"], serde_json::to_value(person()).unwrap());

    rig.agent("theatre.take", json!({ "id": 1 })).unwrap();
    assert!(rig.agent("theatre.take", json!({ "id": 1 })).is_err(), "a note is taken once");
    assert_eq!(answer(rig.agent("theatre.notes", json!({})).unwrap())["notes"], json!([]));
    assert_eq!(answer(rig.agent("theatre.notes", json!({ "all": true })).unwrap())["notes"][0]["taken"], json!(true));
}

#[test]
fn dry_runs_change_nothing() {
    let rig = rig();
    let plan = json!({ "items": [{ "id": "face", "text": "Oval face" }] });
    rig.run_as(&agent(), "theatre.plan", plan.clone(), Mode::DryRun).unwrap();
    let mode = json!({ "agent": "claude#1", "mode": "every_step" });
    rig.run_as(&person(), "theatre.pause_mode", mode, Mode::DryRun).unwrap();
    let direct = json!({ "text": "rounder", "region": { "x": 0.1, "y": 0.1, "w": 0.1, "h": 0.1 } });
    rig.run_as(&person(), "theatre.direct", direct, Mode::DryRun).unwrap();
    rig.run_as(&person(), "theatre.reference", json!({ "picture": "omuzaki://render/a.png" }), Mode::DryRun).unwrap();
    rig.settle();
    assert_eq!(rig.book.snapshot(), omuzaki_theatre::Book::default());
    assert_eq!(rig.book.revision(), 0);

    // The same plan, applied, is checked the same way.
    rig.agent("theatre.plan", plan).unwrap();
    assert_eq!(rig.book.snapshot().plans.len(), 1);
    assert!(rig.agent("theatre.item", json!({ "id": "nose", "status": "running" })).is_err());
    let twice = json!({ "items": [{ "id": "a", "text": "x" }, { "id": "a", "text": "y" }] });
    assert!(rig.agent("theatre.plan", twice).is_err());
}

#[test]
fn the_book_is_saved_beside_the_project_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let rig = rig_with(TheatreBook::for_project(dir.path()));
    rig.agent("theatre.plan", json!({ "items": [{ "id": "face", "text": "Oval face", "status": "running" }] }))
        .unwrap();
    rig.edit("face/shape", json!("oval"));
    rig.agent("theatre.explain", json!({ "text": "Narrower chin." })).unwrap();
    rig.person("theatre.pause_mode", json!({ "agent": "claude#1", "mode": "gates" })).unwrap();
    rig.book.flush();
    let before = rig.book.snapshot();

    let reopened = TheatreBook::for_project(dir.path());
    assert_eq!(reopened.snapshot(), before);
    assert_eq!(reopened.snapshot().mode(&agent()), PauseMode::Gates);
    assert!(dir.path().join(omuzaki_theatre::book::BOOK_FILE).is_file());
}

#[test]
fn the_watcher_takes_steps_and_pauses_on_its_own_thread() {
    omuzaki_theatre::link();
    let book = TheatreBook::new();
    let rig = rig_with(book.clone());
    let runner: Arc<dyn Runner> = rig.exec.clone();
    book.attach(&runner, rig.exec.clone()).unwrap();
    rig.run_as(&person(), "theatre.pause_mode", json!({ "agent": "claude#1", "mode": "every_step" }), Mode::Apply)
        .unwrap();
    rig.run_as(&agent(), "test.set", json!({ "path": "face/shape", "value": "oval" }), Mode::Apply).unwrap();
    // The watcher answers on its own thread: wait for its pause, with a deadline.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !rig.paused() {
        assert!(Instant::now() < deadline, "the watcher did not pause the agent");
        std::thread::yield_now();
    }
    assert_eq!(book.snapshot().steps.iter().map(|s| s.tx).collect::<Vec<TxId>>().len(), 1);
}
