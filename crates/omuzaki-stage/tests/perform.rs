//! The perform loop (PF-24) end to end: a recorded synthetic track through the real `ArkitRetarget` and `Life` and
//! the mock animator, driven by `advance` on a fake clock at 60 Hz.
//!
//! `tests/golden/perform_trace.json` holds the watched channels and the neck and head rotations at four steps;
//! `OMUZAKI_BLESS=1 cargo test` rewrites it after an intended change.

#![allow(clippy::disallowed_methods, reason = "a test reading and writing its own fixtures and evidence; not an op")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{
    Channel, ChannelBuf, ChannelMask, DeviceProfile, FrameSource, HeadPose, HumanoidBone, HumanoidPose, LifeConfig,
    LifeLayer, RetargetConfig, Source, Tick, TrackFrame,
};
use omuzaki_anim_mock::animator::MockAnimator;
use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};
use omuzaki_core::glam::EulerRot;
use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::{MAX_STEPS_PER_ADVANCE, Performer, PerformerParts, STEP_HZ, Tracking};
use serde::{Deserialize, Serialize};

struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn performer_with(source: Box<dyn FrameSource>) -> Performer {
    let parts = PerformerParts {
        source,
        retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
        life: Box::new(Life::new(LifeConfig::new(7))),
        animator: Box::new(MockAnimator),
    };
    Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST)
}

fn replay_of(spec: &SyntheticSpec) -> Box<dyn FrameSource> {
    Box::new(TrackReplay::new(synthetic_track(spec)))
}

fn spec(seconds: f32) -> SyntheticSpec {
    SyntheticSpec { seconds, ..SyntheticSpec::default() }
}

/// The time the middle of step `i` falls at, so ±5 ms of jitter stays inside the step.
fn mid_step(i: u64) -> Seconds {
    Seconds((i as f64 + 0.5) / f64::from(STEP_HZ))
}

#[derive(Serialize, Deserialize)]
struct Sample {
    step: u64,
    jaw_open: f32,
    mouth_smile_left: f32,
    brow_inner_up: f32,
    head_yaw: f32,
    breath: f32,
    neck: [f32; 4],
    head: [f32; 4],
}

impl Sample {
    fn of(step: u64, out: &omuzaki_stage::perform::PerformOutput<'_>) -> Self {
        let v = &out.bus.values;
        Self {
            step,
            jaw_open: v[Channel::JAW_OPEN],
            mouth_smile_left: v[Channel::MOUTH_SMILE_LEFT],
            brow_inner_up: v[Channel::BROW_INNER_UP],
            head_yaw: v[Channel::HEAD_YAW],
            breath: v[Channel::BREATH],
            neck: out.pose.get(HumanoidBone::Neck).to_array(),
            head: out.pose.get(HumanoidBone::Head).to_array(),
        }
    }

    fn values(&self) -> Vec<f32> {
        let mut all = vec![self.jaw_open, self.mouth_smile_left, self.brow_inner_up, self.head_yaw, self.breath];
        all.extend(self.neck);
        all.extend(self.head);
        all
    }
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/perform_trace.json")
}

#[test]
fn the_trace_matches_its_golden() {
    let mut p = performer_with(replay_of(&spec(20.0)));
    let mut seen = Vec::new();
    for i in 0..=1100 {
        let out = p.advance(mid_step(i)).expect("one step per call");
        if [200, 500, 800, 1100].contains(&i) {
            seen.push(Sample::of(i, &out));
        }
    }
    let path = golden_path();
    if std::env::var_os("OMUZAKI_BLESS").is_some() {
        std::fs::write(&path, serde_json::to_string_pretty(&seen).unwrap() + "\n").unwrap();
    }
    let text = std::fs::read_to_string(&path).expect("tests/golden/perform_trace.json (OMUZAKI_BLESS=1 writes it)");
    let golden: Vec<Sample> = serde_json::from_str(&text).unwrap();
    assert_eq!(golden.len(), seen.len());
    for (want, got) in golden.iter().zip(&seen) {
        assert_eq!(want.step, got.step);
        for (w, g) in want.values().iter().zip(got.values()) {
            assert!((w - g).abs() <= 1e-5, "step {}: got {g}, want {w}", got.step);
        }
    }
}

