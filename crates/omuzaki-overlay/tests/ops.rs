//! The overlay and program ops end to end, the way an agent drives them over MCP: widgets added and changed in the
//! document, the compositor following it, the show's events moving the widgets, and a scene switch blending the
//! held frame into the new scene.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use glam::UVec2;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError, Source};
use omuzaki_live_api::ids::EventKind;
use omuzaki_ops_api::{Actor, OpCtx, OpKind, Scope, Services, ServicesBuilder, Undo, find};
use omuzaki_overlay::{ProgramService, Tee};
use omuzaki_render_api::{AlphaEncoding, Image};
use serde_json::{Value, json};

struct Studio {
    doc: MemDoc,
    clock: Arc<FakeClock>,
    program: Arc<ProgramService>,
    services: Services,
}

impl Studio {
    fn new() -> Self {
        omuzaki_overlay::link();
        let clock = Arc::new(FakeClock::new(Seconds(0.0)));
        let program = Arc::new(ProgramService::new(clock.clone()).unwrap());
        let services = ServicesBuilder::new().insert::<ProgramService>(Arc::clone(&program)).build();
        let doc = MemDoc::from_value(json!({ "scenes": { "main": {}, "brb": {} } }));
        Self { doc, clock, program, services }
    }

    /// Runs `name` with `args` as an agent and applies its patch; returns the output, or the error's text.
    fn run(&mut self, name: &str, args: Value) -> Result<Value, String> {
        let reg = find(name).unwrap_or_else(|| panic!("{name} is registered"));
        let op = (reg.decode)(args).map_err(|e| e.to_string())?;
        let actor = Actor::Agent { id: "helper#1".into(), model: None };
        let plan = {
            let ctx = OpCtx::new(&self.doc, &actor, &*self.clock).with_services(&self.services);
            op.plan(&ctx).map_err(|e| e.to_string())?
        };
        if !plan.patch.is_empty() {
            self.doc.apply(&plan.patch).unwrap();
            self.program.follow(&self.doc);
        }
        Ok(plan.output.unwrap_or(Value::Null))
    }

    fn tick(&self, seconds: f64) {
        self.clock.advance(Seconds(seconds));
    }
}

fn frame(rgba: [u8; 4]) -> Image {
    Image::new(UVec2::new(640, 360), AlphaEncoding::Straight, rgba.repeat(640 * 360)).unwrap()
}

fn event(kind: &str, payload: Value) -> LiveEvent {
    LiveEvent::new(EventKind::new(kind).unwrap(), Source::Input, Seconds(0.0), payload)
}

fn user(name: &str) -> Value {
    json!({ "id": name, "login": name, "name": name })
}

/// Pixels that differ between two frames.
fn changed(a: &Image, b: &Image) -> usize {
    a.rgba8.chunks(4).zip(b.rgba8.chunks(4)).filter(|(x, y)| x != y).count()
}

/// The show's end of the tee: it records what it was given.
struct Show(Mutex<Vec<String>>);
impl EventSink for Show {
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError> {
        self.0.lock().unwrap().push(event.kind.as_str().to_owned());
        Ok(())
    }
    fn producer_dropped(&self, _: u64) {}
}

#[test]
fn the_ops_have_the_scopes_and_undo_their_kind_needs() {
    omuzaki_overlay::link();
    for (name, kind, scope, undo) in [
        ("overlay.kinds", OpKind::Query, Scope::Read, Undo::None),
        ("overlay.list", OpKind::Query, Scope::Read, Undo::None),
        ("overlay.add", OpKind::Edit, Scope::EditScene, Undo::Auto),
        ("overlay.set", OpKind::Edit, Scope::EditScene, Undo::Auto),
        ("overlay.remove", OpKind::Edit, Scope::EditScene, Undo::Auto),
        ("overlay.show", OpKind::Edit, Scope::EditScene, Undo::Auto),
        ("overlay.hide", OpKind::Edit, Scope::EditScene, Undo::Auto),
        ("overlay.alert", OpKind::Edit, Scope::Broadcast, Undo::None),
        ("overlay.timer", OpKind::Edit, Scope::Broadcast, Undo::None),
        ("overlay.goal", OpKind::Edit, Scope::Broadcast, Undo::None),
        ("program.switch_scene", OpKind::Edit, Scope::Broadcast, Undo::None),
        ("program.set_transition", OpKind::Edit, Scope::EditScene, Undo::Auto),
        ("program.transitions", OpKind::Query, Scope::Read, Undo::None),
        ("program.status", OpKind::Query, Scope::Read, Undo::None),
    ] {
        let meta = (find(name).unwrap_or_else(|| panic!("{name} is registered")).meta)();
        assert_eq!((meta.kind, meta.scope, meta.undo), (kind, scope, undo), "{name}");
    }
}

