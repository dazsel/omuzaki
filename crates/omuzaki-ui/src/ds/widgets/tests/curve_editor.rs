//! The curve editor: every edit the pointer makes has a keyboard and a screen-reader path, and every edit is one event.

use egui::accesskit::{Action, ActionData, ActionRequest};
use egui::{Event, PointerButton, Pos2};
use egui_kittest::Harness;

use super::*;
use crate::ds::testing::{drag, unlabelled_interactive};
use crate::ds::widgets::CurveCause;

struct Rig {
    curve: Curve,
    events: Vec<CurveEvent>,
    live: Option<f32>,
    disabled: bool,
}

fn rig(curve: Curve) -> Harness<'static, Rig> {
    harness_state(
        Settings::default(),
        vec2(420.0, 520.0),
        Rig { curve, events: Vec::new(), live: None, disabled: false },
        |ui, r| {
            ui.add(Button::new("Before"));
            let mut editor = CurveEditor::new("Jaw response", &mut r.curve).live_input(r.live).id_salt("jaw");
            if r.disabled {
                editor = editor.disabled("Held by Designer");
            }
            r.events.extend(editor.show(ui).event);
        },
    )
}

/// A four-point curve with room to move: (0,0), (0.3,0.2), (0.7,0.8), (1,1).
fn s_curve() -> Curve {
    Curve::unit(Preset::SCurve)
}

fn point<'h>(h: &'h Harness<'_, Rig>, n: usize, of: usize) -> egui_kittest::Node<'h> {
    let wanted = format!("Jaw response, point {n} of {of}");
    h.get_all_by_role(Role::Slider)
        .find(|node| node.accesskit_node().label().as_deref() == Some(wanted.as_str()))
        .unwrap_or_else(|| panic!("no node named '{wanted}'"))
}

fn plot(h: &Harness<'_, Rig>) -> egui::Rect {
    h.get_by_role(Role::Group).rect()
}

fn commits(h: &Harness<'_, Rig>) -> Vec<CurveCause> {
    h.state()
        .events
        .iter()
        .filter_map(|e| if let CurveEvent::Commit { cause, .. } = e { Some(*cause) } else { None })
        .collect()
}

fn focused(h: &Harness<'_, Rig>) -> String {
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().is_focused())
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .unwrap_or_default()
}

#[test]
fn the_plot_is_a_group_with_a_text_summary_whose_children_are_the_points() {
    let h = rig(s_curve());
    let group = h.get_by_role(Role::Group);
    assert_eq!(
        group.accesskit_node().label().as_deref(),
        Some("Jaw response: curve with 4 points. Input 0.50 gives 0.50.")
    );
    for n in 1..=4 {
        let p = point(&h, n, 4);
        assert!(inside(p.accesskit_node(), Role::Group), "point {n} is inside the plot's group");
        assert!(p.rect().width() >= 24.0 && p.rect().height() >= 24.0, "a 24 pt target: {:?}", p.rect());
    }
    assert_eq!(point(&h, 2, 4).value().as_deref(), Some("input 0.30, output 0.20"));
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
}

#[test]
fn a_live_input_moves_the_summary_and_asks_for_no_frames_itself() {
    let mut h = rig(Curve::linear());
    h.state_mut().live = Some(0.25);
    h.run();
    let label = h.get_by_role(Role::Group).accesskit_node().label().unwrap_or_default();
    assert_eq!(label, "Jaw response: curve with 2 points. Input 0.25 gives 0.25.");
    h.run_steps(20);
    assert!(!h.ctx.has_requested_repaint(), "the host repaints as the signal moves; the editor never asks");
}

