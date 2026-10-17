//! The shell reads its workspaces and panels from `omuzaki_ui_api::registry` (#2042, #64 part 2): a panel and a workspace
//! registered by a crate other than `omuzaki-ui` appear in the tab bar, the View menu and their workspace, render, and
//! need no edit to a shared file. Two crates register here: this test (the sheet workspace, its canvas, notes and blank
//! panels) and `omuzaki-ui-mock` (the probe panel and workspace).

use std::borrow::Cow;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::accesskit::Role;
use egui::{Ui, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Event, Grants, OpError, OpInfo, OpRecord, Outcome, Request, Runner, TxId};
use omuzaki_ui::shell::Shell;
use omuzaki_ui::shell::persist::LayoutStore;
use omuzaki_ui::shell::stub::NullRunner;
use omuzaki_ui_api::layout::{Dir, Node};
use omuzaki_ui_api::panel::{EmptyState, Panel, PanelCtx};
use omuzaki_ui_api::registry::{Group, PanelKind, Region};
use omuzaki_ui_api::{register_panel, register_workspace};

/// The sheet canvas: this crate's primary panel. It can never work here, so the shell shows its registered empty state.
struct Canvas;

impl Panel for Canvas {
    fn ui(&mut self, _ui: &mut Ui, _ctx: &PanelCtx<'_>) {}

    fn available(&self, _ctx: &PanelCtx<'_>) -> bool {
        false
    }
}

/// A panel with a body of its own and a screen-reader label that says more than its title.
struct Notes;

impl Panel for Notes {
    fn ui(&mut self, ui: &mut Ui, _ctx: &PanelCtx<'_>) {
        ui.label("Notes body");
    }

    fn accessibility_label(&self) -> Option<Cow<'static, str>> {
        Some(Cow::Borrowed("Notes about the sheet"))
    }
}

/// A panel that cannot work and registered no empty state of its own.
struct Blank;

impl Panel for Blank {
    fn ui(&mut self, _ui: &mut Ui, _ctx: &PanelCtx<'_>) {}

    fn available(&self, _ctx: &PanelCtx<'_>) -> bool {
        false
    }
}

/// A panel that cannot work and registered words of its own, with a screen-reader label that says more than its title.
struct Offline;

impl Panel for Offline {
    fn ui(&mut self, _ui: &mut Ui, _ctx: &PanelCtx<'_>) {}

    fn available(&self, _ctx: &PanelCtx<'_>) -> bool {
        false
    }

    fn accessibility_label(&self) -> Option<Cow<'static, str>> {
        Some(Cow::Borrowed("Offline sheet tools"))
    }
}

register_panel! {
    id: "test.canvas", title: "Canvas", icon: "", region: Region::Center, min_size: [480.0, 320.0], order: 10,
    kind: PanelKind::Primary, empty: EmptyState::new("No sheet is loaded", "Import a sheet"),
    build: |_| Box::new(Canvas),
}
register_panel! {
    id: "test.notes", title: "Notes", icon: "", region: Region::Right, min_size: [288.0, 160.0], order: 900,
    build: |_| Box::new(Notes),
}
register_panel! {
    id: "test.blank", title: "Blank", icon: "", region: Region::Right, min_size: [288.0, 160.0], order: 901,
    build: |_| Box::new(Blank),
}
register_panel! {
    id: "test.offline", title: "Offline", icon: "", region: Region::Right, min_size: [288.0, 160.0], order: 902,
    empty: EmptyState::new("The sheet tools are offline", "Reconnect them"),
    build: |_| Box::new(Offline),
}
register_workspace! {
    id: "test.sheet", title: "Sheet", icon: "", order: 900, group: Group::Make,
    default_layout: || Node::split(
        Dir::Horizontal,
        vec![Node::tabs(&["test.notes", "test.blank", "test.offline"]), Node::pane("test.canvas")],
        &[1.0, 3.0],
    ),
}

fn shell(runner: Arc<dyn Runner>, store: Option<LayoutStore>) -> Harness<'static, Shell> {
    omuzaki_ui_mock::link();
    let mut harness = Harness::builder()
        .with_size(vec2(1920.0, 1080.0))
        .build_ui_state(|ui, shell: &mut Shell| shell.ui(ui), Shell::new(runner, store));
    // Fonts become active a pass after they are installed, and the tree is built on the first.
    harness.run_steps(4);
    harness
}

