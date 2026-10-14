//! `NeutralCapture`: the median rest face over N tracked frames (spec PF-20), checked against the outliers it must
//! ignore, the thresholds it must respect, and `Calibration::rest_at` as the oracle for clean input.
//!
//! The allocation test lives here too, with a counting allocator. It counts only the allocations made on the
//! measuring thread, so the test harness's other threads cannot make it flaky.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use omuzaki_anim::neutral::{MAX_NEUTRAL_FRAMES, MIN_NEUTRAL_FRAMES, NeutralCapture, NeutralError};
use omuzaki_anim_api::{Calibration, Channel, ChannelRange, DeviceProfile, HeadPose, TrackFrame};
use omuzaki_core::glam::Vec3;
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

/// A tracked frame with nothing present and no head.
fn tracked(seq: u64) -> TrackFrame {
    TrackFrame::empty(seq, Seconds(seq as f64 / 30.0), DeviceProfile::Webcam)
}

/// A tracked frame whose head pose is `(yaw, pitch, roll)`, with no position.
fn with_head(seq: u64, yaw: f32, pitch: f32, roll: f32) -> TrackFrame {
    let mut f = tracked(seq);
    f.head = Some(HeadPose { yaw, pitch, roll, position: None });
    f
}

/// Deterministic noise in `-1..1` (xorshift64), so every run sees the same frames.
fn noise(state: &mut u64) -> f32 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    ((*state >> 40) as f32 / (1_u64 << 24) as f32) * 2.0 - 1.0
}

/// The median of `values` (even count: the mean of the two middle values), by sorting a copy.
fn reference_median(mut values: Vec<f32>) -> f32 {
    values.sort_by(f32::total_cmp);
    let mid = values.len() / 2;
    if values.len() % 2 == 1 { values[mid] } else { f32::midpoint(values[mid - 1], values[mid]) }
}

/// Span of a range, the way `Calibration::rest_at` measures it.
fn span(r: ChannelRange) -> f32 {
    r.hi - r.lo
}

#[test]
fn outlier_jaw_frames_do_not_move_the_median() {
    let mut seed = 1;
    let mut cap = NeutralCapture::new(160);
    let mut seq = 0;
    for _ in 0..150 {
        let mut f = tracked(seq);
        f.set(Channel::JAW_OPEN, 0.10 + 0.02 * noise(&mut seed));
        cap.push(&f);
        seq += 1;
    }
    for _ in 0..10 {
        let mut f = tracked(seq);
        f.set(Channel::JAW_OPEN, 0.9);
        cap.push(&f);
        seq += 1;
    }
    let mut base = Calibration::identity(DeviceProfile::Webcam);
    base.set_range(Channel::JAW_OPEN, ChannelRange { lo: 0.0, hi: 0.5 });

    let fitted = cap.finish(&base).unwrap();
    let jaw = fitted.range(Channel::JAW_OPEN);
    assert!((jaw.lo - 0.10).abs() <= 0.005, "lo {} is not within 0.005 of 0.10", jaw.lo);
    assert_eq!(span(jaw), span(base.range(Channel::JAW_OPEN)), "span must be kept exactly");
}

#[test]
fn outlier_head_poses_do_not_move_the_head_rest() {
    // 180 clean poses with noise, then 20 wild ones split evenly above and below. The combined median is then exactly
    // the clean median, so the check is exact rather than a tolerance on a noisy number.
    let mut seed = 7;
    let mut cap = NeutralCapture::new(200);
    let (mut yaws, mut pitches, mut rolls) = (Vec::new(), Vec::new(), Vec::new());
    for seq in 0..180 {
        let (yaw, pitch, roll) =
            (0.2 + 0.02 * noise(&mut seed), -0.1 + 0.02 * noise(&mut seed), 0.05 + 0.02 * noise(&mut seed));
        yaws.push(yaw);
        pitches.push(pitch);
        rolls.push(roll);
        cap.push(&with_head(seq, yaw, pitch, roll));
    }
    for seq in 180..200 {
        let wild = if seq < 190 { -5.0 } else { 5.0 };
        cap.push(&with_head(seq, wild, wild, wild));
    }

    let rest = cap.finish(&Calibration::identity(DeviceProfile::Webcam)).unwrap().head_rest;
    assert!((rest.yaw - reference_median(yaws)).abs() <= 1e-4, "yaw {}", rest.yaw);
    assert!((rest.pitch - reference_median(pitches)).abs() <= 1e-4, "pitch {}", rest.pitch);
    assert!((rest.roll - reference_median(rolls)).abs() <= 1e-4, "roll {}", rest.roll);
}

