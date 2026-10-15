//! `PerformHost` over a real `Performer` (PF-20, PF-21): the commands cross a bounded queue, the status comes back
//! through a shared record, and the perform thread's `advance` stays allocation- and wait-free.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::float_cmp)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{
    Calibration, ChannelBuf, DeviceProfile, FrameSource, HumanoidPose, LifeConfig, RetargetConfig, TrackFrame,
};
use omuzaki_anim_mock::animator::MockAnimator;
use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};
use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::host::{
    COMMAND_QUEUE_LEN, CalibrationState, PerformControl, PerformError, PerformHandle, PerformHost, RetargetChange,
    perform_channel,
};
use omuzaki_stage::perform::{Performer, PerformerParts, STEP_HZ};

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

/// A tracker that never delivers a frame.
struct Silent;

impl FrameSource for Silent {
    fn poll(&mut self, _now: Seconds) -> Option<TrackFrame> {
        None
    }
}

fn performer_with(source: Box<dyn FrameSource>) -> Performer {
    let parts = PerformerParts {
        source,
        retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
        life: Box::new(Life::new(LifeConfig::new(7))),
        animator: Box::new(MockAnimator),
    };
    Performer::new(parts, ChannelBuf::NEUTRAL, HumanoidPose::REST)
}

fn replay(seconds: f32) -> Box<dyn FrameSource> {
    Box::new(TrackReplay::new(synthetic_track(&SyntheticSpec { seconds, ..SyntheticSpec::default() })))
}

/// The time the middle of step `i` falls at, so one `advance` runs exactly one step.
fn mid_step(i: u64) -> Seconds {
    Seconds((i as f64 + 0.5) / f64::from(STEP_HZ))
}

/// A performer on `source` with its two channel ends, advanced once so the first status is published.
fn rig(source: Box<dyn FrameSource>) -> (PerformHandle, PerformControl, Performer) {
    let (handle, mut control) = perform_channel();
    let mut performer = performer_with(source);
    control.advance(&mut performer, mid_step(0));
    (handle, control, performer)
}

fn step(control: &mut PerformControl, performer: &mut Performer, i: u64) {
    control.advance(performer, mid_step(i)).expect("one step per call");
}

#[test]
fn calibrate_takes_150_tracked_frames_then_applies_the_new_calibration() {
    let (host, mut control, mut performer) = rig(replay(20.0));
    let before = host.calibration();
    assert_eq!(before, *performer.retarget().calibration(), "the first status carries the performer's own");
    assert_eq!(host.status().calibration, CalibrationState::Idle);
    host.calibrate(150).unwrap();

    let (mut saw_capturing, mut done_at) = (false, None);
    for i in 1..=600 {
        step(&mut control, &mut performer, i);
        match host.status().calibration {
            CalibrationState::Capturing { frames, of } => {
                assert_eq!(of, 150);
                assert!(frames <= of);
                saw_capturing = true;
            }
            CalibrationState::Done { frames } => {
                assert_eq!(frames, 150);
                done_at = Some(i);
                break;
            }
            other => assert_eq!(other, CalibrationState::Idle, "only the first step may still read idle"),
        }
    }
    assert!(saw_capturing, "the status showed the capture running");
    // The track delivers 30 frames a second to a 60 Hz loop: 150 frames take about 300 steps.
    let done_at = done_at.expect("the capture finishes");
    assert!((299..=305).contains(&done_at), "done at step {done_at}");
    assert_ne!(host.calibration(), before, "a webcam track reads low, so the new rest differs");
    assert_eq!(host.calibration(), *performer.retarget().calibration(), "and it is the one in use");
}

#[test]
fn a_lost_source_fails_the_capture_after_600_steps() {
    let (host, mut control, mut performer) = rig(Box::new(Silent));
    host.calibrate(150).unwrap();
    // Step 0 ran before the request, so the capture's 600th step is step 600.
    for i in 1..=599 {
        step(&mut control, &mut performer, i);
        assert_eq!(host.status().calibration, CalibrationState::Capturing { frames: 0, of: 150 }, "step {i}");
    }
    step(&mut control, &mut performer, 600);
    assert_eq!(host.status().calibration, CalibrationState::Failed { got: 0, need: 150 });
    host.calibrate(150).expect("a failed capture can be asked for again");
}

#[test]
fn a_second_calibrate_while_one_runs_is_refused_and_a_finished_one_frees_the_slot() {
    let (host, mut control, mut performer) = rig(replay(20.0));
    host.calibrate(30).unwrap();
    assert_eq!(host.calibrate(30), Err(PerformError::CalibrationRunning));
    for i in 1..=200 {
        step(&mut control, &mut performer, i);
    }
    assert_eq!(host.status().calibration, CalibrationState::Done { frames: 30 });
    host.calibrate(30).expect("the slot is free again");
}

#[test]
fn a_full_queue_is_refused_and_leaves_no_capture_marked_running() {
    let (host, mut control, mut performer) = rig(replay(5.0));
    for _ in 0..COMMAND_QUEUE_LEN {
        host.set_retarget(RetargetChange { mirror: Some(true), ..RetargetChange::default() }).unwrap();
    }
    let change = RetargetChange { mirror: Some(false), ..RetargetChange::default() };
    assert_eq!(host.set_retarget(change), Err(PerformError::QueueFull));
    assert_eq!(host.set_calibration(*performer.retarget().calibration()), Err(PerformError::QueueFull));
    assert_eq!(host.calibrate(150), Err(PerformError::QueueFull));
    step(&mut control, &mut performer, 1);
    host.calibrate(150).expect("the refused calibrate did not leave the slot taken");
}

