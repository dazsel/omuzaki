//! The dropdown and the list it opens: operated with the keyboard alone, with the pointer, and by a screen reader.

use egui::accesskit::{Action, ActionData, ActionRequest};
use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;

use super::*;
use crate::ds::testing::unlabelled_interactive;

const PARTS: [&str; 7] = ["Back hair", "Bangs", "Body", "Eyes", "Face", "Front hair", "Mouth"];

struct Rig {
    part: usize,
    changes: u32,
}

fn rig(part: usize) -> Harness<'static, Rig> {
    harness_state(Settings::default(), vec2(360.0, 420.0), Rig { part, changes: 0 }, |ui, r| {
        ui.add(Button::new("Before"));
        let choices: Vec<_> = PARTS.iter().enumerate().map(|(i, p)| Choice::new(i, p)).collect();
        if ui.add(Dropdown::new("Part", &mut r.part, &choices)).changed() {
            r.changes += 1;
        }
        ui.add(Button::new("After"));
    })
}

fn combo<'h>(h: &'h Harness<'_, Rig>) -> egui_kittest::Node<'h> {
    h.get_by_role_and_label(Role::ComboBox, "Part")
}

/// Whether egui's keyboard focus is on the dropdown. (AccessKit reports the focus as the *active option* while the list
/// is open, which is the point of an active descendant, so the node's own `is_focused` is the wrong question.)
fn combo_has_focus(h: &Harness<'_, Rig>) -> bool {
    let (node, _tree) = combo(h).accesskit_node().locate();
    h.ctx.memory(egui::Memory::focused).is_some_and(|f| f.accesskit_id() == node)
}

fn open_with_keyboard(h: &mut Harness<'_, Rig>) {
    combo(h).focus();
    h.run();
    press(h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
}

#[test]
fn a_dropdown_is_a_combobox_that_says_its_value_and_whether_it_is_open() {
    let mut h = rig(5);
    let node = combo(&h);
    assert_eq!(node.value().as_deref(), Some("Front hair"));
    assert_eq!(node.accesskit_node().data().is_expanded(), Some(false));
    assert!(h.query_by_role(Role::ListBox).is_none(), "closed: no list exists");
    open_with_keyboard(&mut h);
    assert_eq!(combo(&h).accesskit_node().data().is_expanded(), Some(true));
    assert!(combo_has_focus(&h), "the combobox keeps the focus while its list is open");
    h.get_by_role_and_label(Role::ListBox, "Part");
}

#[test]
fn the_list_is_one_tab_stop_with_its_options_named_positioned_and_inside_the_list() {
    let mut h = rig(5);
    open_with_keyboard(&mut h);
    let list = h.get_by_role_and_label(Role::ListBox, "Part");
    assert_eq!(list.accesskit_node().size_of_set(), Some(PARTS.len()));
    let option = h.get_by_role_and_label(Role::ListBoxOption, "Front hair");
    assert_eq!(option.accesskit_node().position_in_set(), Some(6));
    assert_eq!(option.accesskit_node().is_selected(), Some(true), "the current value is selected");
    assert!(inside(option.accesskit_node(), Role::ListBox), "options belong to the list");
    let active = combo(&h).accesskit_node().active_descendant().map(|n| n.label());
    assert_eq!(active, Some(Some("Front hair".to_owned())), "the active option is the chosen one when it opens");
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
    // Tab from before reaches the combobox and then leaves it: the options are not stops.
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.get_by_role_and_label(Role::Button, "Before").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(combo(&h).is_focused());
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(h.get_by_role_and_label(Role::Button, "After").is_focused());
}

#[test]
fn arrows_move_the_active_option_and_enter_chooses_it() {
    let mut h = rig(5);
    open_with_keyboard(&mut h);
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    h.run_steps(2);
    let active = combo(&h).accesskit_node().active_descendant().and_then(|n| n.label());
    assert_eq!(active.as_deref(), Some("Mouth"));
    assert_eq!(h.state().part, 5, "moving does not choose");
    press(&mut h, Modifiers::NONE, Key::Home);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().part, 0, "Home then Enter chooses the first option");
    assert_eq!(h.state().changes, 1);
    assert!(h.query_by_role(Role::ListBox).is_none(), "choosing closes the list");
    assert!(combo_has_focus(&h), "and the focus is still on the dropdown");
}

#[test]
fn escape_closes_without_choosing_and_tab_chooses_and_moves_on() {
    let mut h = rig(5);
    open_with_keyboard(&mut h);
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert_eq!(h.state().part, 5, "Esc keeps the value");
    assert!(h.query_by_role(Role::ListBox).is_none());
    assert!(combo_has_focus(&h), "and focus stays on the dropdown");
    open_with_keyboard(&mut h);
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run_steps(3);
    assert_eq!(h.state().part, 4, "Tab chooses the active option");
    assert!(h.get_by_role_and_label(Role::Button, "After").is_focused(), "and the focus moves on");
}

