//! The shell end to end, headless: structure at the two reference sizes, layouts that persist, the keys, and an idle
//! window that asks for nothing. Screenshots and frame rates on a real GPU are left to the maintainers' visual sign-off.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use egui::{Key, Modifiers, Vec2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, TxId};

use super::layout::{Dir, Layout, Node};
use super::persist::LayoutStore;
use super::registry::Group;
use super::stub::NullRunner;
use super::{Shell, registry};
use crate::ds::icons;
use crate::ds::testing::{drag, press};
use crate::register_workspace;

// A second workspace, so the numbered keys have something to switch to.
register_workspace! { id: "probe", title: "Probe", icon: icons::EYE, order: 90, group: Group::Make,
layout: probe_layout }

fn probe_layout() -> Layout {
    Layout::new(Node::split(Dir::Horizontal, vec![Node::pane("viewport"), Node::pane("inspector")], &[3.0, 1.0]))
}

/// A runner that remembers what the shell asked of it.
#[derive(Default)]
struct Spy {
    undos: AtomicUsize,
    redos: AtomicUsize,
    asked: Mutex<Vec<u64>>,
}

impl Runner for Spy {
    fn ops(&self) -> Vec<OpInfo> {
        NullRunner.ops()
    }
    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        NullRunner.run(req)
    }
    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        NullRunner.begin(label, actor)
    }
    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        NullRunner.commit(tx, actor)
    }
    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        NullRunner.rollback(tx, actor)
    }
    fn undo(&self, _actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        self.undos.fetch_add(1, Ordering::SeqCst);
        Ok(Some("hair colour".to_owned()))
    }
    fn redo(&self, _actor: &Actor, _grants: &Grants) -> Result<Option<String>, OpError> {
        self.redos.fetch_add(1, Ordering::SeqCst);
        Ok(None)
    }
    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        NullRunner.subscribe(grants)
    }
    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        NullRunner.get(path, grants)
    }
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(after_seq);
        }
        NullRunner.log(after_seq, limit, grants)
    }
}

fn shell_at(size: Vec2, store: Option<LayoutStore>, runner: Arc<dyn Runner>) -> Harness<'static, Shell> {
    let mut harness = Harness::builder()
        .with_size(size)
        .build_ui_state(|ui, shell: &mut Shell| shell.ui(ui), Shell::new(runner, store));
    // Fonts become active a pass after they are installed, and the tree is built on the first.
    harness.run_steps(4);
    harness
}

/// Whether any node on screen carries `label`: a headline is announced once as a label and once as text.
fn shows(harness: &Harness<'_, Shell>, label: &str) -> bool {
    harness.query_all_by_label(label).next().is_some()
}

#[track_caller]
fn assert_shows(harness: &Harness<'_, Shell>, label: &str) {
    assert!(shows(harness, label), "{label:?} is not on screen");
}

fn plain(size: Vec2) -> Harness<'static, Shell> {
    shell_at(size, None, Arc::new(NullRunner))
}

fn store() -> (tempfile::TempDir, LayoutStore) {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = LayoutStore::new(dir.path().join("layouts"));
    (dir, store)
}

fn shares(layout: &Layout) -> Vec<f32> {
    match &layout.root {
        Node::Split { shares, .. } => shares.clone(),
        other => panic!("the root is a split, not {other:?}"),
    }
}

#[test]
fn at_1920_by_1080_every_region_is_there_with_agent_and_history_as_their_own_group() {
    let harness = plain(vec2(1920.0, 1080.0));
    for label in ["Build", "Menu", "Nothing selected", "Nothing to undo", "No agent is working", "Idle", "Go Live"] {
        assert_shows(&harness, label);
    }
    // The shown tab of each group says what its panel is for; the others are tabs.
    for headline in ["Pick a part to edit it", "Start with a character", "Select a part to edit it", "Connect an agent"]
    {
        assert_shows(&harness, headline);
    }
    for tab in ["Outliner", "Library", "Viewport", "Inspector", "Agent", "History"] {
        assert_shows(&harness, tab);
    }
}

