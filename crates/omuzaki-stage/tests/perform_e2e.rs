//! The first end-to-end slice (PF-01, PF-24; spec v3 §8, §22): each step, the test polls `FakeTracker`'s scripted
//! frame source and publishes what it returns through the real track mailbox (`Chain::publish`), and the perform loop
//! reads the mailbox's reader with the real `ArkitRetarget`, `Life` (seed 7) and `BusAnimator`, stepped at 60 Hz on a
//! fake clock.
//!
//! The trace is checked against `tests/golden/perform_trace.json`, which the `MockAnimator` run of `perform.rs` wrote:
//! `BusAnimator` is its counterpart, so the two must agree.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim::{BusAnimator, BusAnimatorParams};
use omuzaki_anim_api::{
    Channel, ChannelBuf, ChannelMask, DeviceProfile, FrameSource, HumanoidBone, HumanoidPose, LifeConfig, Source, Tick,
};
use omuzaki_anim_mock::track::{RecordedTrack, SyntheticSpec, synthetic_track};
use omuzaki_core::glam::EulerRot;
use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::{PerformOutput, Performer, PerformerParts, STEP_HZ};
use omuzaki_track_api::mailbox::{MailboxWriter, mailbox};
use omuzaki_track_api::tracker::{Tracker, TrackerConfig};
use omuzaki_track_mock::{FakeCamera, FakeTracker};
use serde::Deserialize;

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

/// The tracker's side and the stage's side of the mailbox, wired as the app wires them: `FakeTracker` polls the fake
/// camera's script, the newest frame goes into the mailbox writer, and the perform loop polls the reader.
struct Chain {
    feed: Box<dyn FrameSource>,
    writer: MailboxWriter,
    perform: Performer,
}

impl Chain {
    fn new(track: &RecordedTrack) -> Self {
        // `FakeTracker` replays each frame at its own stage time, so the script is the track itself. Its times are
        // whole nanoseconds, and they are truncated rather than rounded: at 30 fps on a 60 Hz step a frame lands
        // exactly on a step, and a rounded-up time would release it one step late.
        let script = track.frames.iter().map(|f| (Duration::from_nanos((f.time.0 * 1e9) as u64), *f)).collect();
        let mut tracker = FakeTracker::scripted(script);
        let feed = tracker.start(&FakeCamera::id(), &TrackerConfig::default()).expect("the fake camera starts");
        let (writer, reader) = mailbox();
        let parts = PerformerParts {
            source: Box::new(reader),
            retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
            life: Box::new(Life::new(LifeConfig::new(7))),
            animator: Box::new(BusAnimator::default()),
        };
        Self { feed, writer, perform: Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST) }
    }

    /// The tracker's turn for step `index`: poll it at the step's pipeline time and publish what it returns.
    fn publish(&mut self, index: u64) {
        if let Some(frame) = self.feed.poll(Tick::at_rate(index, STEP_HZ).time) {
            self.writer.publish(&frame);
        }
    }
}

fn track(seconds: f32, lost: Option<(Seconds, Seconds)>) -> RecordedTrack {
    synthetic_track(&SyntheticSpec { seconds, lost, ..SyntheticSpec::default() })
}

/// The time the middle of step `i` falls at, so ±5 ms of jitter stays inside the step.
fn mid_step(i: u64) -> Seconds {
    Seconds((i as f64 + 0.5) / f64::from(STEP_HZ))
}

/// The golden's sample: the same watched values as `perform.rs` writes.
#[derive(Deserialize)]
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
    fn values(&self) -> Vec<f32> {
        let mut all = vec![self.jaw_open, self.mouth_smile_left, self.brow_inner_up, self.head_yaw, self.breath];
        all.extend(self.neck);
        all.extend(self.head);
        all
    }
}

fn values_of(out: &PerformOutput<'_>) -> Vec<f32> {
    let v = &out.bus.values;
    let mut all = vec![
        v[Channel::JAW_OPEN],
        v[Channel::MOUTH_SMILE_LEFT],
        v[Channel::BROW_INNER_UP],
        v[Channel::HEAD_YAW],
        v[Channel::BREATH],
    ];
    all.extend(out.pose.get(HumanoidBone::Neck).to_array());
    all.extend(out.pose.get(HumanoidBone::Head).to_array());
    all
}

