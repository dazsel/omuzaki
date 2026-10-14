//! Dialogs: the focus trap, Esc, the focus that comes back, and the two-step confirm of a dangerous primary.

use std::sync::Arc;

use egui::{Event, Pos2};
use egui_kittest::Harness;
use omuzaki_core::time::{Clock, FakeClock, Seconds};

use super::*;
use crate::ds::clock::set_clock;
use crate::ds::confirm::ARM_DELAY;
use crate::ds::testing::{press_for, unlabelled_interactive};

struct Rig {
    open: bool,
    action: Option<DialogAction>,
    busy: bool,
    dangerous: bool,
    name: String,
    part: usize,
    opener: Option<egui::Id>,
}

fn rig(dangerous: bool) -> (Harness<'static, Rig>, Arc<FakeClock>) {
    let rig = Rig { open: false, action: None, busy: false, dangerous, name: String::new(), part: 0, opener: None };
    let mut h = harness_state(Settings::default(), vec2(640.0, 420.0), rig, |ui, r| {
        ui.add(Button::new("Behind"));
        let opener = ui.add(Button::new("Open"));
        if opener.clicked() {
            r.open = true;
            r.opener = Some(opener.id);
        }
        if r.open {
            let mut dialog = Dialog::new("d", "Delete 3 parts?", "Delete 3 parts").busy(r.busy);
            if let Some(opener) = r.opener {
                dialog = dialog.opened_from(opener);
            }
            if r.dangerous {
                dialog = dialog.dangerous("Confirm: delete 3 parts");
            } else {
                dialog =
                    dialog.primary_disabled_if(r.name.is_empty(), "Enter a name first").first_focus(FirstFocus::Body);
            }
            let (name, part) = (&mut r.name, &mut r.part);
            let action = dialog.show(ui.ctx(), |ui| {
                ui.label("Hair front, hair back and bangs go.");
                if !r.dangerous {
                    ui.add(TextField::new("Version name", name));
                    ui.add(Dropdown::new("Part", part, &[Choice::new(0, "Hair"), Choice::new(1, "Face")]));
                }
            });
            if action.is_some() {
                r.open = false;
                r.action = action;
            }
        }
    });
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    set_clock(&h.ctx, Arc::clone(&clock) as Arc<dyn Clock>);
    h.run();
    (h, clock)
}

fn open(h: &mut Harness<'_, Rig>) {
    h.get_by_role_and_label(Role::Button, "Open").focus();
    h.run();
    press(h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
}

fn focused<S>(h: &Harness<'_, S>) -> String {
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().is_focused())
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .unwrap_or_default()
}

fn click_at(h: &mut Harness<'_, Rig>, at: Pos2) {
    h.hover_at(at);
    h.run();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.run();
    }
}

#[test]
fn a_dialog_is_a_modal_dialog_node_named_by_its_title_that_holds_its_controls() {
    let (mut h, _) = rig(false);
    assert!(h.query_by_role(Role::Dialog).is_none(), "closed until the caller shows it");
    open(&mut h);
    let dialog = h.get_by_role_and_label(Role::Dialog, "Delete 3 parts?");
    assert!(dialog.accesskit_node().is_modal());
    let primary = h.get_by_role_and_label(Role::Button, "Delete 3 parts");
    assert!(inside(primary.accesskit_node(), Role::Dialog), "the buttons are inside the dialog node");
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
}

#[test]
fn a_dangerous_dialog_is_an_alert_dialog() {
    let (mut h, _) = rig(true);
    open(&mut h);
    h.get_by_role_and_label(Role::AlertDialog, "Delete 3 parts?");
}

