//! UI and op parity (QG-11, #216): every change a panel makes to the document arrives as an op the studio's runner
//! ran, and is in the person's undo history.
//!
//! The studio runs over the real executor through a [`Recorder`]. Every registered panel is brought to the front with
//! the kit character open, and every control in it is operated the way a person would: buttons, toggles, tree rows
//! and tabs clicked, sliders dragged and stepped with the arrows, fields typed in. After each, the recorder judges the
//! change (`omuzaki_ui::testing::op_audit`). A planted panel with a back door (a runner of its own) fails, naming the
//! panel and the control.

use std::cell::RefCell;
use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, Rect, Ui, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_char_api::{BuildServices, RegistryBuilder};
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner, Services};
use omuzaki_ui::ds::icons;
use omuzaki_ui::ds::testing::{drag_for, press_for};
use omuzaki_ui::ds::widgets::Button;
use omuzaki_ui::panels::agent::Agents;
use omuzaki_ui::panels::edit::EditFocus;
use omuzaki_ui::panels::viewport::ViewportServices;
use omuzaki_ui::shell::Shell;
use omuzaki_ui::studio;
use omuzaki_ui::testing::op_audit::Recorder;
use omuzaki_ui::wiring::{Document, core_on};
use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::register_panel;
use omuzaki_ui_api::registry::{Region, Registry};
use serde_json::json;

thread_local! {
    /// The runner the planted panel keeps for itself, when the test plants it: the executor without the studio's
    /// runner in front of it. Per thread, so the other tests running beside it never see it.
    static BACK_DOOR: RefCell<Option<Arc<dyn Runner>>> = const { RefCell::new(None) };
}

/// A panel registered only in this test binary. Its one button renames the character through its own runner while
/// planted, and does nothing otherwise.
struct Planted;

impl Panel for Planted {
    fn ui(&mut self, ui: &mut Ui, _ctx: &PanelCtx<'_>) {
        if ui.add(Button::new("Rename quietly")).clicked() {
            BACK_DOOR.with(|door| {
                if let Some(runner) = door.borrow().as_ref() {
                    let request = Request {
                        op: "character.rename".into(),
                        args: json!({"character": studio::SAMPLE_ID, "name": "Renamed"}),
                        mode: Mode::Apply,
                        actor: Actor::Person { name: "you".into() },
                        grants: Grants::person(),
                        cause: None,
                        tx: None,
                        base: None,
                    };
                    let _ = runner.run(request);
                }
            });
        }
    }
}

register_panel! { id: "test.back_door", title: "Back door", icon: icons::EYE, region: Region::Right,
min_size: [240.0, 160.0], order: 991, build: |_| Box::new(Planted) }

const WINDOW: egui::Vec2 = vec2(1920.0, 1080.0);

struct Studio {
    shell: Shell,
    recorder: Arc<Recorder>,
}

/// The studio as `main.rs` builds it, with the kit character open, its runner the recorder over the executor. With
/// `back_door`, the planted panel gets the executor itself.
fn studio(back_door: bool) -> Harness<'static, Studio> {
    omuzaki_ui::link();
    let core = core_on(Document::empty(), Services::default()).unwrap_or_else(|e| panic!("{e}"));
    let character = studio::open_character(core.runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
    if back_door {
        BACK_DOOR.with(|door| *door.borrow_mut() = Some(Arc::clone(&core.runner)));
    }
    let recorder = Recorder::new(Arc::clone(&core.runner)).unwrap_or_else(|e| panic!("{e}"));
    let mut shell = Shell::new(Arc::clone(&recorder) as Arc<dyn Runner>, None);
    shell.insert_view(EditFocus::new(character));
    shell.insert_view(Agents(Arc::clone(&core.sessions)));
    shell.insert_view(ViewportServices {
        gpu: None,
        builder: Arc::new(RegistryBuilder),
        build: BuildServices::default(),
    });
    let mut h = Harness::builder()
        .with_size(WINDOW)
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui: &mut Ui, s: &mut Studio| s.shell.ui(ui), Studio { shell, recorder });
    h.run_steps(4);
    h.state().recorder.settle();
    h
}

/// A control in a panel, by what it is and its name.
#[derive(Clone, Debug, PartialEq)]
struct Control {
    role: Role,
    label: String,
}

/// The controls a person can operate inside `body`, in tree order.
fn controls(h: &Harness<'_, Studio>, body: Rect) -> Vec<Control> {
    let mut out: Vec<Control> = Vec::new();
    for node in h.root().children_recursive() {
        let ak = node.accesskit_node();
        let operable = matches!(
            ak.role(),
            Role::Button
                | Role::CheckBox
                | Role::Switch
                | Role::RadioButton
                | Role::Slider
                | Role::SpinButton
                | Role::TextInput
                | Role::ColorWell
                | Role::ComboBox
                | Role::TreeItem
                | Role::Tab
                | Role::Link
                | Role::DisclosureTriangle
        );
        let control = Control { role: ak.role(), label: ak.label().unwrap_or_default() };
        if operable && body.contains(node.rect().center()) && !out.contains(&control) {
            out.push(control);
        }
    }
    out
}

