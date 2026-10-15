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
use std::sync::{Arc, Barrier};
use std::time::Duration;

use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim_api::{
    Calibration, Channel, ChannelBuf, ChannelRange, DeviceProfile, FrameSource, HeadPose, HumanoidPose, Layer,
    LifeConfig, Response, Retarget, RetargetConfig, TrackFrame,
};
use omuzaki_anim_mock::animator::MockAnimator;
use omuzaki_anim_mock::track::{SyntheticSpec, TrackReplay, synthetic_track};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::host::{
    COMMAND_QUEUE_LEN, CalibrationState, MAX_COMMANDS_PER_ADVANCE, PerformControl, PerformError, PerformHandle,
    PerformHost, RetargetChange, perform_channel,
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
    performer_on(source, Box::new(ArkitRetarget::new(DeviceProfile::Webcam)))
}

fn performer_on(source: Box<dyn FrameSource>, retarget: Box<dyn Retarget>) -> Performer {
    let parts = PerformerParts {
        source,
        retarget,
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
    rig_on(performer_with(source))
}

fn rig_on(mut performer: Performer) -> (PerformHandle, PerformControl, Performer) {
    let (handle, mut control) = perform_channel(&performer);
    control.advance(&mut performer, mid_step(0));
    (handle, control, performer)
}

/// How long a threaded test waits for the performer to apply a change.
const SHORT_WAIT: Duration = Duration::from_millis(50);

/// A retarget whose `set_config` runs unmeasured: its contract lets it compile tables (and free the old config)
/// between frames, so the allocation test counts the host's own work and not that.
struct Quiet(ArkitRetarget);

impl Retarget for Quiet {
    fn apply(&mut self, frame: &TrackFrame, dt: f32, out: &mut Layer) {
        self.0.apply(frame, dt, out);
    }
    fn calibration(&self) -> &Calibration {
        self.0.calibration()
    }
    fn set_calibration(&mut self, calibration: Calibration) {
        self.0.set_calibration(calibration);
    }
    fn config(&self) -> &RetargetConfig {
        self.0.config()
    }
    fn set_config(&mut self, config: RetargetConfig) {
        let was = MEASURING.with(Cell::get);
        MEASURING.with(|m| m.set(false));
        self.0.set_config(config);
        MEASURING.with(|m| m.set(was));
    }
    fn reset(&mut self) {
        self.0.reset();
    }
}

/// A calibration whose every field depends on `seed`, so two seeds differ everywhere.
fn distinct_calibration(seed: f32, profile: DeviceProfile, position: Option<Vec3>) -> Calibration {
    let mut calibration = Calibration::identity(profile);
    for (i, info) in Channel::arkit().iter().enumerate() {
        let lo = seed + i as f32 * 0.01;
        calibration.set_range(info.channel, ChannelRange { lo, hi: lo + 1.0 + seed });
    }
    calibration.head_rest = HeadPose { yaw: seed + 0.1, pitch: seed + 0.2, roll: seed + 0.3, position };
    calibration
}

/// A config that is not the default and has per-channel responses (so cloning it allocates).
fn custom_config() -> RetargetConfig {
    let mut config = RetargetConfig { smoothing: 0.9, ..RetargetConfig::default() };
    config.responses.insert(Channel::JAW_OPEN, Response::built_in(Channel::JAW_OPEN));
    config.responses.insert(Channel::EYE_BLINK_LEFT, Response::built_in(Channel::EYE_BLINK_LEFT));
    config
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
    // Step 0 ran before the request and step 1 took it, so the capture's 600 steps are steps 2 to 601.
    for i in 1..=600 {
        step(&mut control, &mut performer, i);
        assert_eq!(host.status().calibration, CalibrationState::Capturing { frames: 0, of: 150 }, "step {i}");
    }
    step(&mut control, &mut performer, 601);
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
        host.request_retarget(RetargetChange { mirror: Some(true), ..RetargetChange::default() }).unwrap();
    }
    let change = RetargetChange { mirror: Some(false), ..RetargetChange::default() };
    assert_eq!(host.request_retarget(change), Err(PerformError::QueueFull));
    assert_eq!(host.set_retarget(change), Err(PerformError::QueueFull), "and set_retarget does not wait on a refusal");
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
fn a_reset_late_in_a_session_still_fails_a_stalled_capture_after_600_steps() {
    let (host, mut control, mut performer) = rig(Box::new(Silent));
    for i in 1..=60_000 {
        step(&mut control, &mut performer, i);
    }
    host.calibrate(150).unwrap();
    for i in 60_001..=60_100 {
        step(&mut control, &mut performer, i);
    }
    // The performer's own tick index restarts at the next `advance`; the control counts its steps itself.
    performer.reset();
    for i in 60_101..=60_600 {
        step(&mut control, &mut performer, i);
        assert!(matches!(host.status().calibration, CalibrationState::Capturing { .. }), "step {i}");
    }
    step(&mut control, &mut performer, 60_601);
    assert_eq!(host.status().calibration, CalibrationState::Failed { got: 0, need: 150 });
}

#[test]
fn a_loop_that_advances_once_a_second_still_fails_a_silent_capture_after_10_seconds() {
    let (host, mut control, mut performer) = rig(Box::new(Silent));
    host.calibrate(150).unwrap();
    // One call a second: 60 ticks come due each time and all but 4 are skipped.
    let call = |control: &mut PerformControl, performer: &mut Performer, second: u32| {
        control.advance(performer, Seconds(f64::from(second) + 0.5 / 60.0)).expect("steps are due");
    };
    // The capture starts in the first call (second 1), so its 10 s end in the call at second 11.
    for second in 1..=10 {
        call(&mut control, &mut performer, second);
        assert!(matches!(host.status().calibration, CalibrationState::Capturing { .. }), "second {second}");
    }
    call(&mut control, &mut performer, 11);
    assert_eq!(host.status().calibration, CalibrationState::Failed { got: 0, need: 150 });
    assert!(control.skipped_steps() > 0, "the loop really skipped ticks");
}

#[test]
fn a_capture_asked_for_during_a_pause_gets_its_full_window_when_the_loop_resumes() {
    // Silent tracker: the window is the whole story.
    let (host, mut control, mut performer) = rig(Box::new(Silent));
    // The loop pauses for 10 s; the request arrives during the pause and is taken by the first call after it.
    host.calibrate(150).unwrap();
    let resume = |control: &mut PerformControl, performer: &mut Performer, seconds: f64| {
        control.advance(performer, Seconds(seconds)).expect("steps are due");
    };
    resume(&mut control, &mut performer, 10.5);
    assert!(matches!(host.status().calibration, CalibrationState::Capturing { .. }), "not failed at once");
    resume(&mut control, &mut performer, 19.5);
    assert!(matches!(host.status().calibration, CalibrationState::Capturing { .. }), "still inside its window");
    resume(&mut control, &mut performer, 21.0);
    assert_eq!(host.status().calibration, CalibrationState::Failed { got: 0, need: 150 }, "and 10 s later it ends");

    // With a tracker that is there, the same pause leaves the capture its full window to finish in.
    let (host, mut control, mut performer) = rig(replay(60.0));
    host.calibrate(30).unwrap();
    resume(&mut control, &mut performer, 10.5);
    for i in 0..200_u32 {
        resume(&mut control, &mut performer, 10.5 + f64::from(i + 1) / 60.0);
    }
    assert_eq!(host.status().calibration, CalibrationState::Done { frames: 30 });
}

#[test]
fn a_ten_second_pause_fails_the_capture_even_if_a_frame_arrives_after_it() {
    let (host, mut control, mut performer) = rig(replay(60.0));
    host.calibrate(150).unwrap();
    for i in 1..=20 {
        step(&mut control, &mut performer, i);
    }
    let CalibrationState::Capturing { frames, .. } = host.status().calibration else { panic!("not capturing") };
    assert!(frames > 0, "the capture had started");
    // The loop stalls for 10 s; the tracker is fine and its next frame is there when the loop resumes.
    control.advance(&mut performer, Seconds(10.5 + 21.0 / 60.0)).expect("steps are due");
    assert_eq!(host.status().calibration, CalibrationState::Failed { got: frames, need: 150 });
}

#[test]
fn the_frame_rate_reads_the_trackers_30_fps_off_the_frames_own_timestamps() {
    let (host, mut control, mut performer) = rig(replay(20.0));
    for i in 1..=300 {
        step(&mut control, &mut performer, i);
    }
    let fps = host.status().source_fps;
    assert!((fps - 30.0).abs() < 0.5, "frame rate {fps}");
}

#[test]
fn the_controls_reset_restarts_the_performer_and_its_timeline_together() {
    let (host, mut control, mut performer) = rig(replay(20.0));
    for i in 1..=120 {
        step(&mut control, &mut performer, i);
    }
    assert_eq!(host.status().tracking, omuzaki_stage::perform::Tracking::Tracking);
    control.reset(&mut performer);
    assert_eq!(host.status().tracking, omuzaki_stage::perform::Tracking::NoFrames, "and the status says so at once");
    assert_eq!(host.status().source_fps, 0.0);
    let out = control.advance(&mut performer, mid_step(0)).expect("a step is due");
    assert_eq!(out.tick.index, 0, "the next output is tick 0");
    let out = control.advance(&mut performer, mid_step(1)).expect("a step is due");
    assert_eq!(out.tick.index, 1);
}

#[test]
fn a_reset_during_a_capture_restarts_its_stall_clock_with_the_timeline() {
    let (host, mut control, mut performer) = rig(Box::new(Silent));
    host.calibrate(150).unwrap();
    for i in 1..=300 {
        step(&mut control, &mut performer, i);
    }
    control.reset(&mut performer);
    // The new timeline starts at tick 0, and the capture's window with it: 600 ticks from the first call after.
    for j in 0..600 {
        control.advance(&mut performer, mid_step(j)).expect("a step is due");
        assert!(matches!(host.status().calibration, CalibrationState::Capturing { .. }), "call {j}");
    }
    control.advance(&mut performer, mid_step(600)).expect("a step is due");
    assert_eq!(host.status().calibration, CalibrationState::Failed { got: 0, need: 150 });
}

#[test]
fn every_catch_up_step_feeds_the_capture_and_the_frame_rate() {
    // 30 fps arrives on every other 60 Hz step; calls come every two steps, so each call's frame is on its FIRST step.
    let (host, mut control, mut performer) = rig(replay(20.0));
    host.calibrate(30).unwrap();
    let mut done_at = None;
    for k in 0..200_u64 {
        let out = control.advance(&mut performer, mid_step(2 * k + 1)).expect("two steps due");
        assert_eq!(out.tick.index, 2 * k + 1, "the call ran steps {} and {}", 2 * k, 2 * k + 1);
        if matches!(host.status().calibration, CalibrationState::Done { .. }) {
            done_at = Some(k);
            break;
        }
    }
    let done_at = done_at.expect("the capture sees the frames that arrive on the first step of each call");
    assert!(done_at < 40, "30 frames at 30 fps take about 31 calls, took {done_at}");
    let fps = host.status().source_fps;
    assert!((fps - 30.0).abs() < 3.0, "frame rate {fps}");
}

#[test]
fn a_handle_made_from_the_performer_starts_from_its_real_settings() {
    let mut performer = performer_with(replay(5.0));
    performer.retarget_mut().set_config(custom_config());
    let own_calibration = *performer.retarget().calibration();
    let (host, mut control) = perform_channel(&performer);
    // Before any `advance`: no defaults, no stand-ins.
    assert_eq!(host.status().retarget, custom_config());
    assert_eq!(host.calibration(), own_calibration);
    let ticket = host.request_retarget(RetargetChange { head_gain: Some(2.0), ..RetargetChange::default() }).unwrap();
    control.advance(&mut performer, mid_step(0));
    let mut want = custom_config();
    want.head_gain = 2.0;
    assert_eq!(host.await_retarget(ticket, Duration::ZERO), Ok(want.clone()), "merged into the real base");
    assert_eq!(*performer.retarget().config(), want);
}

#[test]
fn status_and_replies_report_only_what_the_performer_applied() {
    let (host, mut control, mut performer) = rig(replay(5.0));
    let before = host.status().retarget;
    let a = host.request_retarget(RetargetChange { mirror: Some(true), ..RetargetChange::default() }).unwrap();
    let b = host.request_retarget(RetargetChange { head_gain: Some(2.0), ..RetargetChange::default() }).unwrap();
    assert_eq!(host.status().retarget, before, "queued is not applied");
    assert_eq!(host.await_retarget(b, Duration::from_millis(20)), Err(PerformError::Stalled));
    assert_eq!(host.status().retarget, before, "and a timed-out wait changes nothing");
    step(&mut control, &mut performer, 1);
    let mut want = before.clone();
    (want.mirror, want.head_gain) = (true, 2.0);
    assert_eq!(host.await_retarget(a, Duration::ZERO), Ok(want.clone()));
    assert_eq!(host.status().retarget, want);
    assert_eq!(*performer.retarget().config(), want);
}

#[test]
fn a_wait_for_a_request_the_control_never_applies_ends_when_the_control_is_gone() {
    let (host, control, _performer) = rig(replay(5.0));
    let before = host.status().retarget;
    let ticket = host.request_retarget(RetargetChange { mirror: Some(true), ..RetargetChange::default() }).unwrap();
    drop(control);
    assert_eq!(host.await_retarget(ticket, Duration::from_secs(5)), Err(PerformError::Disconnected));
    assert_eq!(host.status().retarget, before, "the unapplied request never shows");
}

#[test]
fn set_retarget_waits_for_the_performer_and_returns_the_applied_settings() {
    let (host, mut control, mut performer) = rig(replay(5.0));
    let stop = AtomicBool::new(false);
    let reply = std::thread::scope(|scope| {
        let stepper = scope.spawn(|| {
            let mut i = 1;
            while !stop.load(Ordering::Acquire) {
                step(&mut control, &mut performer, i);
                i += 1;
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let reply = host.set_retarget(RetargetChange { smoothing: Some(0.8), ..RetargetChange::default() });
        stop.store(true, Ordering::Release);
        stepper.join().unwrap();
        reply
    });
    assert_eq!(reply.unwrap().smoothing, 0.8);
    assert_eq!(host.status().retarget.smoothing, 0.8);
}

#[test]
fn forty_captures_back_to_back_with_no_reads_keep_the_newest_calibration() {
    let (host, mut control, mut performer) = rig(replay(80.0));
    let mut i = 1;
    for _ in 0..40 {
        host.calibrate(30).unwrap();
        // About 62 steps for 30 frames at 30 fps, plus the step that sets the calibration.
        for _ in 0..80 {
            step(&mut control, &mut performer, i);
            i += 1;
        }
    }
    assert_eq!(host.status().calibration, CalibrationState::Done { frames: 30 });
    assert_eq!(host.calibration(), *performer.retarget().calibration(), "the mailbox holds the newest, not an old one");
}

#[test]
fn readers_never_see_a_torn_calibration_while_the_performer_replaces_it() {
    let (host, mut control, mut performer) = rig(replay(60.0));
    // Every word differs between the two (profile, each range, head angles, the position flag and the position), so a
    // read that mixes them matches neither.
    let a = distinct_calibration(0.1, DeviceProfile::Arkit, Some(Vec3::new(1.0, 2.0, 3.0)));
    let b = distinct_calibration(0.5, DeviceProfile::Webcam, None);
    assert_ne!(a, b);
    let initial = host.calibration();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                while !stop.load(Ordering::Acquire) {
                    let seen = host.calibration();
                    assert!(seen == a || seen == b || seen == initial, "torn read: {seen:?}");
                }
            });
        }
        for i in 1..3000 {
            host.set_calibration(if i % 2 == 0 { a } else { b }).unwrap();
            step(&mut control, &mut performer, i);
        }
        stop.store(true, Ordering::Release);
    });
}

