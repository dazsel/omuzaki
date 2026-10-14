//! `measure.liveliness` (VF-05): synthetic streams with known blinks, winks, saccades and frozen spans are measured
//! exactly; the default life layer is measured against the at-rest baseline; the op runs end to end.

// Test helpers may panic (a panic is a clear test failure); frame counts and indices are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
#![allow(clippy::disallowed_methods, reason = "tests write their fixture file; the fence is for ops")]

use std::time::Instant;

use omuzaki_anim::measure::{Baseline, Verdict, liveliness};
use omuzaki_anim_api::{
    Bus, Channel, ChannelBuf, ChannelMask, Cues, HeadPose, LifeConfig, LifeLayer, Personality, Tick,
};
use omuzaki_anim_mock::track::{SyntheticSpec, synthetic_track};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, FsMode, Grants, Op, OpCtx, ProjectDir, find};
use serde_json::{Value, json};

const FPS: f32 = 60.0;
const DT: f32 = 1.0 / FPS;

/// Rates divide by `frames * dt`, which is off by a few parts in 10^7 because 1/60 is not exact in `f32`.
fn near(got: f32, want: f32) {
    assert!((got - want).abs() < want * 1e-5, "{got} vs {want}");
}

/// `seconds` of still face.
fn still(seconds: f32) -> Vec<ChannelBuf> {
    vec![ChannelBuf::NEUTRAL; (seconds * FPS) as usize]
}

/// Sets `frames` frames from `at` with `set`.
fn paint(stream: &mut [ChannelBuf], at: usize, frames: usize, set: impl Fn(&mut ChannelBuf)) {
    for f in &mut stream[at..at + frames] {
        set(f);
    }
}

fn both_lids(f: &mut ChannelBuf) {
    f.set(Channel::EYE_BLINK_LEFT, 1.0);
    f.set(Channel::EYE_BLINK_RIGHT, 1.0);
}

/// Slow head motion (0.002 a frame, above the 0.001 stillness limit): no frame is still, and nothing else counts.
fn drifting(seconds: f32) -> Vec<ChannelBuf> {
    let mut s = still(seconds);
    for (i, f) in s.iter_mut().enumerate() {
        f.set(Channel::HEAD_YAW, 0.002 * (i % 200) as f32);
    }
    s
}

#[test]
fn blinks_are_counted_by_closed_time() {
    let mut s = drifting(10.0);
    paint(&mut s, 60, 3, both_lids); // 50 ms: a blink
    paint(&mut s, 150, 6, both_lids); // 100 ms: a blink
    paint(&mut s, 300, 24, both_lids); // 400 ms: the longest blink
    paint(&mut s, 400, 2, both_lids); // 33 ms: noise
    paint(&mut s, 450, 25, both_lids); // 417 ms: eyes held shut
    let m = liveliness(&s, DT);
    assert_eq!((m.blinks, m.winks), (3, 0));
    near(m.blinks_per_minute, 18.0);
    near(m.seconds, 10.0);
}

#[test]
fn a_wink_is_counted_apart_from_a_blink() {
    let mut s = drifting(6.0);
    paint(&mut s, 60, 9, |f| f.set(Channel::EYE_BLINK_LEFT, 0.9)); // left only, 150 ms
    paint(&mut s, 200, 9, |f| f.set(Channel::EYE_BLINK_RIGHT, 0.9));
    paint(&mut s, 300, 9, both_lids);
    let m = liveliness(&s, DT);
    assert_eq!((m.blinks, m.winks), (1, 2));
    near(m.winks_per_minute, 20.0);
}

#[test]
fn lids_that_close_a_frame_apart_are_one_blink() {
    let mut s = drifting(4.0);
    paint(&mut s, 60, 8, |f| f.set(Channel::EYE_BLINK_LEFT, 0.9));
    paint(&mut s, 61, 8, |f| f.set(Channel::EYE_BLINK_RIGHT, 0.9));
    let m = liveliness(&s, DT);
    assert_eq!((m.blinks, m.winks), (1, 0));
}