/// Operates `control` once, as a person would, and closes whatever it opened.
fn operate(h: &mut Harness<'_, Studio>, control: &Control) -> bool {
    let Some(node) =
        h.query_all_by_role(control.role).find(|n| n.accesskit_node().label().unwrap_or_default() == control.label)
    else {
        return false;
    };
    let rect = node.rect();
    match control.role {
        Role::Slider | Role::SpinButton => {
            node.focus();
            h.run_steps(2);
            press_for(h, Modifiers::NONE, Key::ArrowRight, 4);
            drag_for(h, &[rect.center(), rect.center() + vec2(12.0, 0.0), rect.center() + vec2(24.0, 0.0)], 2);
        }
        Role::TextInput => {
            // Typed and committed, then emptied again, so a search field does not hide the rest of its panel.
            node.focus();
            h.run_steps(2);
            h.event(Event::Text("a".to_owned()));
            h.run_steps(2);
            press_for(h, Modifiers::NONE, Key::Enter, 4);
            let refocused = h
                .query_all_by_role(Role::TextInput)
                .find(|n| n.accesskit_node().label().unwrap_or_default() == control.label)
                .map(|field| field.focus())
                .is_some();
            if refocused {
                h.run_steps(2);
                press_for(h, Modifiers::COMMAND, Key::A, 2);
                press_for(h, Modifiers::NONE, Key::Backspace, 2);
                press_for(h, Modifiers::NONE, Key::Enter, 4);
            }
        }
        Role::DisclosureTriangle => {
            // Folded and unfolded again, so what it holds is there for the controls after it.
            node.click();
            h.run_steps(4);
            if let Some(again) = h
                .query_all_by_role(control.role)
                .find(|n| n.accesskit_node().label().unwrap_or_default() == control.label)
            {
                again.click();
            }
        }
        _ => node.click(),
    }
    h.run_steps(8);
    for _ in 0..2 {
        press_for(h, Modifiers::NONE, Key::Escape, 3);
    }
    true
}

/// What operating every control of every registered panel (or only the panel `only`) found.
struct Audit {
    /// How many controls were operated.
    operated: usize,
    /// How many of those changed the document.
    changed: usize,
    problems: Vec<String>,
}

fn audit(h: &mut Harness<'_, Studio>, only: Option<&str>) -> Audit {
    let registry = Registry::collect();
    let mut problems = Vec::new();
    let (mut operated, mut changed) = (0, 0);
    for def in registry.panels().iter().filter(|d| only.is_none_or(|id| d.id == id)) {
        h.state_mut().shell.show_panel(def.id);
        h.run_steps(4);
        let Some((_, body)) = h.state().shell.panel_rects().into_iter().find(|(id, _)| id == def.id) else {
            problems.push(format!("{}: not on screen after it was shown", def.id));
            continue;
        };
        for control in controls(h, body).into_iter().take(60) {
            // A control may have closed the panel or moved its tab to the back: bring it to the front again.
            h.state_mut().shell.show_panel(def.id);
            h.run_steps(2);
            let before = h.state().recorder.snapshot();
            // A control an earlier one took away (a history row relabelled, a section of another part) is skipped.
            if !operate(h, &control) {
                continue;
            }
            operated += 1;
            changed += usize::from(h.state().recorder.snapshot() != before);
            let what = format!("{}: {:?} '{}'", def.id, control.role, control.label);
            problems.extend(h.state().recorder.check(&before, &what));
        }
    }
    Audit { operated, changed, problems }
}

#[test]
fn every_change_a_panel_makes_is_an_op_the_studio_ran_and_undoes() {
    let mut h = studio(false);
    let Audit { operated, changed, problems } = audit(&mut h, None);
    eprintln!("{operated} controls operated, {changed} of them changed the document");
    assert!(operated > 60, "the panels offer at least that many controls: {operated}");
    assert!(changed > 15, "the inspector's sliders and choices alone change it more often: {changed}");
    assert!(problems.is_empty(), "{} problems in {operated} interactions:\n{}", problems.len(), problems.join("\n"));
}

#[test]
fn a_planted_panel_with_a_back_door_fails_naming_the_panel_and_the_control() {
    let mut h = studio(true);
    let problems = audit(&mut h, Some("test.back_door")).problems;
    BACK_DOOR.with(|door| *door.borrow_mut() = None);
    assert_eq!(
        problems,
        vec![
            "test.back_door: Button 'Rename quietly': character.rename changed the document outside the studio's runner"
        ]
    );
}
