//! The whole pipeline over scripted sessions: detection, tracking between frames, the name and head-sign tables and
//! the error paths. The scripts replay raw model outputs built from a synthetic face (see `common`), so no model
//! file is needed.

// A panic is a clear failure in a test, and the helpers live outside `#[test]` functions, where clippy.toml's
// exemption does not reach; compared values are copied or exactly scripted; the fixtures' casts are tiny.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::similar_names
)]

mod common;

use common::{
    FACE_ROI, LANDMARKS, MESH, Pose, SIDE, blendshapes_script, canonical_face, coefficient, counted_graphs,
    detector_script, frame, geometry, graphs, grey_pixels, landmarks_script, project,
};
use omuzaki_ai_api::Session;
use omuzaki_ai_mock::{FakeSession, echo_signature};
use omuzaki_anim_api::{Channel, DeviceProfile, TrackStatus};
use omuzaki_core::time::Seconds;
use omuzaki_track::pipeline::ReuseConfig;
use omuzaki_track::{FaceGraphs, FaceSample, FaceTrackError, FaceTracker, FaceTrackerConfig, Mode, to_track_frame};

/// The scripted face at `pose`, as a tracker (counted graphs) in `mode`.
fn tracker_for(pose: &Pose, mode: Mode, presence_logit: f32) -> (FaceTracker, common::Runs) {
    let face = project(&canonical_face(), pose, SIDE, SIDE);
    let (graphs, runs) =
        counted_graphs(detector_script(true), landmarks_script(&face, FACE_ROI, presence_logit), blendshapes_script());
    // These tests count what each graph does on every frame; `tests/roi_reuse.rs` covers skipping.
    let config = FaceTrackerConfig { mode, reuse: ReuseConfig::OFF, ..FaceTrackerConfig::default() };
    (FaceTracker::new(graphs, &geometry(), config).unwrap(), runs)
}

fn track(tracker: &mut FaceTracker, frames: u32) -> Option<FaceSample> {
    let pixels = grey_pixels();
    let mut last = None;
    for i in 0..frames {
        last = tracker.track(&frame(&pixels), Seconds(f64::from(i) / 30.0)).unwrap().0;
    }
    last
}

#[test]
fn a_detector_with_no_detections_returns_none_and_runs_nothing_else() {
    let (graphs, runs) = counted_graphs(
        detector_script(false),
        landmarks_script(&canonical_face(), FACE_ROI, 10.0),
        blendshapes_script(),
    );
    let mut tracker = FaceTracker::new(graphs, &geometry(), FaceTrackerConfig::default()).unwrap();
    let pixels = grey_pixels();
    let (sample, timings) = tracker.track(&frame(&pixels), Seconds(0.0)).unwrap();
    assert!(sample.is_none());
    assert_eq!(runs.get(), (1, 0, 0));
    assert_eq!((timings.landmarks_us, timings.blendshapes_us, timings.geometry_us), (0, 0, 0));
}

#[test]
fn a_scripted_face_goes_through_all_three_graphs_and_video_mode_skips_the_detector() {
    let pose = Pose::facing(0.1, -0.05, 0.02);
    let (mut tracker, runs) = tracker_for(&pose, Mode::Video, 10.0);
    let pixels = grey_pixels();

    let (first, _) = tracker.track(&frame(&pixels), Seconds(0.0)).unwrap();
    let first = first.expect("a face on the first frame");
    assert_eq!(runs.get(), (1, 1, 1), "the first frame needs the detector");
    assert!((first.presence - 1.0).abs() < 1e-3);
    let roi = first.roi;
    assert!(
        (roi.x_center - 0.5).abs() < 1e-4 && (roi.width - 0.6).abs() < 1e-4 && roi.rotation.abs() < 1e-4,
        "the detector's box, grown 1.5x: {roi:?}"
    );
    let expected = project(&canonical_face(), &pose, SIDE, SIDE);
    assert!(
        first.landmarks.iter().zip(&expected).all(|(a, b)| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)),
        "the raw outputs project back to the face they were made from"
    );

    let (second, _) = tracker.track(&frame(&pixels), Seconds(1.0 / 30.0)).unwrap();
    assert!(second.is_some());
    assert_eq!(runs.get(), (1, 2, 2), "the second frame reuses the ROI: no detector run");

    tracker.reset();
    let (third, _) = tracker.track(&frame(&pixels), Seconds(2.0 / 30.0)).unwrap();
    assert!(third.is_some());
    assert_eq!(runs.get().0, 2, "after a reset the detector runs again");
}