#[test]
fn a_blink_needs_one_contiguous_run_of_both_lids() {
    // The left lid is held shut for 500 ms while the right closes in three one-frame pulses: no overlap lasts 40 ms.
    let mut s = drifting(4.0);
    paint(&mut s, 60, 30, |f| f.set(Channel::EYE_BLINK_LEFT, 1.0));
    for at in [65, 75, 85] {
        paint(&mut s, at, 1, |f| f.set(Channel::EYE_BLINK_RIGHT, 1.0));
    }
    let m = liveliness(&s, DT);
    assert_eq!((m.blinks, m.winks), (0, 0));
}

#[test]
fn a_lid_that_leads_a_blink_is_not_a_wink() {
    let mut s = drifting(4.0);
    paint(&mut s, 60, 3, |f| f.set(Channel::EYE_BLINK_LEFT, 1.0)); // 50 ms ahead of the right lid
    paint(&mut s, 63, 6, both_lids);
    let m = liveliness(&s, DT);
    assert_eq!((m.blinks, m.winks), (1, 0));
}

#[test]
fn a_lid_at_the_level_is_not_shut() {
    let mut s = drifting(2.0);
    paint(&mut s, 30, 10, |f| {
        f.set(Channel::EYE_BLINK_LEFT, 0.6);
        f.set(Channel::EYE_BLINK_RIGHT, 0.6);
    });
    assert_eq!(liveliness(&s, DT).blinks, 0);
}

/// Moves the gaze from `from` to `to` look units in `frames` equal steps starting at frame `at`, then holds it.
fn glance(stream: &mut [ChannelBuf], at: usize, frames: usize, from: f32, to: f32) {
    for (k, f) in stream.iter_mut().enumerate().skip(at) {
        let t = ((k - at + 1) as f32 / frames as f32).min(1.0);
        f.set_gaze(from + (to - from) * t, 0.0);
    }
}

#[test]
fn saccades_are_counted_by_eye_speed() {
    // One look unit is 40 degrees. 0.5 in 3 frames at 60 fps is 20 degrees in 50 ms: 400 degrees a second.
    let mut s = drifting(10.0);
    glance(&mut s, 60, 3, 0.0, 0.5);
    glance(&mut s, 200, 3, 0.5, -0.5); // 40 degrees in 50 ms
    // A slow drift: 0.05 look units (2 degrees) in 12 frames is 10 degrees a second.
    glance(&mut s, 400, 12, -0.5, -0.45);
    let m = liveliness(&s, DT);
    assert_eq!(m.saccades, 2);
    near(m.saccades_per_second, 0.2);
}

/// A gaze whose speed in each frame step is `degrees_per_second[k]`, along x, from frame `at`.
fn gaze_at_speeds(stream: &mut [ChannelBuf], at: usize, degrees_per_second: &[f32]) {
    let mut x = 0.0;
    for (k, f) in stream.iter_mut().enumerate().skip(at) {
        if let Some(v) = degrees_per_second.get(k - at) {
            x += v * DT / 40.0;
        }
        f.set_gaze(x, 0.0);
    }
}

#[test]
fn two_peaks_within_50_ms_are_one_saccade() {
    // Peaks 2 frames (33 ms) apart, with a valley between that is still above the threshold.
    let mut s = drifting(4.0);
    gaze_at_speeds(&mut s, 60, &[120.0, 240.0, 120.0, 260.0, 120.0]);
    assert_eq!(liveliness(&s, DT).saccades, 1);
}

/// A gaze that moves by `steps` look units a frame, from frame `at`. Multiples of 1/16 are exact in `f32`, so equal
/// steps are exactly equal speeds.
fn gaze_by_steps(stream: &mut [ChannelBuf], at: usize, steps: &[f32]) {
    let mut x = 0.0;
    for (k, f) in stream.iter_mut().enumerate().skip(at) {
        x += steps.get(k - at).copied().unwrap_or(0.0);
        f.set_gaze(x, 0.0);
    }
}

