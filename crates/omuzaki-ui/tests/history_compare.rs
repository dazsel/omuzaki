//! Comparing two History steps (#425) over the real executor and the kit, with a picture per step from a fake
//! thumbnailer: "Compare with…" then a second row, or Ctrl+click, opens the two states; the split moves by drag and by
//! the arrow keys; the heatmap counts the pixels that changed; the parameter diff is grouped by part and Select picks
//! the part in the outliner; Esc closes; nothing is run.
//!
//! The evidence test renders the panel and the window on the software GPU (ignored; run by hand or by the visual job).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use egui::accesskit::Role;
use egui::{Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_char_api::PartId;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner, Services, TxId};
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::testing::{all_strings, harness_state, press};
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::shell::history::{History, HistoryLog};
use omuzaki_ui::shell::pictures::{HistoryPictures, StepPictures};
use omuzaki_ui::studio;
use omuzaki_ui::wiring::{Core, Document, core_on};
use omuzaki_ui_api::panel::{Panel as _, PanelCtx, ViewModels};
use serde_json::json;

const WIDTH: &str = "cranium/half_width";

fn you() -> Actor {
    Actor::Person { name: "you".into() }
}

/// A picture per transaction: a disc on a clear ground, wider for each step.
struct Discs(HashMap<TxId, Vec<u8>>);

impl StepPictures for Discs {
    fn png(&self, tx: TxId) -> Option<Result<Vec<u8>, String>> {
        self.0.get(&tx).cloned().map(Ok)
    }
}

/// A 96x96 picture of a disc `radius` pixels across, in the kit's skin tone, on transparent.
fn disc(radius: f32) -> Vec<u8> {
    let picture = image::RgbaImage::from_fn(96, 96, |x, y| {
        #[allow(clippy::cast_precision_loss)]
        let (dx, dy) = (x as f32 - 48.0, y as f32 - 48.0);
        if (dx / radius).powi(2) + (dy / 36.0).powi(2) <= 1.0 {
            image::Rgba([240, 205, 190, 255])
        } else {
            image::Rgba([0, 0, 0, 0])
        }
    });
    let mut png = Vec::new();
    picture.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    png
}

struct Rig {
    core: Core,
    history: History,
    views: ViewModels,
    focus: EditFocus,
}

fn set_width(runner: &dyn Runner, value: f64) {
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
    runner.run(request).unwrap();
}

/// The kit with its head set to 0.2, then 0.21, then its hair recoloured, each with its picture; History over that log.
fn rig(size: egui::Vec2) -> Harness<'static, Rig> {
    omuzaki_ui::link();
    let core = core_on(Document::empty(), Services::default()).unwrap();
    let character = studio::open_character(core.runner.as_ref()).unwrap();
    let start = core.runner.log(0, 10_000, &Grants::person()).unwrap().len();
    set_width(core.runner.as_ref(), 0.2);
    set_width(core.runner.as_ref(), 0.21);
    let records = core.runner.log(0, 10_000, &Grants::person()).unwrap();
    let pictures: HashMap<TxId, Vec<u8>> =
        records[start..].iter().zip([20.0, 30.0]).map(|(r, radius)| (r.tx, disc(radius))).collect();
    let seen: HashMap<u64, Instant> = records.iter().map(|r| (r.seq, Instant::now())).collect();
    let log = HistoryLog { records, undone: HashSet::new(), seen, error: None, conflict: None };
    let mut views = ViewModels::new();
    views.insert(log);
    views.insert(HistoryPictures::new(Arc::new(Discs(pictures))));
    let focus = EditFocus::new(character);
    views.insert(focus.clone());
    let rig = Rig { core, history: History::default(), views, focus };
    let mut h = harness_state(Settings::default(), size, rig, |ui, r: &mut Rig| {
        let actor = you();
        egui::Panel::right("history").exact_size(360.0).resizable(false).show(ui, |ui| {
            let ctx = PanelCtx::new(r.core.runner.as_ref(), &actor).with_views(&r.views);
            r.history.ui(ui, &ctx);
        });
    });
    h.run_steps(3);
    h
}

/// Clicks the row of the step that set the width to `value`.
fn click_row(h: &mut Harness<'_, Rig>, value: &str) {
    let label = format!("= {value}");
    let found = h.query_all_by_label_contains(&label).find(|n| n.accesskit_node().role() == Role::Button);
    found.unwrap_or_else(|| panic!("a row setting {value}: {:#?}", all_strings(h))).click();
    h.run_steps(3);
}

