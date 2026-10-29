//! The studio's layout audit at the reference window sizes (`shell.md` section 2): every workspace and every registered
//! panel, with no character open and with the kit character open, at 1366 x 768 and 1920 x 1080, in the dark and the
//! light theme, with the design system's fonts. Nothing in the window may be
//!
//! - cut with an ellipsis ("Go L…", a warning that stops mid-sentence),
//! - cut by the left or right edge of its box (a node's port label running out of its node),
//! - drawn over other text, or
//! - past the window's left or right edge, text or control (below the fold of a scrolling panel is fine).
//!
//! The gallery's scale audit (`scale_audit.rs`) checks each component alone in a roomy card. That is where these
//! defects never show: a label is cut when a real panel at a real window size gives it less room than the card did.
//! This test draws the studio as a person sees it, so the first time a panel cuts a label at 1366 x 768 it fails,
//! naming the workspace, the size, the theme and the text.
//!
//! Known defects are listed in [`EXPECTED`], each with its issue; an entry that stops matching fails too, so it is
//! removed the moment its fix lands.

use std::cell::Cell;
use std::sync::Arc;

use egui::accesskit::{Action, Role};
use egui::{Key, Modifiers, Rect, Ui, vec2};
use egui_kittest::kittest::NodeT as _;
use egui_kittest::{Harness, Node};
use omuzaki_char_api::{BuildServices, RegistryBuilder};
use omuzaki_ops_api::ServicesBuilder;
use omuzaki_settings_api::service::Settings as SettingsService;
use omuzaki_ui::ds::audit::{
    Control, LayoutFinding, LayoutProblem, control_bounds, cut_sideways, elided_findings, painted,
};
use omuzaki_ui::ds::icons;
use omuzaki_ui::ds::testing::press_for;
use omuzaki_ui::ds::{Settings, ThemeKind, apply_settings};
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
    /// Whether the planted panel draws its cut button. Per thread, so the test that plants it does not leak it into the
    /// audit running beside it.
    static PLANTED: Cell<bool> = const { Cell::new(false) };
}

/// A panel registered only in this test binary: while planted, a button whose label is cut to a 60 pt box.
struct Planted;

impl Panel for Planted {
    fn ui(&mut self, ui: &mut Ui, _ctx: &PanelCtx<'_>) {
        if PLANTED.with(Cell::get) {
            let label = egui::Label::new("Go Live with the planted label").truncate();
            let _ = ui.add_sized(vec2(60.0, 24.0), label);
        }
    }
}

register_panel! { id: "test.planted-cut", title: "Planted", icon: icons::EYE, region: Region::Right,
min_size: [240.0, 160.0], order: 990, build: |_| Box::new(Planted) }

/// The two reference sizes: the smallest laptop screen the studio is designed for, and the reference monitor.
const SIZES: [egui::Vec2; 2] = [vec2(1366.0, 768.0), vec2(1920.0, 1080.0)];

/// Both everyday themes.
const THEMES: [ThemeKind; 2] = [ThemeKind::Dark, ThemeKind::Light];

/// One known defect: the problem, the text it concerns (its start is enough), and its issue.
struct Expected {
    problem: LayoutProblem,
    /// The start of the text, or of the control's name.
    starts: &'static str,
    /// The issue that fixes it.
    issue: u32,
}

/// The known defects. Each must still be found somewhere in the audit, or the test fails and asks for its removal.
const EXPECTED: &[Expected] = &[];

struct Studio {
    shell: Shell,
    theme: ThemeKind,
    _folder: tempfile::TempDir,
}

