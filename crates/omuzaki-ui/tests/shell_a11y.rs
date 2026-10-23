//! Shell-wide accessibility (SE-02, #226): the gallery's keyboard and screen-reader checks, run over the real studio,
//! every workspace and every registered panel, with no character open and with the kit character open.
//!
//! - every node a person can operate has a name;
//! - Tab follows the reading order inside each part of the window, and visits the parts as the eye does: the top bar,
//!   then the docks left to right and top to bottom, then the status bar;
//! - the focus ring is drawn on every stop;
//! - F6 walks every panel of the layout and Shift+F6 walks them back;
//! - the outliner, the inspector and undo work from the keyboard alone (`shell.md` section 16).
//!
//! A planted panel with an unlabelled button fails, naming the panel.

use std::cell::Cell;
use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Key, Modifiers, Rect, Ui, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_char_api::{BuildServices, RegistryBuilder};
use omuzaki_ops_api::{Grants, Runner, Services};
use omuzaki_ui::ds::icons;
use omuzaki_ui::ds::testing::{TabStop, press_for, reading_order_violations, tab_through, unlabelled_nodes};
use omuzaki_ui::panels::agent::Agents;
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::studio;
use omuzaki_ui::wiring::{Document, core_on};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::register_panel;
use omuzaki_ui_api::registry::{Region, Registry};

thread_local! {
    /// Whether the planted panel draws its unlabelled button. Per thread, so the test that plants it does not leak it
    /// into the others running beside it.
    static PLANTED: Cell<bool> = const { Cell::new(false) };
}

/// A panel registered only in this test binary: a labelled button, and while planted an unlabelled one beside it.
struct Planted;

impl Panel for Planted {
    fn ui(&mut self, ui: &mut Ui, _ctx: &PanelCtx<'_>) {
        let _ = ui.add(omuzaki_ui::ds::widgets::Button::new("Planted action"));
        if PLANTED.with(Cell::get) {
            let _ = ui.add(egui::Button::new(""));
        }
    }
}

register_panel! { id: "test.planted", title: "Planted", icon: icons::EYE, region: Region::Right, min_size: [240.0, 160.0],
order: 990, build: |_| Box::new(Planted) }

/// The reference window (`shell.md` section 2).
const WINDOW: egui::Vec2 = vec2(1920.0, 1080.0);
/// The same width, tall enough that no panel scrolls: Tab scrolls a control into view, so the order of the stops is
/// read where nothing moves under them.
const TALL: egui::Vec2 = vec2(1920.0, 2600.0);
/// The top bar's and the status bar's heights (`shell.md` section 1), give or take.
const BAR: f32 = 48.0;
/// The height of a dock's tab strip: a panel's tabs and its "⋯" sit this far above its body.
const STRIP: f32 = 28.0;

struct Studio {
    shell: Shell,
    runner: Arc<dyn Runner>,
}

/// The studio as `main.rs` builds it at `size`, with the kit character open when `populated`, and no viewport GPU.
fn studio(populated: bool, size: egui::Vec2) -> Harness<'static, Studio> {
    omuzaki_ui::link();
    let core = core_on(Document::empty(), Services::default()).unwrap_or_else(|e| panic!("{e}"));
    let mut shell = Shell::new(Arc::clone(&core.runner), None);
    if populated {
        let character = studio::open_character(core.runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
        shell.insert_view(EditFocus::new(character));
    }
    shell.insert_view(Agents(Arc::clone(&core.sessions)));
    shell.insert_view(ViewportServices {
        gpu: None,
        builder: Arc::new(RegistryBuilder),
        build: BuildServices::default(),
    });
    let mut h = Harness::builder().with_size(size).with_pixels_per_point(1.0).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui: &mut Ui, s: &mut Studio| s.shell.ui(ui),
        Studio { shell, runner: Arc::clone(&core.runner) },
    );
    h.run_steps(4);
    h
}

/// Presses a key and runs a few frames. The Live workspace reads its status on a timer, so the frame never settles
/// and waiting for it to would never end.
fn key(h: &mut Harness<'_, Studio>, modifiers: Modifiers, key: Key) {
    press_for(h, modifiers, key, 3);
}

/// Shows workspace `index` (from 0) with its numbered key.
fn workspace(h: &mut Harness<'_, Studio>, index: usize) {
    let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
    key(h, Modifiers::COMMAND, keys[index]);
    h.run_steps(3);
}

/// A part of the window a stop can belong to.
#[derive(Clone, Debug, PartialEq)]
enum Part {
    TopBar,
    /// A docked panel, by its id, with its body's rect.
    Panel(String, Rect),
    StatusBar,
    /// A divider between docks: `egui_tiles` draws them after every panel they divide, so they are checked for a name and
    /// a ring but not for where they fall in the order.
    Divider,
}