#[test]
fn tab_goes_preset_then_the_points_left_to_right_then_the_three_numbers() {
    let mut h = rig(s_curve());
    h.get_by_role_and_label(Role::Button, "Before").focus();
    h.run();
    let mut seen = Vec::new();
    for _ in 0..8 {
        press(&mut h, Modifiers::NONE, Key::Tab);
        seen.push(focused(&h));
    }
    assert_eq!(
        seen,
        [
            "Preset",
            "Jaw response, point 1 of 4",
            "Jaw response, point 2 of 4",
            "Jaw response, point 3 of 4",
            "Jaw response, point 4 of 4",
            "Input",
            "Output",
            "Tangent",
        ]
    );
}

#[test]
fn arrows_nudge_a_point_by_one_percent_and_shift_by_ten_and_each_nudge_is_one_commit_from_the_old_curve() {
    let mut h = rig(s_curve());
    point(&h, 2, 4).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    assert!((h.state().curve.points()[1].y - 0.21).abs() < 1e-5, "{:?}", h.state().curve.points()[1]);
    press(&mut h, Modifiers::SHIFT, Key::ArrowRight);
    assert!((h.state().curve.points()[1].x - 0.4).abs() < 1e-5);
    assert_eq!(commits(&h), [CurveCause::Keyboard, CurveCause::Keyboard]);
    if let Some(CurveEvent::Commit { from, .. }) = h.state().events.first() {
        assert!((from.points()[1].y - 0.2).abs() < 1e-5, "the commit carries the curve before the gesture");
    } else {
        panic!("no commit");
    }
    // The first point's x is pinned to the edge: the left and right arrows leave it, up and down move it.
    point(&h, 1, 4).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert!(h.state().curve.points()[0].x.abs() < f32::EPSILON);
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    assert!((h.state().curve.points()[0].y - 0.01).abs() < 1e-5);
}

#[test]
fn insert_adds_a_point_after_the_focused_one_delete_removes_it_and_the_ends_stay() {
    let mut h = rig(s_curve());
    point(&h, 2, 4).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Insert);
    h.run_steps(2);
    assert_eq!(h.state().curve.points().len(), 5);
    assert!((h.state().curve.points()[2].x - 0.5).abs() < 1e-5, "halfway to the next point, on the curve");
    assert!(point(&h, 3, 5).is_focused(), "the new point has the focus");
    press(&mut h, Modifiers::NONE, Key::Delete);
    h.run_steps(2);
    assert_eq!(h.state().curve.points().len(), 4);
    assert!(point(&h, 2, 4).is_focused(), "focus goes to the point before");
    assert_eq!(commits(&h), [CurveCause::Added, CurveCause::Removed]);
    point(&h, 1, 4).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Delete);
    assert_eq!(h.state().curve.points().len(), 4, "the first point cannot be removed");
}

#[test]
fn enter_on_a_point_types_its_output_in_the_readout_and_the_number_commits_as_a_readout_edit() {
    let mut h = rig(s_curve());
    point(&h, 2, 4).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Output value").type_text("0.35");
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert!((h.state().curve.points()[1].y - 0.35).abs() < 1e-5, "{:?}", h.state().curve.points());
    assert_eq!(commits(&h), [CurveCause::Readout]);
    assert_eq!(h.get_by_role_and_label(Role::SpinButton, "Output").value().as_deref(), Some("0.350"));
}

#[test]
fn the_tangent_field_overrides_the_slope_and_ctrl_backspace_returns_it_to_automatic() {
    let mut h = rig(s_curve());
    point(&h, 2, 4).focus();
    h.run();
    h.get_by_role_and_label(Role::SpinButton, "Tangent").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Tangent value").type_text("0");
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().curve.points()[1].tangent, Some(0.0));
    h.get_by_role_and_label(Role::SpinButton, "Tangent").focus();
    h.run();
    press(&mut h, Modifiers::CTRL, Key::Backspace);
    assert_eq!(h.state().curve.points()[1].tangent, None, "automatic again");
    assert_eq!(commits(&h), [CurveCause::Readout, CurveCause::Readout]);
}

