//! The inspector (`shell.md` section 7): a form for every registered part kind, one undo step per slider drag, Esc
//! rolling a drag back, search, editing several parts of one kind together, no per-frame allocation that grows with
//! the form, and bench S2 (52 sliders visible, scripted drags: UI p95 ≤ 2 ms).
//!
//! The real `part.*` ops run on `MockRunner` over an in-memory document; the allocation test and the bench use a fake
//! runner that answers `part.describe` with a form of any size and accepts every `part.set_param`, so what they measure
//! is the UI.

#![allow(
    unsafe_code,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a counting allocator, and screen geometry and percentiles read from small numbers"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};
use std::time::Instant;

use egui::accesskit::Role;
use egui::{Context, Event as UiEvent, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_char_api::{CharId, PartId};
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Plan, Request, Runner, TxId};
use omuzaki_ops_mock::MockRunner;
use omuzaki_ui::ds::testing::{drag, harness_state, press, unlabelled_interactive};
use omuzaki_ui::ds::{Settings, apply_settings};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::inspector::Inspector;
use omuzaki_ui::panels::io::Io;
use serde_json::json;

fn id(text: &str) -> CharId {
    CharId::new(text).unwrap_or_else(|_| panic!("an id"))
}

fn part(text: &str) -> PartId {
    PartId::new(text).unwrap_or_else(|_| panic!("a part id"))
}

fn person() -> Actor {
    Actor::Person { name: "kit".to_owned() }
}

/// A runner with the real part ops over a character `probe` holding `parts`.
#[allow(clippy::needless_pass_by_value, reason = "the tests pass a `json!` literal")]
fn real(parts: Value) -> MockRunner<MemDoc> {
    omuzaki_char::link();
    omuzaki_char_mock::link();
    let node = json!({"name": "Probe", "template": "mock", "parts": parts});
    MockRunner::new(MemDoc::from_value(json!({"characters": {"probe": node}})))
}

/// A part of `kind` at its defaults.
fn at_defaults(kind: &str) -> Value {
    let reg = omuzaki_char_api::find_kind(kind).unwrap_or_else(|| panic!("kind {kind} is registered"));
    json!({"kind": kind, "params": (reg.defaults)()})
}

struct Rig<R: Runner> {
    inspector: Inspector,
    runner: R,
    events: Receiver<Event>,
    frame: Vec<Event>,
    actor: Actor,
}

