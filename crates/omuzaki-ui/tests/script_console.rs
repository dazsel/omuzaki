//! The Script workspace (#1097, #1221) over the real executor with the kit character open and My scripts in memory:
//!
//! - #1097: Ctrl+Enter runs the editor's script as one undo step and says so; Ctrl+Shift+Enter dry-runs it and shows
//!   the change it would make with nothing changed; a failing script changes nothing and points at its line; Esc stops
//!   a run and nothing changes; problems show after a pause and F8 goes to them; `ops::` completes areas, ops and a
//!   call's fields; a script someone else saved asks once before it runs; Save keeps it in My scripts and the Scripts
//!   panel lists it.
//! - #1221: Ctrl+Shift+M starts and stops a recording from anywhere in the studio with a REC chip in the status bar;
//!   the recording keeps the person's edits; Save to My scripts keeps it and Replay plays it as one undo step; Save as
//!   script writes a script that checks clean; the library offer says why it isn't there.
//!
//! The evidence test renders the workspace on the software GPU (ignored; run by hand or by the visual job).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Id, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner, ServicesBuilder};
use omuzaki_script::{MemScriptStore, Place, ScriptStore};
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::{all_strings, harness_state, press};
use omuzaki_ui::script::consent::{Author, ConsentBook};
use omuzaki_ui::script::console::{Console, EDITOR_ID};
use omuzaki_ui::script::macros::Macros;
use omuzaki_ui::script::ops_ref::OpsReference;
use omuzaki_ui::script::scripts::Scripts;
use omuzaki_ui::script::{Opened, Scripting};
use omuzaki_ui::shell::Shell;
use omuzaki_ui::studio;
use omuzaki_ui::wiring::{Core, Document, core_on};
use serde_json::json;

#[global_allocator]
static ALLOCATOR: omuzaki_script::MemoryBudget = omuzaki_script::MemoryBudget::system();

/// The head parameter the scripts edit.
const WIDTH: &str = "cranium/half_width";

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

struct Studio {
    core: Core,
    mine: Arc<MemScriptStore>,
}

fn studio() -> Studio {
    omuzaki_ui::link();
    let mine = Arc::new(MemScriptStore::new());
    let services = ServicesBuilder::new().insert::<dyn ScriptStore>(Arc::clone(&mine) as Arc<dyn ScriptStore>).build();
    let core = core_on(Document::empty(), services).unwrap();
    studio::open_character(core.runner.as_ref()).unwrap();
    Studio { core, mine }
}

impl Studio {
    fn runner(&self) -> &dyn Runner {
        self.core.runner.as_ref()
    }

    fn width(&self) -> f64 {
        let kit = CharId::new(studio::SAMPLE_ID).unwrap();
        let head = omuzaki_char_api::PartId::new("head").unwrap();
        let mut path: DocPath = paths::part_params(&kit, &head);
        for segment in WIDTH.split('/') {
            path = path.join(segment);
        }
        self.runner().get(&path, &Grants::person()).unwrap().and_then(|v| v.as_f64()).unwrap()
    }