#[test]
fn a_command_flood_cannot_starve_the_steps() {
    let (host, mut control, mut performer) = rig(replay(5.0));
    let change = RetargetChange { mirror: Some(true), ..RetargetChange::default() };
    for _ in 0..COMMAND_QUEUE_LEN {
        host.request_retarget(change).unwrap();
    }
    step(&mut control, &mut performer, 1);
    for _ in 0..MAX_COMMANDS_PER_ADVANCE {
        host.request_retarget(change).expect("one call took exactly its share and freed that many slots");
    }
    assert_eq!(host.request_retarget(change), Err(PerformError::QueueFull));

    // And with a producer refilling the queue as fast as it can while the performer steps.
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Acquire) {
                let _ = host.request_retarget(change);
            }
        });
        for i in 2..800 {
            let out = control.advance(&mut performer, mid_step(i)).expect("a step is due on every call");
            assert_eq!(out.tick.index, i);
        }
        stop.store(true, Ordering::Release);
    });
}

#[test]
fn a_calibration_racing_the_drop_never_leaves_the_slot_taken() {
    for round in 0..500 {
        let (host, control, _performer) = rig(replay(1.0));
        let barrier = Arc::new(Barrier::new(2));
        let racer = {
            let (host, barrier) = (host.clone(), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                let _ = host.calibrate(150);
            })
        };
        barrier.wait();
        drop(control);
        racer.join().unwrap();
        assert_eq!(host.calibrate(150), Err(PerformError::Disconnected), "round {round}");
    }
}