fn rig<R: Runner + 'static>(runner: R, select: &[&str], size: egui::Vec2) -> Harness<'static, Rig<R>> {
    let focus = EditFocus::new(id("probe"));
    focus.set(select.iter().map(|p| part(p)).collect());
    let events = runner.subscribe(&Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    let state = Rig { inspector: Inspector::new(focus), runner, events, frame: Vec::new(), actor: person() };
    harness_state(Settings::default(), size, state, |ui, r| {
        r.frame = r.events.try_iter().collect();
        r.inspector.ui(ui, Io::new(&r.runner, &r.actor, &r.frame));
    })
}

fn param(runner: &MockRunner<MemDoc>, part: &str, path: &str) -> Value {
    let out = runner
        .run(request("part.describe", json!({"character": "probe", "part": part}), None))
        .unwrap_or_else(|e| panic!("{e}"));
    let answer = out.plan.output.unwrap_or(Value::Null);
    let entries = answer["parameters"].as_array().cloned().unwrap_or_default();
    entries.into_iter().find(|p| p["path"] == json!(path)).map_or(Value::Null, |p| p["current"].clone())
}

fn request(op: &str, args: Value, tx: Option<TxId>) -> Request {
    Request {
        op: op.to_owned(),
        args,
        mode: omuzaki_ops_api::Mode::Apply,
        actor: person(),
        grants: Grants::person(),
        cause: None,
        tx,
        base: None,
    }
}

fn set_params(runner: &MockRunner<MemDoc>) -> Vec<OpRecord> {
    runner.records().into_iter().filter(|r| r.op == "part.set_param").collect()
}

// ---- every kind ----

#[test]
fn a_form_renders_for_every_registered_part_kind() {
    omuzaki_char::link();
    omuzaki_char_mock::link();
    let mut kinds: Vec<&str> = omuzaki_char_api::kinds().map(|k| (k.meta)().id).collect();
    kinds.sort_unstable();
    assert!(kinds.len() >= 8, "the real and mock kinds are linked: {kinds:?}");
    for kind in kinds {
        let mut h = rig(real(json!({"probe": at_defaults(kind)})), &["probe"], vec2(360.0, 4000.0));
        h.run_steps(3);
        let fields = h.state().inspector.fields();
        assert!(fields > 0, "{kind}: the form has no fields");
        // The header's kind line, whole: the kind's bare name can be inside the part's (`robe` in `probe`).
        assert!(h.query_by_label(&format!("Kind: {kind}")).is_some(), "{kind}: the header names the kind");
        assert!(unlabelled_interactive(&h).is_empty(), "{kind}: {:?}", unlabelled_interactive(&h));
        let editable =
            [Role::Slider, Role::SpinButton, Role::Switch, Role::ComboBox, Role::RadioGroup, Role::TextInput]
                .into_iter()
                .map(|role| h.query_all_by_role(role).count())
                .sum::<usize>();
        assert!(editable > 1, "{kind}: {fields} fields but only {editable} editable controls");
    }
}

#[test]
fn nothing_selected_says_how_to_select() {
    let h = rig(real(json!({"probe": at_defaults("mock-head")})), &[], vec2(360.0, 400.0));
    assert!(h.query_all_by_label_contains("Select a part to edit it").next().is_some());
}

// ---- one gesture, one transaction ----

/// The head kind's cheek fullness slider (a label no other head section has).
const SLIDER: &str = "Cheek fullness";

fn head_rig() -> Harness<'static, Rig<MockRunner<MemDoc>>> {
    let mut h = rig(real(json!({"probe": at_defaults("head")})), &["probe"], vec2(420.0, 3000.0));
    h.get_by_role_and_label(Role::DisclosureTriangle, "Advanced").click();
    h.run_steps(3);
    h
}

#[test]
fn a_slider_drag_is_one_undo_step() {
    let mut h = head_rig();
    let before = param(&h.state().runner, "probe", "cranium/cheek_fullness");
    let track = h.get_by_role_and_label(Role::Slider, SLIDER).rect();
    let (from, to) = (track.center(), track.center() + vec2(40.0, 0.0));
    drag(&mut h, &[from, from + vec2(5.0, 0.0), from + vec2(15.0, 0.0), from + vec2(30.0, 0.0), to]);
    h.run_steps(3);
    let after = param(&h.state().runner, "probe", "cranium/cheek_fullness");
    assert_ne!(after, before, "the drag moved the value");
    let records = set_params(&h.state().runner);
    assert!(records.len() > 1, "every move previews: {}", records.len());
    assert!(records.windows(2).all(|w| w[0].tx == w[1].tx), "every preview is in one transaction");
    let state = h.state();
    let undone = state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(undone.as_deref(), Some("Set cranium/cheek_fullness"));
    assert_eq!(param(&state.runner, "probe", "cranium/cheek_fullness"), before, "one undo puts it back");
    assert!(state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}")).is_none());
}