#[test]
fn an_agent_builds_an_overlay_and_the_widgets_draw_from_the_shows_events() {
    let mut s = Studio::new();
    s.run("overlay.add", json!({ "id": "alerts", "kind": "alerts" })).unwrap();
    s.run("overlay.add", json!({ "id": "chat", "kind": "chat", "settings": { "lines": 4 } })).unwrap();
    s.run(
        "overlay.add",
        json!({ "id": "goal", "kind": "goal", "settings": { "title": "Subs", "target": 10 }, "style": { "accent": "#7ad7ff" } }),
    )
    .unwrap();
    let list = s.run("overlay.list", json!({})).unwrap();
    assert_eq!(list["live"]["widgets"].as_array().map(Vec::len), Some(3), "{list}");
    assert_eq!(list["definitions"]["goal"]["accent"], Value::Null, "style is its own table: {list}");
    assert_eq!(list["definitions"]["goal"]["style"]["accent"], "#7ad7ff");

    // Nothing has happened yet: only the goal bar draws (alerts and an empty chat draw nothing).
    let empty = frame([0, 0, 0, 0]);
    let mut first = empty.clone();
    s.program.compose(&mut first);
    let goal_only = changed(&empty, &first);
    assert!(goal_only > 1000, "the goal bar draws: {goal_only} pixels");

    // Events reach the widgets through the tee in front of the show.
    let show = Arc::new(Show(Mutex::new(Vec::new())));
    let tee = Tee::new(show.clone(), Arc::clone(&s.program));
    tee.publish(event("sub", json!({ "user": user("Ann"), "tier": "1000", "months": 2 }))).unwrap();
    tee.publish(event("chat.message", json!({ "user": user("Bo"), "text": "hello chat", "message_id": "m1" })))
        .unwrap();
    assert_eq!(*show.0.lock().unwrap(), ["sub", "chat.message"], "the show still gets every event");

    // The alert comes in over its first third of a second.
    let mut second = empty.clone();
    s.program.compose(&mut second);
    s.tick(1.0);
    second = empty.clone();
    s.program.compose(&mut second);
    assert!(changed(&empty, &second) > goal_only + 3000, "the alert and the chat line are drawn too");
    let list = s.run("overlay.list", json!({})).unwrap();
    let goal = list["live"]["widgets"].as_array().unwrap().iter().find(|w| w["id"] == "goal").unwrap().clone();
    assert_eq!(goal["state"]["value"], 1.0, "{goal}");

    // The alert goes after its five seconds, the chat line after its minute.
    s.tick(6.0);
    let mut later = empty.clone();
    s.program.compose(&mut later);
    assert!(changed(&empty, &later) < changed(&empty, &second));
}

#[test]
fn edits_are_checked_and_keep_the_live_state() {
    let mut s = Studio::new();
    s.run("overlay.add", json!({ "id": "goal", "kind": "goal" })).unwrap();
    s.run("overlay.goal", json!({ "id": "goal", "value": 7 })).unwrap();
    s.run("overlay.set", json!({ "id": "goal", "changes": { "target": 20, "layout": { "anchor": "top_right" } } }))
        .unwrap();
    let list = s.run("overlay.list", json!({})).unwrap();
    assert_eq!(list["definitions"]["goal"]["target"], 20.0);
    assert_eq!(list["live"]["widgets"][0]["state"]["value"], 7.0, "a changed goal keeps its value");

    let bad = s.run("overlay.set", json!({ "id": "goal", "changes": { "style": { "accent": "pink" } } })).unwrap_err();
    assert!(bad.contains("not a #rrggbb colour"), "{bad}");
    let bad = s.run("overlay.set", json!({ "id": "goal", "changes": { "kind": "text" } })).unwrap_err();
    assert!(bad.contains("keeps its kind"), "{bad}");
    let dup = s.run("overlay.add", json!({ "id": "goal", "kind": "text" })).unwrap_err();
    assert!(dup.contains("already"), "{dup}");

    s.run("overlay.hide", json!({ "id": "goal" })).unwrap();
    let empty = frame([0, 0, 0, 0]);
    let mut out = empty.clone();
    s.program.compose(&mut out);
    assert_eq!(changed(&empty, &out), 0, "a hidden widget draws nothing");
    s.run("overlay.remove", json!({ "id": "goal" })).unwrap();
    assert_eq!(s.run("overlay.list", json!({})).unwrap()["live"]["widgets"], json!([]));
}

#[test]
fn a_subathon_timer_runs_from_ops_and_events() {
    let mut s = Studio::new();
    s.run(
        "overlay.add",
        json!({ "id": "subathon", "kind": "timer", "settings": { "mode": "subathon", "seconds": 3600 } }),
    )
    .unwrap();
    s.run("overlay.timer", json!({ "id": "subathon", "action": "start" })).unwrap();
    s.program.observe(&event("sub_gift", json!({ "count": 5, "tier": "1000" })));
    s.run("overlay.timer", json!({ "id": "subathon", "action": "add", "seconds": 120 })).unwrap();
    let mut out = frame([0, 0, 0, 0]);
    s.program.compose(&mut out);
    s.tick(30.0);
    s.program.compose(&mut out);
    let list = s.run("overlay.list", json!({})).unwrap();
    // 3600 + 5 gifts x 60 + 120 - 30
    assert_eq!(list["live"]["widgets"][0]["state"]["seconds"], 3990.0, "{list}");
    let wrong = s.run("overlay.goal", json!({ "id": "subathon", "value": 1 })).unwrap_err();
    assert!(wrong.contains("not a goal"), "{wrong}");
}

