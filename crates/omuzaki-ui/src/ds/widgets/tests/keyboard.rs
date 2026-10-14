//! One tab stop per control, a name for everything a person can operate, and AccessKit actions that work on a node
//! that does not have focus.

use egui::accesskit::{Action, ActionData, ActionRequest};
use egui::{Event, Sense};
use egui_kittest::Harness;

use super::*;
use crate::ds::audit::focus_indicators;
use crate::ds::icons;
use crate::ds::testing::unlabelled_interactive;

struct Rig {
    value: f64,
    events: Vec<ParamEvent>,
    text: String,
}

fn rig(value: f64) -> Harness<'static, Rig> {
    harness_state(
        Settings::default(),
        vec2(360.0, 200.0),
        Rig { value, events: Vec::new(), text: String::new() },
        |ui, r| {
            ui.add(Button::new("Before"));
            let out = ParamSlider::new("Width", &mut r.value, 0.0..=1.0).default_value(0.5).show(ui);
            r.events.extend(out.events);
            ui.add(TextField::new("After", &mut r.text));
        },
    )
}

fn focused_label(h: &Harness<'_, Rig>) -> String {
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().is_focused())
        .map(|n| format!("{:?} {}", n.accesskit_node().role(), n.accesskit_node().label().unwrap_or_default()))
        .unwrap_or_default()
}

#[test]
fn a_param_slider_is_one_tab_stop() {
    let mut h = rig(0.62);
    h.get_by_role_and_label(Role::Button, "Before").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert_eq!(focused_label(&h), "Slider Width", "Tab goes from the button to the track");
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert_eq!(
        focused_label(&h),
        "TextInput After",
        "and the next Tab leaves the slider: its reset button, label and number are not stops (they were: reset, an unnamed label, number)"
    );
    press(&mut h, Modifiers::SHIFT, Key::Tab);
    assert_eq!(focused_label(&h), "Slider Width", "Shift+Tab comes back the same way");
}

#[test]
fn the_reset_button_and_the_number_are_named_for_screen_readers_but_are_not_focus_stops() {
    let h = rig(0.62);
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new(), "no operable node is nameless");
    for name in ["Reset Width", "Edit Width value"] {
        let node = h.get_by_role_and_label(Role::Button, name);
        let data = node.accesskit_node().data().clone();
        assert!(data.supports_action(Action::Click), "{name} can be clicked by a screen reader");
        assert!(!data.supports_action(Action::Focus), "{name} is reached by shortcut, not by Tab");
    }
}

#[test]
fn the_keyboard_still_reaches_the_number_and_the_reset() {
    let mut h = rig(0.62);
    h.get_by_role_and_label(Role::Slider, "Width").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Width value");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Slider, "Width").focus();
    h.run();
    press(&mut h, Modifiers::CTRL, Key::Backspace);
    assert!((h.state().value - 0.5).abs() < 1e-9, "Ctrl+Backspace resets from the track");
}

#[test]
fn double_clicking_the_label_resets_to_the_default() {
    let mut h = rig(0.8);
    let row = h.get_by_role_and_label(Role::Slider, "Width").rect();
    // The label column sits just left of the track.
    let at = pos2(row.left() - 40.0, row.center().y);
    h.hover_at(at);
    h.run();
    let click = |pressed| Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    // Both clicks in one frame, so they are inside egui's double-click interval whatever the frame step is.
    h.input_mut().events.extend([click(true), click(false), click(true), click(false)]);
    h.step();
    h.run();
    assert!((h.state().value - 0.5).abs() < 1e-9, "reset to 0.5, got {}", h.state().value);
}

fn action(h: &mut Harness<'_, Rig>, action: Action, data: Option<ActionData>) {
    let (target_node, target_tree) = h.get_by_role_and_label(Role::Slider, "Width").accesskit_node().locate();
    h.event(Event::AccessKitActionRequest(ActionRequest { action, target_tree, target_node, data }));
    h.run();
}

#[test]
fn a_screen_reader_can_step_and_set_a_slider_that_has_no_focus() {
    let mut h = rig(0.62);
    assert!(!h.get_by_role_and_label(Role::Slider, "Width").is_focused());
    action(&mut h, Action::Increment, None);
    assert!((h.state().value - 0.63).abs() < 1e-9, "Increment steps up without prior focus: {}", h.state().value);
    action(&mut h, Action::Decrement, None);
    action(&mut h, Action::Decrement, None);
    assert!((h.state().value - 0.61).abs() < 1e-9, "Decrement steps down: {}", h.state().value);
    action(&mut h, Action::SetValue, Some(ActionData::NumericValue(0.9)));
    assert!((h.state().value - 0.9).abs() < 1e-9, "SetValue sets it (it was advertised and ignored)");
    assert!(
        matches!(h.state().events.last(), Some(ParamEvent::Commit { to, cause: CommitCause::Typed, .. }) if (*to - 0.9).abs() < 1e-9),
        "one commit per request: {:?}",
        h.state().events
    );
    action(&mut h, Action::SetValue, Some(ActionData::NumericValue(7.0)));
    assert!((h.state().value - 1.0).abs() < 1e-9, "out of range clamps to the end");
    action(&mut h, Action::SetValue, Some(ActionData::Value("0.25".into())));
    assert!((h.state().value - 0.25).abs() < 1e-9, "a text value is read like typed text");
}

