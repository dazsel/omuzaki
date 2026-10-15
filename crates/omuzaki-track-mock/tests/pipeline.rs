//! The contract end to end on the fakes: `FakeCamera` → `ClockMap` → mailbox → `FrameSource::poll`, the camera's
//! errors, and `FakeTracker`'s scripted times and lost gap.

#![allow(clippy::cast_precision_loss)]

use std::time::Duration;

use omuzaki_anim_api::{Channel, DeviceProfile, FrameSource, TrackFrame, TrackStatus};
use omuzaki_core::time::Seconds;
use omuzaki_track_api::{
    Camera, CameraFormat, CameraFormatRequest, CameraId, ClockMap, DeviceTime, PixelFormat, TrackError, Tracker,
    TrackerConfig, mailbox,
};
use omuzaki_track_mock::{FakeCamera, FakeTracker};

const WAIT: Duration = Duration::from_millis(100);
const TRANSPORT: f64 = 0.012;

fn format(pixel: PixelFormat) -> CameraFormat {
    CameraFormat { width: 8, height: 4, fps: 30, pixel }
}

/// What a tracker does per camera frame, minus the model: map the time, build a frame, publish it.
#[test]
fn camera_frames_reach_the_poller_in_order_at_their_capture_times() {
    let mut camera = FakeCamera::synthetic(format(PixelFormat::Nv12), 90);
    let mut stream = camera.open(&FakeCamera::id(), &CameraFormatRequest::default()).unwrap();
    let (mut tx, mut rx) = mailbox();
    let mut clock = ClockMap::new();

    for i in 0..90_u64 {
        let frame = stream.next_frame(WAIT).unwrap().unwrap();
        assert_eq!(frame.seq, i);
        assert_eq!(frame.data[9], FakeCamera::synthetic_luma(1, 1, i), "Y plane, pixel (1, 1)");
        let arrived = Seconds(frame.captured.0 as f64 * 1e-9 + TRANSPORT);
        let time = clock.map(frame.captured, arrived).unwrap();
        let mut track = TrackFrame::empty(frame.seq, time, DeviceProfile::Webcam);
        track.set(Channel::JAW_OPEN, f32::from(frame.data[0]) / 255.0);
        tx.publish(&track);

        let polled = rx.poll(arrived).unwrap();
        assert_eq!(polled.seq, i);
        assert!((polled.time.0 - (i as f64 / 30.0 + TRANSPORT)).abs() < 1e-6, "frame {i} at {}", polled.time.0);
        assert_eq!(rx.poll(arrived), None);
    }
    assert_eq!(stream.next_frame(WAIT).err(), Some(TrackError::Lost), "the script ran out: the camera is gone");
}

#[test]
fn scripted_bytes_and_timestamps_come_back_unchanged() {
    let frames = vec![(DeviceTime(5), vec![1; 96]), (DeviceTime(40), vec![2; 96])];
    let mut camera = FakeCamera::from_frames(format(PixelFormat::Rgb8), frames);
    let mut stream = camera.open(&FakeCamera::id(), &CameraFormatRequest::default()).unwrap();
    let first = stream.next_frame(WAIT).unwrap().unwrap();
    assert_eq!((first.captured, first.stride, first.data[0]), (DeviceTime(5), 24, 1));
    let second = stream.next_frame(WAIT).unwrap().unwrap();
    assert_eq!((second.captured, second.seq, second.data[95]), (DeviceTime(40), 1, 2));
}

#[test]
fn the_camera_reports_its_errors() {
    let mut camera = FakeCamera::synthetic(format(PixelFormat::Yuy2), 1);
    let request = CameraFormatRequest::default();
    let unknown = CameraId("usb:9".to_owned());
    assert_eq!(camera.open(&unknown, &request).err(), Some(TrackError::NoDevice(unknown)));
    let bgra = CameraFormatRequest { pixel: Some(PixelFormat::Bgra8), ..request };
    assert_eq!(camera.open(&FakeCamera::id(), &bgra).err(), Some(TrackError::FormatUnsupported(bgra)));

    let stream = camera.open(&FakeCamera::id(), &request).unwrap();
    assert_eq!(camera.open(&FakeCamera::id(), &request).err(), Some(TrackError::Busy(FakeCamera::id())));
    drop(stream);
    assert!(camera.open(&FakeCamera::id(), &request).is_ok(), "dropping the stream frees the camera");

    let mut mjpg = FakeCamera::synthetic(format(PixelFormat::Mjpg), 1);
    assert!(matches!(mjpg.open(&FakeCamera::id(), &request), Err(TrackError::FormatUnsupported(_))));
}