impl Part {
    fn name(&self) -> &str {
        match self {
            Part::TopBar => "the top bar",
            Part::Panel(title, _) => title,
            Part::StatusBar => "the status bar",
            Part::Divider => "a divider",
        }
    }

    /// Where the part comes in reading order: the top bar, the docks by column then row, the status bar.
    fn rank(&self) -> (u8, f32, f32) {
        match self {
            Part::TopBar => (0, 0.0, 0.0),
            Part::Panel(_, rect) => (1, rect.left().round(), rect.top().round()),
            Part::StatusBar | Part::Divider => (2, 0.0, 0.0),
        }
    }
}

/// The docked panels drawn last frame, by id and body.
fn panels(h: &Harness<'_, Studio>) -> Vec<(String, Rect)> {
    h.state().shell.panel_rects()
}

/// The part of the window `rect` is in: a panel's tab strip and "⋯" belong to the panel below them.
fn part_of(rect: Rect, role: Role, panels: &[(String, Rect)], window: egui::Vec2) -> Part {
    if role == Role::Splitter {
        return Part::Divider;
    }
    let centre = rect.center();
    if centre.y < BAR {
        return Part::TopBar;
    }
    // The innermost region holding it: a panel can hold regions of its own.
    let holder = panels
        .iter()
        .filter(|(_, body)| Rect::from_min_max(body.min - vec2(0.0, STRIP + 1.0), body.max).contains(centre))
        .min_by(|a, b| a.1.area().total_cmp(&b.1.area()));
    match holder {
        Some((title, body)) => Part::Panel(title.clone(), *body),
        None if centre.y > window.y - BAR => Part::StatusBar,
        None => Part::TopBar,
    }
}

/// Every problem the checks find in the frame `h` shows in a `window`-sized harness, each naming the part of the window
/// it is in; `context` says which frame. The order of the stops is checked only where nothing scrolls (`order`).
fn audit(h: &mut Harness<'_, Studio>, window: egui::Vec2, order: bool, context: &str) -> Vec<String> {
    let mut out = Vec::new();
    let shown = panels(h);
    let part = |rect: Rect, role: Role| part_of(rect, role, &shown, window);
    for (role, rect) in unlabelled_nodes(h) {
        out.push(format!("{context}: {}: {role:?} at {rect:?} has no name", part(rect, role).name()));
    }
    let mut stops = tab_through(h, 200);
    // Tab goes round in a ring; it is read from the top bar's first stop, wherever the focus was.
    let parts: Vec<Part> = stops.iter().map(|s| part(s.rect, s.role)).collect();
    if let Some(start) = (0..stops.len())
        .find(|&i| parts[i] == Part::TopBar && parts[(i + stops.len() - 1) % stops.len()] != Part::TopBar)
    {
        stops.rotate_left(start);
    }
    if stops.len() < 4 {
        out.push(format!("{context}: Tab reaches only {stops:?}"));
    }
    let screen = Rect::from_min_size(egui::Pos2::ZERO, window);
    for stop in &stops {
        let at = part(stop.rect, stop.role);
        let what = format!("{context}: {}: {:?} '{}'", at.name(), stop.role, stop.label);
        if !stop.indicated {
            out.push(format!("{what} shows no focus ring"));
        }
        // Focus not obscured: the stop is on screen, and inside its panel once the panel has scrolled to it.
        let inside = match &at {
            Part::Panel(_, body) => Rect::from_min_max(body.min - vec2(0.0, STRIP + 1.0), body.max).expand(1.0),
            _ => screen,
        };
        if !inside.contains(stop.rect.center()) || !screen.contains(stop.rect.center()) {
            out.push(format!("{what} at {:?} is out of sight", stop.rect));
        }
    }
    if !order {
        return out;
    }
    // Reading order inside each part, and between the parts.
    let mut runs: Vec<(Part, Vec<TabStop>)> = Vec::new();
    for stop in stops {
        let at = part(stop.rect, stop.role);
        if at == Part::Divider {
            continue;
        }
        match runs.last_mut() {
            Some((last, run)) if *last == at => run.push(stop),
            _ => runs.push((at, vec![stop])),
        }
    }
    for (at, run) in &runs {
        for violation in reading_order_violations(run) {
            out.push(format!("{context}: {}: {violation}", at.name()));
        }
    }
    for pair in runs.windows(2) {
        let (a, b) = (&pair[0].0, &pair[1].0);
        if b.rank() < a.rank() {
            out.push(format!("{context}: Tab goes from {} back to {}", a.name(), b.name()));
        }
    }
    for (index, (at, _)) in runs.iter().enumerate() {
        if runs[..index].iter().any(|(earlier, _)| earlier == at) {
            let labels: Vec<&str> = runs[index].1.iter().map(|s| s.label.as_str()).collect();
            out.push(format!("{context}: Tab comes back to {} after leaving it, at {labels:?}", at.name()));
        }
    }
    out
}

