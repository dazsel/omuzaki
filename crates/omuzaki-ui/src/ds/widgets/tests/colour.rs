//! `ColourField` from the keyboard: Enter and Space open it, Esc closes it, and focus returns to the field.

use super::*;

const FIELD: &str = "Hair root colour";

fn focused_field() -> egui_kittest::Harness<'static, ColourRig> {
    let mut h = colour_harness();
    h.get_by_role_and_label(Role::ColorWell, FIELD).focus();
    h.run();
    h
}

fn popover_open(h: &egui_kittest::Harness<'_, ColourRig>) -> bool {
    h.query_by_role_and_label(Role::Slider, "Hue").is_some()
}

#[test]
fn enter_opens_the_popover() {
    let mut h = focused_field();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert!(
        popover_open(&h),
        "Enter opens it (the same Enter used to be read again as 'commit' and closed it at once)"
    );
    assert!(h.state().events.is_empty(), "opening commits nothing: {:?}", h.state().events);
}

#[test]
fn space_opens_the_popover() {
    let mut h = focused_field();
    press(&mut h, Modifiers::NONE, Key::Space);
    h.run_steps(3);
    assert!(popover_open(&h));
}

#[test]
fn escape_closes_it_cancels_and_focus_returns_to_the_field() {
    for open_with in [Key::Enter, Key::Space] {
        let mut h = focused_field();
        let original = h.state().colour;
        press(&mut h, Modifiers::NONE, open_with);
        h.run_steps(3);
        h.get_by_role_and_label(Role::Slider, "Hue").focus();
        h.run();
        press(&mut h, Modifiers::SHIFT, Key::ArrowRight);
        assert_ne!(h.state().colour, original, "the hue bar moved the colour");
        press(&mut h, Modifiers::NONE, Key::Escape);
        h.run_steps(3);
        assert!(!popover_open(&h), "Esc closes the popover opened with {open_with:?}");
        assert_eq!(h.state().colour, original, "and restores the colour");
        assert!(h.get_by_role_and_label(Role::ColorWell, FIELD).is_focused(), "focus is back on the field");
    }
}

#[test]
fn enter_inside_the_popover_commits_and_focus_returns_to_the_field() {
    let mut h = focused_field();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Slider, "Hue").focus();
    h.run();
    press(&mut h, Modifiers::SHIFT, Key::ArrowRight);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert!(!popover_open(&h));
    let commits = h.state().events.iter().filter(|e| matches!(e, ColourEvent::Commit { .. })).count();
    assert_eq!(commits, 1, "{:?}", h.state().events);
    assert!(h.get_by_role_and_label(Role::ColorWell, FIELD).is_focused());
}

#[test]
fn a_held_enter_opens_it_once_and_its_repeats_do_not_close_it_again() {
    let mut h = focused_field();
    let enter = |pressed, repeat| egui::Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed,
        repeat,
        modifiers: Modifiers::NONE,
    };
    h.event(enter(true, false));
    h.run_steps(2);
    for _ in 0..4 {
        h.event(enter(true, true));
        h.run_steps(2);
    }
    assert!(popover_open(&h), "auto-repeat of the key that opened it does not toggle it shut");
    h.event(enter(false, false));
    h.run_steps(2);
    assert!(popover_open(&h));
}

#[test]
fn clicking_the_field_still_toggles_it_and_a_disabled_one_does_not_open() {
    let mut h = colour_harness();
    h.get_by_role_and_label(Role::ColorWell, FIELD).click();
    h.run_steps(3);
    assert!(popover_open(&h));
    let mut off = harness_state(Settings::default(), vec2(360.0, 300.0), Colour::rgb(1, 2, 3), |ui, c| {
        ColourField::new("Locked colour", c).disabled("Managed by the theme").show(ui);
    });
    let node = off.get_by_role_and_label(Role::ColorWell, "Locked colour");
    assert_eq!(node.accesskit_node().description().as_deref(), Some("Managed by the theme"));
    node.click();
    off.run_steps(3);
    assert!(off.query_by_role_and_label(Role::Slider, "Hue").is_none());
}