/// Runs a 14 s track lost from 4 s to 12 s (or ending early) and reports the biggest single-step bus move during the
/// hand-off window (4 s to 4.75 s), the first time after
/// 4.75 s that the tracker did not own the left eye, and how often it blinked after that.
fn run_lost(source: Box<dyn FrameSource>) -> (f32, Option<f64>, u32) {
    let mut p = performer_with(source);
    let (mut prev, mut biggest, mut owned_at, mut blinks, mut was_blinking) =
        (None::<ChannelBuf>, 0.0_f32, None, 0, false);
    for i in 0..(14 * 60) {
        let out = p.advance(mid_step(i)).expect("one step per call");
        let t = out.tick.time.0;
        if let Some(prev) = prev.filter(|_| (4.0..=4.75).contains(&t)) {
            // Life's saccades are quick by design (0.2 in a step at seed 7), so the gaze channels are not a hand-off jump.
            for c in ChannelMask::ALL.iter().filter(|c| !ChannelMask::GAZE.contains(*c)) {
                biggest = biggest.max((out.bus.values[c] - prev[c]).abs());
            }
        }
        prev = Some(out.bus.values);
        if t >= 4.75 && owned_at.is_none() && out.bus.owners.of(Channel::EYE_BLINK_LEFT) != Source::Tracker {
            owned_at = Some(t);
        }
        let blinking = out.bus.values[Channel::EYE_BLINK_LEFT] > 0.5;
        if t > 4.75 && blinking && !was_blinking {
            blinks += 1;
        }
        was_blinking = blinking;
    }
    (biggest, owned_at, blinks)
}

#[test]
fn a_lost_face_hands_over_to_life_without_a_jump() {
    let lost = SyntheticSpec { seconds: 14.0, lost: Some((Seconds(4.0), Seconds(12.0))), ..SyntheticSpec::default() };
    let (biggest, owned_at, blinks) = run_lost(replay_of(&lost));
    assert!(biggest <= 0.15, "a channel moved {biggest} in one step");
    let owned_at = owned_at.expect("life never took the eyes");
    assert!(owned_at <= 4.75 + 1.0 / 60.0, "life owned the eyes only at {owned_at} s");
    assert!(blinks > 0, "life never blinked after taking over");
}

#[test]
fn a_source_that_stops_returning_frames_counts_as_lost() {
    // The track has a face for 4 s and then simply ends: no lost frames, just silence.
    let (biggest, owned_at, blinks) = run_lost(replay_of(&spec(4.0)));
    assert!(biggest <= 0.15, "a channel moved {biggest} in one step");
    assert!(owned_at.is_some(), "life never took the eyes from a silent source");
    assert!(blinks > 0);
    let mut p = performer_with(replay_of(&spec(1.0)));
    let tracking = |p: &mut Performer, t: f64| p.advance(Seconds(t)).map(|o| o.tracking);
    assert_eq!(tracking(&mut p, 0.0), Some(Tracking::Tracking));
    assert_eq!(tracking(&mut p, 2.0).map(|_| ()), Some(()));
    let mut q = performer_with(replay_of(&spec(1.0)));
    for i in 0..120 {
        q.advance(mid_step(i));
    }
    assert_eq!(q.advance(mid_step(120)).unwrap().tracking, Tracking::Lost);
}

#[test]
fn before_the_first_frame_the_tracker_layer_is_clear() {
    struct Silent;
    impl FrameSource for Silent {
        fn poll(&mut self, _now: Seconds) -> Option<TrackFrame> {
            None
        }
    }
    let mut p = performer_with(Box::new(Silent));
    for i in 0..120 {
        let out = p.advance(mid_step(i)).unwrap();
        assert_eq!(out.tracking, Tracking::NoFrames);
        assert!(out.frame.is_none());
        assert_ne!(out.bus.owners.of(Channel::EYE_BLINK_LEFT), Source::Tracker);
    }
}

