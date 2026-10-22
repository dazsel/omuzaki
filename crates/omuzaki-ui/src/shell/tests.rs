//! The shell end to end, headless: structure at the two reference sizes, layouts that persist, the keys, and an idle
//! window that asks for nothing. Screenshots and frame rates on a real GPU are left to the maintainers' visual sign-off.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui::{Key, Modifiers, Ui, Vec2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{DocPath, DocVersion, Patch, Value};
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, TxId};

use omuzaki_ui_api::layout::{Dir as ApiDir, Node as ApiNode};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, Region};
use omuzaki_ui_api::{register_panel, register_workspace};

use super::Shell;
use super::layout::{Dir, Layout, Node};
use super::persist::LayoutStore;
use super::stub::NullRunner;
use crate::ds::icons;
use crate::ds::testing::{drag, press};

// A second workspace, so the numbered keys have something to switch to.
register_workspace! { id: "probe", title: "Probe", icon: icons::EYE, order: 90, group: Group::Make,
default_layout: probe_layout }

/// A left-dock panel no layout names: the one the View menu adds where the shell shows only a rail.
struct Extra;

impl Panel for Extra {
    fn ui(&mut self, ui: &mut Ui, _ctx: &PanelCtx<'_>) {
        ui.label("Extra body");
    }
}

register_panel! { id: "test.extra", title: "Extra", icon: icons::EYE, region: Region::Left, min_size: [240.0, 160.0],
order: 900, build: |_| Box::new(Extra) }

fn probe_layout() -> ApiNode {
    ApiNode::split(ApiDir::Horizontal, vec![ApiNode::pane("viewport"), ApiNode::pane("inspector")], &[3.0, 1.0])
}

fn record(seq: u64) -> OpRecord {
    OpRecord::new(seq, "test.set", Value::Null, Actor::System, TxId(seq), Seconds(0.0), Patch::new(), DocVersion(seq))
}