/// The panels F6 visits from where the focus is, until it comes round again.
fn f6_walk(h: &mut Harness<'_, Studio>, modifiers: Modifiers) -> Vec<String> {
    let mut walk: Vec<String> = Vec::new();
    for _ in 0..20 {
        key(h, modifiers, Key::F6);
        let Some(panel) = h.state().shell.focused_panel().map(str::to_owned) else { break };
        if walk.first() == Some(&panel) {
            break;
        }
        walk.push(panel);
    }
    walk
}

/// Every problem in every workspace and with every registered panel brought to the front, empty and populated, in a
/// `window`-sized studio.
fn audit_studio(window: egui::Vec2, order: bool) -> Vec<String> {
    let registry = Registry::collect();
    let mut problems = Vec::new();
    for populated in [false, true] {
        let state = if populated { "with the character" } else { "empty" };
        let mut h = studio(populated, window);
        for (index, ws) in registry.workspaces().iter().enumerate() {
            workspace(&mut h, index);
            let context = format!("{} workspace, {state}, {window:?}", ws.id);
            problems.extend(audit(&mut h, window, order, &context));
            // F6 walks every panel of the layout, and Shift+F6 walks the same panels back.
            let forth = f6_walk(&mut h, Modifiers::NONE);
            let mut back = f6_walk(&mut h, Modifiers::SHIFT);
            back.reverse();
            let regions = panels(&h).len();
            if forth.len() < regions {
                problems.push(format!("{context}: F6 visits {forth:?} of {regions} docked panels"));
            }
            let (mut a, mut b) = (forth.clone(), back.clone());
            a.sort();
            b.sort();
            if a != b {
                problems.push(format!("{context}: F6 visits {forth:?} but Shift+F6 {back:?}"));
            }
        }
        workspace(&mut h, 0);
        for def in registry.panels() {
            h.state_mut().shell.show_panel(def.id);
            h.run_steps(4);
            problems.extend(audit(&mut h, window, order, &format!("{} shown, {state}, {window:?}", def.id)));
        }
    }
    problems
}