    fn set_width(&self, value: f64) {
        let request = Request {
            op: "part.set_param".into(),
            args: json!({"character": studio::SAMPLE_ID, "part": "head", "path": WIDTH, "value": value}),
            mode: Mode::Apply,
            actor: you(),
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        self.runner().run(request).unwrap();
    }

    /// How many undo steps the log holds: one per transaction.
    fn undo_steps(&self) -> usize {
        let log = self.runner().log(0, 10_000, &Grants::person()).unwrap();
        log.iter().map(|record| record.tx).collect::<std::collections::BTreeSet<_>>().len()
    }
}

/// A call that sets the head's width, as a script writes it.
fn widen(value: f64) -> String {
    format!(
        "ops::part::set_param(#{{ character: \"{}\", part: \"head\", path: \"{WIDTH}\", value: {value:?} }});",
        studio::SAMPLE_ID
    )
}

/// The Script workspace's four panels side by side, as the workspace lays them out.
struct Rig {
    studio: Studio,
    scripting: Scripting,
    console: Console,
    macros: Macros,
    scripts: Scripts,
    ops: OpsReference,
}

fn frame(ui: &mut egui::Ui, r: &mut Rig) {
    let me = you();
    egui::Panel::left("scripts").exact_size(260.0).resizable(false).show(ui, |ui| {
        r.scripts.ui(ui, &r.scripting, &me, None);
    });
    egui::Panel::right("macros").exact_size(420.0).resizable(false).show(ui, |ui| {
        ui.allocate_ui(vec2(ui.available_width(), ui.available_height() * 0.55), |ui| {
            r.macros.ui(ui, &r.scripting, &me, None);
        });
        ui.separator();
        r.ops.ui(ui, &r.scripting, None);
    });
    egui::CentralPanel::default().show(ui, |ui| r.console.ui(ui, &r.scripting, &me, None));
}

fn rig() -> Harness<'static, Rig> {
    let studio = studio();
    let mine = Arc::clone(&studio.mine) as Arc<dyn ScriptStore>;
    let scripting = Scripting::new(Arc::clone(&studio.core.runner), Some(mine), "test", ConsentBook::in_memory());
    let rig = Rig {
        studio,
        scripting,
        console: Console::default(),
        macros: Macros::default(),
        scripts: Scripts::default(),
        ops: OpsReference::default(),
    };
    let mut h = harness_state(Settings::default(), vec2(1600.0, 1000.0), rig, frame);
    h.run_steps(3);
    h
}

/// Opens `source` in the console as `author` wrote it.
fn open(h: &mut Harness<'_, Rig>, name: &str, source: &str, author: Author) {
    let opened = Opened { saved: None, name: name.into(), source: source.into(), author };
    h.state().scripting.open(opened);
    h.run_steps(3);
}

fn until<S>(h: &mut Harness<'_, S>, what: &str, done: impl Fn(&Harness<'_, S>) -> bool) {
    let started = Instant::now();
    while !done(h) {
        assert!(started.elapsed() < Duration::from_secs(30), "{what} never happened: {:#?}", all_strings(h));
        std::thread::yield_now();
        h.run_steps(4);
    }
}

fn says<S>(h: &Harness<'_, S>, words: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(words))
}

fn click<S>(h: &mut Harness<'_, S>, label: &str) {
    h.get_by_label(label).scroll_to_me();
    h.run_steps(2);
    h.get_by_label(label).click();
    h.run_steps(3);
}

fn focus_editor<S>(h: &mut Harness<'_, S>) {
    h.get_by_role_and_label(Role::MultilineTextInput, "Script").focus();
    h.run_steps(2);
}

/// The editor's cursor, in characters.
fn cursor<S>(h: &Harness<'_, S>) -> Option<usize> {
    egui::TextEdit::load_state(&h.ctx, Id::new(EDITOR_ID))
        .and_then(|s| s.cursor.char_range())
        .map(|r| r.primary.index.0)
}

#[test]
fn ctrl_enter_runs_the_script_as_one_undo_step_and_says_so() {
    let mut h = rig();
    let before = h.state().studio.width();
    let steps = h.state().studio.undo_steps();
    let source = format!("print(\"widening\");\n{}\n{}", widen(before * 1.1), widen(before * 1.2));
    open(&mut h, "Widen", &source, Author::Me);
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    until(&mut h, "the run's undo step", |h| says(h, "Undo: Widen, 2 ops"));
    assert!(says(&h, "widening"), "the output shows what it printed");
    assert!((h.state().studio.width() - before * 1.2).abs() < 1e-9, "both calls landed");
    assert_eq!(h.state().studio.undo_steps(), steps + 1, "the run is one step in the log");
    let undone = h.state().studio.runner().undo(&you(), &Grants::person()).unwrap();
    assert!(undone.is_some());
    assert!((h.state().studio.width() - before).abs() < 1e-9, "one undo takes the whole run back");
}

