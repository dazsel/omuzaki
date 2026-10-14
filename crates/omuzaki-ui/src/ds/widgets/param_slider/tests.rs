//! Behaviour tests for `ParamSlider`: pointer, keyboard, typing, reset and the AccessKit tree, through `egui_kittest`.

use egui::{Key, Modifiers, Pos2, accesskit::Role, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};

use super::*;
use crate::ds::testing::{drag, harness_state, press};
use crate::ds::theme::Settings;

struct Rig {
    value: f64,
    events: Vec<ParamEvent>,
    changed: u32,
    unit: &'static str,
    range: (f64, f64),
    mixed: bool,
    locked: bool,
}

impl Rig {
    fn new(value: f64) -> Self {
        Self { value, events: Vec::new(), changed: 0, unit: "", range: (0.0, 1.0), mixed: false, locked: false }
    }
}

fn rig(rig: Rig) -> Harness<'static, Rig> {
    harness_state(Settings::default(), vec2(360.0, 60.0), rig, |ui, rig| {
        let mut slider =
            ParamSlider::new("Width", &mut rig.value, rig.range.0..=rig.range.1).default_value(0.5).unit(rig.unit);
        if rig.mixed {
            slider = slider.mixed(true);
        }
        if rig.locked {
            slider = slider.locked_by("Designer");
        }
        let out = slider.show(ui);
        rig.changed += u32::from(out.response.changed());
        rig.events.extend(out.events);
    })
}

fn track_x(harness: &Harness<'_, Rig>, fraction: f64) -> Pos2 {
    let rect = harness.get_by_role_and_label(Role::Slider, "Width").rect();
    let (lo, hi) = (rect.left() + 8.0, rect.right() - 8.0);
    #[allow(clippy::cast_possible_truncation, reason = "a pixel position")]
    pos2(lo + (hi - lo) * fraction as f32, rect.center().y)
}

fn focus_track(harness: &mut Harness<'_, Rig>) {
    harness.get_by_role_and_label(Role::Slider, "Width").focus();
    harness.run();
}

#[test]
fn it_is_a_labelled_slider_with_a_text_value() {
    let harness = rig(Rig::new(0.62));
    let node = harness.get_by_role_and_label(Role::Slider, "Width");
    let ak = node.accesskit_node();
    assert_eq!(ak.numeric_value(), Some(0.62));
    assert_eq!(ak.min_numeric_value(), Some(0.0));
    assert_eq!(ak.max_numeric_value(), Some(1.0));
    assert_eq!(node.value().as_deref(), Some("0.62, default 0.50, modified"));
}

#[test]
fn a_value_at_its_default_says_so() {
    let harness = rig(Rig::new(0.5));
    let value = harness.get_by_role_and_label(Role::Slider, "Width").value();
    assert_eq!(value.as_deref(), Some("0.50, default 0.50, at default"));
}

#[test]
fn arrow_keys_nudge_by_one_step_and_shift_by_ten_and_each_press_commits() {
    let mut h = rig(Rig::new(0.62));
    focus_track(&mut h);
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert!((h.state().value - 0.63).abs() < 1e-9);
    assert_eq!(
        h.state().events.last(),
        Some(&ParamEvent::Commit { from: 0.62, to: 0.63, cause: CommitCause::Keyboard })
    );
    press(&mut h, Modifiers::SHIFT, Key::ArrowLeft);
    assert!((h.state().value - 0.53).abs() < 1e-9, "shift moves ten steps: {}", h.state().value);
    press(&mut h, Modifiers::NONE, Key::PageUp);
    assert!((h.state().value - 0.63).abs() < 1e-9, "Page keys move 10% of the range");
    press(&mut h, Modifiers::NONE, Key::Home);
    assert!(h.state().value.abs() < 1e-9);
    press(&mut h, Modifiers::NONE, Key::End);
    assert!((h.state().value - 1.0).abs() < 1e-9);
}

#[test]
fn ctrl_backspace_resets_to_the_default_in_one_commit() {
    let mut h = rig(Rig::new(0.8));
    focus_track(&mut h);
    press(&mut h, Modifiers::CTRL, Key::Backspace);
    assert!((h.state().value - 0.5).abs() < 1e-9);
    assert_eq!(h.state().events.last(), Some(&ParamEvent::Commit { from: 0.8, to: 0.5, cause: CommitCause::Reset }));
}