#[test]
fn a_plateau_that_keeps_rising_is_one_peak() {
    // Six steps of 1/16 (150 deg/s at 60 fps), then 1/8 (300), then rest: one velocity peak, at the 300.
    let mut s = drifting(4.0);
    gaze_by_steps(&mut s, 60, &[0.0625, 0.0625, 0.0625, 0.0625, 0.0625, 0.0625, 0.125]);
    assert_eq!(liveliness(&s, DT).saccades, 1);
    // A plateau that ends in a drop is one peak too.
    let mut s = drifting(4.0);
    gaze_by_steps(&mut s, 60, &[0.0625, 0.0625, 0.0625]);
    assert_eq!(liveliness(&s, DT).saccades, 1);
}

#[test]
fn two_plateaus_with_an_above_threshold_dip_between_count_apart() {
    // Plateau of 150 deg/s (1/16), a dip to 110 (just above 100), a second plateau of 150: 4 frames (67 ms) apart.
    let mut s = drifting(4.0);
    let dip = 0.045_833_333; // 110 deg/s at 60 fps, in look units
    gaze_by_steps(&mut s, 60, &[0.0625, 0.0625, 0.0625, dip, 0.0625, 0.0625, 0.0625]);
    assert_eq!(liveliness(&s, DT).saccades, 2);
}

#[test]
fn peaks_separated_by_an_above_threshold_valley_count_apart() {
    // 120, 240, 120, 110, 120, 240, 120 degrees a second: the peaks are 4 frames (67 ms) apart.
    let mut s = drifting(4.0);
    gaze_at_speeds(&mut s, 60, &[120.0, 240.0, 120.0, 110.0, 120.0, 240.0, 120.0]);
    assert_eq!(liveliness(&s, DT).saccades, 2);
}

#[test]
fn the_longest_frozen_window_is_measured_in_seconds() {
    let mut s = still(6.0);
    // The jaw steps at frames 60, 150, 210, 270 and 330: the gap from 60 to 150 is 89 still steps, the longest.
    for (k, f) in s.iter_mut().enumerate() {
        f.set(Channel::JAW_OPEN, 0.1 * [60, 150, 210, 270, 330].iter().filter(|&&at| k >= at).count() as f32);
    }
    let m = liveliness(&s, DT);
    assert!((m.longest_frozen_s - 89.0 * DT).abs() < 1e-6, "{}", m.longest_frozen_s);
    assert_eq!(drifting(3.0).len(), 180);
    assert_eq!(liveliness(&drifting(3.0), DT).longest_frozen_s, 0.0);
}

#[test]
fn change_per_second_is_the_mean_absolute_step() {
    // One channel moves 0.01 a frame; the metric averages over every face and head channel.
    let mut s = still(2.0);
    for (k, f) in s.iter_mut().enumerate() {
        f.set(Channel::JAW_OPEN, 0.01 * k as f32);
    }
    let channels = (ChannelMask::ARKIT.count() + ChannelMask::HEAD.count()) as f32;
    let expected = 0.01 / channels / DT;
    let m = liveliness(&s, DT);
    assert!((m.change_per_second - expected).abs() < expected * 1e-3, "{} vs {expected}", m.change_per_second);
}

#[test]
fn head_translation_alone_is_motion() {
    let mut s = still(4.0);
    for (k, f) in s.iter_mut().enumerate() {
        f.set(Channel::HEAD_SHIFT_X, 0.002 * k as f32);
    }
    let m = liveliness(&s, DT);
    assert!(m.change_per_second > 0.0);
    assert_eq!(m.longest_frozen_s, 0.0);
}

#[test]
fn degenerate_input_measures_as_zero() {
    assert_eq!(liveliness(&[], DT).blinks, 0);
    assert_eq!(liveliness(&still(0.01), DT).seconds, 0.0);
    assert_eq!(liveliness(&still(1.0), 0.0).seconds, 0.0);
    assert_eq!(liveliness(&still(1.0), f32::NAN).seconds, 0.0);
}