fn plain() -> Harness<'static, Shell> {
    shell(Arc::new(NullRunner), None)
}

#[track_caller]
fn assert_shows(harness: &Harness<'_, Shell>, label: &str) {
    assert!(harness.query_all_by_label(label).next().is_some(), "{label:?} is not on screen");
}

/// Opens a workspace the way a person does, by clicking its tab.
fn open_workspace(harness: &mut Harness<'_, Shell>, id: &str) {
    let Some(title) = harness.state().registry().workspace(id).map(|w| w.title) else {
        panic!("{id:?} is not a registered workspace");
    };
    harness.get_by_role_and_label(Role::Tab, title).click();
    harness.run_steps(3);
    assert_eq!(harness.state().active_workspace(), Some(id), "the {title} tab opens {id}");
}

/// Whether a region named `label` is on screen.
fn has_region(harness: &Harness<'_, Shell>, label: &str) -> bool {
    harness.query_all_by_role_and_label(Role::Region, label).next().is_some()
}

/// Where each of `wanted` stands in `listed`, which must hold them all and in this order. Other registrations may sit
/// between and around them: a crate that registers more never breaks this.
#[track_caller]
fn assert_in_order(listed: &[&str], wanted: &[&str]) {
    let at = |id: &str| listed.iter().position(|l| *l == id).unwrap_or_else(|| panic!("{id:?} is not in {listed:?}"));
    let places: Vec<usize> = wanted.iter().map(|id| at(id)).collect();
    assert!(places.windows(2).all(|w| w[0] < w[1]), "expected {wanted:?} in this order, got {places:?} in {listed:?}");
}

/// Whether `keys` are strictly increasing: sorted, and no two registrations tied on all of it.
#[track_caller]
fn assert_sorted<K: Ord + std::fmt::Debug>(keys: &[K]) {
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "not sorted: {keys:?}");
}

#[test]
fn registrations_from_other_crates_join_the_shells_lists_by_group_then_order_then_id() {
    let harness = plain();
    let registry = harness.state().registry();
    // Whatever else is registered, the whole list is in the documented order: region (group), then order, then id.
    assert_sorted(&registry.panels().iter().map(|p| (p.region, p.order, p.id)).collect::<Vec<_>>());
    assert_sorted(&registry.workspaces().iter().map(|w| (w.group, w.order, w.id)).collect::<Vec<_>>());
    // These crates' own registrations stand where that order puts them: the centre before the right dock, and within
    // the right dock by order, then id (`mock.probe` and `test.notes` share order 900).
    let panels: Vec<&str> = registry.panels().iter().map(|p| p.id).collect();
    assert_in_order(&panels, &["test.canvas", "mock.probe", "test.notes", "test.blank", "test.offline"]);
    // Make before Watch whatever the order key says (both have order 900).
    let workspaces: Vec<&str> = registry.workspaces().iter().map(|w| w.id).collect();
    assert_in_order(&workspaces, &["test.sheet", "mock.probe"]);
    assert!(registry.duplicates().is_empty(), "{:?}", registry.duplicates());
}

#[test]
fn the_order_is_the_same_on_every_start() {
    let first = plain();
    let second = plain();
    let describe = |h: &Harness<'_, Shell>| {
        let registry = h.state().registry();
        let panels: Vec<_> = registry.panels().iter().map(|p| p.id).collect();
        let workspaces: Vec<_> = registry.workspaces().iter().map(|w| w.id).collect();
        (panels, workspaces)
    };
    assert_eq!(describe(&first), describe(&second));
}