#[test]
fn tab_stays_inside_the_dialog_and_esc_cancels_and_gives_the_focus_back() {
    let (mut h, _) = rig(true);
    open(&mut h);
    assert_eq!(focused(&h), "Cancel", "a dangerous dialog opens on the safe choice");
    let mut seen = Vec::new();
    for _ in 0..6 {
        press(&mut h, Modifiers::NONE, Key::Tab);
        seen.push(focused(&h));
    }
    assert!(seen.iter().all(|l| l == "Cancel" || l == "Delete 3 parts"), "focus never leaves the dialog: {seen:?}");
    assert!(seen.iter().any(|l| l == "Delete 3 parts") && seen.iter().any(|l| l == "Cancel"));
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert_eq!(h.state().action, Some(DialogAction::Cancel));
    assert!(h.query_by_role(Role::AlertDialog).is_none());
    assert_eq!(focused(&h), "Open", "focus returns to the button that opened it");
}

#[test]
fn pointer_clicks_cannot_reach_what_is_behind_a_dialog() {
    let (mut h, _) = rig(true);
    let behind = h.get_by_role_and_label(Role::Button, "Behind").rect().center();
    open(&mut h);
    click_at(&mut h, behind);
    assert!(h.query_by_role(Role::AlertDialog).is_some(), "a click on the scrim does not close it");
    assert_ne!(focused(&h), "Behind");
}

#[test]
fn a_busy_dialog_ignores_esc_and_cancel() {
    let (mut h, _) = rig(false);
    h.state_mut().busy = true;
    open(&mut h);
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_role(Role::Dialog).is_some(), "an irreversible action in progress cannot be walked away from");
    h.get_by_role_and_label(Role::Button, "Cancel").click();
    h.run_steps(3);
    assert!(h.query_by_role(Role::Dialog).is_some());
}

#[test]
fn a_dangerous_primary_needs_a_second_deliberate_press_after_the_dwell() {
    let (mut h, clock) = rig(true);
    open(&mut h);
    let at = {
        let r = h.get_by_role_and_label(Role::Button, "Delete 3 parts").rect();
        pos2(r.right() - 8.0, r.center().y)
    };
    click_at(&mut h, at);
    assert_eq!(h.state().action, None, "the first press only arms it");
    h.get_by_role_and_label(Role::Button, "Confirm: delete 3 parts");
    clock.advance(Seconds(0.15));
    click_at(&mut h, at);
    assert_eq!(h.state().action, None, "the second click of a double-click is ignored");
    clock.advance(Seconds(ARM_DELAY));
    h.run();
    click_at(&mut h, at);
    assert_eq!(h.state().action, Some(DialogAction::Primary), "a fresh press after the dwell goes through");
}

#[test]
fn a_held_enter_cannot_arm_and_confirm_a_dangerous_dialog() {
    let (mut h, clock) = rig(true);
    open(&mut h);
    h.get_by_role_and_label(Role::Button, "Delete 3 parts").focus();
    h.run();
    clock.advance(Seconds(0.0));
    let enter = |pressed, repeat| Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed,
        repeat,
        modifiers: Modifiers::NONE,
    };
    h.event(enter(true, false));
    h.run();
    for _ in 0..4 {
        clock.advance(Seconds(0.2));
        h.event(enter(true, true));
        h.run();
    }
    h.event(enter(false, false));
    h.run();
    assert_eq!(h.state().action, None);
}

#[test]
fn a_plain_primary_is_disabled_with_a_reason_until_the_caller_allows_it() {
    let (mut h, _) = rig(false);
    open(&mut h);
    assert_eq!(focused(&h), "", "a dialog that leaves the first focus to its body does not grab it");
    let primary = h.get_by_role_and_label(Role::Button, "Delete 3 parts");
    assert_eq!(primary.accesskit_node().description().as_deref(), Some("Enter a name first"));
    primary.click();
    h.run_steps(2);
    assert_eq!(h.state().action, None);
    h.get_by_role_and_label(Role::TextInput, "Version name").focus();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Version name").type_text("v3");
    h.run_steps(2);
    h.get_by_role_and_label(Role::Button, "Delete 3 parts").click();
    h.run_steps(3);
    assert_eq!(h.state().action, Some(DialogAction::Primary));
}

