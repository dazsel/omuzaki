//! The body tracker (PF-06 hands, PF-07 arms) on its real graphs, through the `onnx` runtime: the model folder
//! `tools/models/mediapipe-body.sh` writes opens and loads at both pose sizes, a flat frame finds nobody, and, with the
//! kit's posed frames from `omuzaki-stage`'s `body_frames` test, the solved arms and fingers are scored against the
//! poses they show. Gated on `OMUZAKI_BODY_MODELS` (that folder) and `OMUZAKI_ORT_CACHE` (the pinned ONNX Runtime
//! archive); the score also on `OMUZAKI_BODY_FRAMES_DIR`. Each test prints a line and passes when one is unset.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::path::{Path, PathBuf};
use std::time::Instant;

use omuzaki_ai_api::SessionOptions;
use omuzaki_ai_ort::OrtRuntime;
use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_track::RgbFrame;
use omuzaki_track::body::gesture::{HandGestureConfig, classify};
use omuzaki_track::body::models::{BodyModelDir, PoseSize};
use omuzaki_track::body::{BodyTracker, BodyTrackerConfig};
use omuzaki_track_api::body::{BodyConfig, BodyFrame};
use track_support::DirSource;

mod track_support;

fn env(test: &str) -> Option<(PathBuf, DirSource)> {
    let models = std::env::var_os("OMUZAKI_BODY_MODELS").map(PathBuf::from);
    let cache = std::env::var_os("OMUZAKI_ORT_CACHE").map(|dir| DirSource(PathBuf::from(dir)));
    let (Some(models), Some(cache)) = (models, cache) else {
        eprintln!("skipping {test}: set OMUZAKI_BODY_MODELS (the body model folder) and OMUZAKI_ORT_CACHE");
        return None;
    };
    Some((models, cache))
}

fn tracker(models: &Path, natives: &DirSource, size: PoseSize) -> BodyTracker {
    tracker_with(models, natives, size, BodyTrackerConfig::default())
}

fn tracker_with(models: &Path, natives: &DirSource, size: PoseSize, config: BodyTrackerConfig) -> BodyTracker {
    let dir = BodyModelDir::open(models, size).unwrap();
    let graphs = dir.load(&OrtRuntime, natives, &SessionOptions::default()).unwrap();
    BodyTracker::new(graphs, config).unwrap()
}

#[test]
fn both_sizes_load_and_a_flat_frame_finds_nobody() {
    let Some((models, natives)) = env("both_sizes_load_and_a_flat_frame_finds_nobody") else { return };
    let grey = vec![128_u8; 640 * 480 * 3];
    let frame = RgbFrame { data: &grey, width: 640, height: 480, stride: 640 * 3 };
    for size in [PoseSize::Lite, PoseSize::Full] {
        let mut tracker = tracker(&models, &natives, size);
        let mut out = BodyFrame::empty(0, Seconds(0.0));
        for i in 0..3 {
            tracker.track(&frame, f64::from(i) / 30.0, &BodyConfig::default(), &mut out).unwrap();
        }
        assert!(!out.sees_anything() && out.mask.is_empty(), "{size:?}: {out:?}");
        assert!(tracker.landmarks().pose.is_none());
    }
}

/// A binary PPM as RGB.
fn read_ppm(path: &Path) -> (Vec<u8>, u32, u32) {
    let bytes = std::fs::read(path).unwrap();
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(String::from_utf8(bytes[start..at].to_vec()).unwrap());
    }
    assert_eq!(fields[0], "P6");
    let (w, h) = (fields[1].parse().unwrap(), fields[2].parse().unwrap());
    (bytes[at + 1..].to_vec(), w, h)
}

/// The pose a frame shows, from its JSON.
fn read_truth(path: &Path) -> HumanoidPose {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut pose = HumanoidPose::REST;
    for bone in HumanoidBone::ALL {
        if let Some(q) = value["bones"][format!("{bone:?}")].as_array() {
            let q: Vec<f32> = q.iter().map(|v| v.as_f64().unwrap() as f32).collect();
            pose.set(bone, Quat::from_xyzw(q[0], q[1], q[2], q[3]));
        }
    }
    pose
}