#[test]
fn every_registered_workspace_is_a_tab_and_each_renders_its_own_panels() {
    let mut harness = plain();
    let workspaces: Vec<&str> = harness.state().registry().workspaces().iter().map(|w| w.id).collect();
    for id in &workspaces {
        let title = harness.state().registry().workspace(id).map(|w| w.title).expect("registered");
        assert!(harness.query_by_role_and_label(Role::Tab, title).is_some(), "{id} has no tab");
    }
    // The sheet workspace of this crate: its canvas says why it is empty, its notes draw.
    open_workspace(&mut harness, "test.sheet");
    for label in ["No sheet is loaded", "Import a sheet", "Notes body"] {
        assert_shows(&harness, label);
    }
    // The mock's probe: a panel from a second crate, drawing every frame.
    open_workspace(&mut harness, "mock.probe");
    assert!(harness.query_by_label_contains("frames, 0 events").is_some(), "the probe panel draws its body");
}

#[test]
fn a_panel_says_more_to_a_screen_reader_than_its_title_and_an_unavailable_one_without_words_gets_a_notice() {
    let mut harness = plain();
    open_workspace(&mut harness, "test.sheet");
    assert_shows(&harness, "Notes about the sheet");
    harness.get_by_label("Blank").click();
    harness.run_steps(3);
    assert_shows(&harness, "Blank can't work right now");
}

#[test]
fn every_panel_body_is_a_region_named_by_its_label_or_else_its_title_even_when_unavailable() {
    let mut harness = plain();
    open_workspace(&mut harness, "test.sheet");
    // An explicit label names the region, and the tab keeps the title.
    assert!(has_region(&harness, "Notes about the sheet"));
    assert!(!has_region(&harness, "Notes"), "the explicit label replaces the title");
    // An unavailable panel with no label is named by its title (the primary one is always drawn).
    assert!(has_region(&harness, "Canvas"));
    harness.get_by_label("Blank").click();
    harness.run_steps(3);
    assert!(has_region(&harness, "Blank"));
    // An unavailable panel with a label of its own still answers to it, over its empty state.
    harness.get_by_label("Offline").click();
    harness.run_steps(3);
    assert_shows(&harness, "The sheet tools are offline");
    assert!(has_region(&harness, "Offline sheet tools"));
    assert!(!has_region(&harness, "Offline"));
    // An available panel without a label: the probe renames itself every frame, and its region follows.
    open_workspace(&mut harness, "mock.probe");
    let named = |h: &Harness<'_, Shell>| {
        h.query_all_by_role(Role::Region).any(|n| n.accesskit_node().label().is_some_and(|l| l.starts_with("Probe (")))
    };
    assert!(named(&harness), "the probe's region is named by its own title");
}

#[test]
fn the_view_menu_lists_every_registered_panel_and_shows_one_the_workspace_lacks() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let store = LayoutStore::new(dir.path().join("layouts"));
    let mut harness = shell(Arc::new(NullRunner), Some(store.clone()));
    let titles: Vec<String> = harness.state().registry().panels().iter().map(|p| p.title.to_owned()).collect();
    // A panel on screen is also a tab (a button), so the menu's items are the buttons that appear when it opens.
    let buttons = |h: &Harness<'_, Shell>, title: &str| h.query_all_by_role_and_label(Role::Button, title).count();
    let before: Vec<usize> = titles.iter().map(|t| buttons(&harness, t)).collect();
    harness.get_by_label("Menu").click();
    harness.run_steps(3);
    assert_shows(&harness, "Show panel");
    for (title, before) in titles.iter().zip(before) {
        assert_eq!(buttons(&harness, title), before + 1, "{title} is not in the View menu");
    }

    // Build lacks Notes; the menu adds it beside the other right-hand panels, and the arrangement is saved.
    assert!(harness.query_by_label("Notes body").is_none());
    let item = harness.query_all_by_role_and_label(Role::Button, "Notes").last().expect("a Notes item");
    item.click();
    harness.run_steps(4);
    assert_shows(&harness, "Notes body");
    assert_eq!(harness.state().focused_panel(), Some("test.notes"));
    harness.state().flush();
    let saved = store.load("build").expect("readable").expect("saved after the panel was shown");
    assert!(saved.contains("test.notes") && saved.contains("viewport"), "{:?}", saved.panels());
}