#[test]
fn image_mode_runs_the_detector_on_every_frame() {
    let (mut tracker, runs) = tracker_for(&Pose::facing(0.0, 0.0, 0.0), Mode::Image, 10.0);
    assert!(track(&mut tracker, 3).is_some());
    assert_eq!(runs.get(), (3, 3, 3));
}

#[test]
fn a_low_presence_score_drops_the_face_and_the_next_frame_redetects() {
    let (mut tracker, runs) = tracker_for(&Pose::facing(0.0, 0.0, 0.0), Mode::Video, -5.0);
    let pixels = grey_pixels();
    assert!(tracker.track(&frame(&pixels), Seconds(0.0)).unwrap().0.is_none());
    assert_eq!(runs.get(), (1, 1, 0), "no blendshapes for a face that is not there");
    assert!(tracker.track(&frame(&pixels), Seconds(0.1)).unwrap().0.is_none());
    assert_eq!(runs.get().0, 2, "a lost face is searched for again");
}

#[test]
fn landmarks_too_compact_for_a_pose_read_as_no_face() {
    let collapsed = vec![[0.5, 0.5, 0.0]; LANDMARKS];
    let (graphs, runs) =
        counted_graphs(detector_script(true), landmarks_script(&collapsed, FACE_ROI, 10.0), blendshapes_script());
    let mut tracker = FaceTracker::new(graphs, &geometry(), FaceTrackerConfig::default()).unwrap();
    let pixels = grey_pixels();
    assert!(tracker.track(&frame(&pixels), Seconds(0.0)).unwrap().0.is_none());
    assert!(tracker.track(&frame(&pixels), Seconds(0.1)).unwrap().0.is_none());
    assert_eq!(runs.get().0, 2, "the detector runs again after a failed fit");
}

#[test]
fn the_name_table_has_51_channels_without_tongue_out_and_drops_neutral() {
    let (mut tracker, _) = tracker_for(&Pose::facing(0.0, 0.0, 0.0), Mode::Image, 10.0);
    let sample = track(&mut tracker, 1).expect("a face");
    assert_eq!(sample.present.count(), 51);
    assert!(!sample.present.contains(Channel::TONGUE_OUT));
    assert_eq!(sample.blendshapes[Channel::TONGUE_OUT.index()], 0.0);
    assert!(Channel::from_name("_neutral").is_none(), "_neutral is not a channel, so it cannot leak into one");
    for (i, name) in omuzaki_track::blendshapes::NAMES.iter().enumerate().skip(1) {
        let channel = Channel::from_name(name).expect(name);
        assert!(sample.present.contains(channel), "{name}");
        assert_eq!(sample.blendshapes[channel.index()], coefficient(i), "{name} is model output {i}");
    }
    let frame = to_track_frame(Some(&sample), 3, Seconds(0.5));
    assert_eq!((frame.status, frame.profile, frame.seq), (TrackStatus::Tracking, DeviceProfile::Webcam, 3));
    assert_eq!((frame.present, frame.blend), (sample.present, sample.blendshapes));
}