#[test]
fn a_custom_alert_needs_an_alerts_widget() {
    let mut s = Studio::new();
    let none = s.run("overlay.alert", json!({ "title": "Hydrate!" })).unwrap_err();
    assert!(none.contains("no alerts widget"), "{none}");
    s.run("overlay.add", json!({ "id": "alerts", "kind": "alerts" })).unwrap();
    let out = s.run("overlay.alert", json!({ "title": "Hydrate!" })).unwrap();
    assert_eq!(out["queued_on"], "alerts");
}

#[test]
fn a_scene_switch_holds_the_old_frame_and_fades_into_the_new_scene() {
    let mut s = Studio::new();
    s.run("program.set_transition", json!({ "kind": "fade", "ms": 1000 })).unwrap();
    let missing = s.run("program.switch_scene", json!({ "scene": "nowhere" })).unwrap_err();
    assert!(missing.contains("no scene"), "{missing}");
    s.run("program.switch_scene", json!({ "scene": "brb" })).unwrap();

    // With outputs running, the switch waits for one composed frame to hold.
    assert_eq!(s.program.take_switch(true), None);
    let mut old = frame([200, 0, 0, 255]);
    s.program.compose(&mut old);
    assert_eq!(s.program.take_switch(true).as_deref(), Some("brb"), "the stage switches next");

    // The new scene's frames blend from the held one over the second.
    s.tick(0.5);
    let mut mid = frame([0, 0, 200, 255]);
    s.program.compose(&mut mid);
    assert_eq!(mid.pixel(320, 180), Some([100, 0, 100, 255]), "halfway through the fade");
    let status = s.run("program.status", json!({})).unwrap();
    assert_eq!((status["scene"].clone(), status["transition"]["state"].clone()), (json!("brb"), json!("running")));
    s.tick(0.6);
    let mut done = frame([0, 0, 200, 255]);
    s.program.compose(&mut done);
    assert_eq!(done.pixel(320, 180), Some([0, 0, 200, 255]), "the new scene alone");
    assert_eq!(s.run("program.status", json!({})).unwrap()["transition"], Value::Null);
}

#[test]
fn without_outputs_a_switch_is_a_cut() {
    let mut s = Studio::new();
    s.run("program.switch_scene", json!({ "scene": "main", "transition": { "kind": "iris" } })).unwrap();
    assert_eq!(s.program.take_switch(false).as_deref(), Some("main"));
    assert_eq!(s.program.scene().as_deref(), Some("main"));
}

#[test]
fn widgets_limited_to_a_scene_show_only_on_it() {
    let mut s = Studio::new();
    s.run("overlay.add", json!({ "id": "brb", "kind": "text", "scenes": ["brb"] })).unwrap();
    let empty = frame([0, 0, 0, 0]);
    let mut out = empty.clone();
    s.program.compose(&mut out);
    assert_eq!(changed(&empty, &out), 0, "not on the default scene");
    s.run("program.switch_scene", json!({ "scene": "brb", "transition": { "kind": "cut" } })).unwrap();
    s.program.take_switch(false);
    s.program.compose(&mut out);
    assert!(changed(&empty, &out) > 1000, "on the brb scene");
}

#[test]
fn a_dry_run_changes_nothing_live() {
    let mut s = Studio::new();
    s.run("overlay.add", json!({ "id": "goal", "kind": "goal" })).unwrap();
    let reg = find("overlay.goal").unwrap();
    let op = (reg.decode)(json!({ "id": "goal", "value": 9 })).unwrap();
    let actor = Actor::Agent { id: "helper#1".into(), model: None };
    let ctx = OpCtx::new(&s.doc, &actor, &*s.clock).with_services(&s.services).with_dry_run(true);
    assert_eq!(op.plan(&ctx).unwrap().output.unwrap()["applied"], false);
    assert_eq!(s.run("overlay.list", json!({})).unwrap()["live"]["widgets"][0]["state"]["value"], 0.0);
}

#[test]
fn undoing_an_add_takes_the_widget_away() {
    let mut s = Studio::new();
    let reg = find("overlay.add").unwrap();
    let op = (reg.decode)(json!({ "id": "chat", "kind": "chat" })).unwrap();
    let actor = Actor::Person { name: "you".into() };
    let patch = op.plan(&OpCtx::new(&s.doc, &actor, &*s.clock)).unwrap().patch;
    s.doc.apply(&patch).unwrap();
    s.program.follow(&s.doc);
    assert_eq!(s.program.widgets()["widgets"].as_array().map(Vec::len), Some(1));
    let inverse: Patch = patch.inverse();
    s.doc.apply(&inverse).unwrap();
    s.program.follow(&s.doc);
    assert_eq!(s.program.widgets()["widgets"], json!([]));
}
