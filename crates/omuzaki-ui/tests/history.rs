//! The History panel (#446, `shell.md` section 9), drawn alone over a log: newest first, the filters, a row's details
//! from the keyboard, Adjust handed to the shell, and 50,000 records costing a frame what 50 do.

// A counting allocator is the only way to prove "no more allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use egui::accesskit::Role;
use egui::{Context, Event, Key, Modifiers, Pos2, RawInput, Rect, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocVersion, Patch};
use omuzaki_ops_api::{Actor, Cause, CauseKind, OpRecord, TxId};
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{harness_state, press};
use omuzaki_ui::ds::{Settings, apply_settings, set_strings};
use omuzaki_ui::shell::history::{History, HistoryLog};
use omuzaki_ui::shell::requests::{ShellRequest, ShellRequests};
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui::shell::text::{Word, say};
use omuzaki_ui_api::panel::{Panel as _, PanelCtx, ViewModels};
use serde_json::json;

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

fn designer() -> Actor {
    Actor::Agent { id: "designer".into(), model: None }
}

/// Record `seq` of a made-up project: the person sets the head's width, an agent changes the face profile, the app
/// renames the character, in turn.
fn record(seq: u64) -> OpRecord {
    let at = |path: &str| DocPath::parse(path).unwrap_or_else(|e| panic!("{e}"));
    let (op, args, actor, path) = match seq % 3 {
        0 => (
            "part.set_param",
            json!({"character": "kit", "part": "head", "path": "cranium/half_width", "value": 0.5 + seq as f64 / 1e6}),
            you(),
            "characters/kit/parts/head/params/cranium/half_width",
        ),
        1 => (
            "face.set_profile",
            json!({"character": "kit", "profile": "egg"}),
            designer(),
            "characters/kit/face/profile",
        ),
        _ => (
            "character.rename",
            json!({"character": "kit", "name": format!("Kit {seq}")}),
            Actor::System,
            "characters/kit/name",
        ),
    };
    let change =
        Patch::new().with(at(path), Some(json!(0.5)), Some(args.get("value").cloned().unwrap_or(json!("egg"))));
    let record = OpRecord::new(seq, op, args, actor, TxId(seq), Seconds(seq as f64), change, DocVersion(seq));
    if seq % 3 == 1 {
        record.with_cause(Cause { kind: CauseKind::Prompt, text: Some("make the face more oval".into()) })
    } else {
        record
    }
}

fn log(n: u64) -> HistoryLog {
    let records: Vec<OpRecord> = (1..=n).map(record).collect();
    let seen: HashMap<u64, Instant> = records.iter().map(|r| (r.seq, Instant::now())).collect();
    HistoryLog { records, undone: HashSet::from([TxId(n)]), seen, error: None }
}

struct Rig {
    history: History,
    views: ViewModels,
    requests: ShellRequests,
}

fn rig(n: u64) -> Harness<'static, Rig> {
    let mut views = ViewModels::new();
    views.insert(log(n));
    let requests = ShellRequests::default();
    views.insert(requests.clone());
    let rig = Rig { history: History::default(), views, requests };
    let mut h = harness_state(Settings::default(), vec2(360.0, 900.0), rig, |ui, r| {
        let actor = you();
        let ctx = PanelCtx::new(&NullRunner, &actor).with_views(&r.views);
        r.history.ui(ui, &ctx);
    });
    h.run_steps(3);
    h
}

/// The rows on screen, top first, by what they say.
fn rows(h: &Harness<'_, Rig>) -> Vec<String> {
    h.query_all_by_role(Role::Button)
        .filter_map(|n| n.accesskit_node().label())
        .filter(|l| l.contains(": ") && l.contains('.'))
        .collect()
}

#[test]
fn rows_are_newest_first_and_say_who_did_what_and_whether_it_is_undone() {
    let h = rig(30);
    let shown = rows(&h);
    assert!(shown.len() >= 5, "{shown:?}");
    assert!(shown[0].starts_with("You: part.set_param head \u{203a} cranium/half_width = 0.500"), "{shown:?}");
    assert!(shown[0].ends_with("undone"), "the newest step is undone: {shown:?}");
    assert!(shown[1].starts_with("Omuzaki: character.rename"), "{shown:?}");
    assert!(shown[2].starts_with("designer: face.set_profile"), "{shown:?}");
    assert!(h.query_all_by_label("designer").next().is_some(), "the actor chip names the agent");
}

#[test]
fn the_person_is_named_through_the_message_table() {
    // #296: "You" is the person's display name, not their identity, so a translation changes it.
    let mut h = rig(30);
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(3);
    let shown = rows(&h);
    let you = say(&h.ctx, Word::HistoryYou, &[]);
    assert_ne!(you, "You", "the pseudo-locale changes it");
    assert!(shown[0].starts_with(&format!("{you}: ")), "{shown:?}");
    assert!(shown.iter().all(|row| !row.starts_with("You: ")), "{shown:?}");
}