#[test]
fn requests_compose_in_order_and_the_reply_is_the_applied_config() {
    let (host, mut control, mut performer) = rig(replay(5.0));
    let mut want = RetargetConfig::default();
    assert_eq!(host.status().retarget, want);
    let first = RetargetChange { smoothing: Some(0.8), mirror: Some(true), ..RetargetChange::default() };
    let second = RetargetChange { head_gain: Some(2.0), ..RetargetChange::default() };
    let (t1, t2) = (host.request_retarget(first).unwrap(), host.request_retarget(second).unwrap());
    step(&mut control, &mut performer, 1);
    (want.smoothing, want.mirror) = (0.8, true);
    want.head_gain = 2.0;
    assert_eq!(host.await_retarget(t1, Duration::ZERO), Ok(want.clone()), "the second change keeps the first");
    assert_eq!(host.await_retarget(t2, Duration::ZERO), Ok(want.clone()));
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
    let first = host.request_retarget(both(0)).unwrap();
    step(&mut control, &mut performer, 1);
    host.await_retarget(first, Duration::ZERO).unwrap();
    let consistent = |c: &RetargetConfig| (c.exaggeration - (1.0 + c.smoothing)).abs() < 1e-6;
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for t in 0..4_u32 {
            let (host, stop) = (&host, &stop);
            scope.spawn(move || {
                let mut k = t;
                while !stop.load(Ordering::Acquire) {
                    k = (k + 1) % 11;
                    let reply = host.request_retarget(both(k)).and_then(|t| host.await_retarget(t, SHORT_WAIT));
                    match reply {
                        Ok(config) => assert!(consistent(&config), "{config:?}"),
                        // Busy or stopped stepping at the end of the run: both are fine here.
                        Err(PerformError::QueueFull | PerformError::Stalled) => {}
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

/// Runs `f` with allocation counting off: the handle's side may allocate.
fn unmeasured<T>(f: impl FnOnce() -> T) -> T {
    MEASURING.with(|m| m.set(false));
    let out = f();
    MEASURING.with(|m| m.set(true));
    out
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

#[test]
fn advance_allocates_nothing_with_responses_configured_and_commands_arriving() {
    let mut inner = ArkitRetarget::new(DeviceProfile::Webcam);
    inner.set_config(custom_config());
    let (host, mut control, mut performer) = rig_on(performer_on(replay(70.0), Box::new(Quiet(inner))));
    for i in 1..10 {
        step(&mut control, &mut performer, i);
    }
    let calibration = host.calibration();
    MEASURING.with(|m| m.set(true));
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for i in 10..3610 {
        match i % 300 {
            // The handle builds and sends its requests off the perform thread.
            0 => {
                let smoothing = Some(if i % 600 == 0 { 0.3 } else { 0.7 });
                unmeasured(|| {
                    host.request_retarget(RetargetChange { smoothing, ..RetargetChange::default() }).unwrap()
                });
            }
            1 => unmeasured(|| host.set_calibration(calibration).unwrap()),
            2 if i < 3000 => unmeasured(|| host.calibrate(30).unwrap()),
            _ => {}
        }
        step(&mut control, &mut performer, i);
    }
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - before;
    MEASURING.with(|m| m.set(false));
    assert_eq!(allocated, 0, "advance allocated {allocated} times");
    assert_eq!(host.status().calibration, CalibrationState::Done { frames: 30 }, "the capture ran in the window");
    assert_eq!(performer.retarget().config().responses.len(), 2, "and the responses survived the changes");
}
