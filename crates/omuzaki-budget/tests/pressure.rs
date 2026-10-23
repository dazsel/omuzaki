//! Output pressure (#877, record.md D2): the highest encoder load steps quality down and back up like frame time does,
//! and holds frame time's own step up back while the encoders are busy.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_budget::scheduler::{DOWN_SAMPLES, FLAP_WINDOW, OUTPUT_LOAD_LIMIT, UP_SAMPLES};
use omuzaki_budget::{Change, Direction, Scheduler};
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{FrameReport, GpuTimings, Pass, PassTimes, Quality, budget};

// The high preset: every rung of its ladder changes something, so one step is one rung.
const BUDGET: Seconds = budget::HIGH_GPU;

fn high() -> Scheduler {
    Scheduler::new(Quality::high(), BUDGET)
}

fn report(frame: u64, ms: f64) -> FrameReport {
    let mut passes = PassTimes::default();
    passes.set(Pass::Opaque, Seconds(ms / 1000.0));
    FrameReport {
        frame,
        render_size: UVec2::new(1920, 1080),
        cpu: PassTimes::default(),
        gpu: Some(GpuTimings { frame, passes, painters: None }),
        draw_calls: 0,
        triangles: 0,
        particles: 0,
    }
}

/// Feeds `n` samples of `load`, returns the changes.
fn loads(s: &mut Scheduler, n: u32, load: f32) -> Vec<Change> {
    (0..n).filter_map(|_| s.observe_output(load)).collect()
}

/// Feeds `n` frames of `ms` from `*frame` on, returns the changes.
fn frames(s: &mut Scheduler, frame: &mut u64, n: u32, ms: f64) -> Vec<Change> {
    (0..n)
        .filter_map(|_| {
            *frame += 1;
            s.observe(&report(*frame, ms))
        })
        .collect()
}

#[test]
fn a_sustained_high_load_steps_down_one_rung() {
    let mut s = high();
    assert_eq!(loads(&mut s, DOWN_SAMPLES - 1, 0.95), [], "11 samples are not sustained");
    let changes = loads(&mut s, 1, 0.95);
    let [c] = changes[..] else { panic!("one change, got {changes:?}") };
    assert_eq!(c.direction, Direction::Down);
    assert_eq!(s.rungs(), 1);
    assert_eq!(c.measured, BUDGET, "no frame time yet: the change reports the budget, not the load");
    assert_eq!(s.output_load(), Some(0.95));
}

#[test]
fn a_change_reports_the_smoothed_frame_time_when_there_is_one() {
    let mut s = high();
    let mut f = 0;
    frames(&mut s, &mut f, 50, 1.2);
    let c = loads(&mut s, DOWN_SAMPLES, 0.95).pop().expect("a step down");
    assert!((c.measured.0 - 0.0012).abs() < 1e-12, "{:?}", c.measured);
    assert_eq!(c.frame, f, "decided on the newest frame seen");
}

#[test]
fn one_sample_at_or_under_the_limit_resets_the_count() {
    let mut s = high();
    assert_eq!(loads(&mut s, DOWN_SAMPLES - 1, 0.95), []);
    assert_eq!(loads(&mut s, 1, 0.5), []);
    assert_eq!(loads(&mut s, DOWN_SAMPLES - 1, 0.95), []);
    assert_eq!(s.rungs(), 0);
    // Exactly the limit is not over it.
    assert_eq!(loads(&mut s, 10 * DOWN_SAMPLES, OUTPUT_LOAD_LIMIT), []);
    assert_eq!(s.rungs(), 0);
}

#[test]
fn a_calm_encoder_takes_the_rung_back_and_a_flap_doubles_the_hold() {
    let mut s = high();
    assert_eq!(loads(&mut s, DOWN_SAMPLES, 0.95).len(), 1);
    assert_eq!(loads(&mut s, UP_SAMPLES - 1, 0.5), [], "149 calm samples are not enough");
    let up = loads(&mut s, 1, 0.5);
    assert!(matches!(up[..], [Change { direction: Direction::Up, .. }]), "{up:?}");
    assert_eq!(s.rungs(), 0);
    // Straight back down, within FLAP_WINDOW frames of the step up (no frame has passed): the hold doubles.
    assert_eq!(loads(&mut s, DOWN_SAMPLES, 0.95).len(), 1);
    assert_eq!(loads(&mut s, 2 * UP_SAMPLES - 1, 0.5), [], "the step up now waits twice as long");
    assert_eq!(loads(&mut s, 1, 0.5).len(), 1);
}

#[test]
fn a_step_down_long_after_a_step_up_keeps_the_hold() {
    let mut s = high();
    let mut f = 0;
    loads(&mut s, DOWN_SAMPLES, 0.95);
    loads(&mut s, UP_SAMPLES, 0.5);
    frames(&mut s, &mut f, u32::try_from(FLAP_WINDOW).unwrap() + 1, 0.5);
    assert_eq!(loads(&mut s, DOWN_SAMPLES, 0.95).len(), 1);
    assert_eq!(loads(&mut s, UP_SAMPLES, 0.5).len(), 1, "no flap: the hold is back to one");
}

#[test]
fn the_encoder_does_not_step_up_while_the_frame_has_no_room() {
    let mut s = high();
    let mut f = 0;
    loads(&mut s, DOWN_SAMPLES, 0.95);
    // Frames at 90 % of the budget: under it (no step down) but over UP_MARGIN of it.
    let busy = BUDGET.0 * 1000.0 * 0.9;
    frames(&mut s, &mut f, 30, busy);
    assert_eq!(loads(&mut s, 4 * UP_SAMPLES, 0.1), []);
    assert_eq!(s.rungs(), 1);
}

#[test]
fn a_busy_encoder_holds_back_frame_times_step_up() {
    for (load, steps_up) in [(0.8, false), (0.5, true)] {
        let mut s = high();
        let mut f = 0;
        assert_eq!(frames(&mut s, &mut f, DOWN_SAMPLES, 50.0).len(), 1);
        assert_eq!(s.observe_output(load), None);
        let ups = frames(&mut s, &mut f, UP_SAMPLES, 0.5);
        assert_eq!(ups.len() == 1, steps_up, "load {load}: {ups:?}");
    }
}

#[test]
fn offline_and_bad_samples_change_nothing() {
    let mut offline = Scheduler::new(Quality::offline(), budget::HIGH_GPU);
    assert_eq!(loads(&mut offline, 1000, 5.0), []);
    assert_eq!(offline.rungs(), 0);

    let mut s = high();
    assert_eq!(loads(&mut s, DOWN_SAMPLES - 1, 0.95), []);
    for bad in [f32::NAN, -0.5, -f32::INFINITY] {
        assert_eq!(s.observe_output(bad), None);
    }
    assert_eq!(s.output_load(), Some(0.95), "a bad sample is not recorded");
    assert_eq!(loads(&mut s, 1, 0.95).len(), 1, "nor does it reset the count");
}
