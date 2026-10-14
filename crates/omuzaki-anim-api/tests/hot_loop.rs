//! Proves the contract's own per-frame operations allocate nothing (spec §8 "no allocation in the hot loop", §39.3
//! standard 2): blending, buffer clean-up and mirroring, score sampling, pose blending, forward kinematics and skinning.
//!
//! It lives in its own test file, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times, which makes a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{
    BlendMode, Bus, Channel, ChannelBuf, ChannelMask, ChannelRange, ChannelTrack, DelayRamp, Ease, HumanoidBone,
    HumanoidPose, Key, Layer, LayerStack, OneEuroParams, PlayoutDelay, Pose, Response, Score, Skeleton, Transform,
};
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_core::time::Seconds;

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

fn layer(mode: BlendMode, weight: f32, seed: usize) -> Layer {
    let mut layer = Layer::absent(mode);
    for (i, info) in omuzaki_anim_api::CHANNELS.iter().enumerate() {
        layer.values[info.channel] = ((i + seed) % 7) as f32 / 7.0;
    }
    layer.mask = ChannelMask::ALL;
    layer.weight = weight;
    layer
}

#[test]
fn the_per_frame_operations_allocate_nothing() {
    // Everything that allocates is built before the counter starts: stack, score, skeleton, poses.
    let mut stack = LayerStack::new(ChannelBuf::NEUTRAL);
    stack.score = layer(BlendMode::Override, 1.0, 0);
    stack.tracker = layer(BlendMode::Override, 0.8, 1);
    stack.audio = layer(BlendMode::Max, 1.0, 2);
    stack.trigger = layer(BlendMode::Add, 0.3, 3);
    let mut bus = Bus::default();

    let tracks = omuzaki_anim_api::CHANNELS
        .iter()
        .take(20)
        .map(|info| ChannelTrack {
            channel: info.channel,
            initial: 0.0,
            keys: (0..50)
                .map(|i| Key {
                    time: Seconds(f64::from(i) * 0.2),
                    value: (i % 5) as f32 / 5.0,
                    over: 0.1,
                    ease: Ease::OutCubic,
                })
                .collect(),
        })
        .collect();
    let score = Score { duration: Seconds(10.0), tracks, cues: vec![] }.compile().unwrap();
    let mut score_layer = Layer::absent(BlendMode::Override);

    let skeleton = Skeleton::humanoid(&HumanoidBone::ALL, |_| Transform::from_translation(Vec3::Y * 0.1)).unwrap();
    let (local, inverse_bind) = (skeleton.rest_pose(), skeleton.inverse_bind());
    let (mut world, mut skin) = (Pose::IDENTITY, vec![Mat4::IDENTITY; skeleton.len()]);
    let mut pose_a = HumanoidPose::REST;
    pose_a.set(HumanoidBone::Head, Quat::from_rotation_y(0.4));
    let pose_b = HumanoidPose::REST;

    let range = ChannelRange { lo: 0.1, hi: 0.6 };
    let response = Response::built_in(Channel::EYE_BLINK_LEFT);

    // The A/V delay: a boxed ring, as the stage keeps it (35 KB), run past a wrap-around.
    let mut playout = Box::new(PlayoutDelay::<64>::new());
    let mut ramp = DelayRamp::new(Seconds(0.0));
    ramp.set_target(Seconds(0.2));
    let mut delayed = Layer::absent(BlendMode::Override);

    let mut frame = |i: u64| {
        let now = Seconds(i as f64 / 120.0);
        let _ = playout.push(now, stack.tracker);
        let delay = ramp.advance(Seconds(1.0 / 120.0));
        delayed = playout.sample(now, delay).unwrap_or(delayed);
        stack.tracker.weight = 0.5 + 0.5 * ((i % 10) as f32 / 10.0);
        stack.resolve(&mut bus);
        bus.values.sanitize();
        bus.values.mirror();
        bus.values.set_gaze(0.1, -0.1);
        let _ = bus.values.gaze();
        let _ = bus.values.checksum();
        score.sample(Seconds(i as f64 * 0.033), &mut score_layer);
        let _ = score.fired(Seconds(i as f64 * 0.033), Seconds((i + 1) as f64 * 0.033));
        let _ = pose_a.lerp(&pose_b, 0.5).mirrored().checksum();
        skeleton.skin_matrices(&local, &inverse_bind, &mut world, &mut skin);
        let _ = response.apply(range.normalize(0.3), 1.5);
        let _ = OneEuroParams::for_smoothing(0.4, true);
        let _ = bus.owners.human_owns_any(ChannelMask::EYES);
    };
    frame(0); // warm-up

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for i in 1..=1000 {
        frame(i);
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "1000 frames of the contract's hot operations made {allocations} allocations");
}
