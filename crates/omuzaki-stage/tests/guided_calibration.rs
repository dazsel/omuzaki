//! The guided calibration (PF-20) end to end: frames through the input's tap into a run, prompt by prompt on the frames'
//! own clock, the fit applied to the real performer's handle; and the `input.calibrate` refusals over the real
//! `LiveInput`. Each frame waits for the run to have taken it (its status moves on), never on a timer.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_possible_truncation)]

#[path = "live/support.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_anim_api::{CalibrationPrompt, Channel, DeviceProfile, FrameSource, HeadPose, HumanoidPose, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_ops_api::Mode;
use omuzaki_stage::perform::guided::{FrameTap, GuidedRun, GuidedStatus};
use omuzaki_stage::perform::host::{PerformHost, perform_channel};
use omuzaki_stage::perform::input::{InputError, InputHost, LiveInput, NoPrompt, SourceSlot};
use omuzaki_track_api::tracker::TrackerConfig;
use support::{Cameras, face_for, performer};

const BACKSTOP: Duration = Duration::from_secs(30);

/// A source the test feeds one frame at a time.
#[derive(Clone, Default)]
struct Feed(Arc<Mutex<Option<TrackFrame>>>);

impl FrameSource for Feed {
    fn poll(&mut self, _now: Seconds) -> Option<TrackFrame> {
        self.0.lock().unwrap().take()
    }
}

/// The face the routine sees `elapsed` seconds in: the jaw idles at 0.05, opens to 0.6 in the mouth-open prompt and
/// sits at 0.1 elsewhere; no face at all during the big smile.
fn frame_at(elapsed: f64) -> TrackFrame {
    let mut prompt_end = 0.0;
    let mut prompt = CalibrationPrompt::Rest;
    for p in CalibrationPrompt::ROUTINE {
        prompt_end += f64::from(p.seconds());
        prompt = p;
        if elapsed < prompt_end {
            break;
        }
    }
    let time = Seconds(50.0 + elapsed);
    if prompt == CalibrationPrompt::BigSmile {
        return TrackFrame::lost(0, time, DeviceProfile::Webcam);
    }
    let mut frame = TrackFrame::empty(0, time, DeviceProfile::Webcam);
    for info in Channel::arkit() {
        frame.set(info.channel, 0.0);
    }
    let jaw = match prompt {
        CalibrationPrompt::Rest => 0.05,
        CalibrationPrompt::MouthOpen => 0.6,
        _ => 0.1,
    };
    frame.set(Channel::JAW_OPEN, jaw);
    frame.head = Some(HeadPose { yaw: 0.2, ..HeadPose::default() });
    frame
}

/// Waits until `run`'s status satisfies `done`.
fn until(run: &GuidedRun, done: impl Fn(&GuidedStatus) -> bool) -> GuidedStatus {
    let deadline = Instant::now() + BACKSTOP;
    loop {
        let status = run.status();
        if done(&status) {
            return status;
        }
        assert!(Instant::now() < deadline, "stuck at {status:?}");
        std::thread::yield_now();
    }
}

#[test]
fn the_routine_fits_each_prompt_and_applies_the_calibration() {
    let (handle, _control) = perform_channel(&performer(&HumanoidPose::REST));
    let perform: Arc<dyn PerformHost> = Arc::new(handle);
    let tap = Arc::new(FrameTap::default());
    let feed = Feed::default();
    let mut tee = tap.tee(Box::new(feed.clone()));
    let run = GuidedRun::default();
    assert!(run.start(Arc::clone(&tap), perform.requested_calibration(), Arc::clone(&perform)).unwrap());
    assert!(!run.start(Arc::clone(&tap), perform.requested_calibration(), Arc::clone(&perform)).unwrap());
    // Ten frames a second through the whole routine, as the perform thread would poll them.
    let mut saw = Vec::new();
    for i in 0..=380_u32 {
        let elapsed = f64::from(i) / 10.0;
        *feed.0.lock().unwrap() = Some(frame_at(elapsed));
        tee.poll(Seconds(0.0)).unwrap();
        let left = (38.0 - elapsed) as f32;
        let status = until(&run, |s| match s {
            GuidedStatus::Running { left_s, .. } => (left_s - left).abs() < 1e-3,
            GuidedStatus::Done { .. } | GuidedStatus::Failed { .. } => i == 380,
            _ => false,
        });
        if let GuidedStatus::Running { prompt, .. } = status
            && saw.last() != Some(&prompt)
        {
            saw.push(prompt);
        }
    }
    assert_eq!(saw, CalibrationPrompt::ROUTINE, "every prompt showed, in order");
    let GuidedStatus::Done { skipped } = run.status() else { panic!("{:?}", run.status()) };
    assert_eq!(skipped, [CalibrationPrompt::BigSmile], "no face in the big smile");
    let jaw = perform.requested_calibration().range(Channel::JAW_OPEN);
    assert!((jaw.lo - 0.05).abs() < 1e-6 && (jaw.hi - 0.6).abs() < 1e-6, "{jaw:?}");
    assert!((perform.requested_calibration().head_rest.yaw - 0.2).abs() < 1e-6);
}

#[test]
fn a_cancelled_routine_keeps_the_earlier_calibration() {
    let (handle, _control) = perform_channel(&performer(&HumanoidPose::REST));
    let perform: Arc<dyn PerformHost> = Arc::new(handle);
    let before = perform.requested_calibration();
    let run = GuidedRun::default();
    assert!(run.start(Arc::new(FrameTap::default()), before, Arc::clone(&perform)).unwrap());
    assert!(run.cancel());
    assert_eq!(run.status(), GuidedStatus::Cancelled);
    assert_eq!(perform.requested_calibration(), before);
}

#[test]
fn the_input_calibrates_only_over_a_running_capture_and_one_at_a_time() {
    let (tracker, _log) = Cameras::new(vec![("a", face_for(60.0, 1))]);
    let (handle, _control) = perform_channel(&performer(&HumanoidPose::REST));
    let perform: Arc<dyn PerformHost> = Arc::new(handle);
    let input = LiveInput::new(
        Box::new(tracker),
        Box::new(NoPrompt),
        TrackerConfig::default(),
        Arc::new(SourceSlot::new()),
        perform,
    );
    assert_eq!(input.calibrate_guided(Mode::Apply), Err(InputError::CaptureOff));
    input.select(omuzaki_stage::perform::input::SourceKind::Webcam, Mode::Apply).unwrap();
    input.bind_camera(Some(omuzaki_track_api::camera::CameraId("a".into())), Mode::Apply).unwrap();
    input.preview(true, Mode::Apply).unwrap();
    assert_eq!(input.calibrate_guided(Mode::DryRun), Ok(GuidedStatus::Idle), "a dry run starts nothing");
    assert_eq!(input.calibrate_guided(Mode::Apply), Ok(GuidedStatus::Waiting));
    assert_eq!(input.calibrate_guided(Mode::Apply), Err(InputError::CalibrationRunning));
    assert!(input.cancel_guided());
    assert_eq!(input.guided(), GuidedStatus::Cancelled);
}