#[test]
fn a_channel_absent_or_rarely_present_keeps_its_base_range() {
    let mut base = Calibration::identity(DeviceProfile::Webcam);
    base.set_range(Channel::MOUTH_PUCKER, ChannelRange { lo: 0.05, hi: 0.4 });
    base.set_range(Channel::JAW_OPEN, ChannelRange { lo: 0.0, hi: 0.5 });
    base.set_range(Channel::CHEEK_PUFF, ChannelRange { lo: 0.0, hi: 0.5 });

    let mut cap = NeutralCapture::new(150);
    for seq in 0..150 {
        let mut f = tracked(seq);
        // Pucker is never present. Jaw is present in 74 of 150 frames: one short of half, so it must not fit.
        // Cheek puff is present in exactly 75, half, so it fits.
        if seq < 74 {
            f.set(Channel::JAW_OPEN, 0.9);
        }
        if seq < 75 {
            f.set(Channel::CHEEK_PUFF, 0.3);
        }
        cap.push(&f);
    }

    let fitted = cap.finish(&base).unwrap();
    assert_eq!(fitted.range(Channel::MOUTH_PUCKER), base.range(Channel::MOUTH_PUCKER));
    assert_eq!(fitted.range(Channel::JAW_OPEN), base.range(Channel::JAW_OPEN));
    assert_eq!(fitted.range(Channel::CHEEK_PUFF).lo, 0.3, "present in half the frames: it fits");
}

#[test]
fn fourteen_tracked_frames_are_too_few_however_many_are_lost() {
    let mut cap = NeutralCapture::new(20);
    for seq in 0..14 {
        cap.push(&tracked(seq));
    }
    for seq in 14..114 {
        cap.push(&TrackFrame::lost(seq, Seconds(0.0), DeviceProfile::Webcam));
    }
    assert_eq!(cap.progress(), (14, 20));
    let base = Calibration::identity(DeviceProfile::Webcam);
    assert_eq!(cap.finish(&base), Err(NeutralError::TooFewFrames { got: 14, need: MIN_NEUTRAL_FRAMES }));
}

#[test]
fn push_reports_done_exactly_at_the_target_and_ignores_later_frames() {
    let mut cap = NeutralCapture::new(20);
    for seq in 0..19 {
        assert!(!cap.push(&tracked(seq)), "frame {seq} is not yet the target");
    }
    assert!(cap.push(&tracked(19)), "the 20th tracked frame reaches the target");
    assert!(cap.push(&tracked(20)), "later frames stay done");
    assert_eq!(cap.progress(), (20, 20), "frames past the target are not held");
}

#[test]
fn lost_frames_do_not_count_towards_progress() {
    let mut cap = NeutralCapture::new(15);
    for seq in 0..14 {
        cap.push(&tracked(seq));
        cap.push(&TrackFrame::lost(seq, Seconds(0.0), DeviceProfile::Webcam));
    }
    assert_eq!(cap.progress(), (14, 15));
    assert!(cap.push(&tracked(14)));
}

#[test]
fn the_target_is_clamped_to_the_allowed_range() {
    assert_eq!(NeutralCapture::new(3).progress(), (0, MIN_NEUTRAL_FRAMES));
    assert_eq!(NeutralCapture::new(10_000).progress(), (0, MAX_NEUTRAL_FRAMES));
}

/// A frame with every `ARKit` channel present at `level`, and a head pose with position at `head`.
fn full_frame(seq: u64, level: f32, head: f32) -> TrackFrame {
    let mut f = tracked(seq);
    for info in Channel::arkit() {
        f.set(info.channel, level);
    }
    f.head = Some(HeadPose { yaw: head, pitch: head, roll: head, position: Some(Vec3::splat(head)) });
    f
}

#[test]
fn reset_forgets_the_held_frames() {
    let mut cap = NeutralCapture::new(20);
    for seq in 0..20 {
        cap.push(&tracked(seq));
    }
    cap.reset();
    assert_eq!(cap.progress(), (0, 20));
    let base = Calibration::identity(DeviceProfile::Webcam);
    assert_eq!(cap.finish(&base), Err(NeutralError::TooFewFrames { got: 0, need: MIN_NEUTRAL_FRAMES }));
}