#[test]
fn a_dry_run_shows_the_change_and_changes_nothing() {
    let mut h = rig();
    let before = h.state().studio.width();
    open(&mut h, "Widen", &widen(before * 1.5), Author::Me);
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter);
    until(&mut h, "the dry run's result", |h| says(h, "Dry run: nothing changed"));
    assert!(says(&h, "part.set_param"), "it names the op it would call");
    assert!(says(&h, "half_width"), "and the path it would change: {:#?}", all_strings(&h));
    assert!((h.state().studio.width() - before).abs() < 1e-12, "nothing changed");
}

#[test]
fn a_failing_script_changes_nothing_and_points_at_its_line() {
    let mut h = rig();
    let before = h.state().studio.width();
    let source = format!("{}\nlet x = 1;\nthrow \"no more\";", widen(before * 2.0));
    open(&mut h, "Breaks", &source, Author::Me);
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    until(&mut h, "the failure", |h| says(h, "Nothing changed: the run was undone whole."));
    let failed = all_strings(&h).into_iter().find(|s| s.starts_with("Line 3, column")).unwrap_or_else(|| {
        panic!("the failure names line 3: {:#?}", all_strings(&h));
    });
    assert!((h.state().studio.width() - before).abs() < 1e-12, "the first call was rolled back");
    click(&mut h, &failed);
    let at = cursor(&h).unwrap();
    let line_start = source.split('\n').take(2).map(|l| l.chars().count() + 1).sum::<usize>();
    assert!(at >= line_start, "the click put the cursor on line 3 ({at} from {line_start})");
}

#[test]
fn escape_stops_a_run_and_nothing_changes() {
    let mut h = rig();
    let before = h.state().studio.width();
    let source = format!("{}\nlet n = 0;\nloop {{ n += 1; }}", widen(before * 1.05));
    open(&mut h, "Forever", &source, Author::Me);
    focus_editor(&mut h);
    // Ctrl+Enter, one frame for the run to start, then Esc on the next: the loop runs far longer than that.
    key(&mut h, Modifiers::COMMAND, Key::Enter);
    h.step();
    assert!(h.state().console.running(), "the run is under way");
    assert!(says(&h, "Running… Esc stops it."));
    key(&mut h, Modifiers::NONE, Key::Escape);
    h.step();
    until(&mut h, "the stop", |h| says(h, "Stopped. Nothing changed."));
    assert!(!h.state().console.running());
    assert!((h.state().studio.width() - before).abs() < 1e-12, "the stopped run was rolled back");
}

#[test]
fn problems_show_after_a_pause_and_f8_goes_to_each() {
    let mut h = rig();
    let source = "let a = 1;\nlet b = ;\nlet c = 3;\nlet d = ;";
    open(&mut h, "Typos", source, Author::Me);
    until(&mut h, "the problems line", |h| !h.state().console.problems().is_empty());
    let problems = h.state().console.problems();
    assert!(says(&h, "problem"), "the count shows: {:#?}", all_strings(&h));
    focus_editor(&mut h);
    let mut state = egui::TextEdit::load_state(&h.ctx, Id::new(EDITOR_ID)).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(0))));
    state.store(&h.ctx, Id::new(EDITOR_ID));
    press(&mut h, Modifiers::NONE, Key::F8);
    h.run_steps(2);
    let first = cursor(&h).unwrap();
    let line = source[..source.char_indices().nth(first).map_or(source.len(), |(b, _)| b)].matches('\n').count() + 1;
    assert_eq!(u32::try_from(line).unwrap(), problems[0].line, "F8 went to the first problem");
    if problems.len() > 1 {
        press(&mut h, Modifiers::NONE, Key::F8);
        h.run_steps(2);
        assert!(cursor(&h).unwrap() > first, "F8 again went on to the next");
        press(&mut h, Modifiers::SHIFT, Key::F8);
        h.run_steps(2);
        assert_eq!(cursor(&h).unwrap(), first, "Shift+F8 went back");
    }
    // A clean script says so.
    open(&mut h, "Clean", "let a = 1;", Author::Me);
    until(&mut h, "the clean check", |h| says(h, "No problems"));
}