#[test]
fn the_trace_through_the_mailbox_matches_the_perform_golden() {
    let mut chain = Chain::new(&track(20.0, None));
    let mut seen = Vec::new();
    for i in 0..=1100 {
        chain.publish(i);
        let out = chain.perform.advance(mid_step(i)).expect("one step per call");
        if [200, 500, 800, 1100].contains(&i) {
            seen.push((i, values_of(&out)));
        }
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/perform_trace.json");
    let text = std::fs::read_to_string(&path).expect("tests/golden/perform_trace.json");
    let golden: Vec<Sample> = serde_json::from_str(&text).unwrap();
    assert_eq!(golden.len(), seen.len());
    for (want, (step, got)) in golden.iter().zip(&seen) {
        assert_eq!(want.step, *step);
        for (w, g) in want.values().iter().zip(got) {
            assert!((w - g).abs() <= 1e-5, "step {step}: got {g}, want {w}");
        }
    }
}

#[test]
fn a_lost_gap_hands_over_to_life_through_the_mailbox_without_a_jump() {
    // The face is lost from 4 s to 12 s: the script (`synthetic_track`) carries a lost frame at every 30 fps frame
    // in the span, so the hand-off is driven by the script's lost frames.
    let mut chain = Chain::new(&track(14.0, Some((Seconds(4.0), Seconds(12.0)))));
    let (mut biggest, mut owned_at, mut blinks, mut was_blinking) = (0.0_f32, None::<f64>, 0, false);
    let mut prev: Option<ChannelBuf> = None;
    for i in 0..(14 * 60) {
        chain.publish(i);
        let out = chain.perform.advance(mid_step(i)).expect("one step per call");
        let t = out.tick.time.0;
        if let Some(prev) = prev.filter(|_| (4.0..=4.75).contains(&t)) {
            // Life's saccades are quick by design, so the gaze channels are not a hand-off jump.
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
    assert!(biggest <= 0.15, "a channel moved {biggest} in one step");
    let owned_at = owned_at.expect("life never took the eyes");
    assert!(owned_at <= 4.75 + 1.0 / 60.0, "life owned the eyes only at {owned_at} s");
    assert!(blinks > 0, "life never blinked after taking over");
}

#[test]
fn the_neck_and_head_split_the_bus_head_angles_on_every_step() {
    // `BusAnimator` gives the neck its share of each bus angle and the head the rest. The two rotations do not
    // commute, so their product only approximates the bus rotation; each bone is checked against its own share.
    let share = BusAnimatorParams::default().neck_share;
    let mut chain = Chain::new(&track(14.0, Some((Seconds(4.0), Seconds(12.0)))));
    for i in 0..(14 * 60) {
        chain.publish(i);
        let out = chain.perform.advance(mid_step(i)).expect("one step per call");
        let v = &out.bus.values;
        let bus = [v[Channel::HEAD_YAW], v[Channel::HEAD_PITCH], v[Channel::HEAD_ROLL]];
        let check = |bone: HumanoidBone, part: f32| {
            let (yaw, pitch, roll) = out.pose.get(bone).to_euler(EulerRot::YXZ);
            let want = [bus[0] * part, bus[1] * part, bus[2] * part];
            let got = [yaw, pitch, roll];
            let err = got.iter().zip(want).map(|(g, w)| (g - w).abs()).fold(0.0_f32, f32::max);
            assert!(err <= 1e-4, "step {i}: {bone:?} is {got:?}, want {want:?}");
        };
        check(HumanoidBone::Neck, share);
        check(HumanoidBone::Head, 1.0 - share);
    }
}

#[test]
fn the_consumer_side_allocates_nothing_over_3600_steps() {
    let mut chain = Chain::new(&track(30.0, Some((Seconds(10.0), Seconds(14.0)))));
    for i in 0..600 {
        chain.publish(i);
        chain.perform.advance(mid_step(i));
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 600..4200 {
        // The tracker's side publishes outside the measured window; only the stage's advance is counted.
        chain.publish(i);
        MEASURING.with(|m| m.set(true));
        let _ = chain.perform.advance(mid_step(i));
        MEASURING.with(|m| m.set(false));
    }
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "3600 steps made {allocations} allocations");
}