/// The middle of that row.
fn row_centre(h: &Harness<'_, Rig>, value: &str) -> egui::Pos2 {
    let label = format!("= {value}");
    let found = h.query_all_by_label_contains(&label).find(|n| n.accesskit_node().role() == Role::Button);
    found.unwrap_or_else(|| panic!("a row setting {value}")).rect().center()
}

fn says(h: &Harness<'_, Rig>, words: &str) -> bool {
    all_strings(h).iter().any(|s| s.contains(words))
}

fn log_len(h: &Harness<'_, Rig>) -> usize {
    h.state().core.runner.log(0, 10_000, &Grants::person()).unwrap().len()
}

#[test]
fn compare_with_then_a_second_row_opens_both_states_and_runs_nothing() {
    let mut h = rig(vec2(1366.0, 900.0));
    let ops = log_len(&h);
    click_row(&mut h, "0.200");
    h.get_by_label("Compare with…").click();
    h.run_steps(3);
    assert!(says(&h, "Choose a step to compare with part.set_param head"), "{:#?}", all_strings(&h));
    click_row(&mut h, "0.210");
    assert!(says(&h, "Compare steps"), "the window is open: {:#?}", all_strings(&h));
    assert!(says(&h, "Before: part.set_param head \u{203a} cranium/half_width = 0.200"));
    assert!(says(&h, "After: part.set_param head \u{203a} cranium/half_width = 0.210"));

    // The parameter diff, grouped by part.
    let compare = h.state().history.compare().unwrap();
    let groups = compare.groups().unwrap();
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0].title(), "head");
    assert_eq!(groups[0].changes[0].path, "params/cranium/half_width");
    assert!(says(&h, "params/cranium/half_width: 0.200 \u{2192} 0.210"), "{:#?}", all_strings(&h));
    assert!(says(&h, "1 changed"));

    // The heatmap: the wider disc's two crescents, about a twelfth of the picture.
    let changed = h.state().history.compare().unwrap().changed().unwrap();
    assert!((0.05..0.15).contains(&changed), "{changed}");
    assert!(says(&h, "% of the picture changed"));
    h.get_by_label("Show what changed").click();
    h.run_steps(2);

    assert_eq!(log_len(&h), ops, "comparing ran no op");
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().history.compare().is_none(), "Esc closed the window");
}

#[test]
fn ctrl_click_compares_with_the_open_row_and_the_split_moves_by_keyboard() {
    let mut h = rig(vec2(1366.0, 900.0));
    click_row(&mut h, "0.210");
    let at = row_centre(&h, "0.200");
    // Ctrl held for the click, as the OS reports it.
    h.event(egui::Event::ModifiersChanged(Modifiers::COMMAND));
    h.event(egui::Event::PointerMoved(at));
    let (button, modifiers) = (egui::PointerButton::Primary, Modifiers::COMMAND);
    h.event(egui::Event::PointerButton { pos: at, button, pressed: true, modifiers });
    h.event(egui::Event::PointerButton { pos: at, button, pressed: false, modifiers });
    h.run_steps(4);
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    assert!(h.state().history.compare().is_some(), "Ctrl+click opened the compare: {:#?}", all_strings(&h));
    assert!(says(&h, "Before: part.set_param head \u{203a} cranium/half_width = 0.200"), "the older is before");

    let split = h.get_by_label("Before and after, side by side");
    split.focus();
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    let now = h.state().history.compare().unwrap().split();
    assert!((now - 0.6).abs() < 1e-5, "two steps right: {now}");

    // Select picks the part in the outliner.
    h.state().focus.clear();
    h.get_by_label("Select head").click();
    h.run_steps(2);
    assert_eq!(h.state().focus.selected(), vec![PartId::new("head").unwrap()]);
}

/// The panel with the compare window open, for the pull request, on the software GPU at 1366x768 and 1920x1080.
/// `OMUZAKI_EVIDENCE_OUT=<folder> cargo test -p omuzaki-ui --test history_compare -- --ignored`.
#[test]
#[ignore = "renders on the GPU (lavapipe in CI) and writes evidence images"]
fn history_compare_evidence() {
    let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") else { panic!("set OMUZAKI_EVIDENCE_OUT to a folder") };
    let out = std::path::PathBuf::from(out);
    std::fs::create_dir_all(&out).unwrap();
    for (w, hgt) in [(1366.0, 768.0), (1920.0, 1080.0)] {
        let mut h = rig(vec2(w, hgt));
        click_row(&mut h, "0.200");
        h.get_by_label("Compare with…").click();
        h.run_steps(3);
        click_row(&mut h, "0.210");
        h.render().unwrap().save(out.join(format!("history-compare-{w}x{hgt}.png"))).unwrap();
    }
}