#[test]
fn typing_jumps_to_the_row_that_starts_with_the_letters_and_wraps_through_a_shared_initial() {
    let mut h = rig(0);
    combo(&h).focus();
    h.run();
    h.event(Event::Text("m".to_owned()));
    h.run_steps(3);
    let active = combo(&h).accesskit_node().active_descendant().and_then(|n| n.label());
    assert_eq!(active.as_deref(), Some("Mouth"), "one letter opens the list at the first match");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().part, 6);
    // Two letters narrow it down: "b" then "o" is Body, not Bangs.
    combo(&h).focus();
    h.run();
    h.event(Event::Text("b".to_owned()));
    h.run();
    h.event(Event::Text("o".to_owned()));
    h.run_steps(2);
    let active = combo(&h).accesskit_node().active_descendant().and_then(|n| n.label());
    assert_eq!(active.as_deref(), Some("Body"));
}

#[test]
fn the_pointer_opens_it_chooses_a_row_returns_the_focus_and_a_click_outside_closes_it() {
    let mut h = rig(5);
    combo(&h).click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::ListBoxOption, "Bangs").click();
    h.run_steps(3);
    assert_eq!(h.state().part, 1);
    assert!(h.query_by_role(Role::ListBox).is_none(), "choosing closes the list");
    assert!(combo_has_focus(&h), "the focus is back on the dropdown");
    combo(&h).click();
    h.run_steps(3);
    assert!(h.query_by_role(Role::ListBox).is_some());
    h.get_by_role_and_label(Role::Button, "Before").click();
    h.run_steps(3);
    assert!(h.query_by_role(Role::ListBox).is_none(), "a click elsewhere closes it");
    assert_eq!(h.state().part, 1, "without choosing anything");
}

fn request(h: &mut Harness<'_, Rig>, action: Action, data: Option<ActionData>) {
    let (target_node, target_tree) = combo(h).accesskit_node().locate();
    h.event(Event::AccessKitActionRequest(ActionRequest { action, target_tree, target_node, data }));
    h.run_steps(3);
}

#[test]
fn a_screen_reader_can_step_set_and_expand_a_dropdown_that_has_no_focus() {
    let mut h = rig(1);
    assert!(!combo(&h).is_focused());
    request(&mut h, Action::Increment, None);
    assert_eq!(h.state().part, 2, "Increment chooses the next option");
    request(&mut h, Action::Decrement, None);
    request(&mut h, Action::Decrement, None);
    assert_eq!(h.state().part, 0);
    request(&mut h, Action::Decrement, None);
    assert_eq!(h.state().part, 0, "and stops at the first");
    request(&mut h, Action::SetValue, Some(ActionData::Value("Mouth".into())));
    assert_eq!(h.state().part, 6, "SetValue chooses the option with that text");
    request(&mut h, Action::SetValue, Some(ActionData::Value("Nothing like it".into())));
    assert_eq!(h.state().part, 6, "an unknown text changes nothing");
    request(&mut h, Action::Expand, None);
    assert_eq!(combo(&h).accesskit_node().data().is_expanded(), Some(true));
    request(&mut h, Action::Collapse, None);
    assert_eq!(combo(&h).accesskit_node().data().is_expanded(), Some(false));
}

#[test]
fn a_disabled_dropdown_says_why_and_ignores_the_keyboard_and_a_row_that_cannot_be_chosen_is_skipped() {
    let mut h = harness_state(Settings::default(), vec2(360.0, 300.0), 0usize, |ui, v| {
        let (a, b, c) = ("Base", "Close-up", "Game");
        ui.add(Dropdown::new(
            "Variant",
            v,
            &[Choice::new(0, a), Choice::new(1, b).disabled("Not built yet"), Choice::new(2, c)],
        ));
        ui.add(Dropdown::new("Locked", &mut 0usize, &[Choice::new(0, a)]).disabled("Only one variant"));
    });
    let locked = h.get_by_role_and_label(Role::ComboBox, "Locked");
    assert_eq!(locked.accesskit_node().description().as_deref(), Some("Only one variant"));
    locked.focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    assert!(h.query_by_role_and_label(Role::ListBox, "Locked").is_none(), "a disabled dropdown does not open");
    h.get_by_role_and_label(Role::ComboBox, "Variant").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    assert_eq!(*h.state(), 2, "Down skips the option that is not available");
}

#[test]
fn a_held_enter_does_not_open_and_close_the_dropdown_again() {
    let mut h = rig(5);
    combo(&h).focus();
    h.run();
    let enter = |pressed, repeat| Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed,
        repeat,
        modifiers: Modifiers::NONE,
    };
    h.event(enter(true, false));
    h.run_steps(2);
    for _ in 0..3 {
        h.event(enter(true, true));
        h.run();
    }
    h.event(enter(false, false));
    h.run_steps(2);
    assert!(h.query_by_role(Role::ListBox).is_some(), "the auto-repeat of the opening key is not another press");
}

#[test]
fn a_list_that_shrinks_while_the_dropdown_is_open_does_not_panic_or_choose_a_row_that_is_gone() {
    let mut h = harness_state(Settings::default(), vec2(360.0, 420.0), (3usize, 5usize), |ui, (value, n)| {
        let labels = ["a", "b", "c", "d", "e", "f"];
        let choices: Vec<_> = labels.iter().take(*n).enumerate().map(|(i, l)| Choice::new(i, l)).collect();
        ui.add(Dropdown::new("Part", value, &choices));
    });
    h.get_by_role_and_label(Role::ComboBox, "Part").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    press(&mut h, Modifiers::NONE, Key::End);
    h.state_mut().1 = 2;
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    assert!(h.state().0 < 2 || h.state().0 == 3, "no row beyond the list was chosen: {}", h.state().0);
}
