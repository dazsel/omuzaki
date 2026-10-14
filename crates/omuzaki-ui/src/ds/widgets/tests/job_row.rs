//! Job rows: Cancel while a job runs, Retry when it failed, and a bar that never animates.

use egui_kittest::Harness;

use super::*;
use crate::ds::testing::unlabelled_interactive;

struct Rig {
    state: JobState<'static>,
    cancels: u32,
    retries: u32,
}

fn rig(state: JobState<'static>) -> Harness<'static, Rig> {
    harness_state(Settings::default(), vec2(360.0, 200.0), Rig { state, cancels: 0, retries: 0 }, |ui, r| {
        let out = JobRow::new("Render 24 sheets", r.state).cost(&["~40 s", "1.2 GB"]).elapsed("0:17").show(ui);
        r.cancels += u32::from(out.cancel);
        r.retries += u32::from(out.retry);
    })
}

#[test]
fn a_running_job_is_a_progress_indicator_with_its_fraction_cost_and_elapsed_time_as_its_value() {
    let h = rig(JobState::Running { progress: Some(0.4) });
    let node = h.get_by_role_and_label(Role::ProgressIndicator, "Render 24 sheets");
    let ak = node.accesskit_node();
    assert!((ak.numeric_value().unwrap_or_default() - 0.4).abs() < 1e-6);
    assert_eq!((ak.min_numeric_value(), ak.max_numeric_value()), (Some(0.0), Some(1.0)));
    assert_eq!(node.value().as_deref(), Some("Working, 40%, ~40 s, 1.2 GB, 0:17 elapsed"));
}

#[test]
fn a_job_with_no_known_fraction_has_no_value_and_asks_for_no_frames() {
    let mut h = rig(JobState::Running { progress: None });
    let node = h.get_by_role_and_label(Role::ProgressIndicator, "Render 24 sheets");
    assert_eq!(node.accesskit_node().numeric_value(), None, "indeterminate: no fraction is claimed");
    assert_eq!(node.value().as_deref(), Some("Working, ~40 s, 1.2 GB, 0:17 elapsed"));
    h.run_steps(20);
    assert!(
        !h.ctx.has_requested_repaint(),
        "the hatched bar is still: nothing loops (WCAG 2.2.2) and an idle list costs nothing"
    );
}

#[test]
fn cancel_names_the_job_stops_it_and_is_reached_by_tab() {
    let mut h = rig(JobState::Running { progress: Some(0.4) });
    let cancel = h.get_by_role_and_label(Role::Button, "Cancel Render 24 sheets");
    assert!(cancel.rect().width() >= 24.0 && cancel.rect().height() >= 24.0);
    h.get_by_role_and_label(Role::ProgressIndicator, "Render 24 sheets");
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(h.get_by_role_and_label(Role::Button, "Cancel Render 24 sheets").is_focused());
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().cancels, 1);
    assert_eq!(h.state().retries, 0);
}

#[test]
fn a_failed_job_shows_its_error_and_retry_instead_of_cancel() {
    let mut h = harness_state(Settings::default(), vec2(360.0, 200.0), (0u32, 0u32), |ui, n| {
        let out =
            JobRow::new("Render 24 sheets", JobState::Failed { error: "GPU out of memory. Lower the sheet size." })
                .show(ui);
        n.0 += u32::from(out.cancel);
        n.1 += u32::from(out.retry);
    });
    assert!(h.query_by_role_and_label(Role::Button, "Cancel Render 24 sheets").is_none());
    h.get_by_role_and_label(Role::Button, "Retry Render 24 sheets").click();
    h.run();
    assert_eq!(*h.state(), (0, 1));
    let value = h.get_by_role(Role::ProgressIndicator).value();
    assert_eq!(
        value.as_deref(),
        Some("Failed, GPU out of memory. Lower the sheet size."),
        "the error is in the node's text"
    );
    assert!(
        crate::ds::audit::painted(h.output()).iter().any(|p| p.text == "Failed"),
        "and the word is painted, not only the colour"
    );
}

#[test]
fn a_finished_job_has_no_buttons_and_every_state_names_everything_it_offers() {
    for state in [JobState::Queued, JobState::Running { progress: Some(1.0) }, JobState::Done] {
        let h = rig(state);
        assert_eq!(unlabelled_interactive(&h), Vec::<String>::new(), "{state:?}");
        let buttons = h.query_all_by_role(Role::Button).count();
        assert_eq!(buttons, usize::from(state != JobState::Done), "{state:?}");
    }
}

#[test]
fn a_long_title_and_error_end_in_an_ellipsis_and_the_row_keeps_its_height() {
    use crate::ds::audit::painted;
    let height = |title: &'static str| {
        let h = harness_state(Settings::default(), vec2(300.0, 200.0), (), move |ui, ()| {
            JobRow::new(
                title,
                JobState::Failed {
                    error: "A very long error message that cannot possibly fit on a single line of this row",
                },
            )
            .show(ui);
        });
        let shown = painted(h.output());
        assert!(shown.iter().any(|p| p.elided), "something was cut: {shown:?}");
        h.get_by_role(Role::ProgressIndicator).rect().height()
    };
    assert!(
        (height("Short") - height("A job title that is far longer than the row is wide, in any language")).abs() < 0.5
    );
}

fn enter(pressed: bool, repeat: bool) -> egui::Event {
    egui::Event::Key { key: Key::Enter, physical_key: Some(Key::Enter), pressed, repeat, modifiers: Modifiers::NONE }
}

#[test]
fn a_held_enter_on_retry_retries_once_and_never_cancels_the_job_it_turned_into() {
    // The host turns a failed job into a running one when Retry is pressed; Retry and Cancel sit in the same place.
    let mut h =
        harness_state(Settings::default(), vec2(360.0, 200.0), (true, 0u32, 0u32), |ui, (failed, retries, cancels)| {
            let state = if *failed {
                JobState::Failed { error: "GPU out of memory" }
            } else {
                JobState::Running { progress: Some(0.1) }
            };
            let out = JobRow::new("Render 24 sheets", state).id_salt("job").show(ui);
            *retries += u32::from(out.retry);
            *cancels += u32::from(out.cancel);
            if out.retry {
                *failed = false;
            }
        });
    h.get_by_role_and_label(Role::Button, "Retry Render 24 sheets").focus();
    h.run();
    h.event(enter(true, false));
    h.run_steps(2);
    for _ in 0..4 {
        h.event(enter(true, true));
        h.run();
    }
    assert_eq!((h.state().1, h.state().2), (1, 0), "one retry, and the repeats cancel nothing");
}

#[test]
fn a_title_cut_with_an_ellipsis_shows_the_whole_title_in_a_tooltip() {
    use crate::ds::audit::painted;
    let long = "Render the twenty-four contact sheets for the whole expression strip";
    let mut h = harness_state(Settings::default(), vec2(300.0, 200.0), (), move |ui, ()| {
        JobRow::new(long, JobState::Queued).id_salt("job").show(ui);
    });
    assert!(painted(h.output()).iter().any(|p| p.text == long && p.elided));
    h.hover_at(egui::pos2(60.0, 32.0));
    h.run_steps(10);
    assert!(painted(h.output()).iter().any(|p| p.text == long && !p.elided), "hovering the title shows it whole");
}