fn tracked(seq: u64) -> TrackFrame {
    let mut f = TrackFrame::empty(seq, Seconds(0.0), DeviceProfile::Webcam);
    f.set(Channel::EYE_BLINK_LEFT, 0.5);
    f
}

/// Frames every 1/30 s for 0.5 s, nothing until 1.0 s, then frames again; polled at 60 Hz.
#[test]
fn a_lost_gap_yields_one_lost_frame_and_tracking_resumes() {
    let at = |i: u64| Duration::from_secs_f64(i as f64 / 30.0);
    let script = (0..=15).chain(30..=40).map(|i| (at(i), tracked(i))).collect();
    let mut tracker = FakeTracker::scripted(script);
    let config = TrackerConfig::default();
    let mut source = tracker.start(&FakeCamera::id(), &config).unwrap();

    let mut polled = Vec::new();
    for step in 0..=80 {
        if let Some(f) = source.poll(Seconds(f64::from(step) / 60.0 + 1e-9)) {
            polled.push(f);
        }
    }
    let lost: Vec<_> = polled.iter().filter(|f| f.status == TrackStatus::Lost).collect();
    assert_eq!(lost.len(), 1, "one lost signal per gap");
    assert_eq!(lost[0].seq, 15, "no new camera frame, so no new seq");
    assert!((lost[0].time.0 - (0.5 + 0.2)).abs() < 1e-9, "at the last frame plus lost_after");

    let tracked_seqs: Vec<_> = polled.iter().filter(|f| f.status == TrackStatus::Tracking).map(|f| f.seq).collect();
    let expected: Vec<_> = (0..=15).chain(30..=40).collect();
    assert_eq!(tracked_seqs, expected, "each scripted frame once, in order");
    for f in polled.iter().filter(|f| f.status == TrackStatus::Tracking) {
        assert!((f.time.0 - f.seq as f64 / 30.0).abs() < 1e-9, "frame {} at its scripted time", f.seq);
    }
    assert_eq!(tracker.latency().frames, polled.len() as u64);

    tracker.stop();
    assert_eq!(source.poll(Seconds(10.0)), None, "a stopped session's source stays empty");
}

#[test]
fn a_slow_poller_gets_only_the_newest_scripted_frame() {
    let script = (0..10).map(|i| (Duration::from_millis(i * 10), tracked(i))).collect();
    let mut tracker = FakeTracker::scripted(script);
    let mut source = tracker.start(&FakeCamera::id(), &TrackerConfig::default()).unwrap();
    assert_eq!(source.poll(Seconds(0.095)).map(|f| f.seq), Some(9));
    assert_eq!(source.poll(Seconds(0.1)), None);
    assert!(tracker.start(&CameraId("usb:1".to_owned()), &TrackerConfig::default()).is_err());
}

#[test]
fn start_rejects_a_camera_request_the_device_cannot_meet() {
    let mut tracker = FakeTracker::scripted(vec![]);
    let camera = CameraFormatRequest { pixel: Some(PixelFormat::Bgra8), ..CameraFormatRequest::default() };
    let config = TrackerConfig { camera, ..TrackerConfig::default() };
    assert_eq!(tracker.start(&FakeCamera::id(), &config).err(), Some(TrackError::FormatUnsupported(camera)));
}

#[test]
fn a_refused_restart_still_stops_the_old_source() {
    let script = vec![(Duration::ZERO, tracked(1))];
    let mut tracker = FakeTracker::scripted(script);
    let mut old = tracker.start(&FakeCamera::id(), &TrackerConfig::default()).unwrap();
    assert!(old.poll(Seconds(0.0)).is_some());

    let missing = CameraId("nope".to_owned());
    assert_eq!(tracker.start(&missing, &TrackerConfig::default()).err(), Some(TrackError::NoDevice(missing)));
    assert_eq!(old.poll(Seconds(1.0)), None, "the old session ended with the refused restart");
}
