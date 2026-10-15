//! ROI reuse and frame skipping (`docs/design/track-roi-reuse.md`) over scripted sessions: a fast head turn is never
//! skipped, a run of skips is bounded, the detector re-runs on low confidence and after N frames, and the same
//! frames always give the same decisions.

#![allow(clippy::unwrap_used)] // tests unwrap scripted calls

mod common;

use common::{
    FACE_ROI, Runs, SIDE, blendshapes_script, counted_graphs, detector_script, frame, geometry, grey_pixels,
    landmarks_script, steady_face, textured_pixels,
};
use omuzaki_ai_api::TensorBuf;
use omuzaki_core::time::Seconds;
use omuzaki_track::pipeline::ReuseConfig;
use omuzaki_track::{FaceSample, FaceTracker, FaceTrackerConfig};

/// What one frame cost the graphs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Work {
    detector: u64,
    landmarks: u64,
}

struct Rig {
    tracker: FaceTracker,
    runs: Runs,
    seen: (u64, u64, u64),
    index: u32,
}

impl Rig {
    /// A tracker over the scripted face; `logits` is the landmark graph's presence script (it cycles per run) and
    /// `detector` the detector's.
    fn new(reuse: ReuseConfig, logits: &[f32], detector: Vec<Vec<TensorBuf>>) -> Self {
        let face = steady_face();
        let landmarks = logits.iter().flat_map(|l| landmarks_script(&face, FACE_ROI, *l)).collect();
        let (graphs, runs) = counted_graphs(detector, landmarks, blendshapes_script());
        let config = FaceTrackerConfig { reuse, ..FaceTrackerConfig::default() };
        Self { tracker: FaceTracker::new(graphs, &geometry(), config).unwrap(), runs, seen: (0, 0, 0), index: 0 }
    }

    fn confident(reuse: ReuseConfig) -> Self {
        Self::new(reuse, &[10.0], detector_script(true))
    }

    /// Tracks `pixels` as the next frame at 30 fps; returns the sample and the graph runs it took.
    fn step(&mut self, pixels: &[u8]) -> (Option<FaceSample>, Work) {
        let time = Seconds(f64::from(self.index) / 30.0);
        self.index += 1;
        let (sample, _) = self.tracker.track(&frame(pixels), time).unwrap();
        let now = self.runs.get();
        let work = Work { detector: now.0 - self.seen.0, landmarks: now.1 - self.seen.1 };
        self.seen = now;
        (sample, work)
    }
}

/// The frame numbers (0-based) whose landmark pass was skipped.
fn skipped(works: &[Work]) -> Vec<usize> {
    works.iter().enumerate().filter(|(_, w)| w.landmarks == 0).map(|(i, _)| i).collect()
}

/// The longest run of consecutive frame numbers.
fn longest_run(frames: &[usize]) -> usize {
    let (mut best, mut run) = (0, 0);
    for (i, f) in frames.iter().enumerate() {
        run = if i > 0 && *f == frames[i - 1] + 1 { run + 1 } else { 1 };
        best = best.max(run);
    }
    best
}

#[test]
fn a_fast_head_turn_is_never_skipped() {
    let mut rig = Rig::confident(ReuseConfig::default());
    // 12 pixels of stripe shift a frame is 36 levels of change in the ROI, far over the 2% threshold.
    let works: Vec<Work> = (0..40).map(|i| rig.step(&textured_pixels(12 * i, 2, i)).1).collect();
    assert_eq!(skipped(&works), Vec::<usize>::new(), "{works:?}");
    assert!(works.iter().all(|w| w.landmarks == 1));
}

#[test]
fn a_small_local_change_is_not_skipped_even_when_the_mean_barely_moves() {
    let mut rig = Rig::confident(ReuseConfig::default());
    let still = textured_pixels(0, 0, 0);
    assert_eq!(rig.step(&still).1, Work { detector: 1, landmarks: 1 });
    // A 24x24 patch in the middle of the ROI (a mouth opening) is 2.4% of the area: the mean change stays under 2%.
    let mut mouth = still.clone();
    for y in 116..140 {
        for x in 116..140 {
            mouth[(y * SIDE as usize + x) * 3..][..3].fill(255);
        }
    }
    assert_eq!(rig.step(&mouth).1.landmarks, 1);

    // The same change with the peak check out of the way is skipped: it is the peak that caught it, not the mean.
    let blind = ReuseConfig { skip_peak: 1.0, ..ReuseConfig::default() };
    let mut rig = Rig::confident(blind);
    rig.step(&still);
    assert_eq!(rig.step(&mouth).1.landmarks, 0);
}

#[test]
fn a_still_face_skips_at_most_max_skips_frames_in_a_row() {
    for max_skips in [1, 2, 5] {
        let mut rig = Rig::confident(ReuseConfig { max_skips, redetect_every: 0, ..ReuseConfig::default() });
        // Sensor noise of +-2 levels changes every frame but stays under the thresholds.
        let works: Vec<Work> = (0..100).map(|i| rig.step(&textured_pixels(0, 2, i)).1).collect();
        let skips = skipped(&works);
        assert_eq!(longest_run(&skips), max_skips as usize, "max_skips {max_skips}");
        // The pass after a full run always runs, so one frame in max_skips + 1 is a full pass.
        assert_eq!(skips.len(), 99 - 99 / (max_skips as usize + 1), "max_skips {max_skips}");
        assert_eq!(works.iter().map(|w| w.detector).sum::<u64>(), 1, "the detector ran only on the first frame");
    }
}