/// The studio as `main.rs` builds it at `size`, with the kit character open when `populated`, no viewport GPU, and the
/// design system's settings applied every frame (as the app's settings sync does).
fn studio(populated: bool, size: egui::Vec2, theme: ThemeKind) -> Harness<'static, Studio> {
    omuzaki_ui::link();
    // A settings store in a folder of its own, so the Settings window draws its rows rather than an error.
    let folder = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let paths = omuzaki_settings::Paths::new(folder.path().join("config"), folder.path().join("data"));
    let store: Arc<dyn SettingsService> = Arc::new(omuzaki_settings::Store::open(&paths));
    let services = ServicesBuilder::new().insert::<dyn SettingsService>(store).build();
    let core = core_on(Document::empty(), services).unwrap_or_else(|e| panic!("{e}"));
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
        |ui: &mut Ui, s: &mut Studio| {
            apply_settings(ui.ctx(), Settings { theme: s.theme, ..Settings::default() });
            s.shell.ui(ui);
        },
        Studio { shell, theme, _folder: folder },
    );
    // The fonts become active a pass after they are installed, and a trailing row measures itself once they are.
    h.run_steps(6);
    h
}

/// Shows workspace `index` (from 0) with its numbered key.
fn workspace(h: &mut Harness<'_, Studio>, index: usize) {
    let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
    press_for(h, Modifiers::COMMAND, keys[index], 3);
    h.run_steps(4);
}

/// Every problem in the frame `h` last drew.
fn audit(h: &Harness<'_, Studio>, window: egui::Vec2) -> Vec<LayoutFinding> {
    audit_with(h, window, true)
}

/// [`audit`], with the check that text stays inside its docked panel only when `docked` (no window floats over the
/// panels).
fn audit_with(h: &Harness<'_, Studio>, window: egui::Vec2, docked: bool) -> Vec<LayoutFinding> {
    let screen = Rect::from_min_size(egui::Pos2::ZERO, window);
    let mut found = elided_findings(h.output());
    found.extend(cut_sideways(h.output()));
    if docked {
        found.extend(out_of_panel(h));
    }
    // A control below the fold of a scrolling panel is out of the window by design; one past its left or right edge
    // cannot be scrolled to.
    let across = Rect::from_x_y_ranges(screen.x_range(), f32::NEG_INFINITY..=f32::INFINITY);
    found.extend(control_bounds(&controls(h), across));
    found
}

/// Text that starts inside a docked panel's body and runs past its left or right edge: a label wider than its panel,
/// which no clip rectangle cuts because it is drawn over the next panel or the gap between them.
fn out_of_panel(h: &Harness<'_, Studio>) -> Vec<LayoutFinding> {
    let bodies = h.state().shell.panel_rects();
    let mut out = Vec::new();
    for text in painted(h.output()) {
        let start = text.rect.left_center() + vec2(1.0, 0.0);
        let Some((_, body)) =
            bodies.iter().filter(|(_, body)| body.contains(start)).min_by(|a, b| a.1.area().total_cmp(&b.1.area()))
        else {
            continue;
        };
        if text.rect.right() > body.right() + 0.5 {
            out.push(LayoutFinding { problem: LayoutProblem::Clipped, what: text.text, rect: text.rect, with: None });
        }
    }
    out
}

/// Every node a person can operate, as `scale_audit.rs` lists them.
fn controls<S>(harness: &Harness<'_, S>) -> Vec<Control> {
    let mut out = Vec::new();
    for child in harness.root().children() {
        collect(child, None, &mut out);
    }
    out
}

fn collect(node: Node<'_>, parent: Option<usize>, out: &mut Vec<Control>) {
    let mut operable_parent = parent;
    let ak = node.accesskit_node();
    let interactive = matches!(
        ak.role(),
        Role::Button
            | Role::CheckBox
            | Role::Switch
            | Role::RadioButton
            | Role::Slider
            | Role::SpinButton
            | Role::TextInput
            | Role::ComboBox
            | Role::TreeItem
            | Role::Link
            | Role::Tab
    );
    if ak.role() != Role::ScrollBar
        && (interactive || ak.data().supports_action(Action::Focus) || ak.data().supports_action(Action::Click))
    {
        out.push(Control {
            name: ak.label().unwrap_or_default(),
            role: ak.role(),
            rect: node.rect(),
            controls: None,
            parent,
        });
        operable_parent = Some(out.len() - 1);
    }
    for child in node.children() {
        collect(child, operable_parent, out);
    }
}