#[test]
fn after_a_reset_only_the_new_samples_decide_the_calibration() {
    let base = Calibration::identity(DeviceProfile::Webcam);
    let mut cap = NeutralCapture::new(20);
    // First take: every channel and the full head pose at 0.8.
    for seq in 0..20 {
        assert_eq!(cap.push(&full_frame(seq, 0.8, 0.8)), seq == 19);
    }
    cap.reset();
    // Second take: only jawOpen and the head angles (no position), at 0.2. Stale counts or samples would show as
    // other channels or a position still fitted from the first take, or as a median pulled towards 0.8.
    for seq in 0..20 {
        let mut f = with_head(seq, 0.2, 0.2, 0.2);
        f.set(Channel::JAW_OPEN, 0.2);
        cap.push(&f);
    }
    let fitted = cap.finish(&base).unwrap();
    assert_eq!(fitted.range(Channel::JAW_OPEN).lo, 0.2);
    assert_eq!(fitted.range(Channel::CHEEK_PUFF), base.range(Channel::CHEEK_PUFF), "absent in the second take");
    assert_eq!((fitted.head_rest.yaw, fitted.head_rest.pitch, fitted.head_rest.roll), (0.2, 0.2, 0.2));
    assert_eq!(fitted.head_rest.position, base.head_rest.position, "no position in the second take");

    // A third take refills the capture: it matches a capture that never saw the first take.
    cap.reset();
    let mut fresh = NeutralCapture::new(20);
    for seq in 0..20 {
        let f = full_frame(seq, 0.1 + 0.01 * (seq % 7) as f32, 0.05 * (seq % 5) as f32);
        cap.push(&f);
        fresh.push(&f);
    }
    assert_eq!(cap.finish(&base).unwrap(), fresh.finish(&base).unwrap());
}

#[test]
fn finish_twice_gives_the_same_result() {
    let mut cap = NeutralCapture::new(16);
    for seq in 0..16 {
        let mut f = with_head(seq, 0.1 * seq as f32, 0.0, 0.0);
        f.set(Channel::JAW_OPEN, 0.01 * seq as f32);
        cap.push(&f);
    }
    let base = Calibration::identity(DeviceProfile::Webcam);
    let first = cap.finish(&base).unwrap();
    assert_eq!(cap.finish(&base).unwrap(), first);
}

#[test]
fn an_even_count_takes_the_mean_of_the_two_middle_values() {
    // Eight frames at 0.2 and eight at 0.6: the middle two are 0.2 and 0.6, so the median is 0.4. The five lost frames
    // carry 0.99, which must not count.
    let mut cap = NeutralCapture::new(16);
    for seq in 0..16 {
        let mut f = tracked(seq);
        f.set(Channel::JAW_OPEN, if seq % 2 == 0 { 0.2 } else { 0.6 });
        cap.push(&f);
    }
    for seq in 16..21 {
        let mut f = TrackFrame::lost(seq, Seconds(0.0), DeviceProfile::Webcam);
        f.set(Channel::JAW_OPEN, 0.99);
        cap.push(&f);
    }
    let fitted = cap.finish(&Calibration::identity(DeviceProfile::Webcam)).unwrap();
    assert!((fitted.range(Channel::JAW_OPEN).lo - 0.4).abs() < 1e-6);
}

#[test]
fn identical_frames_match_calibration_rest_at() {
    let base = Calibration::from_gains(DeviceProfile::Webcam, |_| 2.0);
    let mut frame = with_head(0, 0.1, -0.2, 0.3);
    frame.head = Some(HeadPose { yaw: 0.1, pitch: -0.2, roll: 0.3, position: Some(Vec3::new(0.1, 0.2, 0.6)) });
    frame.set(Channel::JAW_OPEN, 0.3);
    frame.set(Channel::CHEEK_PUFF, 0.7);
    frame.set(Channel::MOUTH_PUCKER, 0.2);

    let mut oracle = base;
    oracle.rest_at(&frame);

    let mut cap = NeutralCapture::new(20);
    for _ in 0..20 {
        cap.push(&frame);
    }
    assert_eq!(cap.finish(&base).unwrap(), oracle);
}

