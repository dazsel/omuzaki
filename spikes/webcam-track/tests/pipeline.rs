//! End-to-end checks against the real models. They need the ONNX files and an ONNX Runtime library, which are not in
//! git (see `models/PROVENANCE.md`), so they run only when these variables are set and are skipped otherwise:
//!
//! * `WEBCAM_TRACK_ORT_LIB`: path to `libonnxruntime.so`
//! * `WEBCAM_TRACK_MODELS`: directory with the three `.onnx` files and the geometry metadata
//! * `WEBCAM_TRACK_FRAMES` (optional): directory of PNG frames containing a face, for the face tests

// A panic is a clear failure in a test, and the helpers below live outside `#[test]` functions, where clippy.toml's
// test exemption does not reach.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use webcam_track::frame::Frame;
use webcam_track::pipeline::{FaceTracker, Mode, TrackerConfig};
use webcam_track::runtime::{RuntimeOptions, init_runtime};

fn tracker(mode: Mode) -> Option<FaceTracker> {
    let (Some(lib), Some(models)) = (std::env::var_os("WEBCAM_TRACK_ORT_LIB"), std::env::var_os("WEBCAM_TRACK_MODELS"))
    else {
        eprintln!("skipped: set WEBCAM_TRACK_ORT_LIB and WEBCAM_TRACK_MODELS to run the end-to-end tests");
        return None;
    };
    init_runtime(&PathBuf::from(lib)).unwrap();
    let config = TrackerConfig {
        models: PathBuf::from(models),
        runtime: RuntimeOptions::default(),
        mode,
        min_detection_confidence: 0.5,
        min_presence_confidence: 0.5,
    };
    Some(FaceTracker::new(&config).unwrap())
}

/// The first PNG frames of `WEBCAM_TRACK_FRAMES`, in name order.
fn face_frames(count: usize) -> Option<Vec<Frame>> {
    let dir = PathBuf::from(std::env::var_os("WEBCAM_TRACK_FRAMES")?);
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    files.sort();
    Some(files.iter().take(count).map(|f| Frame::from_png(f).unwrap()).collect())
}

#[test]
fn a_flat_frame_has_no_face() {
    let Some(mut tracker) = tracker(Mode::Image) else { return };
    let frame = Frame::new(320, 240, vec![128; 320 * 240 * 3]).unwrap();
    let (result, timings) = tracker.track(&frame, 0).unwrap();
    assert!(result.is_none());
    assert!(timings.detector_ran && timings.total > 0.0);
}

#[test]
fn a_face_gives_plausible_blendshapes_landmarks_and_pose() {
    let (Some(mut tracker), Some(frames)) = (tracker(Mode::Image), face_frames(1)) else { return };
    let (result, _) = tracker.track(&frames[0], 0).unwrap();
    let face = result.expect("a face in the first frame");
    assert_eq!(face.landmarks.len(), 478);
    assert!(
        face.landmarks.iter().all(|p| (-0.2..=1.2).contains(&p[0]) && (-0.2..=1.2).contains(&p[1])),
        "landmarks lie on the frame"
    );
    assert!(face.blendshapes.iter().all(|v| (-0.05..=1.05).contains(v)), "blendshapes are scores");
    assert!(face.presence > 0.5);
    // The rotation block is a rotation times one scale; the face is in front of the camera (negative z in camera space).
    let r = |i: usize| nalgebra::Vector3::new(face.pose[i], face.pose[i + 4], face.pose[i + 8]);
    let scale = r(0).norm();
    assert!(
        (r(1).norm() - scale).abs() < 1e-3
            && (r(2).norm() - scale).abs() < 1e-3
            && r(0).dot(&r(1)).abs() < 1e-3 * scale * scale
    );
    assert!(face.pose[11] < 0.0);
}

#[test]
fn video_mode_tracks_from_the_previous_frame() {
    let (Some(mut tracker), Some(frames)) = (tracker(Mode::Video), face_frames(2)) else { return };
    let (first, t0) = tracker.track(&frames[0], 0).unwrap();
    assert!(first.is_some() && t0.detector_ran, "the first frame needs the detector");
    let (second, t1) = tracker.track(&frames[0], 33).unwrap();
    assert!(second.is_some() && !t1.detector_ran, "the second frame reuses the previous ROI");
    tracker.reset();
    let (_, t2) = tracker.track(&frames[0], 66).unwrap();
    assert!(t2.detector_ran, "after a reset the detector runs again");
}