#[test]
fn ops_completes_areas_then_ops_then_a_calls_fields() {
    let mut h = rig();
    focus_editor(&mut h);
    h.event(Event::Text("ops::par".into()));
    h.run_steps(3);
    assert!(h.query_by_label_contains("part  ").is_some(), "the area list offers part: {:#?}", all_strings(&h));
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    assert!(h.state().console.source().starts_with("ops::part::"), "{}", h.state().console.source());
    h.event(Event::Text("set_p".into()));
    h.run_steps(3);
    assert!(
        h.query_all_by_label_contains("set_param  ").next().is_some(),
        "the op list offers set_param: {:#?}",
        all_strings(&h)
    );
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(2);
    let source = h.state().console.source().to_owned();
    assert!(source.starts_with("ops::part::set_param"), "{source}");
    // Inside the call's map, Ctrl+Space lists its fields with their kinds.
    let at_end = source.chars().count();
    let mut state = egui::TextEdit::load_state(&h.ctx, Id::new(EDITOR_ID)).unwrap_or_default();
    state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(at_end))));
    state.store(&h.ctx, Id::new(EDITOR_ID));
    if !source.contains("#{") {
        h.event(Event::Text("(#{ ".into()));
        h.run_steps(2);
    }
    press(&mut h, Modifiers::COMMAND, Key::Space);
    h.run_steps(3);
    assert!(
        h.query_all_by_label_contains("character  ").next().is_some(),
        "the field list offers character: {:#?}",
        all_strings(&h)
    );
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(2);
    assert!(h.query_by_label_contains("character  ").is_none(), "Esc closed the list");
}

#[test]
fn a_script_someone_else_saved_asks_once_before_it_runs() {
    let mut h = rig();
    let before = h.state().studio.width();
    open(&mut h, "From the agent", &widen(before * 1.3), Author::Agent("designer".into()));
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    h.run_steps(3);
    assert!(says(&h, "Run this script?"), "the card asks: {:#?}", all_strings(&h));
    assert!(says(&h, "The agent designer saved this script."));
    assert!(says(&h, "part.set_param"), "it lists the op the script calls");
    click(&mut h, "Cancel");
    assert!(!says(&h, "Run this script?"));
    assert!((h.state().studio.width() - before).abs() < 1e-12, "Cancel ran nothing");

    press(&mut h, Modifiers::COMMAND, Key::Enter);
    h.run_steps(3);
    // The card's Run is the last one; the toolbar's is behind it.
    h.query_all_by_label("Run").last().unwrap().click();
    h.run_steps(3);
    until(&mut h, "the agreed run", |h| says(h, "Undo: From the agent, 1 ops"));
    assert!((h.state().studio.width() - before * 1.3).abs() < 1e-9);

    // Agreed once, the same script runs without asking again.
    h.state().studio.set_width(before);
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    h.run_steps(3);
    assert!(!says(&h, "Run this script?"), "the agreement holds for the same script");
    until(&mut h, "the second run", |h| (h.state().studio.width() - before * 1.3).abs() < 1e-9);

    // The person's own script that stays in the studio never asks; a dry run never asks.
    open(&mut h, "Mine", &widen(before), Author::Me);
    press(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::Enter);
    h.run_steps(3);
    assert!(!says(&h, "Run this script?"));
}

#[test]
fn save_keeps_the_script_in_my_scripts_and_the_list_shows_it() {
    let mut h = rig();
    open(&mut h, "Tidy head", "let a = 1;", Author::Me);
    click(&mut h, "Save");
    until(&mut h, "the save note", |h| says(h, "Saved as Tidy head"));
    let saved = h.state().studio.mine.list().unwrap();
    assert!(saved.iter().any(|s| s.name == "Tidy head"), "My scripts holds it: {saved:?}");
    until(&mut h, "the Scripts list", |h| h.query_by_label("Open Tidy head, script").is_some());

    click(&mut h, "Save to project");
    until(&mut h, "the project copy", |h| {
        h.state().scripts.listed().iter().any(|l| l.place == Place::Project && l.name.as_deref() == Some("Tidy head"))
    });

    // Opening it from the list puts it in the console.
    open(&mut h, "", "", Author::Me);
    h.query_all_by_label("Open Tidy head, script").next().unwrap().click();
    h.run_steps(3);
    assert_eq!(h.state().console.source(), "let a = 1;");

    // Delete asks once.
    h.get_all_by_label("Delete Tidy head").next().unwrap().click();
    h.run_steps(3);
    click(&mut h, "Delete");
    until(&mut h, "one copy gone", |h| {
        h.state().scripts.listed().iter().filter(|l| l.name.as_deref() == Some("Tidy head")).count() == 1
    });
}