#[test]
fn a_locked_slider_ignores_a_screen_readers_requests() {
    let mut h = harness_state(Settings::default(), vec2(360.0, 60.0), 0.5_f64, |ui, v| {
        ParamSlider::new("Width", v, 0.0..=1.0).default_value(0.5).locked_by("Designer").show(ui);
    });
    let (target_node, target_tree) = h.get_by_role_and_label(Role::Slider, "Width").accesskit_node().locate();
    h.event(Event::AccessKitActionRequest(ActionRequest {
        action: Action::SetValue,
        target_tree,
        target_node,
        data: Some(ActionData::NumericValue(0.9)),
    }));
    h.run();
    assert!((*h.state() - 0.5).abs() < 1e-9);
}

#[test]
fn the_clear_button_of_a_text_field_has_a_focus_ring() {
    let mut h = harness_state(Settings::default(), vec2(360.0, 80.0), "twintails".to_owned(), |ui, text| {
        ui.add(TextField::new("Part name", text));
    });
    h.get_by_role_and_label(Role::TextInput, "Part name").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Tab);
    let clear = h.get_by_role_and_label(Role::Button, "Clear Part name");
    assert!(clear.is_focused(), "Tab moves from the text to the clear button");
    let rect = clear.rect();
    let theme = crate::ds::theme::ds(&h.ctx).theme;
    assert!(
        focus_indicators(h.output(), &theme).iter().any(|ring| ring.intersects(rect)),
        "a focused clear button shows a ring (it showed nothing)"
    );
    assert!(rect.width() >= 24.0 && rect.height() >= 24.0, "and is a 24 pt target: {rect:?}");
}

#[test]
fn the_inline_rename_field_of_a_tree_row_has_a_name() {
    let h = harness_state(Settings::default(), vec2(360.0, 120.0), String::from("Hair"), |ui, name| {
        TreeRow::new(1, "Hair", icons::CUBE).renaming(name).show(ui);
    });
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
    h.get_by_role_and_label(Role::TextInput, "Rename Hair");
}

#[test]
fn the_lint_flags_a_custom_widget_that_senses_clicks_but_describes_nothing() {
    let nameless = harness_state(Settings::default(), vec2(200.0, 80.0), (), |ui, ()| {
        let (rect, _) = ui.allocate_exact_size(vec2(40.0, 24.0), Sense::hover());
        ui.interact(rect, Id::new("nameless"), Sense::click());
    });
    let found = unlabelled_interactive(&nameless);
    assert_eq!(found.len(), 1, "{found:?}");
    let named = harness_state(Settings::default(), vec2(200.0, 80.0), (), |ui, ()| {
        ui.add(Button::new("Named"));
    });
    assert_eq!(unlabelled_interactive(&named), Vec::<String>::new());
}

#[test]
fn the_colour_popover_has_no_nameless_control() {
    let mut h = colour_harness();
    h.get_by_role_and_label(Role::ColorWell, "Hair root colour").click();
    h.run_steps(4);
    h.get_by_role_and_label(Role::Slider, "Hue");
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
}

#[test]
fn only_repeats_tells_a_held_key_from_a_press_a_click_and_a_screen_reader() {
    use crate::ds::input::only_repeats;
    // (clicks, clicks that were only a key repeating)
    let mut h = harness_state(Settings::default(), vec2(200.0, 80.0), (0u32, 0u32), |ui, counts| {
        let response = ui.add(Button::new("Go"));
        if response.clicked() {
            counts.0 += 1;
            counts.1 += u32::from(only_repeats(&response));
        }
    });
    h.get_by_role_and_label(Role::Button, "Go").focus();
    h.run();
    let enter = |pressed, repeat| Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed,
        repeat,
        modifiers: Modifiers::NONE,
    };
    h.event(enter(true, false));
    h.run();
    assert_eq!(*h.state(), (1, 0), "the key going down is a press");
    for _ in 0..3 {
        h.event(enter(true, true));
        h.run();
    }
    assert_eq!(*h.state(), (4, 3), "its repeats are clicks to egui, and repeats to us");
    h.event(enter(false, false));
    h.run();
    h.get_by_role_and_label(Role::Button, "Go").click();
    h.run();
    assert_eq!(*h.state(), (5, 3), "a pointer click is not a repeat");
}