#[test]
fn clicking_the_track_jumps_there_as_one_commit() {
    let mut h = rig(Rig::new(0.1));
    let target = track_x(&h, 0.75);
    h.hover_at(target);
    h.run();
    h.event(egui::Event::PointerButton {
        pos: target,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    h.event(egui::Event::PointerButton {
        pos: target,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    assert!((h.state().value - 0.75).abs() < 0.02, "jumped to {}", h.state().value);
    assert!(matches!(h.state().events.last(), Some(ParamEvent::Commit { cause: CommitCause::Click, .. })));
}

#[test]
fn a_drag_is_one_open_transaction_that_previews_then_commits_once() {
    let mut h = rig(Rig::new(0.5));
    let (from, to) = (track_x(&h, 0.5), track_x(&h, 0.9));
    let path: Vec<Pos2> = (0..=8).map(|i| from + (to - from) * (i as f32 / 8.0)).collect();
    drag(&mut h, &path);
    let events = &h.state().events;
    assert!(matches!(events.first(), Some(ParamEvent::Begin(v)) if (*v - 0.5).abs() < 1e-9), "{events:?}");
    assert!(
        events.iter().filter(|e| matches!(e, ParamEvent::Preview(_))).count() >= 3,
        "previews while dragging: {events:?}"
    );
    let commits: Vec<_> = events.iter().filter(|e| matches!(e, ParamEvent::Commit { .. })).collect();
    assert_eq!(commits.len(), 1, "one drag, one transaction: {events:?}");
    assert!(matches!(commits[0], ParamEvent::Commit { cause: CommitCause::Drag, .. }));
    assert!((h.state().value - 0.9).abs() < 0.03, "ended at {}", h.state().value);
}

#[test]
fn esc_during_a_drag_restores_the_start_value_and_commits_nothing() {
    let mut h = rig(Rig::new(0.5));
    let (from, to) = (track_x(&h, 0.5), track_x(&h, 0.9));
    h.hover_at(from);
    h.run();
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    for i in 1..=4 {
        h.event(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 4.0)));
        h.run();
    }
    assert!(h.state().value > 0.7, "the drag is under way");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.event(egui::Event::PointerMoved(to));
    h.run();
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    assert!((h.state().value - 0.5).abs() < 1e-9, "restored, got {}", h.state().value);
    let events = &h.state().events;
    assert!(events.contains(&ParamEvent::Rollback(0.5)), "{events:?}");
    assert!(!events.iter().any(|e| matches!(e, ParamEvent::Commit { .. })), "{events:?}");
}

/// Clicks the number, types `text`, presses Enter.
fn type_value(h: &mut Harness<'_, Rig>, text: &str) {
    h.get_by_role_and_label(Role::Button, "Edit Width value").click();
    h.run_steps(2);
    let field = h.get_by_role_and_label(Role::TextInput, "Width value");
    field.focus();
    h.run();
    // Replace the selected text.
    h.get_by_role_and_label(Role::TextInput, "Width value").type_text(text);
    h.run();
    press(h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
}

#[test]
fn typed_entry_accepts_arithmetic() {
    let mut h = rig(Rig::new(0.5));
    type_value(&mut h, "0.5*1.2");
    assert!((h.state().value - 0.6).abs() < 1e-9, "{}", h.state().value);
    assert!(matches!(h.state().events.last(), Some(ParamEvent::Commit { cause: CommitCause::Typed, .. })));
}

#[test]
fn typed_entry_accepts_a_percent_of_the_range_and_a_unit() {
    let mut h = rig(Rig::new(0.0));
    h.state_mut().range = (-45.0, 45.0);
    h.state_mut().unit = "°";
    h.run_steps(2);
    type_value(&mut h, "30°");
    assert!((h.state().value - 30.0).abs() < 1e-9, "{}", h.state().value);
    type_value(&mut h, "75%");
    assert!((h.state().value - 22.5).abs() < 1e-9, "75% of -45..45 is 22.5, got {}", h.state().value);
}

#[test]
fn an_invalid_entry_keeps_the_field_open_and_the_value_unchanged() {
    let mut h = rig(Rig::new(0.5));
    type_value(&mut h, "abc");
    assert!((h.state().value - 0.5).abs() < 1e-9);
    assert!(
        h.query_by_role_and_label(Role::TextInput, "Width value").is_some(),
        "the field stays open for another try"
    );
    assert!(h.state().events.is_empty(), "nothing committed: {:?}", h.state().events);
}

#[test]
fn esc_while_typing_reverts_and_closes_the_field() {
    let mut h = rig(Rig::new(0.5));
    h.get_by_role_and_label(Role::Button, "Edit Width value").click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Width value").type_text("0.9");
    h.run();
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(2);
    assert!((h.state().value - 0.5).abs() < 1e-9);
    assert!(h.query_by_role_and_label(Role::TextInput, "Width value").is_none());
}

#[test]
fn the_reset_button_returns_to_the_default_and_hides_at_the_default() {
    let mut h = rig(Rig::new(0.9));
    h.get_by_role_and_label(Role::Button, "Reset Width").click();
    h.run_steps(2);
    assert!((h.state().value - 0.5).abs() < 1e-9);
    assert!(h.query_by_role_and_label(Role::Button, "Reset Width").is_none(), "no reset button at the default");
}

#[test]
fn a_mixed_selection_says_so_and_dragging_sets_them_all() {
    let mut h = rig(Rig::new(0.5));
    h.state_mut().mixed = true;
    h.run_steps(2);
    assert_eq!(h.get_by_role_and_label(Role::Slider, "Width").value().as_deref(), Some("mixed values"));
    let (from, to) = (track_x(&h, 0.5), track_x(&h, 0.8));
    let path: Vec<Pos2> = (0..=4).map(|i| from + (to - from) * (i as f32 / 4.0)).collect();
    drag(&mut h, &path);
    assert!((h.state().value - 0.8).abs() < 0.03, "{}", h.state().value);
}

#[test]
fn a_locked_slider_ignores_the_keyboard_and_names_its_owner() {
    let mut h = rig(Rig::new(0.5));
    h.state_mut().locked = true;
    h.run_steps(2);
    let node = h.get_by_role_and_label(Role::Slider, "Width");
    assert!(node.value().is_some_and(|v| v.contains("locked by Designer")));
    node.focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert!((h.state().value - 0.5).abs() < 1e-9);
}

#[test]
fn the_row_needs_no_more_than_the_spec_minimum_width() {
    // shell.md section 4: an inspector row needs 288 pt (8 padding each side).
    let h = harness_state(Settings::default(), vec2(304.0, 60.0), Rig::new(0.62), |ui, rig| {
        ParamSlider::new("Width", &mut rig.value, 0.0..=1.0).default_value(0.5).show(ui);
    });
    let slider = h.get_by_role_and_label(Role::Slider, "Width").rect();
    let field = h.get_by_role_and_label(Role::Button, "Edit Width value").rect();
    assert!(slider.width() >= 79.0, "the track keeps its 80 pt minimum, got {}", slider.width());
    assert!(field.left() >= slider.right(), "the number field never overlaps the track");
}
