//! The scheduler's rules, driven with hand-made [`FrameReport`]s so every number is exact.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use omuzaki_budget::scheduler::{DOWN_SAMPLES, FLAP_WINDOW, UP_SAMPLES};
use omuzaki_budget::{Change, Direction, FacePressure, Scheduler, Step};
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{FrameReport, GpuTimings, Pass, PassTimes, Quality, budget};

const LIVE: Seconds = budget::LIVE_GPU;

/// A report whose GPU time is `ms` in the opaque pass, measured on `gpu_frame` while `frame` is being submitted.
fn report(frame: u64, gpu_frame: u64, ms: f64) -> FrameReport {
    let mut passes = PassTimes::default();
    passes.set(Pass::Opaque, Seconds(ms / 1000.0));
    FrameReport {
        frame,
        render_size: UVec2::new(1920, 1080),
        cpu: PassTimes::default(),
        gpu: Some(GpuTimings { frame: gpu_frame, passes, painters: None }),
        draw_calls: 0,
        triangles: 0,
        particles: 0,
    }
}

/// Feeds `n` frames of `ms` starting at `*frame`, returns the changes.
fn feed(s: &mut Scheduler, frame: &mut u64, n: u32, ms: f64) -> Vec<Change> {
    (0..n)
        .filter_map(|_| {
            *frame += 1;
            s.observe(&report(*frame, *frame, ms))
        })
        .collect()
}

fn high() -> Scheduler {
    Scheduler::new(Quality::high(), budget::HIGH_GPU)
}

#[test]
fn under_budget_changes_nothing() {
    let mut s = high();
    let mut f = 0;
    assert_eq!(feed(&mut s, &mut f, 1000, 3.0).len(), 0);
    assert_eq!(s.quality(), Quality::high());
    assert!((s.smoothed().unwrap().0 - 0.003).abs() < 1e-12);
}

#[test]
fn sustained_overrun_steps_down_one_rung_with_the_numbers() {
    let mut s = high();
    let mut f = 0;
    assert!(feed(&mut s, &mut f, DOWN_SAMPLES - 1, 5.0).is_empty(), "not yet");
    let changes = feed(&mut s, &mut f, 1, 5.0);
    let [c] = changes[..] else { panic!("one change, got {changes:?}") };
    assert_eq!((c.direction, c.step), (Direction::Down, Step::DepthOfFieldOff));
    assert_eq!(c.frame, u64::from(DOWN_SAMPLES));
    assert!((c.measured.0 - 0.005).abs() < 1e-12 && c.budget == budget::HIGH_GPU);
    assert_eq!((c.from, c.to), (Quality::high(), s.quality()));
    let post = s.quality().post_effects;
    assert!(!post.depth_of_field && post.bloom && post.grain_and_grade, "only the most expensive effect goes first");
    assert_eq!(s.quality().shadows, Quality::high().shadows);
    assert_eq!(s.rungs(), 1);
}

#[test]
fn a_short_spike_is_ignored() {
    let mut s = high();
    let mut f = 0;
    assert_eq!(feed(&mut s, &mut f, 100, 3.0).len(), 0);
    // Two 10 ms frames lift the average to 4.3 ms, and it is back under 4 ms three samples later.
    assert!(feed(&mut s, &mut f, 2, 10.0).is_empty(), "smoothed time is over budget for fewer than 12 samples");
    assert_eq!(feed(&mut s, &mut f, 200, 3.0).len(), 0);
    assert_eq!(s.rungs(), 0);
}

#[test]
fn keeps_stepping_down_until_the_floor_then_stops() {
    let mut s = high();
    let mut f = 0;
    let all = feed(&mut s, &mut f, 20_000, 50.0);
    assert_eq!(all.len(), Step::LADDER.len(), "every rung once, in ladder order");
    for (change, step) in all.iter().zip(Step::LADDER) {
        assert_eq!(change.step, step);
    }
    assert_eq!(s.rungs(), Step::LADDER.len());
    assert_eq!(s.quality().validate(), Ok(()));
}

#[test]
fn rungs_that_change_nothing_are_skipped() {
    // Live: post already off, interval 2, Low shadows, scale 1, 4 samples.
    let mut s = Scheduler::new(Quality::live(), LIVE);
    let mut f = 0;
    let all = feed(&mut s, &mut f, 20_000, 50.0);
    let steps: Vec<_> = all.iter().map(|c| c.step).collect();
    assert_eq!(
        steps,
        [
            Step::RenderScaleHigh,
            Step::ShadowsLessOften,
            Step::RenderScaleLow,
            Step::ShadowsOff,
            Step::SamplesTwo,
            Step::SamplesOne
        ],
        "no post effect or ShadowsLow rung: nothing to lower there"
    );
}

#[test]
fn a_repeated_gpu_frame_counts_once() {
    let mut s = high();
    // The same finished frame reported on every call, as when a query resolves two frames late.
    for submitted in 1..=100 {
        assert!(s.observe(&report(submitted, 1, 50.0)).is_none());
    }
    assert_eq!(s.rungs(), 0, "one sample is not a sustained overrun");
}

