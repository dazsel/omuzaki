//! Toast lifetime: measured on the context's clock, never by adding up frame times.

use egui::{Pos2, pos2};

use super::*;

#[test]
fn a_four_second_toast_is_gone_after_four_seconds_of_clock_with_only_two_repaints() {
    // A reactive app repaints when something asks it to: here, once when the toast is pushed and once at its deadline.
    // Each frame reports a 1/60 s step (what egui says after a sleep); adding those up made a 4 s toast last minutes.
    let (ctx, clock) = toast_context();
    let mut toasts = Toasts::default();
    toasts.push(&ctx, Toast::new(Status::Ok, "Designer changed the face profile"));
    frame(&ctx, &mut toasts, None);
    assert_eq!(toasts.visible(), 1, "it is on screen after the first repaint");
    advance(&clock, 3.9);
    frame(&ctx, &mut toasts, None);
    assert_eq!(toasts.visible(), 1, "and still there at 3.9 s");
    advance(&clock, 0.2);
    frame(&ctx, &mut toasts, None);
    assert_eq!(toasts.visible(), 0, "gone after 4.1 s of clock, three repaints in all");

    let mut two = Toasts::default();
    two.push(&ctx, Toast::new(Status::Info, "Autosaved 14:32"));
    frame(&ctx, &mut two, None);
    advance(&clock, 4.0);
    frame(&ctx, &mut two, None);
    assert_eq!(two.visible(), 0, "with only two repaints: one at the push, one four seconds later");
}

#[test]
fn warnings_take_eight_seconds_and_errors_stay() {
    let (ctx, clock) = toast_context();
    let mut toasts = Toasts::default();
    toasts.push(&ctx, Toast::new(Status::Warn, "Tracking confidence is low"));
    toasts.push(&ctx, Toast::new(Status::Danger, "Render failed"));
    frame(&ctx, &mut toasts, None);
    advance(&clock, 7.9);
    frame(&ctx, &mut toasts, None);
    assert_eq!(toasts.visible(), 2);
    advance(&clock, 0.2);
    frame(&ctx, &mut toasts, None);
    assert_eq!(toasts.visible(), 1, "the warning left at 8 s");
    advance(&clock, 3600.0);
    frame(&ctx, &mut toasts, None);
    assert_eq!(toasts.visible(), 1, "an error stays however long you are away");
}

/// A point inside the stack: it sits at the bottom right of an 800 x 600 screen.
const OVER_THE_STACK: Pos2 = pos2(700.0, 545.0);
const AWAY: Pos2 = pos2(100.0, 100.0);

#[test]
fn the_timer_pauses_while_the_pointer_is_over_a_toast_and_the_time_is_given_back() {
    let (ctx, clock) = toast_context();
    let mut toasts = Toasts::default();
    toasts.push(&ctx, Toast::new(Status::Ok, "Designer changed the face profile"));
    for _ in 0..3 {
        advance(&clock, 0.1);
        frame(&ctx, &mut toasts, None);
    }
    // The pointer arrives, and stays through a long gap between repaints.
    advance(&clock, 0.1);
    frame(&ctx, &mut toasts, Some(OVER_THE_STACK));
    advance(&clock, 30.0);
    frame(&ctx, &mut toasts, Some(OVER_THE_STACK));
    assert_eq!(toasts.visible(), 1, "30 s under the pointer cost the toast nothing");
    // The pointer leaves; the 30 s it was held are not counted either.
    advance(&clock, 1.0);
    frame(&ctx, &mut toasts, Some(AWAY));
    assert_eq!(toasts.visible(), 1, "it has its remaining time again");
    advance(&clock, 3.3);
    frame(&ctx, &mut toasts, Some(AWAY));
    assert_eq!(toasts.visible(), 1, "about 4 s are used up now: 0.3 before the pointer came, 4.3 after is too much");
    advance(&clock, 1.0);
    frame(&ctx, &mut toasts, Some(AWAY));
    assert_eq!(toasts.visible(), 0);
}

#[test]
fn a_repeated_toast_starts_its_four_seconds_again() {
    let (ctx, clock) = toast_context();
    let mut toasts = Toasts::default();
    let toast = Toast::new(Status::Ok, "Designer changed the face profile");
    toasts.push(&ctx, toast.clone());
    frame(&ctx, &mut toasts, None);
    advance(&clock, 3.0);
    toasts.push(&ctx, toast);
    frame(&ctx, &mut toasts, None);
    advance(&clock, 3.0);
    frame(&ctx, &mut toasts, None);
    assert_eq!(toasts.visible(), 1, "6 s after the first push, 3 s after the second");
    assert_eq!(toasts.log().last().map(|entry| entry.time), Some(3.0), "the log is stamped with the clock");
}

#[test]
fn an_idle_stack_asks_to_be_woken_at_its_deadline_not_every_frame() {
    let (ctx, clock) = toast_context();
    let mut toasts = Toasts::default();
    toasts.push(&ctx, Toast::new(Status::Ok, "Autosaved 14:32"));
    // Past the fade-in (egui's own animations run on its frame time, 1/60 s a frame here), nothing moves, so the next
    // wake-up is the deadline.
    for _ in 0..30 {
        advance(&clock, 0.02);
        frame(&ctx, &mut toasts, None);
    }
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
        ..egui::RawInput::default()
    };
    let output = ctx.run_ui(input, |ui| {
        toasts.show(ui.ctx(), vec2(16.0, 40.0));
    });
    let wake = output.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay.as_secs_f64());
    output.drop_without_applying_deltas();
    assert!(wake.is_some_and(|w| (3.0..=3.5).contains(&w)), "woken at the deadline, about 3.4 s away: {wake:?}");
}