#[test]
fn the_input_of_an_end_point_is_pinned_and_says_why() {
    let h = rig(s_curve());
    let input = h.get_by_role_and_label(Role::SpinButton, "Input");
    assert!(input.accesskit_node().is_disabled(), "the first point is selected");
    assert_eq!(
        input.accesskit_node().description().as_deref(),
        Some("The first and last points stay at the ends of the axis")
    );
}

fn centre(h: &Harness<'_, Rig>, n: usize, of: usize) -> Pos2 {
    point(h, n, of).rect().center()
}

#[test]
fn dragging_a_point_previews_while_it_moves_and_commits_once_from_the_curve_before() {
    let mut h = rig(s_curve());
    let from = centre(&h, 2, 4);
    let to = from + vec2(20.0, -30.0);
    drag(&mut h, &[from, from + vec2(8.0, -10.0), from + vec2(14.0, -20.0), to]);
    let events = &h.state().events;
    assert!(events.contains(&CurveEvent::Preview), "{events:?}");
    assert_eq!(commits(&h), [CurveCause::Drag]);
    let p = h.state().curve.points()[1];
    assert!(p.x > 0.3 && p.y > 0.2, "the point moved up and right: {p:?}");
    if let Some(CurveEvent::Commit { from, .. }) = events.last() {
        assert!((from.points()[1].x - 0.3).abs() < 1e-5, "from is the curve before the drag");
    } else {
        panic!("the drag ends in a commit: {events:?}");
    }
}