/// A runner whose event stream the test feeds.
struct Feed {
    events: Mutex<Option<Receiver<Event>>>,
}

impl Feed {
    fn new() -> (Arc<Self>, Sender<Event>) {
        let (tx, rx) = channel();
        (Arc::new(Self { events: Mutex::new(Some(rx)) }), tx)
    }
}

impl Runner for Feed {
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
    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        NullRunner.undo(actor, grants)
    }
    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        NullRunner.redo(actor, grants)
    }
    fn subscribe(&self, _grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.events.lock().ok().and_then(|mut slot| slot.take()).ok_or_else(|| OpError::unavailable("events"))
    }
    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        NullRunner.get(path, grants)
    }
    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        NullRunner.log(after_seq, limit, grants)
    }
}

/// How long a test waits for the event thread before it calls the shell deaf. A pass takes microseconds, so this is
/// only ever spent when the shell is broken.
const WAKE: Duration = Duration::from_secs(60);

/// Hands the test a message each time egui asks its integration for a frame, as eframe's window wake-up does.
fn wakes(harness: &Harness<'_, Shell>) -> Receiver<()> {
    let (tx, rx) = channel();
    harness.ctx.set_request_repaint_callback(move |_| {
        // The test may have stopped listening; the shell does not care.
        tx.send(()).ok();
    });
    rx
}

/// Settles the harness, then forgets the wake-ups its own passes asked for: a request that repaints leaves a second one
/// for the frame after, so the shell must be idle before the next event can be told apart.
fn settle(harness: &mut Harness<'_, Shell>, wakes: &Receiver<()>) {
    harness.run();
    while wakes.try_recv().is_ok() {}
}

fn committed(n: u64) -> Event {
    Event::tx_committed(TxId(n), "Edit", Actor::System)
}

#[test]
fn panels_see_the_runners_events_once_each() {
    let (feed, sender) = Feed::new();
    let mut harness = shell(feed, None);
    open_workspace(&mut harness, "mock.probe");
    let wakes = wakes(&harness);
    settle(&mut harness, &wakes);
    assert!(harness.query_by_label_contains("frames, 0 events").is_some());
    for n in 1..=3 {
        sender.send(committed(n)).expect("the shell holds the receiver");
        // The event thread has put the event in the inbox by the time it asks for the frame this waits for.
        wakes.recv_timeout(WAKE).expect("the event woke the shell");
        harness.run();
        let counted = format!("frames, {n} events");
        assert!(harness.query_by_label_contains(&counted).is_some(), "the probe counted {n} events once");
        while wakes.try_recv().is_ok() {}
    }
    harness.run_steps(3);
    assert!(harness.query_by_label_contains("frames, 3 events").is_some(), "later frames bring no new events");
}

#[test]
fn an_event_wakes_an_idle_shell_without_a_frame_being_stepped() {
    let (feed, sender) = Feed::new();
    let mut harness = shell(feed, None);
    let wakes = wakes(&harness);
    settle(&mut harness, &wakes);
    // Idle: no frame is asked for until something happens.
    assert!(wakes.try_recv().is_err(), "an idle shell asks for nothing");
    sender.send(committed(1)).expect("the shell holds the receiver");
    wakes.recv_timeout(WAKE).expect("an event the runner sends wakes the idle shell");
}
