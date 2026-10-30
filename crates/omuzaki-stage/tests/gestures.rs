//! Face gestures (PF-25) end to end: frames through the input's tap into the gesture watch, which presses the trigger
//! key bound to the gesture on the real performer's handle; the performer shows the expression while the gesture holds
//! and lets it go after. Hand gestures (PF-06) the same way, from the body board. Each step waits on what the watch
//! reports, never on a timer.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "live/support.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_anim_api::{
    Channel, ChannelBuf, ChannelMask, DeviceProfile, FrameSource, HumanoidPose, TrackFrame, TrackStatus,
};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{ExpressionId, ExpressionSet};
use omuzaki_stage::perform::gesture::GestureWatch;
use omuzaki_stage::perform::guided::FrameTap;
use omuzaki_stage::perform::host::{PerformHost, perform_channel};
use omuzaki_stage::perform::triggers::TriggerBinding;
use omuzaki_track::gesture::Gesture;
use omuzaki_track_api::body::{BodyBoard, HandGesture, HandGestures};
use support::{mid_step, performer};

const BACKSTOP: Duration = Duration::from_secs(30);

/// A source the test feeds one frame at a time.
#[derive(Clone, Default)]
struct Feed(Arc<Mutex<Option<TrackFrame>>>);

impl FrameSource for Feed {
    fn poll(&mut self, _now: Seconds) -> Option<TrackFrame> {
        self.0.lock().unwrap().take()
    }
}

/// A phone frame at `time` with the tongue at `tongue`.
fn phone(time: f64, tongue: f32) -> TrackFrame {
    let mut frame = TrackFrame::empty(0, Seconds(time), DeviceProfile::Arkit);
    frame.status = TrackStatus::Tracking;
    frame.present = ChannelMask::ARKIT;
    frame.set(Channel::TONGUE_OUT, tongue);
    frame
}

/// Feeds frames of `tongue` at 60 fps from `start` until `watch` reports `want`; returns the time reached.
fn feed_until(
    tee: &mut dyn FrameSource,
    feed: &Feed,
    watch: &GestureWatch,
    start: f64,
    tongue: f32,
    want: bool,
) -> f64 {
    let deadline = Instant::now() + BACKSTOP;
    let mut time = start;
    while watch.active().contains(&Gesture::TongueOut) != want {
        assert!(Instant::now() < deadline, "the watch never saw the tongue {}", if want { "out" } else { "in" });
        *feed.0.lock().unwrap() = Some(phone(time, tongue));
        tee.poll(Seconds(time));
        time += 1.0 / 60.0;
        std::thread::sleep(Duration::from_millis(5));
    }
    time
}

#[test]
fn a_tongue_held_out_shows_its_bound_expression_until_it_goes_back_in() {
    let mut performer = performer(&HumanoidPose::REST);
    let (handle, mut control) = perform_channel(&performer);
    handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
    let expression: ExpressionId = ExpressionSet::starter().0.keys().next().expect("a starter expression").clone();
    let key = Gesture::TongueOut.key().to_owned();
    let binding = TriggerBinding { key: key.clone(), expression, mode: omuzaki_anim::TriggerMode::Hold };
    handle.bind_trigger(binding).unwrap();
    let handle = Arc::new(handle);

    let tap = Arc::new(FrameTap::default());
    let feed = Feed::default();
    let mut tee = tap.tee(Box::new(feed.clone()));
    let watch = GestureWatch::start(Arc::clone(&tap), None, Arc::clone(&handle) as Arc<dyn PerformHost>).unwrap();
    let shown = |control: &mut omuzaki_stage::perform::host::PerformControl, performer: &mut _, step: u64| {
        control.advance(performer, mid_step(step));
        handle.triggers().iter().find(|t| t.binding.key == key).is_some_and(|t| t.active)
    };

    let time = feed_until(tee.as_mut(), &feed, &watch, 10.0, 0.9, true);
    assert!(shown(&mut control, &mut performer, 0), "the performer shows the bound expression");
    feed_until(tee.as_mut(), &feed, &watch, time, 0.0, false);
    // The release is taken on the next step; then the expression fades out (0.2 s).
    let gone = (1..=30).map(|step| shown(&mut control, &mut performer, step)).last();
    assert_eq!(gone, Some(false), "and lets it go when the tongue goes back in");
}

#[test]
fn a_gesture_still_held_when_frames_stop_is_released() {
    let (handle, _control) = perform_channel(&performer(&HumanoidPose::REST));
    let tap = Arc::new(FrameTap::default());
    let feed = Feed::default();
    let mut tee = tap.tee(Box::new(feed.clone()));
    let watch = GestureWatch::start(Arc::clone(&tap), None, Arc::new(handle)).unwrap();
    feed_until(tee.as_mut(), &feed, &watch, 0.0, 1.0, true);
    // No more frames: the capture stopped. The watch releases after a quiet half second.
    let deadline = Instant::now() + BACKSTOP;
    while !watch.active().is_empty() {
        assert!(Instant::now() < deadline, "the gesture was never released");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until `watch` reports the hand gestures `want`.
fn hands_until(watch: &GestureWatch, want: &[HandGesture]) {
    let deadline = Instant::now() + BACKSTOP;
    while watch.hands() != want {
        assert!(Instant::now() < deadline, "the watch never showed {want:?}, it shows {:?}", watch.hands());
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_peace_sign_shows_its_bound_expression_until_the_body_tracker_lets_go() {
    let mut performer = performer(&HumanoidPose::REST);
    let (handle, mut control) = perform_channel(&performer);
    handle.set_expressions(ExpressionSet::starter(), ChannelBuf::NEUTRAL).unwrap();
    let expression: ExpressionId = ExpressionSet::starter().0.keys().next().expect("a starter expression").clone();
    let key = HandGesture::Peace.key().to_owned();
    let binding = TriggerBinding { key: key.clone(), expression, mode: omuzaki_anim::TriggerMode::Hold };
    handle.bind_trigger(binding).unwrap();
    let handle = Arc::new(handle);

    let (board, _reader) = BodyBoard::new();
    let tap = Arc::new(FrameTap::default());
    let watch = GestureWatch::start(tap, Some(Arc::clone(&board)), Arc::clone(&handle) as Arc<dyn PerformHost>).unwrap();
    let shown = |control: &mut omuzaki_stage::perform::host::PerformControl, performer: &mut _, step: u64| {
        control.advance(performer, mid_step(step));
        handle.triggers().iter().find(|t| t.binding.key == key).is_some_and(|t| t.active)
    };

    // The body tracker decided a peace sign: the watch presses its key, with no face frames at all.
    let mut writer = board.writer().expect("free");
    writer.show(HandGestures::NONE.with(HandGesture::Peace));
    hands_until(&watch, &[HandGesture::Peace]);
    assert!(shown(&mut control, &mut performer, 0), "the performer shows the bound expression");
    // The tracker stops: its gestures go with it, and the watch releases the key.
    drop(writer);
    hands_until(&watch, &[]);
    let gone = (1..=30).map(|step| shown(&mut control, &mut performer, step)).last();
    assert_eq!(gone, Some(false), "and lets it go");
}