#[test]
fn at_1366_by_768_agent_and_history_join_the_inspector_group_and_tabs_become_icons() {
    let harness = plain(vec2(1366.0, 768.0));
    assert_shows(&harness, "Select a part to edit it");
    assert!(!shows(&harness, "Connect an agent"), "Agent is a tab behind the Inspector now");
    for tab in ["Inspector", "Agent", "History", "Outliner", "Library"] {
        assert_shows(&harness, tab);
    }
    // The active workspace keeps its label, the other is an icon whose label is its name.
    assert_shows(&harness, "Build");
    assert_shows(&harness, "Probe");
}

#[test]
fn at_the_smallest_window_the_left_dock_is_a_rail_whose_button_opens_a_drawer() {
    let mut harness = plain(vec2(1280.0, 720.0));
    assert!(!shows(&harness, "Pick a part to edit it"), "the Outliner is not docked");
    harness.get_by_label("Show Outliner").click();
    harness.run_steps(3);
    assert_shows(&harness, "Pick a part to edit it");
    press(&mut harness, Modifiers::NONE, Key::Escape);
    harness.run_steps(3);
    assert!(!shows(&harness, "Pick a part to edit it"), "Esc closes the drawer");
}

#[test]
fn a_layout_the_person_changes_is_saved_and_comes_back_in_the_next_session() {
    let (_keep, store) = store();
    let size = vec2(1920.0, 1080.0);
    let mut first = shell_at(size, Some(store.clone()), Arc::new(NullRunner));
    assert!(store.load("build").expect("readable").is_none(), "nothing is written until the person changes something");
    // Drag the divider between the viewport and the right dock 100 points to the left.
    drag(&mut first, &[egui::pos2(1560.0, 500.0), egui::pos2(1510.0, 500.0), egui::pos2(1460.0, 500.0)]);
    let saved = store.load("build").expect("readable").expect("saved when the drag ended");
    let default = shares(&plain(size).state().shown_layout().expect("built"));
    let ratio = |s: &[f32]| s[2] / s[1];
    assert!(ratio(&shares(&saved)) > ratio(&default), "the right dock got wider: {saved:?}");
    let second = shell_at(size, Some(store.clone()), Arc::new(NullRunner));
    assert_eq!(second.state().shown_layout(), Some(saved), "the next session shows the saved arrangement");
}

#[test]
fn a_small_window_never_overwrites_the_layout_made_on_a_big_one() {
    let (_keep, store) = store();
    let mut big = shell_at(vec2(1920.0, 1080.0), Some(store.clone()), Arc::new(NullRunner));
    drag(&mut big, &[egui::pos2(1560.0, 500.0), egui::pos2(1500.0, 500.0)]);
    let on_disk = std::fs::read_to_string(store.dir().join("build.toml")).expect("saved");
    let mut small = shell_at(vec2(1366.0, 768.0), Some(store.clone()), Arc::new(NullRunner));
    small.get_by_label("History").click();
    small.run_steps(3);
    drag(&mut small, &[egui::pos2(1066.0, 500.0), egui::pos2(1000.0, 500.0)]);
    assert_eq!(std::fs::read_to_string(store.dir().join("build.toml")).expect("still there"), on_disk);
}

#[test]
fn reset_layout_forgets_the_saved_one_and_a_corrupt_file_falls_back_to_the_default() {
    let (_keep, store) = store();
    let mut harness = shell_at(vec2(1920.0, 1080.0), Some(store.clone()), Arc::new(NullRunner));
    drag(&mut harness, &[egui::pos2(1560.0, 500.0), egui::pos2(1500.0, 500.0)]);
    assert!(store.dir().join("build.toml").exists());
    harness.state_mut().reset_layout();
    harness.run_steps(3);
    assert!(!store.dir().join("build.toml").exists());
    std::fs::write(store.dir().join("build.toml"), "not a layout [").expect("write");
    let corrupt = shell_at(vec2(1920.0, 1080.0), Some(store), Arc::new(NullRunner));
    assert_shows(&corrupt, "Connect an agent");
}