/// Each side's arm bones (shoulder, upper arm, lower arm, hand) and the direction its T-pose arm points.
const ARMS: [([HumanoidBone; 4], Vec3); 2] = [
    (
        [HumanoidBone::LeftShoulder, HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand],
        Vec3::X,
    ),
    (
        [
            HumanoidBone::RightShoulder,
            HumanoidBone::RightUpperArm,
            HumanoidBone::RightLowerArm,
            HumanoidBone::RightHand,
        ],
        Vec3::NEG_X,
    ),
];

/// The upper arm's and forearm's directions in the upper chest's frame.
fn arm_directions(pose: &HumanoidPose, side: usize) -> (Vec3, Vec3) {
    let ([shoulder, upper, lower, _], axis) = ARMS[side];
    let upper_world = pose.get(shoulder) * pose.get(upper);
    (upper_world * axis, upper_world * pose.get(lower) * axis)
}

fn degrees_between(a: Vec3, b: Vec3) -> f32 {
    a.normalize().dot(b.normalize()).clamp(-1.0, 1.0).acos().to_degrees()
}

/// The non-thumb fingers' bones by side: three per finger.
fn finger_bones(side: usize) -> [[HumanoidBone; 3]; 4] {
    use HumanoidBone as B;
    if side == 0 {
        [
            [B::LeftIndexProximal, B::LeftIndexIntermediate, B::LeftIndexDistal],
            [B::LeftMiddleProximal, B::LeftMiddleIntermediate, B::LeftMiddleDistal],
            [B::LeftRingProximal, B::LeftRingIntermediate, B::LeftRingDistal],
            [B::LeftLittleProximal, B::LeftLittleIntermediate, B::LeftLittleDistal],
        ]
    } else {
        [
            [B::RightIndexProximal, B::RightIndexIntermediate, B::RightIndexDistal],
            [B::RightMiddleProximal, B::RightMiddleIntermediate, B::RightMiddleDistal],
            [B::RightRingProximal, B::RightRingIntermediate, B::RightRingDistal],
            [B::RightLittleProximal, B::RightLittleIntermediate, B::RightLittleDistal],
        ]
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one table, frame by frame
fn kit_frames_score_against_their_poses() {
    let Some((models, natives)) = env("kit_frames_score_against_their_poses") else { return };
    let Some(frames) = std::env::var_os("OMUZAKI_BODY_FRAMES_DIR").map(PathBuf::from) else {
        eprintln!(
            "skipping kit_frames_score_against_their_poses: set OMUZAKI_BODY_FRAMES_DIR (omuzaki-stage body_frames)"
        );
        return;
    };
    let size = if std::env::var_os("OMUZAKI_BODY_FULL").is_some() { PoseSize::Full } else { PoseSize::Lite };
    // The pose detector finds people by their faces, and scores a drawn character's face lower than a person's: the
    // score can lower its bar (`OMUZAKI_BODY_MIN_DETECTION`) to measure the landmarks and the solve on the kit.
    let min = std::env::var("OMUZAKI_BODY_MIN_DETECTION").ok().and_then(|v| v.parse().ok()).unwrap_or(0.5);
    let config = BodyTrackerConfig { min_pose_detection: min, ..BodyTrackerConfig::default() };
    let mut tracker = tracker_with(&models, &natives, size, config);
    let mut names: Vec<PathBuf> = std::fs::read_dir(&frames)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ppm"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no frames in {}", frames.display());
    let (mut arm_errors, mut finger_errors, mut times) = (Vec::new(), Vec::new(), Vec::new());
    let mut rows = Vec::new();
    println!("frame | pose | hands L R | upper L R (deg) | fore L R (deg) | finger flex err and gesture L R (deg)");
    for path in &names {
        let (rgb, width, height) = read_ppm(path);
        let truth = read_truth(&path.with_extension("json"));
        let frame = RgbFrame { data: &rgb, width, height, stride: width as usize * 3 };
        let mut out = BodyFrame::empty(0, Seconds(0.0));
        tracker.reset();
        // Video mode on a still: the detector finds the person, then the regions settle on their own landmarks.
        for i in 0..4 {
            let started = Instant::now();
            tracker.track(&frame, f64::from(i) / 30.0, &BodyConfig::default(), &mut out).unwrap();
            if i == 3 {
                times.push(started.elapsed().as_secs_f32() * 1e3);
            }
        }
        let landmarks = tracker.landmarks();
        let mut cells = Vec::new();
        let mut row = serde_json::json!({ "frame": path.file_stem().unwrap().to_string_lossy() });
        if let Some(pose) = &landmarks.pose {
            row["pose"] = serde_json::json!(pose.image.iter().map(|p| [p.0, p.1]).collect::<Vec<_>>());
            row["visibility"] = serde_json::json!(pose.visibility.to_vec());
        }
        for (side, hand) in landmarks.hands.iter().enumerate() {
            if let Some(hand) = hand {
                row[format!("hand_{side}")] =
                    serde_json::json!(hand.image.iter().map(|p| [p.0, p.1]).collect::<Vec<_>>());
            }
        }
        for side in 0..2 {
            if out.arms[side] <= 0.0 {
                cells.push(("-".to_owned(), "-".to_owned()));
                continue;
            }
            let (tu, tl) = arm_directions(&truth, side);
            let (su, sl) = arm_directions(&out.pose, side);
            let (eu, el) = (degrees_between(tu, su), degrees_between(tl, sl));
            arm_errors.push(eu);
            arm_errors.push(el);
            row[format!("upper_{side}")] = eu.into();
            row[format!("fore_{side}")] = el.into();
            cells.push((format!("{eu:.0}"), format!("{el:.0}")));
        }
        let mut flex = Vec::new();
        for side in 0..2 {
            let Some(fingers) = out.fingers[side] else {
                flex.push("-".to_owned());
                continue;
            };
            let mut total = 0.0;
            for (bones, solved) in finger_bones(side).iter().zip(&fingers[1..]) {
                let truth_flex: f32 = bones.iter().map(|&b| truth.get(b).angle_between(Quat::IDENTITY)).sum();
                let solved_flex = solved.mcp.abs() + solved.pip + solved.dip;
                total += (truth_flex - solved_flex).abs().to_degrees() / 4.0;
            }
            finger_errors.push(total);
            let gesture = classify(&fingers, &HandGestureConfig::default());
            row[format!("gesture_{side}")] = serde_json::json!(gesture);
            row[format!("solved_{side}")] =
                serde_json::json!(fingers.iter().map(|f| [f.spread, f.mcp, f.pip, f.dip]).collect::<Vec<_>>());
            row[format!("fingers_{side}")] = total.into();
            flex.push(format!("{total:.0} {}", gesture.map_or("-".to_owned(), |g| format!("{g:?}"))));
        }
        let hands = landmarks.hands.map(|h| h.map_or("-".to_owned(), |h| format!("{:.2}", h.presence)));
        println!(
            "{} | {} | {} {} | {} {} | {} {} | {} {}",
            path.file_stem().unwrap().to_string_lossy(),
            landmarks.pose.map_or("-".to_owned(), |p| format!("{:.2}", p.presence)),
            hands[0],
            hands[1],
            cells.first().map_or("-", |c| c.0.as_str()),
            cells.get(1).map_or("-", |c| c.0.as_str()),
            cells.first().map_or("-", |c| c.1.as_str()),
            cells.get(1).map_or("-", |c| c.1.as_str()),
            flex[0],
            flex[1],
        );
        rows.push(row);
    }
    let median = |v: &mut Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(f32::NAN)
    };
    let found = rows.iter().filter(|r| r.get("upper_0").is_some() || r.get("upper_1").is_some()).count();
    println!(
        "{size:?}: {found}/{} frames with an arm; arm direction error median {:.1} deg over {} bones; finger flex \
         error median {:.1} deg over {} hands; steady-state frame {:.1} ms median",
        names.len(),
        median(&mut arm_errors),
        arm_errors.len(),
        median(&mut finger_errors),
        finger_errors.len(),
        median(&mut times),
    );
    if let Some(out) = std::env::var_os("OMUZAKI_BODY_SCORE_OUT") {
        std::fs::write(out, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
    }
}