#[test]
fn slow_drift_is_caught_because_the_reference_is_the_last_full_pass() {
    let mut rig = Rig::confident(ReuseConfig { max_skips: 1000, redetect_every: 0, ..ReuseConfig::default() });
    // One pixel a frame is about three levels (1.2%): each step alone is under the threshold, the sum of a few is not.
    let works: Vec<Work> = (0..60).map(|i| rig.step(&textured_pixels(i, 0, 0)).1).collect();
    let skips = skipped(&works);
    assert!(!skips.is_empty(), "the first frame after a pass changed by less than the threshold");
    assert!(longest_run(&skips) <= 1, "{skips:?}");
}

#[test]
fn a_skipped_frame_repeats_the_last_result() {
    let mut rig = Rig::confident(ReuseConfig::default());
    let pixels = textured_pixels(0, 0, 0);
    let (first, _) = rig.step(&pixels);
    let (second, work) = rig.step(&pixels);
    assert_eq!(work.landmarks, 0);
    assert_eq!(first, second);
}

#[test]
fn low_confidence_redetects_on_the_next_frame() {
    // Runs 0, 1 confident; run 2 (the third full pass) loses the face; then confident again.
    let mut rig = Rig::new(ReuseConfig::default(), &[10.0, 10.0, -5.0, 10.0], detector_script(true));
    let pixels = textured_pixels(0, 0, 0);
    let works_and_faces: Vec<(Work, bool)> = (0..8)
        .map(|_| {
            let (sample, work) = rig.step(&pixels);
            (work, sample.is_some())
        })
        .collect();
    let at = |i: usize| works_and_faces[i];
    assert_eq!(at(0), (Work { detector: 1, landmarks: 1 }, true));
    assert_eq!((at(1).0.landmarks, at(2).0.landmarks), (0, 0), "a confident still face is skipped");
    assert_eq!(at(3), (Work { detector: 0, landmarks: 1 }, true), "the bounded run ends in a tracked pass");
    assert_eq!((at(4).0.landmarks, at(5).0.landmarks), (0, 0));
    assert_eq!(at(6), (Work { detector: 0, landmarks: 1 }, false), "the pass finds a presence of 0.007: lost");
    assert_eq!(at(7), (Work { detector: 1, landmarks: 1 }, true), "the next frame detects again, not skips");
}

#[test]
fn a_doubtful_face_is_looked_at_on_every_frame() {
    // Presence of sigmoid(1.0) = 0.73: above the 0.5 that keeps a face, under the 0.9 that allows a skip.
    let mut rig = Rig::new(ReuseConfig::default(), &[1.0], detector_script(true));
    let pixels = textured_pixels(0, 0, 0);
    let works: Vec<Work> = (0..20).map(|_| rig.step(&pixels).1).collect();
    assert_eq!(skipped(&works), Vec::<usize>::new());
    assert_eq!(works.iter().map(|w| w.detector).sum::<u64>(), 1);
}

#[test]
fn the_detector_reruns_every_n_frames() {
    let mut rig = Rig::confident(ReuseConfig { redetect_every: 10, ..ReuseConfig::default() });
    let pixels = textured_pixels(0, 0, 0);
    let detections: Vec<usize> = (0..35)
        .filter(|_| true)
        .map(|i| (i, rig.step(&pixels).1))
        .filter(|(_, w)| w.detector == 1)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(detections, [0, 10, 20, 30]);
}

#[test]
fn a_timed_redetection_that_finds_nothing_keeps_the_tracked_face() {
    // The detector's script cycles: a face on the first run, then none.
    let mut detector = detector_script(true);
    detector.extend(detector_script(false));
    let mut rig = Rig::new(ReuseConfig { redetect_every: 10, ..ReuseConfig::default() }, &[10.0], detector);
    let pixels = textured_pixels(0, 0, 0);
    for i in 0..25 {
        let (sample, work) = rig.step(&pixels);
        assert!(sample.is_some(), "frame {i}");
        if i == 10 {
            assert_eq!(work, Work { detector: 1, landmarks: 1 }, "the detector missed; the tracked ROI was used");
        }
    }
}

#[test]
fn off_is_mediapipes_video_mode() {
    let mut rig = Rig::confident(ReuseConfig::OFF);
    let pixels = grey_pixels();
    let works: Vec<Work> = (0..130).map(|_| rig.step(&pixels).1).collect();
    assert_eq!(works[0], Work { detector: 1, landmarks: 1 });
    assert!(works[1..].iter().all(|w| *w == Work { detector: 0, landmarks: 1 }), "no skips, no timed detection");
}

#[test]
fn track_roi_ends_a_run_of_skips() {
    let mut rig = Rig::confident(ReuseConfig::default());
    let pixels = textured_pixels(0, 0, 0);
    rig.step(&pixels);
    assert_eq!(rig.step(&pixels).1.landmarks, 0);
    rig.tracker.track_roi(&frame(&pixels), FACE_ROI, Seconds(1.0)).unwrap();
    rig.seen = rig.runs.get();
    let (_, work) = rig.step(&pixels);
    assert_eq!(work.landmarks, 1, "the ROI changed under the reference");
}

#[test]
fn the_same_frames_give_the_same_decisions_and_samples() {
    let play = || {
        let mut rig = Rig::confident(ReuseConfig::default());
        (0..90_u32)
            .map(|i| {
                // Still, then slow, then fast.
                let shift = if i < 30 {
                    0
                } else if i < 60 {
                    i
                } else {
                    12 * i
                };
                rig.step(&textured_pixels(shift, 2, i))
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(play(), play());
}
