//! The outliner (`shell.md` section 6): the part tree read from the document, every action an op recorded by a fake
//! runner, a keyboard-only flow, the filter, and bench S4: 10,000 rows cost what 40 do.
//!
//! The ops are the real `part.*` ops from `omuzaki-char`, run by `MockRunner` over an in-memory document holding the
//! mock character; the runner is wrapped to record every request the panel makes.

// A counting allocator is the only way to prove "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::accesskit::Role;
use egui::{Context, Event as UiEvent, Key, Modifiers, RawInput, Rect, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_char_api::{CharId, PartId};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, TxId};
use omuzaki_ops_mock::MockRunner;
use omuzaki_ui::ds::testing::{harness_state, press};
use omuzaki_ui::ds::{Settings, apply_settings};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::io::Io;
use omuzaki_ui::panels::outliner::Outliner;
use serde_json::json;

// ---- a runner that records what the panel asks for ----

/// `MockRunner` with the real part ops, recording the name of every op the panel runs.
struct Recorder {
    inner: MockRunner<MemDoc>,
    ops: Mutex<Vec<String>>,
}

impl Recorder {
    fn ops(&self) -> Vec<String> {
        self.ops.lock().map(|o| o.clone()).unwrap_or_default()
    }
}

impl Runner for Recorder {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        if let Ok(mut ops) = self.ops.lock() {
            ops.push(req.op.clone());
        }
        self.inner.run(req)
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
    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.inner.subscribe(grants)
    }
    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.inner.log(after_seq, limit, grants)
    }
}

fn mock() -> CharId {
    CharId::new("mock").unwrap_or_else(|_| panic!("an id"))
}

fn part(id: &str) -> PartId {
    PartId::new(id).unwrap_or_else(|_| panic!("a part id"))
}

/// A runner over a document holding the mock character with `parts` instead of its own (or its own, for `None`).
fn runner(parts: Option<Value>) -> Arc<Recorder> {
    omuzaki_char::link();
    omuzaki_char_mock::link();
    let mut node: Value = serde_json::from_str(omuzaki_char_mock::CHARACTER_JSON).unwrap_or(Value::Null);
    if let Some(parts) = parts {
        node["parts"] = parts;
    }
    let doc = MemDoc::from_value(json!({"characters": {"mock": node}}));
    Arc::new(Recorder { inner: MockRunner::new(doc), ops: Mutex::new(Vec::new()) })
}

struct Rig {
    outliner: Outliner,
    focus: EditFocus,
    runner: Arc<Recorder>,
    events: Receiver<Event>,
    frame: Vec<Event>,
    actor: Actor,
}

fn person() -> Actor {
    Actor::Person { name: "kit".to_owned() }
}

fn rig_over(runner: Arc<Recorder>, size: egui::Vec2) -> Harness<'static, Rig> {
    let focus = EditFocus::new(mock());
    let events = runner.subscribe(&Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    let rig = Rig { outliner: Outliner::new(focus.clone()), focus, runner, events, frame: Vec::new(), actor: person() };
    harness_state(Settings::default(), size, rig, |ui, r| {
        r.frame = r.events.try_iter().collect();
        let io = Io::new(&*r.runner, &r.actor, &r.frame);
        r.outliner.ui(ui, io);
    })
}

fn rig() -> Harness<'static, Rig> {
    rig_over(runner(None), vec2(320.0, 480.0))
}

fn row_names(h: &Harness<'_, Rig>) -> Vec<String> {
    let tree = h.state().outliner.tree();
    let mut out = Vec::new();
    let mut stack: Vec<usize> = tree.roots().iter().rev().copied().collect();
    while let Some(i) = stack.pop() {
        out.push(tree.nodes()[i].id.to_string());
        stack.extend(tree.nodes()[i].children.iter().rev());
    }
    out
}

fn enabled(h: &Harness<'_, Rig>, id: &str) -> Option<bool> {
    let tree = h.state().outliner.tree();
    tree.find(&part(id)).map(|i| tree.nodes()[i].enabled)
}

fn settle(h: &mut Harness<'_, Rig>) {
    h.run_steps(4);
}