#[test]
fn head_signs_follow_the_anim_api_conventions() {
    let canonical = canonical_face();
    let nose = (0..MESH).max_by(|&a, &b| canonical[a][2].total_cmp(&canonical[b][2])).unwrap();
    let top = (0..MESH).max_by(|&a, &b| canonical[a][1].total_cmp(&canonical[b][1])).unwrap();
    let centre = |face: &[[f32; 3]], axis: usize| face[..MESH].iter().map(|p| p[axis]).sum::<f32>() / MESH as f32;
    let neutral = project(&canonical, &Pose::facing(0.0, 0.0, 0.0), SIDE, SIDE);

    let pixels = grey_pixels();
    let cases: [(&str, [f64; 3]); 6] = [
        ("yaw +", [0.3, 0.0, 0.0]),
        ("yaw -", [-0.3, 0.0, 0.0]),
        ("pitch +", [0.0, 0.3, 0.0]),
        ("pitch -", [0.0, -0.3, 0.0]),
        ("roll +", [0.0, 0.0, 0.3]),
        ("roll -", [0.0, 0.0, -0.3]),
    ];
    for (name, [yaw, pitch, roll]) in cases {
        let pose = Pose::facing(yaw, pitch, roll);
        let face = project(&canonical, &pose, SIDE, SIDE);
        // The fixture itself obeys the documented meaning of each sign, read off the image: positive yaw turns the
        // nose toward the image's right, positive pitch down, positive roll tips the top of the head to the left.
        let (nose_dx, nose_dy) = (face[nose][0] - neutral[nose][0], face[nose][1] - neutral[nose][1]);
        let top_dx = face[top][0] - centre(&face, 0) - (neutral[top][0] - centre(&neutral, 0));
        match name {
            "yaw +" => assert!(nose_dx > 0.01, "{name}: nose moved {nose_dx}"),
            "yaw -" => assert!(nose_dx < -0.01, "{name}: nose moved {nose_dx}"),
            "pitch +" => assert!(nose_dy > 0.01, "{name}: nose moved {nose_dy} (image y points down)"),
            "pitch -" => assert!(nose_dy < -0.01, "{name}: nose moved {nose_dy}"),
            "roll +" => assert!(top_dx < -0.01, "{name}: top moved {top_dx}"),
            _ => assert!(top_dx > 0.01, "{name}: top moved {top_dx}"),
        }

        let graphs = graphs(detector_script(true), landmarks_script(&face, FACE_ROI, 10.0), blendshapes_script());
        let config = FaceTrackerConfig { mode: Mode::Image, ..FaceTrackerConfig::default() };
        let mut tracker = FaceTracker::new(graphs, &geometry(), config).unwrap();
        let sample = tracker.track_roi(&frame(&pixels), FACE_ROI, Seconds(0.0)).unwrap();
        let head = to_track_frame(Some(&sample), 0, Seconds(0.0)).head.expect("a head pose");
        let [want_yaw, want_pitch, want_roll] = [yaw, pitch, roll].map(|a| a as f32);
        assert!((head.yaw - want_yaw).abs() < 1e-3, "{name}: yaw {} wanted {want_yaw}", head.yaw);
        assert!((head.pitch - want_pitch).abs() < 1e-3, "{name}: pitch {} wanted {want_pitch}", head.pitch);
        assert!((head.roll - want_roll).abs() < 1e-3, "{name}: roll {} wanted {want_roll}", head.roll);
    }
}

#[test]
fn the_pose_translation_in_centimetres_becomes_metres() {
    let pose = Pose { angles: [0.0; 3], translation: [3.0, -2.0, -50.0] };
    let face = project(&canonical_face(), &pose, SIDE, SIDE);
    let graphs = graphs(detector_script(true), landmarks_script(&face, FACE_ROI, 10.0), blendshapes_script());
    let mut tracker = FaceTracker::new(graphs, &geometry(), FaceTrackerConfig::default()).unwrap();
    let pixels = grey_pixels();
    let sample = tracker.track_roi(&frame(&pixels), FACE_ROI, Seconds(0.0)).unwrap();
    assert!((sample.pose[12] - 3.0).abs() < 0.05 && (sample.pose[13] + 2.0).abs() < 0.05, "{:?}", &sample.pose[12..15]);
    assert!((sample.pose[14] + 50.0).abs() < 0.05, "centimetres in the matrix: {}", sample.pose[14]);
    let position = sample.head_pose().position.expect("a position");
    assert!(
        (position.x - 0.03).abs() < 5e-4 && (position.y + 0.02).abs() < 5e-4 && (position.z + 0.5).abs() < 5e-4,
        "metres in the head pose: {position:?}"
    );
}

#[test]
fn a_lost_face_becomes_a_lost_track_frame() {
    let frame = to_track_frame(None, 9, Seconds(2.0));
    assert_eq!((frame.status, frame.seq, frame.time, frame.head), (TrackStatus::Lost, 9, Seconds(2.0), None));
}

#[test]
fn a_wrong_output_shape_is_a_shape_error_not_a_panic() {
    let face = project(&canonical_face(), &Pose::facing(0.0, 0.0, 0.0), SIDE, SIDE);
    let short = |n| vec![vec![common::tensor(&vec![0.0; n], &[n])]];
    let two = |a: usize, b: usize| vec![vec![common::tensor(&vec![0.0; a], &[a]), common::tensor(&vec![0.0; b], &[b])]];
    let pixels = grey_pixels();
    let run = |graphs: FaceGraphs| {
        let mut tracker = FaceTracker::new(graphs, &geometry(), FaceTrackerConfig::default()).unwrap();
        tracker.track(&frame(&pixels), Seconds(0.0)).map(|(sample, _)| sample)
    };
    assert_eq!(
        run(graphs(two(100, 896), landmarks_script(&face, FACE_ROI, 10.0), blendshapes_script())).unwrap_err(),
        FaceTrackError::Shape { graph: "detector" }
    );
    assert_eq!(
        run(graphs(detector_script(true), two(1434, 2), blendshapes_script())).unwrap_err(),
        FaceTrackError::Shape { graph: "landmarks" }
    );
    assert_eq!(
        run(graphs(detector_script(true), landmarks_script(&face, FACE_ROI, 10.0), short(50))).unwrap_err(),
        FaceTrackError::Shape { graph: "blendshapes" }
    );
}