#[test]
fn the_numbered_keys_switch_workspaces_and_each_keeps_its_own_layout() {
    let mut harness = plain(vec2(1920.0, 1080.0));
    assert_eq!(harness.state().active_workspace(), Some("build"));
    press(&mut harness, Modifiers::COMMAND, Key::Num2);
    assert_eq!(harness.state().active_workspace(), Some("probe"));
    assert!(!shows(&harness, "Pick a part to edit it"), "Probe has no Outliner");
    press(&mut harness, Modifiers::COMMAND, Key::Num9);
    assert_eq!(harness.state().active_workspace(), Some("probe"), "there is no ninth tab");
    press(&mut harness, Modifiers::COMMAND, Key::Num1);
    assert_eq!(harness.state().active_workspace(), Some("build"));
    press(&mut harness, Modifiers::COMMAND, Key::Num0);
    assert_eq!(harness.state().active_workspace(), Some("build"), "there is no Theatre yet");
    assert!(registry::workspace("theatre").is_none());
}

#[test]
fn undo_and_redo_keys_go_through_the_runner_and_the_status_bar_says_what_happened() {
    let spy = Arc::new(Spy::default());
    let mut harness = shell_at(vec2(1920.0, 1080.0), None, spy.clone());
    press(&mut harness, Modifiers::COMMAND, Key::Z);
    assert_eq!(spy.undos.load(Ordering::SeqCst), 1);
    assert_shows(&harness, "Undid hair colour");
    press(&mut harness, Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    assert_eq!((spy.undos.load(Ordering::SeqCst), spy.redos.load(Ordering::SeqCst)), (1, 1));
    assert_shows(&harness, "Nothing to redo");
    assert_eq!(spy.asked.lock().expect("lock").len(), 3, "the log is read at start and after each step");
}

#[test]
fn lock_maximise_and_panel_cycling_keys_change_what_they_say() {
    let mut harness = plain(vec2(1920.0, 1080.0));
    press(&mut harness, Modifiers::COMMAND | Modifiers::SHIFT, Key::L);
    assert!(harness.state().locked());
    press(&mut harness, Modifiers::COMMAND | Modifiers::SHIFT, Key::L);
    assert!(!harness.state().locked());
    // Ctrl+Space with nothing focused does nothing; after F6 the focused panel fills the dock area, Esc restores.
    press(&mut harness, Modifiers::COMMAND, Key::Space);
    assert_shows(&harness, "Start with a character");
    press(&mut harness, Modifiers::NONE, Key::F6);
    press(&mut harness, Modifiers::COMMAND, Key::Space);
    harness.run_steps(2);
    assert!(!shows(&harness, "Start with a character"), "only the focused panel (the Outliner) is shown");
    assert_shows(&harness, "Pick a part to edit it");
    press(&mut harness, Modifiers::NONE, Key::Escape);
    harness.run_steps(2);
    assert_shows(&harness, "Start with a character");
}

/// Counts the repaints the shell asks the integration for.
fn count_repaints(harness: &Harness<'_, Shell>) -> Arc<AtomicUsize> {
    let counter = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&counter);
    harness.ctx.set_request_repaint_callback(move |_| {
        seen.fetch_add(1, Ordering::SeqCst);
    });
    counter
}

#[test]
fn an_idle_window_asks_for_no_repaint_over_thirty_simulated_seconds() {
    for size in [vec2(1920.0, 1080.0), vec2(1366.0, 768.0)] {
        let mut harness = plain(size);
        harness.run();
        let repaints = count_repaints(&harness);
        // Each step is a quarter of a simulated second.
        harness.run_steps(120);
        assert_eq!(repaints.load(Ordering::SeqCst), 0, "{size:?}: an idle shell keeps the CPU idle");
        // The counter does see a request, so the zero above means something.
        harness.ctx.request_repaint();
        assert!(repaints.load(Ordering::SeqCst) > 0);
    }
}