/// A primary click at `pos`.
fn click_at(h: &mut Harness<'_, Rig>, pos: egui::Pos2) {
    h.event(UiEvent::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(UiEvent::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    settle(h);
}

// ---- the tree ----

#[test]
fn the_tree_lists_the_characters_parts_in_build_order_and_a_click_selects() {
    let mut h = rig();
    assert_eq!(row_names(&h), ["body", "hair", "head", "shirt", "vest"]);
    h.get_by_role_and_label(Role::TreeItem, "hair").click();
    settle(&mut h);
    assert_eq!(h.state().focus.selected(), vec![part("hair")]);
    h.get_by_role_and_label(Role::TreeItem, "vest").click_modifiers(Modifiers::COMMAND);
    settle(&mut h);
    assert_eq!(h.state().focus.selected(), vec![part("hair"), part("vest")], "Ctrl extends");
    h.get_by_role_and_label(Role::TreeItem, "body").click_modifiers(Modifiers::SHIFT);
    settle(&mut h);
    assert_eq!(h.state().focus.count(), 5, "Shift takes the range from the anchor (vest) to body");
    assert!(h.state().runner.ops().is_empty(), "selecting is not a document change");
}

#[test]
fn no_open_character_shows_why_and_the_fix() {
    let mut h = rig();
    h.state().focus.open(None);
    settle(&mut h);
    assert!(h.query_all_by_label_contains("No character is open").next().is_some());
    assert!(!h.state().outliner.available());
}

// ---- every action is an op ----

#[test]
fn every_action_is_an_op_and_the_tree_follows_the_document() {
    let mut h = rig();
    // The eye: part.set_enabled.
    h.get_by_role_and_label(Role::TreeItem, "shirt").click();
    settle(&mut h);
    let hair = h.get_by_role_and_label(Role::TreeItem, "hair").rect();
    click_at(&mut h, egui::pos2(hair.right() - 14.0, hair.center().y));
    assert_eq!(
        enabled(&h, "hair"),
        Some(false),
        "the hair row's eye turned it off: {:?} {:?}",
        h.state().runner.ops(),
        omuzaki_ui::ds::testing::accesskit_strings(&h)
    );

    // Move down from the context menu: part.reorder.
    h.get_by_role_and_label(Role::TreeItem, "shirt").click_secondary();
    settle(&mut h);
    h.get_by_label("Move down").click();
    settle(&mut h);
    assert_eq!(row_names(&h), ["body", "hair", "head", "vest", "shirt"], "the shirt now builds after the vest");

    // Ctrl+D on the focused row: part.duplicate.
    h.get_by_role_and_label(Role::TreeItem, "hair").click();
    settle(&mut h);
    press(&mut h, Modifiers::COMMAND, Key::D);
    settle(&mut h);
    assert!(row_names(&h).contains(&"hair-copy".to_owned()), "{:?}", row_names(&h));
    assert_eq!(h.state().focus.selected(), vec![part("hair-copy")], "the copy is selected");

    // Delete: part.remove.
    press(&mut h, Modifiers::NONE, Key::Delete);
    settle(&mut h);
    assert!(!row_names(&h).contains(&"hair-copy".to_owned()));

    assert_eq!(
        h.state().runner.ops(),
        ["part.set_enabled", "part.reorder", "part.duplicate", "part.remove"],
        "each action is exactly one op, and nothing else wrote"
    );
}

#[test]
fn deleting_several_parts_is_one_undo_step() {
    let mut h = rig();
    h.get_by_role_and_label(Role::TreeItem, "shirt").click();
    h.get_by_role_and_label(Role::TreeItem, "vest").click_modifiers(Modifiers::COMMAND);
    settle(&mut h);
    press(&mut h, Modifiers::NONE, Key::Delete);
    settle(&mut h);
    assert_eq!(row_names(&h), ["body", "hair", "head"]);
    let state = h.state();
    let undone = state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(undone.as_deref(), Some("Delete parts"));
    settle(&mut h);
    assert_eq!(row_names(&h), ["body", "hair", "head", "shirt", "vest"], "one undo brings both back");
}

#[test]
fn a_refused_op_says_why_and_changes_nothing() {
    let mut h = rig_over(
        runner(Some(json!({
            "body": {"kind": "mock-body"},
            "robe": {"kind": "mock-garment", "layer": 20},
            "hood": {"kind": "mock-garment", "parent": "robe", "layer": 20}
        }))),
        vec2(320.0, 480.0),
    );
    h.get_by_role_and_label(Role::TreeItem, "robe").click();
    settle(&mut h);
    press(&mut h, Modifiers::NONE, Key::Delete);
    settle(&mut h);
    assert!(h.query_by_label_contains("That didn't work").is_some(), "a part with children can't be removed");
    assert_eq!(row_names(&h), ["body", "robe", "hood"]);
}

#[test]
fn a_drag_onto_a_sibling_reorders_and_esc_cancels() {
    let mut h = rig();
    let vest = h.get_by_role_and_label(Role::TreeItem, "vest").rect().center();
    let shirt = h.get_by_role_and_label(Role::TreeItem, "shirt").rect();
    let above_shirt = egui::pos2(shirt.center().x, shirt.top() + 3.0);

    // Esc during the drag cancels it.
    h.hover_at(vest);
    h.event(UiEvent::PointerButton {
        pos: vest,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    for p in [vest + vec2(0.0, -10.0), above_shirt] {
        h.event(UiEvent::PointerMoved(p));
        h.run();
    }
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.event(UiEvent::PointerButton {
        pos: above_shirt,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    settle(&mut h);
    assert!(h.state().runner.ops().is_empty(), "{:?}", h.state().runner.ops());

    omuzaki_ui::ds::testing::drag(&mut h, &[vest, vest + vec2(0.0, -10.0), above_shirt, above_shirt]);
    settle(&mut h);
    assert_eq!(
        row_names(&h),
        ["body", "hair", "head", "vest", "shirt"],
        "{:?} {:?}",
        h.state().runner.ops(),
        omuzaki_ui::ds::testing::accesskit_strings(&h)
    );
    assert_eq!(h.state().runner.ops(), ["part.reorder"]);
}

// ---- the keyboard ----

#[test]
fn a_keyboard_only_flow_selects_expands_filters_and_edits() {
    let mut h = rig_over(
        runner(Some(json!({
            "body": {"kind": "mock-body"},
            "robe": {"kind": "mock-garment", "layer": 20},
            "hood": {"kind": "mock-garment", "parent": "robe", "layer": 20},
            "clasp": {"kind": "mock-garment", "parent": "robe", "layer": 20}
        }))),
        vec2(320.0, 480.0),
    );
    // Tab from the filter field reaches the tree's one tab stop.
    press(&mut h, Modifiers::NONE, Key::Tab);
    press(&mut h, Modifiers::NONE, Key::Tab);
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    assert_eq!(h.state().focus.primary(), Some(part("robe")));
    press(&mut h, Modifiers::NONE, Key::ArrowLeft);
    settle(&mut h);
    assert!(h.query_by_label("hood").is_none(), "Left collapses");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(h.state().focus.primary(), Some(part("clasp")), "Right expands, then goes to the first child");
    press(&mut h, Modifiers::NONE, Key::ArrowLeft);
    assert_eq!(h.state().focus.primary(), Some(part("robe")), "Left on a child goes to its parent");
    press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    assert_eq!(h.state().focus.count(), 2, "Shift+Down extends");
    press(&mut h, Modifiers::NONE, Key::End);
    assert_eq!(h.state().focus.selected(), vec![part("hood")]);
    press(&mut h, Modifiers::NONE, Key::Home);
    assert_eq!(h.state().focus.selected(), vec![part("body")]);
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(h.state().focus.take_inspector_request(), "Enter sends the keyboard to the inspector");

    // Typing starts the filter, which keeps a match's ancestors.
    h.event(UiEvent::Text("hoo".to_owned()));
    settle(&mut h);
    assert!(h.query_by_label("hood").is_some() && h.query_by_label("robe").is_some());
    assert!(h.query_by_label("body").is_none() && h.query_by_label("clasp").is_none());
    // Down from the filter goes back to the rows.
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    assert_eq!(h.state().focus.primary(), Some(part("hood")));
    press(&mut h, Modifiers::COMMAND, Key::D);
    settle(&mut h);
    assert_eq!(h.state().runner.ops(), ["part.duplicate"]);
}

// ---- agents ----

#[test]
fn a_part_an_agent_changed_gets_the_violet_dot_until_it_is_selected() {
    let mut h = rig();
    let agent = Actor::Agent { id: "designer".to_owned(), model: None };
    let request = Request {
        op: "part.set_enabled".to_owned(),
        args: json!({"character": "mock", "part": "vest", "enabled": false}),
        mode: omuzaki_ops_api::Mode::Apply,
        actor: agent,
        grants: Grants::agent_default(),
        cause: None,
        tx: None,
        base: None,
    };
    h.state().runner.inner.run(request).unwrap_or_else(|e| panic!("{e}"));
    settle(&mut h);
    let described = |h: &Harness<'_, Rig>| {
        h.get_by_role_and_label(Role::TreeItem, "vest").accesskit_node().description().unwrap_or_default()
    };
    assert!(described(&h).contains("Touched by an agent (designer)"), "{}", described(&h));
    assert!(described(&h).contains("Hidden"), "the eye says the vest is off: {}", described(&h));
    h.get_by_role_and_label(Role::TreeItem, "vest").click();
    settle(&mut h);
    assert!(!described(&h).contains("agent"), "seen once selected: {}", described(&h));
}

// ---- bench S4: 10,000 rows cost what 40 do ----

struct Counting;

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// A character with `count` garments in a flat list.
fn flat(count: usize) -> Value {
    let parts: serde_json::Map<String, Value> =
        (0..count).map(|i| (format!("part-{i:05}"), json!({"kind": "mock-garment", "layer": 20}))).collect();
    Value::Object(parts)
}

/// Median frame time and allocations per frame of an outliner over `count` parts, scrolled half way, at 1920×1080's
/// left dock (320 × 1000 points).
fn s4(count: usize) -> (f64, f64) {
    let runner = runner(Some(flat(count)));
    let focus = EditFocus::new(mock());
    let mut outliner = Outliner::new(focus.clone());
    focus.select(part(&format!("part-{:05}", count / 2)));
    let actor = person();
    let ctx = Context::default();
    apply_settings(&ctx, Settings::default());
    let mut frame = |time: f64, measure: bool| {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(320.0, 1000.0))),
            time: Some(time),
            predicted_dt: 1.0 / 60.0,
            ..RawInput::default()
        };
        MEASURING.with(|m| m.set(measure));
        let start = Instant::now();
        let output = ctx.run_ui(input, |ui| outliner.ui(ui, Io::new(&*runner, &actor, &[])));
        let took = start.elapsed().as_secs_f64();
        MEASURING.with(|m| m.set(false));
        output.drop_without_applying_deltas();
        took
    };
    for i in 0..40 {
        frame(f64::from(i) / 60.0, false);
    }
    COUNT.with(|c| c.set(0));
    let mut times: Vec<f64> = (0..120).map(|i| frame(1.0 + f64::from(i) / 60.0, true)).collect();
    let allocations = COUNT.with(Cell::get) as f64 / 120.0;
    times.sort_by(f64::total_cmp);
    (times[times.len() / 2], allocations)
}

#[test]
fn bench_s4_ten_thousand_rows_cost_what_forty_do() {
    let (small_ms, small_alloc) = s4(40);
    let (big_ms, big_alloc) = s4(10_000);
    eprintln!(
        "S4: 40 rows {:.3} ms/frame, {small_alloc:.1} allocations; 10,000 rows {:.3} ms/frame, {big_alloc:.1} allocations",
        small_ms * 1000.0,
        big_ms * 1000.0
    );
    assert!(
        big_alloc <= small_alloc + 1.0,
        "allocations grow with the tree: {small_alloc} at 40, {big_alloc} at 10,000"
    );
    // The spec's bar is ±10%; a shared test machine's timer noise is larger, so the gate here catches a cost that grows
    // with the tree (10,000 rows walked per frame is 250 times the work), and the PR quotes the release numbers.
    assert!(big_ms <= small_ms * 1.5 + 0.000_2, "a frame costs more with 10,000 rows: {small_ms} s vs {big_ms} s");
}