#[test]
fn measurements_from_before_a_change_are_ignored() {
    let mut s = high();
    let mut f = 0;
    assert_eq!(feed(&mut s, &mut f, DOWN_SAMPLES, 5.0).len(), 1);
    // Frames up to the change were rendered with the old quality; the GPU reports them two frames late.
    assert!(s.observe(&report(f + 1, f - 1, 50.0)).is_none());
    assert!(s.observe(&report(f + 2, f, 50.0)).is_none());
    assert_eq!(s.smoothed(), None, "nothing measured since the change");
    s.observe(&report(f + 3, f + 1, 2.0));
    assert!((s.smoothed().unwrap().0 - 0.002).abs() < 1e-12, "the first frame after the change seeds the average");
}

#[test]
fn steps_back_up_after_a_long_comfortable_stretch() {
    let mut s = high();
    let mut f = 0;
    feed(&mut s, &mut f, DOWN_SAMPLES, 5.0);
    assert_eq!(s.rungs(), 1);
    assert!(
        feed(&mut s, &mut f, UP_SAMPLES - 1, 2.0).is_empty(),
        "2 ms is 50% of 4 ms: comfortable, but not for long enough"
    );
    let up = feed(&mut s, &mut f, 1, 2.0);
    let [c] = up[..] else { panic!("one change, got {up:?}") };
    assert_eq!((c.direction, c.step), (Direction::Up, Step::DepthOfFieldOff));
    assert_eq!(s.quality(), Quality::high());
}

#[test]
fn the_dead_band_between_70_and_100_percent_holds() {
    let mut s = high();
    let mut f = 0;
    feed(&mut s, &mut f, DOWN_SAMPLES, 5.0);
    // 3.6 ms is 90% of the budget: not over, not comfortable.
    assert_eq!(feed(&mut s, &mut f, 5000, 3.6).len(), 0);
    assert_eq!(s.rungs(), 1);
}

#[test]
fn a_flapping_step_up_doubles_the_wait() {
    let mut s = high();
    let mut f = 0;
    feed(&mut s, &mut f, DOWN_SAMPLES, 5.0);
    assert_eq!(feed(&mut s, &mut f, UP_SAMPLES, 2.0).len(), 1, "up");
    // Back at full quality it overruns again right away.
    assert_eq!(feed(&mut s, &mut f, DOWN_SAMPLES, 5.0).len(), 1, "down again, inside the flap window");
    assert!(u64::from(DOWN_SAMPLES) < FLAP_WINDOW);
    assert!(feed(&mut s, &mut f, 2 * UP_SAMPLES - 1, 2.0).is_empty(), "now it waits twice as long");
    assert_eq!(feed(&mut s, &mut f, 1, 2.0).len(), 1);
}

#[test]
fn offline_quality_is_never_degraded() {
    let mut s = Scheduler::new(Quality::offline(), budget::HIGH_GPU);
    let mut f = 0;
    assert_eq!(feed(&mut s, &mut f, 1000, 500.0).len(), 0);
    assert_eq!(s.quality(), Quality::offline());
}

#[test]
fn without_gpu_timings_the_cpu_time_is_used() {
    let mut s = high();
    for frame in 1..=u64::from(DOWN_SAMPLES) {
        let mut cpu = PassTimes::default();
        cpu.set(Pass::Opaque, Seconds(0.006));
        let r = FrameReport { gpu: None, cpu, ..report(frame, frame, 0.0) };
        let change = s.observe(&r);
        assert_eq!(change.is_some(), frame == u64::from(DOWN_SAMPLES));
    }
}

#[test]
fn face_painting_has_its_own_line() {
    let mut s = high();
    assert_eq!(s.face_pressure(), FacePressure::Unknown);
    let mut f = 0;
    // 3 ms of face painting against 0.5 ms: the face is over, and the frame budget does not see it.
    for _ in 0..100 {
        f += 1;
        let mut r = report(f, f, 1.0);
        r.gpu.as_mut().unwrap().passes.set(Pass::PaintTextures, Seconds(0.003));
        assert!(s.observe(&r).is_none(), "paint time is excluded from the frame total");
    }
    assert_eq!(s.face_pressure(), FacePressure::Over);
    assert!((s.smoothed().unwrap().0 - 0.001).abs() < 1e-12);
    for _ in 0..200 {
        f += 1;
        let mut r = report(f, f, 1.0);
        r.gpu.as_mut().unwrap().passes.set(Pass::PaintTextures, Seconds(0.0001));
        s.observe(&r);
    }
    assert_eq!(s.face_pressure(), FacePressure::Under);
}

#[test]
fn a_new_request_starts_from_full_quality() {
    let mut s = high();
    let mut f = 0;
    feed(&mut s, &mut f, DOWN_SAMPLES, 5.0);
    s.set_requested(Quality::live(), LIVE);
    assert_eq!((s.quality(), s.rungs(), s.budget()), (Quality::live(), 0, LIVE));
}