#[test]
fn esc_during_a_drag_puts_the_point_back_and_nothing_commits() {
    let mut h = rig(s_curve());
    let from = centre(&h, 2, 4);
    h.hover_at(from);
    h.run();
    h.event(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    for step in 1..=3 {
        h.event(Event::PointerMoved(from + vec2(6.0 * step as f32, -8.0 * step as f32)));
        h.run();
    }
    assert!(h.state().curve.points()[1].y > 0.25, "the drag is under way");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.event(Event::PointerMoved(from + vec2(40.0, -40.0)));
    h.run();
    h.event(Event::PointerButton {
        pos: from,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    assert_eq!(h.state().curve, s_curve(), "back where it started");
    assert!(h.state().events.contains(&CurveEvent::Rollback));
    assert_eq!(commits(&h), Vec::<CurveCause>::new());
}

#[test]
fn dragging_a_point_far_off_the_plot_removes_it_and_a_double_click_adds_one() {
    let mut h = rig(s_curve());
    let from = centre(&h, 2, 4);
    drag(&mut h, &[from, from + vec2(0.0, 60.0), from + vec2(0.0, 240.0)]);
    assert_eq!(h.state().curve.points().len(), 3);
    assert_eq!(commits(&h), [CurveCause::Removed]);
    let area = plot(&h);
    let at = pos2(area.left() + area.width() * 0.5, area.center().y);
    h.hover_at(at);
    h.run();
    let click =
        |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    h.input_mut().events.extend([click(true), click(false), click(true), click(false)]);
    h.step();
    h.run();
    assert_eq!(h.state().curve.points().len(), 4, "a double-click on the plot adds a point");
    assert_eq!(commits(&h).last(), Some(&CurveCause::Added));
}

#[test]
fn a_screen_reader_steps_and_sets_a_points_output_whether_or_not_it_has_focus() {
    let mut h = rig(s_curve());
    let (target_node, target_tree) = point(&h, 2, 4).accesskit_node().locate();
    let send = |h: &mut Harness<'_, Rig>, action, data| {
        h.event(Event::AccessKitActionRequest(ActionRequest { action, target_tree, target_node, data }));
        h.run();
    };
    send(&mut h, Action::Increment, None);
    assert!((h.state().curve.points()[1].y - 0.21).abs() < 1e-5);
    send(&mut h, Action::SetValue, Some(ActionData::NumericValue(0.5)));
    assert!((h.state().curve.points()[1].y - 0.5).abs() < 1e-5);
    assert_eq!(commits(&h), [CurveCause::Keyboard, CurveCause::Keyboard]);
}

#[test]
fn a_preset_replaces_the_curve_in_one_commit_and_an_edit_makes_it_custom_again() {
    let mut h = rig(s_curve());
    assert_eq!(h.get_by_role_and_label(Role::ComboBox, "Preset").value().as_deref(), Some("S-curve"));
    let (target_node, target_tree) = h.get_by_role_and_label(Role::ComboBox, "Preset").accesskit_node().locate();
    h.event(Event::AccessKitActionRequest(ActionRequest {
        action: Action::SetValue,
        target_tree,
        target_node,
        data: Some(ActionData::Value("Inverted".into())),
    }));
    h.run_steps(3);
    assert_eq!(h.state().curve.preset(), Some(Preset::Inverted));
    assert_eq!(commits(&h), [CurveCause::Preset]);
    point(&h, 1, 2).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    h.run_steps(2);
    assert_eq!(h.state().curve.preset(), None);
    assert_eq!(
        h.get_by_role_and_label(Role::ComboBox, "Preset").value().as_deref(),
        Some(""),
        "Custom is shown as the placeholder"
    );
}

#[test]
fn a_disabled_editor_ignores_every_edit_and_says_why() {
    let mut h = rig(s_curve());
    h.state_mut().disabled = true;
    h.run();
    let node = point(&h, 2, 4);
    assert!(node.accesskit_node().is_disabled());
    node.focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowUp);
    press(&mut h, Modifiers::NONE, Key::Delete);
    assert_eq!(h.state().curve, s_curve());
    assert_eq!(
        h.get_by_role_and_label(Role::SpinButton, "Output").accesskit_node().description().as_deref(),
        Some("Held by Designer")
    );
}

fn held(h: &mut Harness<'_, Rig>, key: Key) {
    let event = |repeat| Event::Key { key, physical_key: Some(key), pressed: true, repeat, modifiers: Modifiers::NONE };
    h.event(event(false));
    h.run();
    for _ in 0..4 {
        h.event(event(true));
        h.run();
    }
}

#[test]
fn a_held_insert_adds_one_point_and_a_held_delete_removes_one() {
    let mut h = rig(s_curve());
    point(&h, 2, 4).focus();
    h.run();
    held(&mut h, Key::Insert);
    assert_eq!(h.state().curve.points().len(), 5, "one point, not one per repeat");
    held(&mut h, Key::Delete);
    assert_eq!(h.state().curve.points().len(), 4, "one point removed, not one per repeat");
}

#[test]
fn the_axis_labels_do_not_overlap_each_other_or_the_first_handle() {
    use crate::ds::audit::painted;
    let signed = Curve::new(
        -1.0..=1.0,
        -1.0..=1.0,
        vec![crate::ds::widgets::CurvePoint::new(-1.0, -1.0), crate::ds::widgets::CurvePoint::new(1.0, 1.0)],
    )
    .unwrap_or_else(|_| Curve::linear());
    for curve in [s_curve(), signed] {
        let h = rig(curve);
        let labels: Vec<_> = painted(h.output()).into_iter().filter(|p| p.text.parse::<f32>().is_ok()).collect();
        assert!(labels.len() >= 6, "{labels:?}");
        for (i, a) in labels.iter().enumerate() {
            for b in &labels[i + 1..] {
                assert!(!a.rect.intersects(b.rect), "{:?} {:?} overlaps {:?} {:?}", a.text, a.rect, b.text, b.rect);
            }
        }
        let handle = point(&h, 1, h.state().curve.points().len()).rect();
        let handle = egui::Rect::from_center_size(handle.center(), vec2(10.0, 10.0));
        assert!(labels.iter().all(|l| !l.rect.intersects(handle)), "a label sits on the first handle");
    }
}

#[test]
fn enter_held_on_a_point_opens_the_readout_and_a_repeat_three_frames_later_does_not_end_it() {
    let mut h = rig(s_curve());
    point(&h, 2, 4).focus();
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
    assert_eq!(commits(&h), Vec::<CurveCause>::new());
}