/// Every finding in every workspace and with every panel brought to the front, as one line each, deduplicated: the
/// same cut label in the same place at both sizes is two lines, since the fix may differ.
fn audit_studio() -> Vec<(LayoutFinding, String)> {
    let registry = Registry::collect();
    let mut out: Vec<(LayoutFinding, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // With OMUZAKI_EVIDENCE_OUT set, every frame with a finding is also saved as a picture, for the issue or the pull
    // request that fixes it.
    let pictures = std::env::var("OMUZAKI_EVIDENCE_OUT").ok();
    let mut add = |h: &mut Harness<'_, Studio>, found: Vec<LayoutFinding>, context: String| {
        if let (Some(out), false) = (&pictures, found.is_empty()) {
            let name: String = context.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
            let image = h.render().unwrap_or_else(|e| panic!("{e}"));
            let path = format!("{out}/{name}.png");
            image.save(&path).unwrap_or_else(|e| panic!("writing {path}: {e}"));
        }
        for finding in found {
            let key = (context.clone(), format!("{:?}", finding.problem), finding.what.clone());
            if seen.insert(key) {
                out.push((finding, context.clone()));
            }
        }
    };
    for size in SIZES {
        for theme in THEMES {
            for populated in [false, true] {
                let state = if populated { "with the character" } else { "empty" };
                let at = format!("{}x{} {theme:?}, {state}", size.x, size.y);
                let mut h = studio(populated, size, theme);
                for (index, ws) in registry.workspaces().iter().enumerate() {
                    workspace(&mut h, index);
                    let found = audit(&h, size);
                    add(&mut h, found, format!("{} workspace, {at}", ws.id));
                }
                // Each panel joins the default arrangement alone, where a person who opens it from the View menu meets
                // it: panels shown one after another pile up into ever narrower columns (#2423).
                workspace(&mut h, 0);
                for def in registry.panels() {
                    h.state_mut().shell.reset_layout();
                    h.run_steps(2);
                    h.state_mut().shell.show_panel(def.id);
                    h.run_steps(4);
                    let found = audit(&h, size);
                    add(&mut h, found, format!("{} shown, {at}", def.id));
                }
                // Half a second: the window has faded in, so a picture of it shows it as a person sees it.
                h.state_mut().shell.toggle_settings();
                h.run_steps(30);
                let found = audit_with(&h, size, false);
                add(&mut h, found, format!("the Settings window, {at}"));
            }
        }
    }
    out
}

#[test]
fn at_both_reference_sizes_and_both_themes_nothing_in_the_studio_is_cut_or_out_of_the_window() {
    let found = audit_studio();
    let mut problems = Vec::new();
    let mut matched = vec![false; EXPECTED.len()];
    for (finding, context) in &found {
        let known = EXPECTED.iter().position(|e| e.problem == finding.problem && finding.what.starts_with(e.starts));
        match known {
            Some(index) => matched[index] = true,
            None => problems.push(format!("{context}: {finding}")),
        }
    }
    for (index, expected) in EXPECTED.iter().enumerate() {
        if !matched[index] {
            problems.push(format!(
                "{:?} {:?} (#{}) is no longer found: remove its entry from EXPECTED",
                expected.problem, expected.starts, expected.issue
            ));
        }
    }
    assert!(problems.is_empty(), "{} layout problems:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn a_planted_cut_label_fails_naming_its_panel_size_and_theme() {
    PLANTED.with(|planted| planted.set(true));
    let found = audit_studio();
    PLANTED.with(|planted| planted.set(false));
    let planted: Vec<&String> = found
        .iter()
        .filter(|(f, _)| f.problem == LayoutProblem::Elided && f.what.starts_with("Go Live with"))
        .map(|(_, context)| context)
        .collect();
    for size in ["1366x768", "1920x1080"] {
        for theme in ["Dark", "Light"] {
            assert!(
                planted
                    .iter()
                    .any(|c| c.starts_with("test.planted-cut shown") && c.contains(size) && c.contains(theme)),
                "the planted cut label must be found at {size} {theme}: {planted:?}"
            );
        }
    }
}