#[test]
fn irregular_calls_give_the_same_trace_as_regular_ones() {
    let digest = |jitter: &dyn Fn(u64) -> f64| {
        let mut p = performer_with(replay_of(&spec(10.0)));
        let mut h = 0xcbf2_9ce4_8422_2325_u64;
        for i in 0..600 {
            let out = p.advance(Seconds(mid_step(i).0 + jitter(i))).expect("one step per call");
            h = (h ^ out.bus.values.checksum()).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    };
    // ±5 ms from a fixed pseudo-random pattern.
    let jittered = |i: u64| (((i * 2_654_435_761) % 1000) as f64 / 1000.0 - 0.5) * 0.010;
    assert_eq!(digest(&|_| 0.0), digest(&jittered));
}

#[test]
fn a_long_stall_runs_four_steps_then_skips() {
    let mut p = performer_with(replay_of(&spec(10.0)));
    assert_eq!(p.advance(Seconds(0.0)).unwrap().tick.index, 0);
    let stalled = p.advance(Seconds(1.0)).unwrap().tick.index;
    assert_eq!(stalled, u64::from(MAX_STEPS_PER_ADVANCE), "steps 1..=4 ran");
    assert_eq!(p.skipped_steps(), 61 - 1 - u64::from(MAX_STEPS_PER_ADVANCE));
    // The loop carries on from the present, not from the backlog.
    assert_eq!(p.advance(Seconds(1.0 + 1.0 / 60.0)).unwrap().tick.index, 61);
}

#[test]
fn a_head_yaw_on_the_bus_turns_the_head_with_the_documented_handedness() {
    struct Still;
    impl LifeLayer for Still {
        fn step(&mut self, _tick: Tick, _cues: &omuzaki_anim_api::Cues, _bus: &mut omuzaki_anim_api::Bus) {}
        fn config(&self) -> &LifeConfig {
            static CONFIG: std::sync::OnceLock<LifeConfig> = std::sync::OnceLock::new();
            CONFIG.get_or_init(|| LifeConfig::new(0))
        }
        fn set_config(&mut self, _config: LifeConfig) {}
        fn reset(&mut self) {}
    }
    struct Facing;
    impl FrameSource for Facing {
        fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
            let mut frame = TrackFrame::empty(0, now, DeviceProfile::Arkit);
            frame.head = Some(HeadPose { yaw: 0.3, ..HeadPose::default() });
            Some(frame)
        }
    }
    let head_yaw = |mirror: bool| {
        let parts = PerformerParts {
            source: Box::new(Facing),
            retarget: Box::new(ArkitRetarget::new(DeviceProfile::Arkit)),
            life: Box::new(Still),
            animator: Box::new(MockAnimator),
        };
        let mut p = Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST);
        p.retarget_mut().set_config(RetargetConfig { mirror, ..RetargetConfig::default() });
        let mut out_yaw = (0.0, 0.0);
        for i in 0..120 {
            let out = p.advance(mid_step(i)).unwrap();
            out_yaw = (out.bus.values[Channel::HEAD_YAW], out.pose.get(HumanoidBone::Head).to_euler(EulerRot::YXZ).0);
        }
        out_yaw
    };
    let (bus, rotation) = head_yaw(false);
    assert!(bus > 0.0 && rotation > 0.0, "bus {bus}, head Y {rotation}");
    let (bus, rotation) = head_yaw(true);
    assert!(bus < 0.0 && rotation < 0.0, "mirrored: bus {bus}, head Y {rotation}");
}

#[test]
fn the_loop_allocates_nothing_over_an_hour_of_steps() {
    let lost = SyntheticSpec { seconds: 30.0, lost: Some((Seconds(10.0), Seconds(14.0))), ..SyntheticSpec::default() };
    let mut p = performer_with(Box::new(TrackReplay::new(synthetic_track(&lost)).looping()));
    for i in 0..600 {
        p.advance(mid_step(i));
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 600..4200 {
        let _ = p.advance(mid_step(i));
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "3600 steps made {allocations} allocations");
}

#[test]
fn a_frame_is_reported_only_on_the_steps_it_arrives() {
    // 30 fps frames on 60 Hz steps: arrival and silence alternate, and after the track ends nothing is reported.
    let mut p = performer_with(replay_of(&spec(1.0)));
    let seen: Vec<bool> = (0..120).map(|i| p.advance(mid_step(i)).unwrap().frame.is_some()).collect();
    assert_eq!(seen[..60].iter().filter(|s| **s).count(), 30, "{seen:?}");
    assert!(seen[..60].windows(2).all(|w| w[0] != w[1]), "arrival and silence alternate: {seen:?}");
    assert!(seen[62..].iter().all(|s| !s), "no frame is repeated after the source goes quiet");
}

#[test]
fn switching_source_forgets_the_old_sources_held_pose() {
    struct Smiling;
    impl FrameSource for Smiling {
        fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
            let mut frame = TrackFrame::empty(0, now, DeviceProfile::Arkit);
            frame.set(Channel::JAW_OPEN, 0.9);
            Some(frame)
        }
    }
    struct LostOnce(bool);
    impl FrameSource for LostOnce {
        fn poll(&mut self, now: Seconds) -> Option<TrackFrame> {
            (!std::mem::replace(&mut self.0, true)).then(|| TrackFrame::lost(0, now, DeviceProfile::Arkit))
        }
    }
    let parts = PerformerParts {
        source: Box::new(Smiling),
        retarget: Box::new(ArkitRetarget::new(DeviceProfile::Arkit)),
        life: Box::new(Life::new(LifeConfig::new(7))),
        animator: Box::new(MockAnimator),
    };
    let mut p = Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST);
    for i in 0..60 {
        p.advance(mid_step(i));
    }
    assert!(p.advance(mid_step(60)).unwrap().bus.values[Channel::JAW_OPEN] > 0.5, "source A drives the jaw");
    p.set_source(Box::new(LostOnce(false)));
    for i in 61..70 {
        let out = p.advance(mid_step(i)).unwrap();
        // Life's jaw spring eases the old value out, but the tracker must not own it again.
        assert_ne!(out.bus.owners.of(Channel::JAW_OPEN), Source::Tracker, "step {i}");
    }
}