// ---- the life layer, the baselines, the op ----

/// The life layer on a still face.
fn life_run(seed: u64, seconds: f32, personality: Personality) -> Vec<ChannelBuf> {
    let mut life = omuzaki_anim::life::Life::new(LifeConfig::new(seed).with_personality(personality));
    (0..(seconds * FPS) as u64)
        .map(|i| {
            let mut bus = Bus::default();
            life.step(Tick::at_rate(i, FPS), &Cues::default(), &mut bus);
            bus.values
        })
        .collect()
}

#[test]
fn the_default_life_layer_lands_in_the_at_rest_band() {
    let m = liveliness(&life_run(1, 120.0, Personality::Natural), DT);
    println!("life layer, seed 1, 120 s: {m:?}");
    let verdict = Baseline::builtin("rest").unwrap().compare(&m);
    println!("verdict against rest: {verdict:?}");
    assert!(verdict.all_ok(), "{m:?} -> {verdict:?}");
}

#[test]
fn measuring_is_deterministic() {
    let s = life_run(3, 30.0, Personality::Energetic);
    assert_eq!(liveliness(&s, DT), liveliness(&s, DT));
    assert_eq!(liveliness(&s, DT), liveliness(&life_run(3, 30.0, Personality::Energetic), DT));
}

#[test]
fn a_minute_of_60_fps_data_measures_in_a_millisecond() {
    let s = life_run(1, 60.0, Personality::Natural);
    let best = (0..20)
        .map(|_| {
            let t = Instant::now();
            std::hint::black_box(liveliness(std::hint::black_box(&s), DT));
            t.elapsed()
        })
        .min()
        .unwrap();
    println!("liveliness of 3600 frames: {best:?}");
    // The budget is 1 ms in an optimised build; a debug build is an order of magnitude slower, so it only guards
    // against an accidental quadratic.
    let budget = if cfg!(debug_assertions) { 25 } else { 1 };
    assert!(best.as_millis() < budget, "{best:?}");
}

#[test]
fn a_dead_face_is_low_and_a_jittery_one_is_high() {
    let rest = Baseline::builtin("rest").unwrap();
    let dead = rest.compare(&liveliness(&still(30.0), DT));
    assert_eq!(dead.blinks_per_minute, Verdict::Low);
    assert_eq!(dead.change_per_second, Verdict::Low);
    assert_eq!(dead.longest_frozen_s, Verdict::High);
    let mut noisy = still(30.0);
    for (k, f) in noisy.iter_mut().enumerate() {
        f.set(Channel::JAW_OPEN, if k % 2 == 0 { 0.0 } else { 1.0 });
    }
    assert_eq!(rest.compare(&liveliness(&noisy, DT)).change_per_second, Verdict::High);
}

#[test]
fn every_builtin_baseline_loads_and_cites_sources() {
    for name in ["rest", "talking", "reading"] {
        let b = Baseline::builtin(name).unwrap();
        assert_eq!(b.name, name);
        assert!(b.sources.len() >= 4, "{name} cites a source for each metric");
        for band in [b.change_per_second, b.blinks_per_minute, b.saccades_per_second, b.longest_frozen_s] {
            assert!(band.low <= band.high, "{name}: {band:?}");
        }
    }
    assert!(Baseline::builtin("sprinting").is_err());
}

fn run_op(args: Value, project: Option<&ProjectDir>) -> Result<Value, omuzaki_ops_api::OpError> {
    let (doc, actor, clock) =
        (MemDoc::default(), Actor::Agent { id: "t".to_owned(), model: None }, FakeClock::new(Seconds(0.0)));
    let grants = Grants::agent_default();
    let mut ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants);
    if let Some(dir) = project {
        ctx = ctx.with_project_dir(dir);
    }
    let reg = find("measure.liveliness").expect("the op registers itself");
    let plan = (reg.decode)(args)?.plan(&ctx)?;
    Ok(plan.output.expect("a query answers"))
}