#[test]
fn esc_during_a_drag_rolls_it_back() {
    let mut h = head_rig();
    let before = param(&h.state().runner, "probe", "cranium/cheek_fullness");
    let from = h.get_by_role_and_label(Role::Slider, SLIDER).rect().center();
    h.hover_at(from);
    h.event(UiEvent::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    for dx in [5.0, 20.0, 40.0] {
        h.event(UiEvent::PointerMoved(from + vec2(dx, 0.0)));
        h.run();
    }
    assert_ne!(param(&h.state().runner, "probe", "cranium/cheek_fullness"), before, "the drag previews live");
    press(&mut h, Modifiers::NONE, Key::Escape);
    let end = from + vec2(40.0, 0.0);
    h.event(UiEvent::PointerButton {
        pos: end,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(3);
    assert_eq!(param(&h.state().runner, "probe", "cranium/cheek_fullness"), before);
    assert!(!h.state().runner.has_open_tx(), "nothing left open");
    let state = h.state();
    assert!(
        state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}")).is_none(),
        "nothing to undo"
    );
}

/// `MockRunner` whose rollbacks are refused (a `Conflict`, as when the document changed under the transaction) until
/// `refuse` is switched off.
struct Refusing {
    inner: MockRunner<MemDoc>,
    refuse: std::sync::atomic::AtomicBool,
}

impl Runner for Refusing {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.inner.run(req)
    }
    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        if self.refuse.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(OpError::conflict("the document changed under the edit"));
        }
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

/// A drag of the slider cancelled with Esc while the runner refuses the rollback.
fn refused_rollback() -> Harness<'static, Rig<Refusing>> {
    let runner = Refusing {
        inner: real(json!({"probe": at_defaults("head")})),
        refuse: std::sync::atomic::AtomicBool::new(true),
    };
    let mut h = rig(runner, &["probe"], vec2(420.0, 3000.0));
    h.get_by_role_and_label(Role::DisclosureTriangle, "Advanced").click();
    h.run_steps(3);
    let from = h.get_by_role_and_label(Role::Slider, SLIDER).rect().center();
    h.hover_at(from);
    h.event(UiEvent::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    for dx in [5.0, 20.0, 40.0] {
        h.event(UiEvent::PointerMoved(from + vec2(dx, 0.0)));
        h.run();
    }
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.event(UiEvent::PointerButton {
        pos: from + vec2(40.0, 0.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(3);
    h
}

#[test]
fn a_refused_rollback_keeps_the_transaction_and_offers_a_retry() {
    let mut h = refused_rollback();
    assert!(h.state().runner.inner.has_open_tx(), "the transaction is still open, not forgotten");
    assert!(h.query_by_label("Try again").is_some() && h.query_by_label("Keep the change").is_some());
    // The conflict clears; retrying rolls it back and closes it.
    h.state().runner.refuse.store(false, std::sync::atomic::Ordering::SeqCst);
    h.get_by_label("Try again").click();
    h.run_steps(3);
    assert!(!h.state().runner.inner.has_open_tx(), "the retry closed it");
    assert!(h.query_by_label("Try again").is_none(), "and the way out is gone");
    let before = param(&h.state().runner.inner, "probe", "cranium/cheek_fullness");
    let default = param(&real(json!({"probe": at_defaults("head")})), "probe", "cranium/cheek_fullness");
    assert_eq!(before, default, "the drag's previews are undone");
}

#[test]
fn a_refused_rollback_can_be_kept_as_one_undo_step() {
    let mut h = refused_rollback();
    assert!(h.state().runner.inner.has_open_tx());
    h.get_by_label("Keep the change").click();
    h.run_steps(3);
    assert!(!h.state().runner.inner.has_open_tx(), "committed");
    let state = h.state();
    let undone = state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(undone.as_deref(), Some("Set cranium/cheek_fullness"), "the kept drag is one undo step");
}

#[test]
fn a_key_press_is_one_step_and_the_form_follows_the_document() {
    let mut h = head_rig();
    let before = param(&h.state().runner, "probe", "cranium/cheek_fullness");
    h.get_by_role_and_label(Role::Slider, SLIDER).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    h.run_steps(3);
    assert_ne!(param(&h.state().runner, "probe", "cranium/cheek_fullness"), before);
    assert_eq!(set_params(&h.state().runner).len(), 1);
    let state = h.state();
    state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    h.run_steps(3);
    let shown = h.state().inspector.value().unwrap_or(Value::Null);
    assert_eq!(shown["cranium"]["cheek_fullness"], before, "an undo elsewhere reaches the form");
}

// ---- search, several parts ----

#[test]
fn search_keeps_the_matching_settings() {
    let mut h = head_rig();
    let all = h.state().inspector.fields();
    h.get_by_role_and_label(Role::TextInput, "Search settings\u{2026}").focus();
    h.event(UiEvent::Text("cheek".to_owned()));
    h.run_steps(3);
    let some = h.state().inspector.fields();
    assert!(some >= 1 && some < all, "{some} of {all}");
    assert!(h.query_by_role_and_label(Role::Slider, SLIDER).is_some());
    h.event(UiEvent::Text("zzz".to_owned()));
    h.run_steps(3);
    assert!(h.query_by_label_contains("No setting matches").is_some());
}

#[test]
fn several_parts_of_one_kind_are_edited_together_in_one_step() {
    let parts =
        json!({"a": at_defaults("mock-garment"), "b": at_defaults("mock-garment"), "c": at_defaults("mock-head")});
    let mut h = rig(real(parts), &["a", "c", "b"], vec2(420.0, 2000.0));
    h.run_steps(3);
    assert!(h.query_by_label_contains("2 parts selected").is_some(), "c is another kind");
    let slider = h.query_all_by_role(Role::Slider).next().map(|n| n.rect().center());
    let Some(at) = slider else { panic!("a garment has a slider") };
    h.hover_at(at);
    h.run();
    let focused = h.query_all_by_role(Role::Slider).next().map(|n| n.focus());
    assert!(focused.is_some());
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    h.run_steps(3);
    let records = set_params(&h.state().runner);
    assert_eq!(records.len(), 2, "one set_param per part of the kind");
    assert_eq!(records[0].tx, records[1].tx, "in one transaction");
}

// ---- a fake runner for the measurements ----

/// Answers `part.describe` with `count` bounded numbers (every third with a default) and accepts every
/// `part.set_param`.
struct Fake {
    count: usize,
    sets: Mutex<usize>,
    next_tx: Mutex<u64>,
}

impl Fake {
    fn new(count: usize) -> Self {
        Self { count, sets: Mutex::new(0), next_tx: Mutex::new(0) }
    }

    fn answer(&self) -> Value {
        let parameters: Vec<Value> = (0..self.count)
            .map(|i| {
                json!({"path": format!("s{}/p{i:02}", i % 4), "type": "number", "minimum": 0.0, "maximum": 1.0,
                       "current": 0.5, "default": if i % 3 == 0 { 0.5 } else { 0.25 }})
            })
            .collect();
        json!({"kind": "fake", "parameters": parameters})
    }
}

impl Runner for Fake {
    fn ops(&self) -> Vec<OpInfo> {
        Vec::new()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        match req.op.as_str() {
            "part.describe" => Ok(Outcome::new(Plan::answer(self.answer()))),
            "part.set_param" => {
                if let Ok(mut sets) = self.sets.lock() {
                    *sets += 1;
                }
                Ok(Outcome::new(Plan::edit(Patch::new())))
            }
            other => Err(OpError::not_found(format!("no op {other}"))),
        }
    }
    fn begin(&self, _: &str, _: &Actor) -> TxId {
        let mut next = self.next_tx.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *next += 1;
        TxId(*next)
    }
    fn commit(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn rollback(&self, _: TxId, _: &Actor) -> Result<(), OpError> {
        Ok(())
    }
    fn undo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn redo(&self, _: &Actor, _: &Grants) -> Result<Option<String>, OpError> {
        Ok(None)
    }
    fn subscribe(&self, _: &Grants) -> Result<Receiver<Event>, OpError> {
        Ok(channel().1)
    }
    fn get(&self, _: &DocPath, _: &Grants) -> Result<Option<Value>, OpError> {
        Ok(None)
    }
    fn log(&self, _: u64, _: usize, _: &Grants) -> Result<Vec<OpRecord>, OpError> {
        Ok(Vec::new())
    }
}

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

/// The screen: 1920×1080's right dock, tall enough that every field is drawn.
fn screen() -> Rect {
    Rect::from_min_size(Pos2::ZERO, vec2(360.0, 2400.0))
}

/// One frame of `inspector` on `ctx` with `events`.
fn frame(ctx: &Context, inspector: &mut Inspector, runner: &Fake, i: u32, events: Vec<UiEvent>) -> egui::FullOutput {
    let actor = person();
    let input = RawInput {
        screen_rect: Some(screen()),
        time: Some(f64::from(i) / 60.0),
        predicted_dt: 1.0 / 60.0,
        events,
        ..RawInput::default()
    };
    ctx.run_ui(input, |ui| inspector.ui(ui, Io::new(runner, &actor, &[])))
}

fn inspector() -> Inspector {
    let focus = EditFocus::new(id("probe"));
    focus.select(part("probe"));
    Inspector::new(focus)
}

/// Where the sliders of a `count`-field form are, found once through AccessKit on a context of its own.
fn sliders(count: usize) -> Vec<Rect> {
    let (runner, mut inspector, ctx) = (Fake::new(count), inspector(), Context::default());
    apply_settings(&ctx, Settings::default());
    ctx.enable_accesskit();
    let mut rects = Vec::new();
    for i in 0..3 {
        let output = frame(&ctx, &mut inspector, &runner, i, Vec::new());
        if let Some(update) = &output.platform_output.accesskit_update {
            rects = update
                .nodes
                .iter()
                .filter(|(_, n)| n.role() == Role::Slider)
                .filter_map(|(_, n)| n.bounds())
                .map(|b| Rect::from_min_max(Pos2::new(b.x0 as f32, b.y0 as f32), Pos2::new(b.x1 as f32, b.y1 as f32)))
                .collect();
        }
        output.drop_without_applying_deltas();
    }
    rects.sort_by(|a, b| a.top().total_cmp(&b.top()));
    rects
}

/// An inspector over `count` fake fields, driven for `frames` frames by `script` (the events of frame `i`, given the
/// sliders' rects). Returns the frame times, sorted, and the allocations per frame.
fn drive(count: usize, frames: u32, mut script: impl FnMut(u32, &[Rect]) -> Vec<UiEvent>) -> (Vec<f64>, f64) {
    let rects = sliders(count);
    let (runner, mut inspector, ctx) = (Fake::new(count), inspector(), Context::default());
    apply_settings(&ctx, Settings::default());
    let mut run = |i: u32, measure: bool, events: Vec<UiEvent>| {
        MEASURING.with(|m| m.set(measure));
        let start = Instant::now();
        let output = frame(&ctx, &mut inspector, &runner, i, events);
        let took = start.elapsed().as_secs_f64();
        MEASURING.with(|m| m.set(false));
        output.drop_without_applying_deltas();
        took
    };
    for i in 0..40 {
        run(i, false, Vec::new());
    }
    COUNT.with(|c| c.set(0));
    let mut times: Vec<f64> = (0..frames).map(|i| run(40 + i, true, script(i, &rects))).collect();
    let allocations = COUNT.with(Cell::get) as f64 / f64::from(frames);
    times.sort_by(f64::total_cmp);
    (times, allocations)
}

#[test]
fn the_inspector_allocates_no_more_per_frame_with_more_fields() {
    let idle = |_: u32, _: &[Rect]| Vec::new();
    let (_, small) = drive(24, 60, idle);
    let (_, big) = drive(52, 60, idle);
    eprintln!("inspector: {small:.1} allocations/frame at 24 fields, {big:.1} at 52");
    assert!(big - small <= 2.0, "the inspector allocates per field per frame: {small} at 24, {big} at 52");
}

/// Bench S2: 52 sliders drawn and a scripted drag over one slider after another, each one gesture (press, twelve
/// moves, release), for 600 frames. The frame time is the UI's: the fake runner's ops cost nothing.
#[test]
fn bench_s2_scripted_slider_drags_hold_the_frame_budget() {
    const FRAMES: u32 = 600;
    const GESTURE: u32 = 14;
    let script = |i: u32, rects: &[Rect]| {
        let (step, which) = (i % GESTURE, (i / GESTURE) as usize);
        let Some(rect) = rects.get(which % rects.len().max(1)) else { return Vec::new() };
        let pos = Pos2::new(rect.left() + rect.width() * (0.4 + 0.02 * step as f32), rect.center().y);
        let button = |pressed| UiEvent::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        match step {
            0 => vec![UiEvent::PointerMoved(pos), button(true)],
            s if s == GESTURE - 1 => vec![button(false)],
            _ => vec![UiEvent::PointerMoved(pos)],
        }
    };
    assert_eq!(sliders(52).len(), 52, "all 52 sliders are drawn");
    let (times, allocations) = drive(52, FRAMES, script);
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize] * 1000.0;
    eprintln!(
        "S2: 52 sliders, {FRAMES} frames of scripted drags: p50 {:.3} ms, p95 {:.3} ms, p99 {:.3} ms, max {:.3} ms; \
         {allocations:.1} allocations/frame",
        at(0.5),
        at(0.95),
        at(0.99),
        at(1.0)
    );
    // The budget is a release build's; a debug build is several times slower, so it gets the headroom.
    let budget = if cfg!(debug_assertions) { 20.0 } else { 2.0 };
    assert!(at(0.95) <= budget, "UI p95 {} ms is over {budget} ms", at(0.95));
}

#[test]
fn the_inspector_is_registered_and_passes_the_shells_conformance_check() {
    omuzaki_ui::link();
    let runner = MockRunner::new(MemDoc::new());
    let actor = omuzaki_ops_api::Actor::System;
    let ctx = omuzaki_ui_api::panel::PanelCtx::new(&runner, &actor);
    let host = omuzaki_ui_mock::MockHost::new();
    let problems = host.check("inspector", &ctx);
    assert!(problems.is_empty(), "{problems:#?}");
}

// ---- task flows (the cloud half of the usability evidence; timing against other apps needs the local lane) ----

#[test]
fn task_flow_edit_a_parameter_then_undo_is_one_gesture_and_one_key() {
    // The part is already selected: in the app that is one click on its outliner row.
    let mut h = head_rig();
    let before = param(&h.state().runner, "probe", "cranium/cheek_fullness");
    let form_before = h.state().inspector.value();
    let track = h.get_by_role_and_label(Role::Slider, SLIDER).rect();
    let (from, to) = (track.center(), track.center() + vec2(40.0, 0.0));
    let start = Instant::now();
    drag(&mut h, &[from, from + vec2(20.0, 0.0), to]);
    h.run_steps(3);
    let edited = start.elapsed();
    assert_ne!(param(&h.state().runner, "probe", "cranium/cheek_fullness"), before, "one drag edits");

    // Undo is the shell's Ctrl+Z, one key; it reaches the runner, and the form follows the document.
    let start = Instant::now();
    {
        let state = h.state();
        state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    }
    h.run_steps(3);
    let undone = start.elapsed();
    assert_eq!(param(&h.state().runner, "probe", "cranium/cheek_fullness"), before, "one undo puts it back");
    assert_eq!(h.state().inspector.value(), form_before, "the form shows the value again");
    println!(
        "FLOW edit a parameter: 1 drag {:.1} ms; undo: 1 key {:.1} ms (headless, harness included)",
        edited.as_secs_f64() * 1e3,
        undone.as_secs_f64() * 1e3
    );
}

/// The wings kind (a text field, "Feathers", beside sliders and a choice) with its Advanced section open.
fn wings_rig() -> Harness<'static, Rig<MockRunner<MemDoc>>> {
    let mut h = rig(real(json!({"probe": at_defaults("wings")})), &["probe"], vec2(420.0, 4000.0));
    if h.query_by_role_and_label(Role::DisclosureTriangle, "Advanced").is_some() {
        h.get_by_role_and_label(Role::DisclosureTriangle, "Advanced").click();
        h.run_steps(3);
    }
    h
}

/// Presses the primary button at the centre of the node in one frame and releases it in the next, as a mouse does.
fn click_with_mouse(h: &mut Harness<'_, Rig<MockRunner<MemDoc>>>, role: Role, label: &str) {
    let pos = h.get_by_role_and_label(role, label).rect().center();
    h.hover_at(pos);
    h.run_steps(3);
    for pressed in [true, false] {
        h.event(UiEvent::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(3);
}

#[test]
fn a_click_on_another_control_while_a_text_field_is_focused_applies_both_edits_as_two_undo_steps() {
    // Clicking a segment takes focus from the Feathers field: in the release frame the form reports the field's
    // Commit and the segment's Commit. The later report used to replace the earlier, and the inspector never sent
    // the segment's change (its refresh put the old one back).
    let mut h = wings_rig();
    h.get_by_role_and_label(Role::TextInput, "Feathers").focus();
    h.run_steps(3);
    h.event(UiEvent::Text("7".to_owned()));
    h.run_steps(3);
    assert_eq!(param(&h.state().runner, "probe", "feathers"), json!(7), "typing previews");
    click_with_mouse(&mut h, Role::RadioButton, "bat");
    let runner = &h.state().runner;
    assert_eq!(param(runner, "probe", "kind"), json!("bat"), "the segment's change was applied");
    assert_eq!(param(runner, "probe", "feathers"), json!(7), "and the typed number kept");
    assert!(h.query_by_role_and_label(Role::RadioButton, "bat").is_some());
    let state = h.state();
    assert!(state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}")).is_some());
    assert_eq!(param(&state.runner, "probe", "kind"), json!("feathered"), "one undo takes the segment back");
    assert_eq!(param(&state.runner, "probe", "feathers"), json!(7), "and leaves the typing");
    let second = state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(second.as_deref(), Some("Set feathers"), "the typing is its own step");
    assert_eq!(param(&state.runner, "probe", "feathers"), Value::Null);
    assert!(state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}")).is_none());
}

#[test]
fn a_slider_drag_begun_while_a_text_field_is_focused_closes_the_fields_gesture_first() {
    let mut h = wings_rig();
    h.get_by_role_and_label(Role::TextInput, "Feathers").focus();
    h.run_steps(3);
    h.event(UiEvent::Text("7".to_owned()));
    h.run_steps(3);
    let from = h.get_by_role_and_label(Role::Slider, "Gravity").rect().center();
    h.hover_at(from);
    h.run_steps(3);
    h.event(UiEvent::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    for dx in [10.0, 30.0] {
        h.event(UiEvent::PointerMoved(from + vec2(dx, 0.0)));
        h.step();
    }
    h.event(UiEvent::PointerButton {
        pos: from + vec2(30.0, 0.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(4);
    let runner = &h.state().runner;
    assert_eq!(param(runner, "probe", "feathers"), json!(7));
    assert_ne!(param(runner, "probe", "gravity"), json!(0.05), "the drag moved the slider");
    let state = h.state();
    let first = state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(first.as_deref(), Some("Set gravity"), "the drag is one undo step");
    assert_eq!(param(&state.runner, "probe", "gravity"), json!(0.05), "and one undo undoes all of it");
    let gravity: Vec<_> =
        set_params(&state.runner).into_iter().filter(|r| r.args["path"] == json!("gravity")).collect();
    assert!(
        gravity.len() > 1 && gravity.windows(2).all(|w| w[0].tx == w[1].tx),
        "every move of the drag is in one transaction"
    );
    let second = state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(second.as_deref(), Some("Set feathers"), "and the typing is another, not part of the drag");
    assert!(state.runner.undo(&state.actor, &Grants::person()).unwrap_or_else(|e| panic!("{e}")).is_none());
}