#[test]
fn bad_frames_failing_sessions_and_wrong_signatures_are_errors() {
    let pixels = grey_pixels();
    let (mut tracker, _) = tracker_for(&Pose::facing(0.0, 0.0, 0.0), Mode::Video, 10.0);
    let mut bad = frame(&pixels);
    bad.stride = 10;
    assert_eq!(tracker.track(&bad, Seconds(0.0)).unwrap_err(), FaceTrackError::BadFrame { width: SIDE, height: SIDE });

    let face = project(&canonical_face(), &Pose::facing(0.0, 0.0, 0.0), SIDE, SIDE);
    let crashing = FaceGraphs {
        detector: Box::new(
            FakeSession::scripted(omuzaki_track::detector_signature(), detector_script(true)).crashing_after(0),
        ),
        landmarks: Box::new(FakeSession::scripted(
            omuzaki_track::landmarks_signature(),
            landmarks_script(&face, FACE_ROI, 10.0),
        )),
        blendshapes: Box::new(FakeSession::scripted(omuzaki_track::blendshapes_signature(), blendshapes_script())),
    };
    let mut tracker = FaceTracker::new(crashing, &geometry(), FaceTrackerConfig::default()).unwrap();
    assert!(matches!(tracker.track(&frame(&pixels), Seconds(0.0)), Err(FaceTrackError::Session(_))));

    let wrong: Box<dyn Session> = Box::new(FakeSession::affine(echo_signature(), 1.0, 0.0));
    let mut mismatched = graphs(detector_script(true), landmarks_script(&face, FACE_ROI, 10.0), blendshapes_script());
    mismatched.landmarks = wrong;
    assert!(matches!(
        FaceTracker::new(mismatched, &geometry(), FaceTrackerConfig::default()),
        Err(FaceTrackError::Session(message)) if message.contains("landmarks")
    ));
}

#[test]
fn track_roi_skips_the_detector_and_rejects_a_region_with_no_area() {
    let (mut tracker, runs) = tracker_for(&Pose::facing(0.0, 0.0, 0.0), Mode::Video, -5.0);
    let pixels = grey_pixels();
    let sample = tracker.track_roi(&frame(&pixels), FACE_ROI, Seconds(0.0)).unwrap();
    assert_eq!(runs.get(), (0, 1, 1), "no detector, and the presence threshold does not apply");
    assert!(sample.presence < 0.01);
    let flat = omuzaki_track::NormRect { width: 0.0, ..FACE_ROI };
    assert_eq!(
        tracker.track_roi(&frame(&pixels), flat, Seconds(0.1)).unwrap_err(),
        FaceTrackError::Shape { graph: "roi" }
    );
}

#[test]
fn track_roi_timed_splits_the_landmark_stage_and_matches_track_roi() {
    let pose = Pose::facing(0.1, -0.05, 0.02);
    let (mut timed, _) = tracker_for(&pose, Mode::Image, 10.0);
    let (mut plain, _) = tracker_for(&pose, Mode::Image, 10.0);
    let pixels = grey_pixels();
    let (sample, timings) = timed.track_roi_timed(&frame(&pixels), FACE_ROI, Seconds(0.0)).unwrap();
    let expected = plain.track_roi(&frame(&pixels), FACE_ROI, Seconds(0.0)).unwrap();
    assert_eq!(sample.landmarks, expected.landmarks);
    assert_eq!(sample.blendshapes, expected.blendshapes);
    // The parts never exceed the stage they split, and the detector did not run.
    assert!(timings.warp_us + timings.landmark_run_us <= timings.landmarks_us, "{timings:?}");
    assert_eq!(timings.landmark_decode_us(), timings.landmarks_us - timings.warp_us - timings.landmark_run_us);
    assert_eq!(timings.detect_us, 0);
    assert_eq!(timings.total_us(), timings.landmarks_us + timings.blendshapes_us + timings.geometry_us);
}