#[test]
fn the_filters_narrow_by_who_by_area_and_by_text() {
    let mut h = rig(30);
    h.get_by_label("Agents").click();
    h.run_steps(2);
    assert!(rows(&h).iter().all(|r| r.starts_with("designer:")), "{:?}", rows(&h));
    h.get_by_label("Everyone").click();
    h.run_steps(2);

    h.get_by_role_and_label(Role::ComboBox, "Area").click();
    h.run_steps(2);
    h.get_by_label("character").click();
    h.run_steps(2);
    assert!(!rows(&h).is_empty() && rows(&h).iter().all(|r| r.contains("character.rename")), "{:?}", rows(&h));

    h.get_by_role_and_label(Role::ComboBox, "Area").click();
    h.run_steps(2);
    h.get_by_label("All areas").click();
    h.run_steps(2);
    h.get_by_label("Filter history").focus();
    h.run();
    h.event(Event::Text("kit 11".to_owned()));
    h.run_steps(2);
    let shown = rows(&h);
    assert_eq!(
        shown,
        ["Omuzaki: character.rename, now"],
        "only the rename to `Kit 11` matches: the filter reads the arguments too"
    );
}

#[test]
fn the_keyboard_moves_through_rows_and_opens_a_steps_details() {
    let mut h = rig(30);
    let first = rows(&h)[0].clone();
    h.get_by_label(&first).focus();
    h.run();
    for _ in 0..2 {
        press(&mut h, Modifiers::NONE, Key::ArrowDown);
    }
    let focused =
        h.query_all_by_role(Role::Button).find(egui_kittest::Node::is_focused).and_then(|n| n.accesskit_node().label());
    assert!(focused.as_deref().is_some_and(|l| l.starts_with("designer: face.set_profile")), "{focused:?}");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    assert!(h.query_by_label("By designer, for: make the face more oval").is_some(), "the cause");
    assert!(h.query_by_label_contains("characters/kit/face/profile: 0.500 \u{2192} \"egg\"").is_some(), "the change");

    h.get_by_role_and_label(Role::Button, "Adjust").click();
    h.run_steps(2);
    let asked = h.state().requests.take();
    assert_eq!(
        asked,
        [ShellRequest::Adjust { op: "face.set_profile".into(), args: json!({"character": "kit", "profile": "egg"}) }],
        "Adjust hands the step to the palette as it ran"
    );

    let focused = h
        .query_all_by_role(Role::Button)
        .find(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with("designer:")));
    if let Some(row) = focused {
        row.focus();
    }
    h.run();
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label("By designer, for: make the face more oval").is_none(), "Esc closes the details");
}

#[test]
fn arrow_down_scrolls_the_list_to_rows_that_were_off_screen() {
    let mut h = rig(300);
    let first = rows(&h)[0].clone();
    h.get_by_label(&first).focus();
    h.run();
    for _ in 0..40 {
        press(&mut h, Modifiers::NONE, Key::ArrowDown);
    }
    h.run_steps(2);
    let focused =
        h.query_all_by_role(Role::Button).find(egui_kittest::Node::is_focused).and_then(|n| n.accesskit_node().label());
    // Row 40 from the top is record 260: 260 % 3 == 2, the app's rename.
    assert_eq!(focused.as_deref(), Some("Omuzaki: character.rename, now"));
    assert!(!rows(&h).contains(&first), "the list scrolled: the first row is off screen");
}

#[test]
fn an_empty_log_says_so() {
    let mut views = ViewModels::new();
    views.insert(HistoryLog::default());
    let mut h = harness_state(Settings::default(), vec2(360.0, 600.0), (History::default(), views), |ui, s| {
        let actor = you();
        let ctx = PanelCtx::new(&NullRunner, &actor).with_views(&s.1);
        s.0.ui(ui, &ctx);
    });
    h.run_steps(3);
    assert!(h.query_all_by_label("Your changes will appear here").next().is_some());
}

// ---- 50,000 records cost what 50 do ----

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

/// Allocations and seconds per frame for History over `n` records, in steady state.
fn per_frame(n: u64) -> (f64, f64) {
    let ctx = Context::default();
    apply_settings(&ctx, Settings::default());
    let mut views = ViewModels::new();
    views.insert(log(n));
    let mut history = History::default();
    let actor = you();
    let mut time = 0.0;
    let mut run = |time: f64| {
        let input = RawInput {
            time: Some(time),
            predicted_dt: 1.0 / 60.0,
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(360.0, 900.0))),
            ..RawInput::default()
        };
        ctx.run_ui(input, |ui| {
            let panel = PanelCtx::new(&NullRunner, &actor).with_views(&views);
            history.ui(ui, &panel);
        })
        .drop_without_applying_deltas();
    };
    for _ in 0..40 {
        time += 1.0 / 60.0;
        run(time);
    }
    let before = COUNT.with(Cell::get);
    let start = Instant::now();
    MEASURING.with(|m| m.set(true));
    for _ in 0..30 {
        time += 1.0 / 60.0;
        run(time);
    }
    MEASURING.with(|m| m.set(false));
    let seconds = start.elapsed().as_secs_f64() / 30.0;
    ((COUNT.with(Cell::get) - before) as f64 / 30.0, seconds)
}

#[test]
fn fifty_thousand_records_cost_a_frame_what_fifty_do() {
    let (small, small_time) = per_frame(50);
    let (large, large_time) = per_frame(50_000);
    assert!(large <= small * 1.05 + 2.0, "allocations per frame: {small} for 50 records, {large} for 50,000");
    // A ceiling, not a measurement: the work per frame is the visible rows', so it must not grow with the log.
    assert!(large_time < small_time * 4.0 + 0.004, "seconds per frame: {small_time} for 50, {large_time} for 50,000");
}