#[test]
fn every_workspace_and_panel_is_named_ringed_and_in_reading_order() {
    let problems = audit_studio(TALL, true);
    assert!(problems.is_empty(), "{} problems:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn at_the_reference_size_every_stop_is_named_ringed_and_scrolled_into_sight() {
    let problems = audit_studio(WINDOW, false);
    assert!(problems.is_empty(), "{} problems:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn a_planted_unlabelled_button_fails_naming_its_panel() {
    PLANTED.with(|planted| planted.set(true));
    let mut h = studio(false, WINDOW);
    h.state_mut().shell.show_panel("test.planted");
    h.run_steps(4);
    let problems = audit(&mut h, WINDOW, true, "planted");
    PLANTED.with(|planted| planted.set(false));
    let named: Vec<&String> = problems.iter().filter(|p| p.starts_with("planted: test.planted: Button at ")).collect();
    assert_eq!(named.len(), 1, "{problems:#?}");
    assert!(named[0].ends_with("has no name"), "{named:?}");
}

/// The keyboard focus, by role, name and value.
fn focused(h: &Harness<'_, Studio>) -> Option<(Role, String, Option<f64>)> {
    h.root().children_recursive().find(|n| n.accesskit_node().is_focused()).map(|n| {
        let ak = n.accesskit_node();
        (ak.role(), ak.label().unwrap_or_default(), ak.numeric_value())
    })
}

/// Presses Tab until the focus is on a node with `role`, and says what it is.
fn tab_to(h: &mut Harness<'_, Studio>, role: Role) -> (Role, String, Option<f64>) {
    for _ in 0..80 {
        if let Some(stop) = focused(h).filter(|(r, _, _)| *r == role) {
            return stop;
        }
        key(h, Modifiers::NONE, Key::Tab);
    }
    panic!("Tab never reaches a {role:?}");
}

#[test]
fn the_outliner_the_inspector_and_undo_work_from_the_keyboard_alone() {
    let mut h = studio(true, WINDOW);
    let records = |h: &Harness<'_, Studio>| h.state().runner.log(0, 1000, &Grants::person()).map_or(0, |log| log.len());
    // F6 reaches the outliner; Tab enters its tree, Right opens the character's first part and Down picks the next row.
    for _ in 0..8 {
        if h.state().shell.focused_panel() == Some("outliner") {
            break;
        }
        key(&mut h, Modifiers::NONE, Key::F6);
    }
    assert_eq!(h.state().shell.focused_panel(), Some("outliner"));
    let (_, first, _) = tab_to(&mut h, Role::TreeItem);
    key(&mut h, Modifiers::NONE, Key::ArrowRight);
    key(&mut h, Modifiers::NONE, Key::ArrowDown);
    let (role, picked, _) = focused(&h).expect("the tree keeps the keyboard");
    assert_eq!(role, Role::TreeItem);
    assert_ne!(picked, first, "Down moved to the next part");
    // Enter sends the keyboard to the inspector, which shows the part picked.
    key(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    let (role, name, _) = focused(&h).expect("the keyboard went somewhere");
    assert_eq!((role, name.as_str()), (Role::TextInput, "Search settings…"), "Enter goes to the inspector's search");
    // Tab reaches its first slider; an arrow changes the value, through an op.
    let (_, slider, before) = tab_to(&mut h, Role::Slider);
    let ops = records(&h);
    key(&mut h, Modifiers::NONE, Key::ArrowRight);
    h.run_steps(4);
    let (_, still, after) = focused(&h).expect("the slider keeps the keyboard");
    assert_eq!(still, slider);
    assert!(after > before, "{slider}: {before:?} to {after:?}");
    assert_eq!(records(&h), ops + 1, "the change is one op in the log");
    // Ctrl+Z takes it back.
    key(&mut h, Modifiers::COMMAND, Key::Z);
    h.run_steps(4);
    assert_eq!(focused(&h).and_then(|(_, _, value)| value), before, "{slider} is back where it was");
}

/// A tab group too narrow for every name shows its closed tabs as their icons, each still named in full, and never
/// the tab strip's scroll arrows, which have no focus ring and come before the tabs in Tab order.
#[test]
fn a_crowded_tab_strip_shows_icons_named_in_full_and_no_scroll_arrows() {
    let mut h = studio(false, vec2(1366.0, 768.0));
    for id in ["live", "live-channels", "history", "agent", "test.planted"] {
        h.state_mut().shell.show_panel(id);
        h.run_steps(4);
    }
    for arrow in ["⏴", "⏵"] {
        assert!(h.query_all_by_label(arrow).next().is_none(), "no scroll arrow {arrow}");
    }
    for name in ["Inspector", "History", "Agent", "Planted"] {
        assert!(
            h.query_all_by_role(Role::Button).any(|n| n.accesskit_node().label().as_deref() == Some(name)),
            "the {name} tab is named in full"
        );
    }
    if let Ok(out) = std::env::var("OMUZAKI_EVIDENCE_OUT") {
        let image = h.render().unwrap_or_else(|e| panic!("{e}"));
        let path = format!("{out}/dock-crowded-1366x768.png");
        image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
    }
}

#[test]
fn a_tab_group_too_narrow_for_its_icons_offers_the_rest_in_its_menu_and_never_scrolls() {
    let registry = Registry::collect();
    // The smallest window the studio is laid out for, every right panel shown in its one group.
    let mut h = studio(false, vec2(1366.0, 768.0));
    let right: Vec<&str> = registry.panels().iter().filter(|d| d.region == Region::Right).map(|d| d.id).collect();
    for id in &right {
        h.state_mut().shell.show_panel(id);
        h.run_steps(4);
    }
    // egui_tiles' own scroll arrows have no focus ring and sit out of reading order: the strip never shows them.
    assert!(h.query_by_label("⏵").is_none() && h.query_by_label("⏴").is_none(), "the tabs scroll");
    let last = right.last().copied().unwrap_or_else(|| panic!("a right panel"));
    let def = registry.panel(last).unwrap_or_else(|| panic!("{last}"));
    h.get_by_label(&format!("{} options", def.title)).click();
    h.run_steps(2);
    let shows: Vec<String> = h
        .query_all_by_role(Role::Button)
        .filter_map(|n| n.accesskit_node().label())
        .filter(|l| l.starts_with("Show "))
        .collect();
    let tabs: Vec<String> = h.query_all_by_role(Role::Tab).filter_map(|n| n.accesskit_node().label()).collect();
    assert!(!shows.is_empty(), "the menu offers the tabs that didn't fit: {tabs:?}");
    let first = shows[0].clone();
    h.get_by_label(&first).click();
    h.run_steps(4);
    let opened = registry
        .panels()
        .iter()
        .find(|d| first == format!("Show {}", d.title))
        .map_or_else(|| panic!("{first} names a panel"), |d| d.id);
    assert_eq!(h.state().shell.focused_panel(), Some(opened), "Show opened its panel");
}