#[test]
fn the_op_measures_a_life_run() {
    let a = run_op(json!({"source": {"life": {"seed": 1, "seconds": 20.0}}, "baseline": "rest"}), None).unwrap();
    let b = run_op(
        json!({"source": {"life": {"seed": 1, "seconds": 20.0, "personality": "natural"}}, "baseline": "rest"}),
        None,
    )
    .unwrap();
    assert_eq!(a, b);
    assert_eq!(a["baseline"], "rest");
    assert!((a["measured"]["seconds"].as_f64().unwrap() - 20.0).abs() < 1e-3);
    assert!(a["verdict"]["blinks_per_minute"].is_string());
}

#[test]
fn the_op_refuses_bad_input() {
    for args in [
        json!({"source": {"life": {"seed": 1, "seconds": 0.0}}, "baseline": "rest"}),
        json!({"source": {"life": {"seed": 1, "seconds": 99999.0}}, "baseline": "rest"}),
        json!({"source": {"life": {"seed": 1, "seconds": 5.0}}, "baseline": "dancing"}),
    ] {
        assert!(run_op(args.clone(), None).is_err(), "{args}");
    }
    assert!(OpCtxless::meta_is_query());
}

struct OpCtxless;
impl OpCtxless {
    fn meta_is_query() -> bool {
        omuzaki_anim::measure::MeasureLiveliness::meta().kind == omuzaki_ops_api::OpKind::Query
    }
}

#[test]
fn the_op_measures_a_recorded_track() {
    let dir = tempfile::tempdir().unwrap();
    let track = synthetic_track(&SyntheticSpec { seconds: 20.0, fps: 30.0, ..SyntheticSpec::default() });
    std::fs::write(dir.path().join("take.json"), track.to_json().unwrap()).unwrap();
    let project = ProjectDir::open(dir.path()).unwrap();
    let out = run_op(json!({"source": {"recording": "take.json"}, "baseline": "talking"}), Some(&project)).unwrap();
    let measured = &out["measured"];
    assert!((measured["seconds"].as_f64().unwrap() - 20.033).abs() < 0.01, "{measured}");
    // The synthetic track blinks every 1.5 to 4.5 s, for 150 ms: some blinks, no saccades, a moving head.
    assert!(measured["blinks"].as_u64().unwrap() >= 4, "{measured}");
    assert_eq!(measured["saccades"], 0);
    assert!(measured["change_per_second"].as_f64().unwrap() > 0.0);
    // A missing file and a file that is not a track are refused, not panicked on.
    assert!(run_op(json!({"source": {"recording": "nope.json"}, "baseline": "rest"}), Some(&project)).is_err());
    std::fs::write(dir.path().join("bad.json"), "{}").unwrap();
    assert!(run_op(json!({"source": {"recording": "bad.json"}, "baseline": "rest"}), Some(&project)).is_err());
    let _ = FsMode::Read;
}

#[test]
fn a_translation_only_recording_is_not_frozen() {
    let dir = tempfile::tempdir().unwrap();
    let mut track = synthetic_track(&SyntheticSpec { seconds: 5.0, fps: 30.0, ..SyntheticSpec::default() });
    for (k, frame) in track.frames.iter_mut().enumerate() {
        frame.blend = [0.0; 52];
        frame.head =
            Some(HeadPose { yaw: 0.0, pitch: 0.0, roll: 0.0, position: Some(Vec3::new(0.01 * k as f32, 0.0, 0.6)) });
    }
    std::fs::write(dir.path().join("slide.json"), track.to_json().unwrap()).unwrap();
    let project = ProjectDir::open(dir.path()).unwrap();
    let out = run_op(json!({"source": {"recording": "slide.json"}, "baseline": "rest"}), Some(&project)).unwrap();
    assert!(out["measured"]["change_per_second"].as_f64().unwrap() > 0.0, "{out}");
    assert_eq!(out["measured"]["longest_frozen_s"], 0.0, "{out}");
}