#[test]
fn push_and_finish_allocate_nothing() {
    // Build every frame before counting, so only `push` and `finish` are measured.
    let frames: Vec<TrackFrame> = (0..MAX_NEUTRAL_FRAMES as u64)
        .map(|seq| {
            let mut f = with_head(seq, 0.01 * (seq % 9) as f32, 0.0, 0.0);
            f.head = Some(HeadPose { position: Some(Vec3::new(0.0, 0.1, 0.5)), ..f.head.unwrap_or_default() });
            f.set(Channel::JAW_OPEN, 0.1 + 0.001 * (seq % 13) as f32);
            f.set(Channel::CHEEK_PUFF, 0.2);
            f
        })
        .collect();
    let base = Calibration::identity(DeviceProfile::Webcam);
    let mut cap = NeutralCapture::new(MAX_NEUTRAL_FRAMES);

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for frame in &frames {
        cap.push(frame);
    }
    let fitted = cap.finish(&base);
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;

    assert!(fitted.is_ok());
    assert_eq!(cap.progress(), (MAX_NEUTRAL_FRAMES, MAX_NEUTRAL_FRAMES));
    assert_eq!(allocations, 0, "{allocations} allocations over 600 pushes and a finish");
}

/// The `push`/`finish` cost at the largest capture, every channel and the whole head pose present, against the live
/// frame (spec §10: 60 fps is 16.67 ms; `cpu.total` allows 8 ms of CPU per frame at 30 fps, tracker included).
///
/// Wall time is a stopwatch, not the CPU-time counters §10 asks for, so the bounds are loose: a thousandth of a frame
/// for `push` and a tenth for `finish` (a whole frame in a debug build). Run `cargo test -p omuzaki-anim --release --test neutral
/// push_and_finish_fit_the_frame_budget -- --nocapture` to print the numbers.
#[test]
fn push_and_finish_fit_the_frame_budget() {
    const FRAME_MS: f64 = 1000.0 / 60.0;
    const RUNS: usize = 200;
    let frames: Vec<TrackFrame> = (0..MAX_NEUTRAL_FRAMES as u64)
        .map(|seq| full_frame(seq, 0.1 + 0.001 * (seq % 17) as f32, 0.01 * (seq % 11) as f32))
        .collect();
    let base = Calibration::identity(DeviceProfile::Webcam);
    let mut cap = NeutralCapture::new(MAX_NEUTRAL_FRAMES);
    let mut pushes = Vec::with_capacity(RUNS * MAX_NEUTRAL_FRAMES);
    let mut finishes = Vec::with_capacity(RUNS);
    for _ in 0..RUNS {
        cap.reset();
        for frame in &frames {
            let start = Instant::now();
            std::hint::black_box(cap.push(std::hint::black_box(frame)));
            pushes.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        let start = Instant::now();
        let fitted = cap.finish(std::hint::black_box(&base));
        finishes.push(start.elapsed().as_secs_f64() * 1000.0);
        assert!(fitted.is_ok());
    }
    let (push_mean, push_p95) = mean_and_p95(&mut pushes);
    let (finish_mean, finish_p95) = mean_and_p95(&mut finishes);
    println!("push   (52 channels + head + position): mean {:.2} us, p95 {:.2} us", push_mean * 1e3, push_p95 * 1e3);
    println!(
        "finish (600 frames, 58 columns):        mean {:.1} us, p95 {:.1} us",
        finish_mean * 1e3,
        finish_p95 * 1e3
    );
    println!("a 60 fps frame is {FRAME_MS:.2} ms");
    assert!(push_p95 < FRAME_MS / 1000.0, "push p95 {push_p95} ms");
    // An unoptimised build runs this about 35 times slower, so it is held to a whole frame instead.
    let finish_limit = if cfg!(debug_assertions) { FRAME_MS } else { FRAME_MS / 10.0 };
    assert!(finish_p95 < finish_limit, "finish p95 {finish_p95} ms");
}

/// `(mean, p95)` of `samples`, which it sorts.
fn mean_and_p95(samples: &mut [f64]) -> (f64, f64) {
    samples.sort_by(f64::total_cmp);
    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
    (mean, samples[(samples.len() * 95 / 100).min(samples.len() - 1)])
}