#[test]
fn esc_closes_a_dropdown_inside_the_dialog_before_it_closes_the_dialog() {
    let (mut h, _) = rig(false);
    open(&mut h);
    h.get_by_role_and_label(Role::ComboBox, "Part").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert!(h.query_by_role(Role::ListBox).is_some(), "the dropdown opened over the dialog");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert!(h.query_by_role(Role::ListBox).is_none(), "the first Esc closes the list");
    assert!(h.query_by_role(Role::Dialog).is_some(), "and the dialog stays");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert_eq!(h.state().action, Some(DialogAction::Cancel), "the second one closes the dialog");
}

#[test]
fn opening_a_plain_dialog_with_enter_and_holding_it_does_not_press_the_primary() {
    // The first focus is the primary button. Enter opens the dialog; the key then repeats while held (egui can deliver
    // `repeat: true` presses): none of it may act as a press of the primary.
    let mut h = harness_state(Settings::default(), vec2(640.0, 420.0), (false, 0u32), |ui, s| {
        if ui.add(Button::new("Open")).clicked() {
            s.0 = true;
        }
        if s.0 {
            let action = Dialog::new("plain", "Save", "Save version").show(ui.ctx(), |ui| {
                ui.label("Body");
            });
            s.1 += u32::from(action == Some(DialogAction::Primary));
        }
    });
    h.get_by_role_and_label(Role::Button, "Open").focus();
    h.run();
    // The key goes down and is held: no release until the end.
    h.event(Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(3);
    assert!(h.query_by_role(Role::Dialog).is_some());
    for _ in 0..4 {
        h.event(Event::Key {
            key: Key::Enter,
            physical_key: Some(Key::Enter),
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        });
        h.run();
    }
    assert_eq!(h.state().1, 0, "repeats of the Enter that opened the dialog must not press Save version");
}

#[test]
fn the_release_of_the_enter_that_opened_a_dialog_is_not_a_press_of_its_primary() {
    let mut h = harness_state(Settings::default(), vec2(640.0, 420.0), (false, 0u32), |ui, s| {
        if ui.add(Button::new("Open")).clicked() {
            s.0 = true;
        }
        if s.0 {
            let action = Dialog::new("plain2", "Save", "Save version").show(ui.ctx(), |ui| {
                ui.label("Body");
            });
            s.1 += u32::from(action == Some(DialogAction::Primary));
        }
    });
    h.get_by_role_and_label(Role::Button, "Open").focus();
    h.run();
    h.event(Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(3);
    h.event(Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed: false,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(3);
    assert_eq!(h.state().1, 0);
}

#[test]
fn a_dialog_opened_with_the_mouse_gives_the_focus_back_to_its_opener() {
    let (mut h, _) = rig(true);
    let at = h.get_by_role_and_label(Role::Button, "Open").rect().center();
    click_at(&mut h, at);
    h.run_steps(3);
    assert!(h.query_by_role(Role::AlertDialog).is_some());
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert_eq!(focused(&h), "Open", "focus returns to the button that was clicked");
}

#[test]
fn every_dialog_of_the_gallery_returns_the_focus_to_the_button_that_opened_it() {
    use crate::ds::gallery::Gallery;
    let mut h = egui_kittest::Harness::builder()
        .with_size(vec2(1680.0, 6_500.0))
        .build_ui_state(|ui, g: &mut Gallery| g.ui(ui), Gallery::new(Settings::default()));
    h.run_steps(6);
    for (opener, dialog) in [("Name a version…", Role::Dialog), ("Delete parts…", Role::AlertDialog)] {
        let at = h.get_by_role_and_label(Role::Button, opener).rect().center();
        h.hover_at(at);
        h.run_steps(2);
        for pressed in [true, false] {
            h.event(Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Modifiers::NONE,
            });
            h.run_steps(2);
        }
        h.run_steps(3);
        assert!(h.query_by_role(dialog).is_some(), "{opener} opens its dialog");
        press_for(&mut h, Modifiers::NONE, Key::Escape, 4);
        assert_eq!(focused(&h), opener, "closing the dialog of {opener} returns the focus");
    }
}