#[test]
fn an_index_going_backwards_starts_a_new_sequence_without_degrading() {
    let mut s = high();
    for label in 1000..1050 {
        assert!(s.observe(&report(label, label, 3.0)).is_none());
    }
    // The numbering restarts at 0 while the scene is far over budget: that report only resets the history.
    assert!(s.observe(&report(0, 0, 50.0)).is_none(), "no decision on the report that went backwards");
    assert_eq!(s.smoothed(), None);
    // The new sequence measures normally, and a sustained overrun in it still steps down once.
    let mut changes = 0;
    for label in 1..=u64::from(DOWN_SAMPLES) {
        changes += usize::from(s.observe(&report(label, label, 50.0)).is_some());
    }
    assert_eq!((changes, s.rungs()), (1, 1));
}

#[test]
fn a_paused_label_repeated_counts_once() {
    let mut s = high();
    // The same frame label on every report while paused: a repeat is a cached result, counted once.
    for _ in 0..100 {
        assert!(s.observe(&report(7, 7, 50.0)).is_none());
    }
    assert_eq!(s.rungs(), 0);
    assert!((s.smoothed().unwrap().0 - 0.05).abs() < 1e-12, "one sample, not a hundred");
}

#[test]
fn a_cached_gpu_result_reported_on_later_frames_counts_once() {
    let mut s = high();
    let mut f = 0;
    feed(&mut s, &mut f, 10, 3.0);
    // The renderer keeps reporting the newest finished frame (10) while 40 more frames are submitted.
    for submitted in 11..=50 {
        assert!(s.observe(&report(submitted, 10, 50.0)).is_none());
    }
    assert!((s.smoothed().unwrap().0 - 0.003).abs() < 1e-12, "frame 10 was already measured");
}

#[test]
fn timings_in_flight_when_the_request_changes_are_ignored() {
    let mut s = high();
    let mut f = 0;
    feed(&mut s, &mut f, DOWN_SAMPLES - 1, 5.0);
    s.set_requested(Quality::live(), budget::HIGH_GPU);
    // Two late timings of frames submitted before the switch, still showing the old quality's cost.
    assert!(s.observe(&report(f + 1, f, 50.0)).is_none());
    assert!(s.observe(&report(f + 2, f - 1, 50.0)).is_none());
    assert_eq!(s.smoothed(), None, "nothing counted from before the switch");
    f += 2;
    // Genuinely cheap frames follow: no downgrade.
    assert_eq!(feed(&mut s, &mut f, 100, 1.0).len(), 0);
    assert!((s.smoothed().unwrap().0 - 0.001).abs() < 1e-12);
    assert_eq!(s.rungs(), 0);
}

#[test]
fn a_restart_ignores_gpu_timings_cached_from_before_it() {
    let mut s = high();
    for label in 1000..1050 {
        s.observe(&report(label, label, 3.0));
    }
    // Numbering restarts at 0, but the renderer still hands back its newest finished frame (1049).
    assert!(s.observe(&report(0, 0, 3.0)).is_none());
    for label in 1..=100 {
        assert!(s.observe(&report(label, 1049, 50.0)).is_none(), "a timing from the future is ignored");
    }
    assert_eq!(s.smoothed(), None);
    // Fresh timings of the new sequence are still accepted from frame 1 on.
    s.observe(&report(101, 1, 2.0));
    assert!((s.smoothed().unwrap().0 - 0.002).abs() < 1e-12);
}

#[test]
fn delayed_face_timings_after_a_change_are_ignored() {
    let face = |frame: u64, gpu_frame: u64, ms: f64| {
        let mut r = report(frame, gpu_frame, 1.0);
        r.gpu.as_mut().unwrap().passes.set(Pass::PaintTextures, Seconds(ms / 1000.0));
        r
    };
    // After a new request: report 10 measures frame 8 cheaply, then frame 9 (submitted before the switch) arrives dear.
    let mut s = high();
    s.observe(&face(10, 8, 0.1));
    assert_eq!(s.face_pressure(), FacePressure::Under);
    s.set_requested(Quality::live(), LIVE);
    s.observe(&face(11, 9, 5.0));
    assert_eq!(s.face_pressure(), FacePressure::Under, "stale after set_requested");
    s.observe(&face(12, 11, 5.0));
    assert!((s.smoothed().is_some()) && s.face_pressure() != FacePressure::Under, "frame 11 is fresh");

    // After an automatic change.
    let mut s = high();
    let mut f = 0;
    feed(&mut s, &mut f, DOWN_SAMPLES, 5.0);
    s.observe(&face(f + 1, f - 1, 0.1));
    s.observe(&face(f + 2, f, 5.0));
    assert_ne!(s.face_pressure(), FacePressure::Over, "frame {f} was submitted before the change");
}