#[test]
fn a_dropped_control_disconnects_the_handle() {
    let (host, control, _performer) = rig(replay(5.0));
    drop(control);
    assert_eq!(host.calibrate(150), Err(PerformError::Disconnected));
    assert_eq!(host.calibrate(150), Err(PerformError::Disconnected), "and the slot stays free");
    assert_eq!(host.set_retarget(RetargetChange::default()), Err(PerformError::Disconnected));
    assert_eq!(host.set_calibration(host.calibration()), Err(PerformError::Disconnected));
    assert_eq!(host.status().calibration, CalibrationState::Idle, "the last status can still be read");
}

#[test]
fn a_control_dropped_mid_capture_frees_the_slot_so_the_handle_says_disconnected() {
    let (host, control, _performer) = rig(replay(5.0));
    host.calibrate(150).unwrap();
    drop(control);
    assert_eq!(host.calibrate(150), Err(PerformError::Disconnected));
}

#[test]
fn a_performer_reset_during_a_capture_does_not_break_the_stall_clock() {
    let (host, mut control, mut performer) = rig(Box::new(Silent));
    host.calibrate(150).unwrap();
    for i in 1..=100 {
        step(&mut control, &mut performer, i);
    }
    performer.reset();
    // The tick index restarts at 0, below the capture's anchor.
    for i in 0..=700 {
        step(&mut control, &mut performer, i);
    }
    assert!(matches!(host.status().calibration, CalibrationState::Failed { got: 0, need: 150 }));
}

#[test]
fn set_retarget_merges_into_the_config_in_use_and_publishes_it() {
    let (host, mut control, mut performer) = rig(replay(5.0));
    let mut want = RetargetConfig::default();
    assert_eq!(host.status().retarget, want);
    let first = RetargetChange { smoothing: Some(0.8), mirror: Some(true), ..RetargetChange::default() };
    (want.smoothing, want.mirror) = (0.8, true);
    assert_eq!(host.set_retarget(first).unwrap(), want, "returns the full config");
    let second = RetargetChange { head_gain: Some(2.0), ..RetargetChange::default() };
    want.head_gain = 2.0;
    assert_eq!(host.set_retarget(second).unwrap(), want, "the second change keeps the first");
    step(&mut control, &mut performer, 1);
    assert_eq!(*performer.retarget().config(), want);
    assert_eq!(host.status().retarget, want);
}

#[test]
fn set_calibration_replaces_the_calibration_in_use() {
    let (host, mut control, mut performer) = rig(replay(5.0));
    let new = Calibration::identity(DeviceProfile::Arkit);
    host.set_calibration(new).unwrap();
    step(&mut control, &mut performer, 1);
    assert_eq!(*performer.retarget().calibration(), new);
    assert_eq!(host.calibration(), new);
}

#[test]
fn four_threads_reading_and_writing_while_the_performer_steps_neither_deadlock_nor_see_torn_status() {
    let (host, mut control, mut performer) = rig(replay(60.0));
    // One change sets both dials, so a consistent status always has `exaggeration == 1 + smoothing`.
    let both = |k: u32| RetargetChange {
        smoothing: Some(k as f32 / 10.0),
        exaggeration: Some(1.0 + k as f32 / 10.0),
        ..RetargetChange::default()
    };
    host.set_retarget(both(0)).unwrap();
    step(&mut control, &mut performer, 1);
    let consistent = |c: &RetargetConfig| (c.exaggeration - (1.0 + c.smoothing)).abs() < 1e-6;
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for t in 0..4_u32 {
            let (host, stop) = (&host, &stop);
            scope.spawn(move || {
                let mut k = t;
                while !stop.load(Ordering::Acquire) {
                    k = (k + 1) % 11;
                    match host.set_retarget(both(k)) {
                        Ok(config) => assert!(consistent(&config), "{config:?}"),
                        Err(PerformError::QueueFull) => {}
                        Err(e) => panic!("{e}"),
                    }
                    let status = host.status();
                    assert!(consistent(&status.retarget), "{status:?}");
                    if let CalibrationState::Capturing { frames, of } = status.calibration {
                        assert!(frames <= of);
                    }
                }
            });
        }
        for i in 2..3602 {
            step(&mut control, &mut performer, i);
        }
        stop.store(true, Ordering::Release);
    });
    assert!(host.status().source_fps > 0.0);
}

#[test]
fn advance_allocates_nothing_over_3600_quiet_steps() {
    let (_host, mut control, mut performer) = rig(replay(70.0));
    for i in 1..10 {
        step(&mut control, &mut performer, i);
    }
    MEASURING.with(|m| m.set(true));
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 10..3610 {
        step(&mut control, &mut performer, i);
    }
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - before;
    MEASURING.with(|m| m.set(false));
    assert_eq!(allocated, 0, "advance allocated {allocated} times");
}
