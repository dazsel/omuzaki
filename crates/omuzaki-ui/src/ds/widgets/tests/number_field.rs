//! Number fields: type, nudge, reset, revert, and what a screen reader can do to one.

use egui::accesskit::{Action, ActionData, ActionRequest};
use egui::{Event, Ui};
use egui_kittest::Harness;

use super::*;
use crate::ds::testing::unlabelled_interactive;

struct Rig {
    value: f64,
    changes: u32,
    resets: u32,
}

fn rig(value: f64) -> Harness<'static, Rig> {
    harness_state(Settings::default(), vec2(360.0, 160.0), Rig { value, changes: 0, resets: 0 }, |ui: &mut Ui, r| {
        ui.add(Button::new("Before"));
        let out = NumberField::new("Output", &mut r.value, 0.0..=1.0).default_value(0.5).unit("").show(ui);
        r.changes += u32::from(out.response.changed());
        r.resets += u32::from(out.reset);
    })
}

fn field<'h>(h: &'h Harness<'_, Rig>) -> egui_kittest::Node<'h> {
    h.get_by_role_and_label(Role::SpinButton, "Output")
}

fn type_into(h: &mut Harness<'_, Rig>, text: &str) {
    field(h).click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Output value").type_text(text);
    h.run();
    press(h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
}

#[test]
fn it_is_a_spin_button_with_its_range_step_and_text() {
    let h = rig(0.62);
    let ak = field(&h).accesskit_node();
    assert_eq!((ak.min_numeric_value(), ak.max_numeric_value()), (Some(0.0), Some(1.0)));
    assert!((ak.numeric_value().unwrap_or_default() - 0.62).abs() < 1e-9);
    assert_eq!(field(&h).value().as_deref(), Some("0.62"));
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
}

#[test]
fn typing_replaces_the_number_arithmetic_and_percent_work_and_esc_puts_the_old_value_back() {
    let mut h = rig(0.62);
    type_into(&mut h, "0.5*1.2");
    assert!((h.state().value - 0.6).abs() < 1e-9, "{}", h.state().value);
    assert_eq!(h.state().changes, 1);
    type_into(&mut h, "25%");
    assert!((h.state().value - 0.25).abs() < 1e-9);
    field(&h).click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Output value").type_text("0.9");
    h.run();
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    assert!((h.state().value - 0.25).abs() < 1e-9, "Esc reverts");
    assert!(h.query_by_role_and_label(Role::TextInput, "Output value").is_none(), "and stops typing");
    assert!(field(&h).is_focused(), "with the focus back on the field");
}

#[test]
fn text_that_is_not_a_number_keeps_the_field_open_and_says_what_is_wanted() {
    let mut h = rig(0.62);
    type_into(&mut h, "abc");
    assert!((h.state().value - 0.62).abs() < 1e-9);
    h.get_by_role_and_label(Role::TextInput, "Output value");
    assert!(
        crate::ds::testing::all_strings(&h).iter().any(|s| s.contains("Enter a number from 0.00 to 1.00")),
        "the message is painted: {:?}",
        crate::ds::testing::all_strings(&h)
    );
}

#[test]
fn up_down_page_and_ctrl_backspace_work_on_the_focused_field() {
    let mut h = rig(0.62);
    field(&h).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    assert!((h.state().value - 0.63).abs() < 1e-9, "{}", h.state().value);
    press(&mut h, Modifiers::SHIFT, Key::ArrowDown);
    assert!((h.state().value - 0.53).abs() < 1e-9);
    press(&mut h, Modifiers::NONE, Key::PageUp);
    assert!((h.state().value - 0.63).abs() < 1e-9);
    press(&mut h, Modifiers::CTRL, Key::Backspace);
    assert!((h.state().value - 0.5).abs() < 1e-9, "back to the default");
    assert_eq!(h.state().resets, 1);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Output value");
}

#[test]
fn a_screen_reader_steps_and_sets_it_without_focus_and_a_disabled_field_says_why() {
    let mut h = rig(0.5);
    let (target_node, target_tree) = field(&h).accesskit_node().locate();
    let send = |h: &mut Harness<'_, Rig>, action, data| {
        h.event(Event::AccessKitActionRequest(ActionRequest { action, target_tree, target_node, data }));
        h.run();
    };
    send(&mut h, Action::Increment, None);
    assert!((h.state().value - 0.51).abs() < 1e-9);
    send(&mut h, Action::SetValue, Some(ActionData::NumericValue(0.9)));
    assert!((h.state().value - 0.9).abs() < 1e-9);
    send(&mut h, Action::SetValue, Some(ActionData::Value("2".into())));
    assert!((h.state().value - 1.0).abs() < 1e-9, "clamped into the range");

    let h = harness_state(Settings::default(), vec2(200.0, 60.0), 0.5_f64, |ui: &mut Ui, v| {
        ui.add(NumberField::new("Input", v, 0.0..=1.0).disabled("Pinned to the edge"));
    });
    let node = h.get_by_role_and_label(Role::SpinButton, "Input");
    assert!(node.accesskit_node().is_disabled());
    assert_eq!(node.accesskit_node().description().as_deref(), Some("Pinned to the edge"));
}

#[test]
fn enter_held_to_start_typing_does_not_end_it_when_the_key_repeats_three_frames_later() {
    let mut h = rig(0.62);
    field(&h).focus();
    h.run();
    let key = |repeat| Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed: true,
        repeat,
        modifiers: Modifiers::NONE,
    };
    h.event(key(false));
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Output value");
    h.event(key(true));
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Output value");
    assert_eq!(h.state().changes, 0);
}