#[test]
fn a_recorded_macro_saves_replays_as_one_step_and_becomes_a_clean_script() {
    let mut h = rig();
    let before = h.state().studio.width();
    click(&mut h, "Record a macro");
    assert!(h.state().scripting.recording());
    assert!(says(&h, "● REC"));
    h.state().studio.set_width(before * 1.25);
    h.state().studio.set_width(before * 1.5);
    h.run_steps(3);
    click(&mut h, "Stop recording");
    assert!(!h.state().scripting.recording());
    let recorded = h.state().scripting.recorded().unwrap();
    assert_eq!(recorded.steps.len(), 2, "both edits were recorded");
    assert!(says(&h, "1. part.set_param"), "the steps show: {:#?}", all_strings(&h));

    // Save to My scripts, then pick it and replay it.
    h.get_by_label("Macro name").focus();
    h.run_steps(1);
    press(&mut h, Modifiers::COMMAND, Key::A);
    h.event(Event::Text("Wide head".into()));
    h.run_steps(2);
    click(&mut h, "Save to My scripts");
    until(&mut h, "the save", |h| says(h, "Saved as Wide head"));
    assert!(h.state().scripting.recorded().is_none(), "the recording was put away");
    until(&mut h, "the saved macro", |h| h.query_by_label_contains("Wide head  ·").is_some());
    h.query_by_label_contains("Wide head  ·").unwrap().click();
    h.run_steps(3);

    h.state().studio.set_width(before);
    let steps = h.state().studio.undo_steps();
    click(&mut h, "Replay");
    until(&mut h, "the replay", |h| says(h, "Replayed 2 steps as one undo step."));
    assert!((h.state().studio.width() - before * 1.5).abs() < 1e-9, "the replay made the same edits");
    assert_eq!(h.state().studio.undo_steps(), steps + 1, "as one step");

    // The library offer says why it isn't there in this build.
    let library = h.get_by_label("Save as library item");
    assert!(library.accesskit_node().is_disabled(), "the library save isn't in this build, and the button says so");

    // Save as script writes a script into My scripts and opens it; it checks clean and runs.
    click(&mut h, "Save as script");
    let source = h.state().console.source().to_owned();
    assert!(source.contains("ops::part::set_param"), "{source}");
    until(&mut h, "the check", |h| says(h, "No problems"));
    assert!(h.state().studio.mine.list().unwrap().iter().any(|s| s.id == "wide-head-script"));
    h.state().studio.set_width(before);
    press(&mut h, Modifiers::COMMAND, Key::Enter);
    until(&mut h, "the script's run", |h| says(h, "Undo: Wide head, 2 ops"));
    assert!((h.state().studio.width() - before * 1.5).abs() < 1e-9);
}