/// A runner that remembers what the shell asked of it.
#[derive(Default)]
struct Spy {
    /// How many records the log holds.
    total: u64,
    /// The most records one `log` call returns, however many were asked for; 0 means as many as asked.
    page: usize,
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
    fn log(&self, after_seq: u64, limit: usize, _grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        if let Ok(mut asked) = self.asked.lock() {
            asked.push(after_seq);
        }
        let limit = if self.page == 0 { limit } else { limit.min(self.page) };
        let last = self.total.min(after_seq.saturating_add(limit as u64));
        Ok(((after_seq + 1)..=last).map(record).collect())
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
    for headline in ["No character is open", "Start with a character", "Select a part to edit it", "Connect an agent"] {
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
    assert!(!shows(&harness, "No character is open"), "the Outliner is not docked");
    harness.get_by_label("Show Outliner").click();
    harness.run_steps(3);
    assert_shows(&harness, "No character is open");
    press(&mut harness, Modifiers::NONE, Key::Escape);
    harness.run_steps(3);
    assert!(!shows(&harness, "No character is open"), "Esc closes the drawer");
}

/// The rail's buttons, which are one per left-dock panel the rail stands for.
fn rail_shows(harness: &Harness<'_, Shell>, panel: &str) -> bool {
    harness.query_by_label(&format!("Show {panel}")).is_some()
}

#[test]
fn a_left_panel_shown_while_the_dock_is_a_rail_joins_the_rail_and_is_still_there_when_the_dock_unfolds() {
    let (_keep, store) = store();
    let mut harness = shell_at(vec2(1280.0, 720.0), Some(store.clone()), Arc::new(NullRunner));
    assert!(!rail_shows(&harness, "Extra"), "Build does not dock the Extra panel");
    harness.state_mut().show_panel("test.extra");
    harness.run_steps(3);
    assert!(rail_shows(&harness, "Extra"), "the rail stands for it now");
    assert_shows(&harness, "Extra body");
    // Unfolding: the dock is back with the panel in it, in the arrangement that is saved.
    harness.set_size(vec2(1920.0, 1080.0));
    harness.run_steps(4);
    assert!(harness.state().shown_layout().is_some_and(|l| l.contains("test.extra")), "it is docked at full size");
    harness.state().flush();
    let saved = store.load("build").expect("readable").expect("the full-size arrangement was saved");
    assert!(saved.contains("test.extra") && saved.contains("viewport"), "{:?}", saved.panels());
    // Folding again keeps it on the rail.
    harness.set_size(vec2(1280.0, 720.0));
    harness.run_steps(4);
    assert!(rail_shows(&harness, "Extra"));
}

#[test]
fn a_panel_shown_or_closed_in_a_folded_window_stays_so_when_the_fold_changes() {
    // 1366 merges Agent and History into the Inspector group: a fold, but the left dock stays open.
    let mut harness = shell_at(vec2(1366.0, 768.0), None, Arc::new(NullRunner));
    harness.state_mut().show_panel("test.extra");
    harness.run_steps(3);
    assert_shows(&harness, "Extra body");
    for size in [vec2(1280.0, 720.0), vec2(1920.0, 1080.0)] {
        harness.set_size(size);
        harness.run_steps(4);
        let rail = rail_shows(&harness, "Extra");
        let docked = harness.state().shown_layout().is_some_and(|l| l.contains("test.extra"));
        assert!(rail || docked, "{size:?}: the panel shown at 1366 is still there");
        // The panel the person was in stays open: docked, or as the drawer when its dock folds into the rail.
        assert!(shows(&harness, "Extra body"), "{size:?}: the panel the person was in is open");
    }
    harness.set_size(vec2(1366.0, 768.0));
    harness.run_steps(4);
    harness.state_mut().close_panel("test.extra");
    harness.run_steps(3);
    for size in [vec2(1280.0, 720.0), vec2(1920.0, 1080.0)] {
        harness.set_size(size);
        harness.run_steps(4);
        assert!(!rail_shows(&harness, "Extra"), "{size:?}: closed at 1366, so not on the rail");
        assert!(!harness.state().shown_layout().is_some_and(|l| l.contains("test.extra")), "{size:?}: nor docked");
    }
}

#[test]
fn a_left_panel_added_to_the_rail_is_there_in_the_next_session() {
    let (_keep, store) = store();
    let size = vec2(1280.0, 720.0);
    let mut first = shell_at(size, Some(store.clone()), Arc::new(NullRunner));
    first.state_mut().show_panel("test.extra");
    first.run_steps(3);
    first.state().flush();
    let second = shell_at(size, Some(store.clone()), Arc::new(NullRunner));
    assert!(rail_shows(&second, "Extra"), "a new session at the small size still has it on the rail");
    // And a big window finds it docked, not only a small one.
    let big = shell_at(vec2(1920.0, 1080.0), Some(store), Arc::new(NullRunner));
    assert!(big.state().shown_layout().is_some_and(|l| l.contains("test.extra")));
}

#[test]
fn a_layout_the_person_changes_is_saved_and_comes_back_in_the_next_session() {
    let (_keep, store) = store();
    let size = vec2(1920.0, 1080.0);
    let mut first = shell_at(size, Some(store.clone()), Arc::new(NullRunner));
    assert!(store.load("build").expect("readable").is_none(), "nothing is written until the person changes something");
    // Drag the divider between the viewport and the right dock 100 points to the left.
    drag(&mut first, &[egui::pos2(1552.0, 500.0), egui::pos2(1510.0, 500.0), egui::pos2(1460.0, 500.0)]);
    first.state().flush();
    let saved = store.load("build").expect("readable").expect("saved when the drag ended");
    let default = shares(&plain(size).state().shown_layout().expect("built"));
    let ratio = |s: &[f32]| s[2] / s[1];
    assert!(ratio(&shares(&saved)) > ratio(&default), "the right dock got wider: {saved:?}");
    let second = shell_at(size, Some(store.clone()), Arc::new(NullRunner));
    assert_eq!(second.state().shown_layout(), Some(saved), "the next session shows the saved arrangement");
}

/// A `build.toml` exactly as #70's shell wrote it: the Inspector moved to the left of the viewport, Agent and History
/// pulled out into a group of their own, and a panel (`telemetry`) that a later version of the app no longer has.
const SAVED_BY_70: &str = r#"version = 1

[root]
kind = "split"
dir = "horizontal"
shares = [360.0, 1200.0, 300.0]

[[root.children]]
kind = "split"
dir = "vertical"
shares = [2.0, 1.0]

[[root.children.children]]
kind = "pane"
panel = "inspector"

[[root.children.children]]
kind = "tabs"
active = 1

[[root.children.children.children]]
kind = "pane"
panel = "agent"

[[root.children.children.children]]
kind = "pane"
panel = "history"

[[root.children]]
kind = "pane"
panel = "viewport"

[[root.children]]
kind = "tabs"
active = 0

[[root.children.children]]
kind = "pane"
panel = "outliner"

[[root.children.children]]
kind = "pane"
panel = "telemetry"
"#;

#[test]
fn a_layout_file_written_by_the_first_shell_still_loads_through_the_registry() {
    let (_keep, store) = store();
    std::fs::create_dir_all(store.dir()).expect("dir");
    std::fs::write(store.dir().join("build.toml"), SAVED_BY_70).expect("write");
    let harness = shell_at(vec2(1920.0, 1080.0), Some(store), Arc::new(NullRunner));
    let shown = harness.state().shown_layout().expect("built");
    // The arrangement is the saved one, less the panel nobody registers any more; the tab group left with one panel
    // is just that panel.
    assert_eq!(shown.panels(), ["inspector", "agent", "history", "viewport", "outliner"]);
    let Node::Split { children, shares, .. } = &shown.root else { panic!("the root is a split") };
    assert_eq!(shares, &[360.0, 1200.0, 300.0]);
    assert_eq!(children[2], Node::pane("outliner"));
    assert_shows(&harness, "No character is open");
    assert_shows(&harness, "Select a part to edit it");
}

#[test]
fn a_small_window_keeps_its_own_layout_and_never_overwrites_the_one_made_on_a_big_one() {
    let (_keep, store) = store();
    let mut big = shell_at(vec2(1920.0, 1080.0), Some(store.clone()), Arc::new(NullRunner));
    drag(&mut big, &[egui::pos2(1552.0, 500.0), egui::pos2(1500.0, 500.0)]);
    big.state().flush();
    let on_disk = std::fs::read_to_string(store.dir().join("build.toml")).expect("saved");
    big.state().flush();
    let size = vec2(1366.0, 768.0);
    let mut small = shell_at(size, Some(store.clone()), Arc::new(NullRunner));
    drag(&mut small, &[egui::pos2(1058.0, 500.0), egui::pos2(1000.0, 500.0)]);
    small.state().flush();
    assert_eq!(std::fs::read_to_string(store.dir().join("build.toml")).expect("still there"), on_disk);
    // What was done in the small window is kept under its own name and comes back at that size.
    let compact = store.dir().join("build-compact-la.toml");
    assert!(compact.exists(), "the compact arrangement is saved apart");
    let again = shell_at(size, Some(store), Arc::new(NullRunner));
    assert_eq!(again.state().shown_layout(), small.state().shown_layout());
}

#[test]
fn reset_layout_forgets_the_saved_one_and_a_corrupt_file_falls_back_to_the_default() {
    let (_keep, store) = store();
    let mut harness = shell_at(vec2(1920.0, 1080.0), Some(store.clone()), Arc::new(NullRunner));
    drag(&mut harness, &[egui::pos2(1552.0, 500.0), egui::pos2(1500.0, 500.0)]);
    harness.state().flush();
    assert!(store.dir().join("build.toml").exists());
    harness.state_mut().reset_layout();
    harness.run_steps(3);
    harness.state().flush();
    assert!(!store.dir().join("build.toml").exists());
    let default = plain(vec2(1920.0, 1080.0)).state().shown_layout();
    assert_eq!(
        harness.state().shown_layout(),
        default,
        "the default is back at once, not the file that was being removed"
    );
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
    assert!(!shows(&harness, "No character is open"), "Probe has no Outliner");
    press(&mut harness, Modifiers::COMMAND, Key::Num9);
    assert_eq!(harness.state().active_workspace(), Some("probe"), "there is no ninth tab");
    press(&mut harness, Modifiers::COMMAND, Key::Num1);
    assert_eq!(harness.state().active_workspace(), Some("build"));
    press(&mut harness, Modifiers::COMMAND, Key::Num0);
    assert_eq!(harness.state().active_workspace(), Some("build"), "there is no Theatre yet");
    assert!(harness.state().registry().workspace("theatre").is_none());
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
    assert_shows(&harness, "No character is open");
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

#[test]
fn the_keys_still_work_after_a_click_has_focused_a_tab_or_button() {
    let mut harness = plain(vec2(1920.0, 1080.0));
    harness.get_by_label("Build").click();
    harness.run_steps(3);
    press(&mut harness, Modifiers::COMMAND, Key::Num2);
    assert_eq!(harness.state().active_workspace(), Some("probe"));
}

#[test]
fn focus_follows_tab_presses_workspace_switches_and_f6_which_shows_the_panel_it_picks() {
    let mut harness = plain(vec2(1920.0, 1080.0));
    harness.get_by_role_and_label(Role::Button, "History").click();
    harness.run_steps(3);
    assert_eq!(harness.state().focused_panel(), Some("history"));
    press(&mut harness, Modifiers::COMMAND, Key::Space);
    harness.run_steps(2);
    assert_shows(&harness, "Your changes will appear here");
    assert!(!shows(&harness, "Start with a character"), "only History fills the dock area");
    press(&mut harness, Modifiers::NONE, Key::Escape);
    // A workspace without the focused panel does not inherit it.
    press(&mut harness, Modifiers::COMMAND, Key::Num2);
    assert_eq!(harness.state().focused_panel(), None);
    press(&mut harness, Modifiers::COMMAND, Key::Space);
    harness.run_steps(2);
    assert_shows(&harness, "Start with a character");
    assert_shows(&harness, "Select a part to edit it");
    // F6 walks the reading order and brings a panel behind a tab to the front.
    press(&mut harness, Modifiers::COMMAND, Key::Num1);
    press(&mut harness, Modifiers::NONE, Key::F6);
    press(&mut harness, Modifiers::NONE, Key::F6);
    harness.run_steps(2);
    assert_eq!(harness.state().focused_panel(), Some("library"));
    assert_shows(&harness, "The library is empty");
    assert!(!shows(&harness, "No character is open"), "the Outliner tab went to the back");
}

#[test]
fn a_maximised_panel_has_no_tab_so_its_body_names_itself_by_its_title() {
    let mut harness = plain(vec2(1920.0, 1080.0));
    let region = |h: &Harness<'_, Shell>, label: &str| h.query_all_by_role_and_label(Role::Region, label).count();
    // Docked, every panel body is a region named by its title (these panels give no label of their own).
    assert_eq!(region(&harness, "Outliner"), 1);
    press(&mut harness, Modifiers::NONE, Key::F6);
    press(&mut harness, Modifiers::COMMAND, Key::Space);
    harness.run_steps(2);
    assert_eq!(region(&harness, "Outliner"), 1, "the maximised panel is named, with no tab beside it");
    assert!(harness.query_by_role_and_label(Role::Button, "Outliner").is_none(), "no tab is drawn");
    assert_eq!(region(&harness, "Viewport"), 0, "the others are not drawn");
}

#[test]
fn the_primary_panel_cannot_be_dragged_out_and_the_lock_stops_every_other_move_and_divider() {
    let mut harness = plain(vec2(1920.0, 1080.0));
    let before = harness.state().shown_layout();
    let viewport = harness.get_by_role_and_label(Role::Button, "Viewport").rect().center();
    drag(&mut harness, &[viewport, egui::pos2(1700.0, 300.0), egui::pos2(1700.0, 320.0)]);
    assert_eq!(harness.state().shown_layout(), before, "the viewport tab stays in the centre");
    // Control: another panel's tab does move when unlocked, and not when locked.
    let history = harness.get_by_role_and_label(Role::Button, "History").rect().center();
    press(&mut harness, Modifiers::COMMAND | Modifiers::SHIFT, Key::L);
    drag(&mut harness, &[history, egui::pos2(150.0, 600.0), egui::pos2(150.0, 620.0)]);
    drag(&mut harness, &[egui::pos2(1552.0, 500.0), egui::pos2(1500.0, 500.0)]);
    assert_eq!(harness.state().shown_layout(), before, "locked: nothing moves and no divider drags");
    press(&mut harness, Modifiers::COMMAND | Modifiers::SHIFT, Key::L);
    drag(&mut harness, &[history, egui::pos2(150.0, 600.0), egui::pos2(150.0, 620.0)]);
    assert_ne!(harness.state().shown_layout(), before, "unlocked: the History tab moved");
}

#[test]
fn a_divider_cannot_take_a_panel_below_its_registered_minimum() {
    let mut harness = plain(vec2(1920.0, 1080.0));
    let widths = |harness: &Harness<'_, Shell>| {
        let shares = shares(&harness.state().shown_layout().expect("built"));
        let total: f32 = shares.iter().sum();
        shares.iter().map(|s| s / total * 1920.0).collect::<Vec<_>>()
    };
    drag(&mut harness, &[egui::pos2(308.0, 500.0), egui::pos2(150.0, 500.0), egui::pos2(40.0, 500.0)]);
    harness.run_steps(3);
    assert!(widths(&harness)[0] >= 238.0, "the left dock keeps its 240: {:?}", widths(&harness));
    drag(&mut harness, &[egui::pos2(1552.0, 500.0), egui::pos2(1800.0, 500.0), egui::pos2(1900.0, 500.0)]);
    harness.run_steps(3);
    assert!(widths(&harness)[2] >= 286.0, "the inspector keeps its 288: {:?}", widths(&harness));
}

#[test]
fn folded_docks_are_sized_again_so_the_inspector_is_288_wide_at_1280() {
    let harness = plain(vec2(1280.0, 720.0));
    let shares = shares(&harness.state().shown_layout().expect("built"));
    assert_eq!(shares.len(), 2, "the rail's panels are gone");
    let right = shares[1] / (shares[0] + shares[1]) * (1264.0 - 48.0);
    assert!((right - 288.0).abs() < 1.0, "{right} {shares:?}");
}

#[test]
fn the_history_panel_keeps_the_newest_records_of_a_long_log() {
    for (total, kept, first) in [(0_u64, 0_usize, 0_u64), (3, 3, 1), (500, 500, 1), (1200, 500, 701)] {
        let spy = Spy { total, ..Spy::default() };
        let records = super::app::newest_records(&spy, 500).expect("readable");
        assert_eq!(records.len(), kept, "{total}");
        assert_eq!(records.first().map_or(0, |r| r.seq), first, "{total}");
        assert_eq!(records.last().map_or(0, |r| r.seq), total, "{total}: the last record is the newest");
    }
}

#[test]
fn a_runner_that_returns_short_pages_still_gives_the_newest_records() {
    let spy = Spy { total: 1200, page: 100, ..Spy::default() };
    let records = super::app::newest_records(&spy, 500).expect("readable");
    assert_eq!((records.first().map(|r| r.seq), records.last().map(|r| r.seq)), (Some(701), Some(1200)));
}

#[test]
fn a_compact_layout_survives_a_resize_even_when_its_save_has_not_landed() {
    // A store that cannot write or read: the edit can only come back from memory.
    let dir = tempfile::tempdir().expect("a temporary directory");
    let blocked = dir.path().join("layouts");
    std::fs::write(&blocked, "a file where the folder should be").expect("write");
    let mut harness = shell_at(vec2(1366.0, 768.0), Some(LayoutStore::new(blocked)), Arc::new(NullRunner));
    let default = harness.state().shown_layout();
    drag(&mut harness, &[egui::pos2(1058.0, 500.0), egui::pos2(1000.0, 500.0)]);
    let edited = harness.state().shown_layout();
    assert_ne!(edited, default, "the drag changed the compact layout");
    harness.set_size(vec2(1280.0, 720.0));
    harness.run_steps(3);
    harness.set_size(vec2(1366.0, 768.0));
    harness.run_steps(3);
    assert_eq!(harness.state().shown_layout(), edited, "back at the first size the edit is still there");
}

#[test]
fn reset_removes_every_compact_layout_of_the_workspace_and_the_ones_held_in_memory() {
    let (_keep, store) = store();
    let layout =
        Layout::new(Node::split(Dir::Horizontal, vec![Node::pane("viewport"), Node::pane("inspector")], &[3.0, 1.0]));
    for key in ["build", "build-compact-a", "build-compact-la", "build-compact-lar", "probe-compact-a"] {
        store.save(key, &layout).expect("saves");
    }
    let mut harness = shell_at(vec2(1366.0, 768.0), Some(store.clone()), Arc::new(NullRunner));
    drag(&mut harness, &[egui::pos2(1058.0, 500.0), egui::pos2(1000.0, 500.0)]);
    harness.state_mut().reset_layout();
    harness.run_steps(3);
    harness.state().flush();
    let left: Vec<_> = std::fs::read_dir(store.dir()).expect("dir").flatten().map(|e| e.file_name()).collect();
    assert_eq!(left, [std::ffi::OsString::from("probe-compact-a.toml")], "only the other workspace's file is left");
    let fresh = plain(vec2(1366.0, 768.0));
    assert_eq!(harness.state().shown_layout(), fresh.state().shown_layout(), "the edit held in memory is gone too");
    // Resizing to another fold and back does not bring the old arrangement back.
    harness.set_size(vec2(1280.0, 720.0));
    harness.run_steps(3);
    harness.set_size(vec2(1366.0, 768.0));
    harness.run_steps(3);
    assert_eq!(harness.state().shown_layout(), fresh.state().shown_layout());
}

#[test]
fn a_layout_reset_this_session_is_not_read_back_from_a_file_the_writer_has_not_deleted_yet() {
    let (_keep, store) = store();
    let layout =
        Layout::new(Node::split(Dir::Horizontal, vec![Node::pane("viewport"), Node::pane("inspector")], &[3.0, 1.0]));
    let mut harness = shell_at(vec2(1366.0, 768.0), Some(store.clone()), Arc::new(NullRunner));
    harness.state_mut().reset_layout();
    harness.state().flush();
    // The file comes back as a delete that had not run yet would have left it.
    store.save("build-compact-a", &layout).expect("saves");
    assert_eq!(harness.state().stored("build-compact-a", &layout), None, "a reset layout reads as absent");
    assert_eq!(harness.state().stored("probe", &layout), None, "nothing was saved for the other workspace");
}

#[test]
fn a_layout_reset_without_a_saver_is_still_not_read_back_from_its_file() {
    let (_keep, store) = store();
    let layout =
        Layout::new(Node::split(Dir::Horizontal, vec![Node::pane("viewport"), Node::pane("inspector")], &[3.0, 1.0]));
    store.save("build-compact-a", &layout).expect("saves");
    let mut harness = shell_at(vec2(1366.0, 768.0), Some(store), Arc::new(NullRunner));
    harness.state_mut().lose_saver();
    assert!(harness.state().stored("build-compact-a", &layout).is_some(), "the file is read before the reset");
    harness.state_mut().reset_layout();
    assert_eq!(harness.state().stored("build-compact-a", &layout), None, "a reset layout reads as absent");
}

#[test]
fn the_status_bar_names_what_is_selected() {
    use crate::panels::edit::EditFocus;
    use omuzaki_char_api::{CharId, PartId};
    let part = |id: &str| PartId::new(id).expect("a part id");
    let focus = EditFocus::new(CharId::new("kit").expect("a character id"));
    let mut harness = plain(vec2(1920.0, 1080.0));
    harness.state_mut().insert_view(focus.clone());
    harness.run_steps(2);
    assert_shows(&harness, "Nothing selected");
    focus.select(part("head"));
    harness.run_steps(2);
    assert_shows(&harness, "Selected: head");
    focus.set(vec![part("head"), part("hair")]);
    harness.run_steps(2);
    assert_shows(&harness, "2 parts selected");
}
