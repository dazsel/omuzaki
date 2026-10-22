//! The slider's clipboard and op extras: Paste value, Copy op call, Show in History, and their keys.

use egui::{Event, OutputCommand, ViewportCommand};
use egui_kittest::Harness;

use super::*;

struct Rig {
    value: f64,
    events: Vec<ParamEvent>,
    with_op: bool,
    /// Every text the slider asked the platform to copy, over all frames: the harness runs a frame per queued event,
    /// so the last frame's output alone would miss a copy made by a key.
    copied: Vec<String>,
}

fn rig(with_op: bool) -> Harness<'static, Rig> {
    harness_state(
        Settings::default(),
        vec2(360.0, 300.0),
        Rig { value: 0.62, events: Vec::new(), with_op, copied: Vec::new() },
        |ui, r| {
            let mut slider = ParamSlider::new("Width", &mut r.value, 0.0..=1.0).default_value(0.5);
            if r.with_op {
                slider =
                    slider.op_call(OpCall { op: "part.set_param", target: "\"part\":\"hair\",\"param\":\"width\"" });
            }
            r.events.extend(slider.show(ui).events);
            ui.ctx().output(|o| {
                for command in &o.commands {
                    match command {
                        OutputCommand::CopyText(text) => r.copied.push(text.clone()),
                        OutputCommand::OpenUrl(_) | OutputCommand::CopyImage(_) => {}
                    }
                }
            });
        },
    )
}

fn focus_track(h: &mut Harness<'_, Rig>) {
    h.get_by_role_and_label(Role::Slider, "Width").focus();
    h.run();
}

fn open_menu(h: &mut Harness<'_, Rig>) {
    h.get_by_role_and_label(Role::Slider, "Width").click_secondary();
    h.run_steps(3);
}

#[test]
fn the_menu_lists_reset_copy_paste_op_call_and_history_in_the_order_the_spec_gives() {
    let mut h = rig(true);
    open_menu(&mut h);
    let labels: Vec<String> = h
        .get_all_by_role(Role::Button)
        .filter_map(|b| b.accesskit_node().label())
        .filter(|l| {
            ["Reset to default", "Copy value", "Paste value", "Copy op call", "Show in History"].contains(&l.as_str())
        })
        .collect();
    assert_eq!(labels, ["Reset to default", "Copy value", "Paste value", "Copy op call", "Show in History"]);
}

#[test]
fn copy_op_call_is_offered_only_when_the_host_says_which_op_sets_the_value() {
    let mut h = rig(false);
    open_menu(&mut h);
    assert!(h.query_by_role_and_label(Role::Button, "Copy op call").is_none());
    h.get_by_role_and_label(Role::Button, "Paste value");
}

#[test]
fn copy_op_call_copies_an_mcp_call_with_the_value_as_it_is_now() {
    let mut h = rig(true);
    open_menu(&mut h);
    h.get_by_role_and_label(Role::Button, "Copy op call").click();
    h.run();
    assert_eq!(
        h.state().copied.first().map(String::as_str),
        Some(r#"{"name":"part_set_param","arguments":{"part":"hair","param":"width","value":0.62}}"#)
    );
}

#[test]
fn show_in_history_is_an_event_the_host_acts_on() {
    let mut h = rig(false);
    open_menu(&mut h);
    h.get_by_role_and_label(Role::Button, "Show in History").click();
    h.run_steps(2);
    assert!(h.state().events.contains(&ParamEvent::ShowInHistory), "{:?}", h.state().events);
    assert!((h.state().value - 0.62).abs() < 1e-9, "and it changes nothing");
}

#[test]
fn paste_value_asks_the_platform_for_the_clipboard_and_takes_its_answer_as_one_commit() {
    let mut h = rig(false);
    open_menu(&mut h);
    h.get_by_role_and_label(Role::Button, "Paste value").click();
    // The click is the last event of the step, so the frame that handled it is the last frame.
    h.step();
    let asked = h.output().viewport_output.values().any(|v| v.commands.contains(&ViewportCommand::RequestPaste));
    assert!(asked, "the menu sends RequestPaste");
    h.event(Event::Paste("0.8".to_owned()));
    h.run();
    assert!((h.state().value - 0.8).abs() < 1e-9, "{}", h.state().value);
    assert!(matches!(
        h.state().events.last(),
        Some(ParamEvent::Commit { from, to, cause: CommitCause::Paste }) if (*from - 0.62).abs() < 1e-9 && (*to - 0.8).abs() < 1e-9
    ));
}

#[test]
fn a_paste_nobody_asked_for_is_not_taken_and_neither_is_text_that_is_not_a_number() {
    let mut h = rig(false);
    h.event(Event::Paste("0.9".to_owned()));
    h.run();
    assert!((h.state().value - 0.62).abs() < 1e-9, "an unfocused slider ignores a stray paste");
    focus_track(&mut h);
    h.event(Event::Paste("not a number".to_owned()));
    h.run();
    assert!((h.state().value - 0.62).abs() < 1e-9, "the clipboard can hold anything");
    h.event(Event::Paste(" 30% ".to_owned()));
    h.run();
    assert!((h.state().value - 0.3).abs() < 1e-9, "a percent of the range, read like typed text: {}", h.state().value);
}

#[test]
fn a_menu_paste_that_never_gets_an_answer_does_not_take_a_much_later_paste() {
    let mut h = rig(false);
    open_menu(&mut h);
    h.get_by_role_and_label(Role::Button, "Paste value").click();
    h.run_steps(30);
    h.event(Event::Paste("0.9".to_owned()));
    h.run();
    assert!((h.state().value - 0.62).abs() < 1e-9);
}

#[test]
fn ctrl_v_pastes_and_ctrl_c_and_ctrl_shift_c_copy_on_a_focused_track() {
    let mut h = rig(true);
    focus_track(&mut h);
    h.event(Event::Copy);
    h.run();
    assert_eq!(h.state().copied, ["0.62"], "Ctrl+C copies the value as shown");
    press(&mut h, Modifiers::CTRL | Modifiers::SHIFT, Key::C);
    let op = h.state().copied.last().cloned().unwrap_or_default();
    assert!(op.contains("\"name\":\"part_set_param\""), "Ctrl+Shift+C copies the op call: {op}");
    h.event(Event::Paste("0.25".to_owned()));
    h.run();
    assert!((h.state().value - 0.25).abs() < 1e-9);
}