#[test]
fn ctrl_shift_m_records_from_anywhere_with_a_rec_chip_in_the_status_bar() {
    let studio = studio();
    let mine = Arc::clone(&studio.mine) as Arc<dyn ScriptStore>;
    let scripting = Scripting::new(Arc::clone(&studio.core.runner), Some(mine), "test", ConsentBook::in_memory());
    let mut shell = Shell::new(Arc::clone(&studio.core.runner), None);
    shell.insert_view(scripting.clone());
    let mut h = Harness::builder()
        .with_size(vec2(1600.0, 1000.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, s: &mut Shell| s.ui(ui), shell);
    h.run_steps(4);
    press_steps(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::M);
    assert!(scripting.recording(), "Ctrl+Shift+M started a recording");
    assert!(says(&h, "● REC"), "the status bar shows REC: {:#?}", all_strings(&h));
    assert!(says(&h, "Recording a macro. Ctrl+Shift+M stops."));
    let before = studio.width();
    studio.set_width(before * 1.1);
    h.run_steps(4);
    press_steps(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::M);
    assert!(!scripting.recording(), "and stopped it");
    assert!(!says(&h, "● REC"));
    assert!(says(&h, "Recording stopped."));
    assert_eq!(scripting.recorded().map(|m| m.steps.len()), Some(1), "the edit is in the recording");

    // The Script workspace is there, with its four panels.
    h.query_all_by_label("Script")
        .find(|n| matches!(n.accesskit_node().role(), Role::Tab | Role::Button))
        .unwrap()
        .click();
    h.run_steps(4);
    for panel in ["Scripts", "Console", "Macros", "Ops"] {
        assert!(h.query_all_by_label(panel).next().is_some(), "the {panel} panel: {:#?}", all_strings(&h));
    }
}

/// Presses a key and runs a few frames (the shell repaints on its own clock, so it never idles).
fn press_steps<S>(h: &mut Harness<'_, S>, modifiers: Modifiers, key: Key) {
    self::key(h, modifiers, key);
    h.run_steps(4);
}

/// Queues a key's press and release for the next frame.
fn key<S>(h: &mut Harness<'_, S>, modifiers: Modifiers, key: Key) {
    for pressed in [true, false] {
        h.event(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
    }
}

/// The Script workspace for the pull request, on the software GPU at 1920x1080 and 1366x768: a run done in the
/// console, a recording waiting to be saved in Macros. `OMUZAKI_EVIDENCE_OUT=<folder> cargo test -p omuzaki-ui --test
/// script_console -- --ignored`.
#[test]
#[ignore = "renders on the GPU (lavapipe in CI) and writes evidence images"]
fn script_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    for (w, hgt) in [(1920.0, 1080.0), (1366.0, 768.0)] {
        let image = script_workspace(vec2(w, hgt));
        image.save(out.join(format!("script-{w}x{hgt}.png"))).unwrap();
    }
}

/// The studio in the Script workspace at `size`: a run done, a recording waiting to be saved.
fn script_workspace(size: egui::Vec2) -> image::RgbaImage {
    let studio = studio();
    let mine = Arc::clone(&studio.mine) as Arc<dyn ScriptStore>;
    let scripting = Scripting::new(Arc::clone(&studio.core.runner), Some(mine), "test", ConsentBook::in_memory());
    let mut shell = Shell::new(Arc::clone(&studio.core.runner), None);
    shell.insert_view(scripting.clone());
    let mut h = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, s: &mut Shell| s.ui(ui), shell);
    h.run_steps(4);
    h.query_all_by_label("Script")
        .find(|n| matches!(n.accesskit_node().role(), Role::Tab | Role::Button))
        .unwrap()
        .click();
    h.run_steps(4);
    press_steps(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::M);
    studio.set_width(0.2);
    studio.set_width(0.21);
    h.run_steps(4);
    press_steps(&mut h, Modifiers::COMMAND | Modifiers::SHIFT, Key::M);
    let source = format!(
        "// Widen the head a little, then a little more.\nprint(\"widening\");\n{}\n{}",
        widen(0.22),
        widen(0.23)
    );
    scripting.open(Opened { saved: None, name: "Widen".into(), source, author: Author::Me });
    until(&mut h, "the check", |h| says(h, "No problems"));
    press_steps(&mut h, Modifiers::COMMAND, Key::Enter);
    until(&mut h, "the run", |h| says(h, "Undo: Widen"));
    // Away from the tab the click left the pointer on.
    h.event(Event::PointerMoved(egui::pos2(size.x / 2.0, size.y - 40.0)));
    h.run_steps(8);
    h.render().unwrap()
}
