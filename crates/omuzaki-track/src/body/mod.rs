//! The body tracker (spec v3 §22 PF-06 hands, PF-07 body): MediaPipe's pose and hand landmarkers on a webcam frame,
//! solved into the bones and fingers of a [`BodyFrame`].
//!
//! It runs as MediaPipe's holistic graph does in video mode, so a steady person costs one pose and two hand passes a
//! frame and no detector:
//!
//! - **Pose.** The pose detector finds the person once; after that the landmark model's own auxiliary points give the
//!   next frame's region, until the person's presence drops.
//! - **Hands from the pose.** Each hand's region comes from the pose's wrist, index and little-finger points (the
//!   holistic graph's `HandDetectionsFromPoseToRects`), then from the hand's own landmarks while it stays found. So the
//!   pose decides which hand is which: no handedness guess, and no swap when the hands cross.
//! - **Palm fallback.** A hand the pose can't place (its wrist hidden, or no pose at all: a desk shot of the hands
//!   only) is searched for with the palm detector every few frames, and given the side of the nearest pose wrist, or
//!   the model's handedness when there is no pose.
//!
//! Everything a frame needs is allocated once; [`BodyTracker::track`] then allocates nothing.

mod filter;
pub mod gesture;
mod graph;
mod hand;
pub mod models;
mod palm;
mod pose;
mod solve;

use std::f32::consts::FRAC_PI_2;

use omuzaki_ai_api::Session;
use omuzaki_track_api::body::{BodyConfig, BodyFrame, Side};

pub use filter::{EuroParams, EuroPoint};
pub use hand::{HAND_POINTS, HandLandmarks, HandModel, hand_signature};
pub use palm::{PalmDetector, palm_signature};
pub use pose::{
    POSE_POINTS, PoseDetector, PoseLandmarks, PoseModel, pose_detector_signature, pose_landmarks_signature,
};
pub use solve::{BodyLandmarks, BodySolver};

use crate::error::Result;
use crate::frame::RgbFrame;
use crate::roi::NormRect;

/// The body tracker's sessions, one per graph it runs (each hand has its own, so both can carry state).
pub struct BodyGraphs {
    /// [`pose_detector_signature`].
    pub pose_detector: Box<dyn Session>,
    /// [`pose_landmarks_signature`].
    pub pose_landmarks: Box<dyn Session>,
    /// [`palm_signature`].
    pub palm: Box<dyn Session>,
    /// [`hand_signature`], one per side.
    pub hands: [Box<dyn Session>; 2],
}

/// Thresholds, MediaPipe's own defaults for the landmarkers in video mode.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BodyTrackerConfig {
    /// A person the pose detector scores under this is ignored.
    pub min_pose_detection: f32,
    /// A tracked person whose presence drops under this is searched for again.
    pub min_pose_presence: f32,
    /// A palm the detector scores under this is ignored.
    pub min_palm_detection: f32,
    /// A hand whose presence is under this is not a hand.
    pub min_hand_presence: f32,
    /// A pose wrist less visible than this doesn't place a hand.
    pub min_wrist_visibility: f32,
    /// Frames between palm searches while a hand is missing.
    pub palm_every: u32,
}

impl Default for BodyTrackerConfig {
    fn default() -> Self {
        Self {
            min_pose_detection: 0.5,
            min_pose_presence: 0.5,
            min_palm_detection: 0.5,
            min_hand_presence: 0.5,
            min_wrist_visibility: 0.5,
            palm_every: 10,
        }
    }
}

/// Pose landmarks of each side's wrist, little-finger knuckle and index knuckle.
const POSE_HAND_POINTS: [[usize; 3]; 2] = [[15, 17, 19], [16, 18, 20]];
/// The holistic graph's hand region from the pose: twice the wrist-to-middle distance, grown 2.7 times after a tenth's
/// shift toward the fingers.
const POSE_HAND_SCALE: f32 = 2.7;
const POSE_HAND_SHIFT_Y: f32 = -0.1;
/// Two hand regions whose centres are closer than this fraction of their size are the same hand.
const SAME_HAND: f32 = 0.5;

/// The region of `side`'s hand from the pose, or `None` when its wrist is not seen well enough.
fn hand_rect_from_pose(pose: &PoseLandmarks, side: Side, min_visibility: f32, fw: f32, fh: f32) -> Option<NormRect> {
    let [wrist, pinky, index] = POSE_HAND_POINTS[side.index()];
    if pose.visibility[wrist] < min_visibility {
        return None;
    }
    let (w, p, i) = (pose.image[wrist], pose.image[pinky], pose.image[index]);
    // The middle finger's knuckle, estimated two thirds of the way from the little finger's to the index's.
    let middle = ((2.0 * i.0 + p.0) / 3.0, (2.0 * i.1 + p.1) / 3.0);
    // A square around the knuckle reaching twice to the wrist, turned so the wrist is straight below.
    let rect = NormRect::aligned(middle, w, fw, fh, -FRAC_PI_2);
    (rect.width > 0.0).then(|| rect.shifted(fw, fh, 0.0, POSE_HAND_SHIFT_Y).expanded(fw, fh, POSE_HAND_SCALE, true))
}

/// Whether two regions cover the same hand.
fn same_hand(a: &NormRect, b: &NormRect, fw: f32, fh: f32) -> bool {
    let distance = ((a.x_center - b.x_center) * fw).hypot((a.y_center - b.y_center) * fh);
    distance < SAME_HAND * (a.width * fw).min(b.width * fw)
}

/// Which side the model's handedness score names, for a hand found without a pose. MediaPipe's hand model is trained
/// on mirrored (selfie) images; on the unmirrored frames the tracker reads, a high score is the performer's left.
fn side_from_handedness(handedness: f32) -> Side {
    if handedness >= 0.5 { Side::Left } else { Side::Right }
}

/// Puts each found hand on the side whose pose wrist it is at. A region grown from one wrist can take in the other
/// hand when the hands are close (held together in front of the chest), and the hand model then follows that one:
/// a hand nearer the other side's wrist moves there, or is dropped when that side already has its own hand. Only
/// when both pose wrists are seen; otherwise the regions are trusted.
fn reconcile(
    hands: &mut [Option<HandLandmarks>; 2],
    rects: &mut [Option<NormRect>; 2],
    pose: &PoseLandmarks,
    min: f32,
    (fw, fh): (f32, f32),
) {
    let wrists = [POSE_HAND_POINTS[0][0], POSE_HAND_POINTS[1][0]];
    if wrists.iter().any(|&w| pose.visibility[w] < min) {
        return;
    }
    let distance = |a: (f32, f32), b: (f32, f32)| ((a.0 - b.0) * fw).hypot((a.1 - b.1) * fh);
    // Which side each found hand's wrist is nearest.
    let nearest = |hand: &HandLandmarks| {
        let at = |side: usize| distance(hand.image[0], pose.image[wrists[side]]);
        usize::from(at(0) > at(1))
    };
    let owner = [hands[0].as_ref().map(nearest), hands[1].as_ref().map(nearest)];
    match owner {
        // Swapped: each hand is at the other's wrist; or one hand, on the other side's wrist: it moves there.
        [Some(1), Some(0) | None] | [None, Some(0)] => {
            hands.swap(0, 1);
            rects.swap(0, 1);
        }
        // Both on one wrist: the one farther from it lets go.
        [Some(a), Some(b)] if a == b => {
            let off = |side: usize| hands[side].map_or(f32::MAX, |h| distance(h.image[0], pose.image[wrists[a]]));
            let far = usize::from(off(0) <= off(1));
            hands[far] = None;
            rects[far] = None;
        }
        _ => {}
    }
}

/// The body tracker. Cold to make; [`BodyTracker::track`] is the per-frame call.
pub struct BodyTracker {
    pose_detector: PoseDetector,
    pose_model: PoseModel,
    palm: PalmDetector,
    hands: [HandModel; 2],
    solver: BodySolver,
    config: BodyTrackerConfig,
    /// The person's region for the next frame, while tracked.
    pose_rect: Option<NormRect>,
    /// Each hand's region for the next frame, while tracked.
    hand_rects: [Option<NormRect>; 2],
    /// Frames since the last palm search.
    since_palm: u32,
    palms: Vec<NormRect>,
    landmarks: BodyLandmarks,
}

impl BodyTracker {
    /// The tracker over `graphs`.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`](crate::FaceTrackError::Session) when a graph's signature is not its slot's.
    pub fn new(graphs: BodyGraphs, config: BodyTrackerConfig) -> Result<Self> {
        let BodyGraphs { pose_detector, pose_landmarks, palm, hands: [left, right] } = graphs;
        Ok(Self {
            pose_detector: PoseDetector::new(pose_detector, config.min_pose_detection)?,
            pose_model: PoseModel::new(pose_landmarks)?,
            palm: PalmDetector::new(palm, config.min_palm_detection)?,
            hands: [HandModel::new(left)?, HandModel::new(right)?],
            solver: BodySolver::new(),
            config,
            pose_rect: None,
            hand_rects: [None; 2],
            since_palm: u32::MAX,
            palms: Vec::with_capacity(4),
            landmarks: BodyLandmarks::NONE,
        })
    }

    /// Forgets every tracked region and the solver's history: the next frame starts with the detectors.
    pub fn reset(&mut self) {
        self.pose_detector.reset();
        self.pose_model.reset();
        self.palm.reset();
        self.hands.iter_mut().for_each(HandModel::reset);
        self.solver.reset();
        self.pose_rect = None;
        self.hand_rects = [None; 2];
        self.since_palm = u32::MAX;
        self.landmarks = BodyLandmarks::NONE;
    }

    /// The landmarks of the last frame tracked.
    pub fn landmarks(&self) -> &BodyLandmarks {
        &self.landmarks
    }

    /// Tracks `frame`, captured at `time` seconds, and solves what `wanted` asks for into `out` (its `seq` and `time`
    /// are the caller's).
    ///
    /// # Errors
    /// A session failure or a graph output of the wrong size; the tracked regions are dropped, so the next frame
    /// starts over.
    pub fn track(&mut self, frame: &RgbFrame<'_>, time: f64, wanted: &BodyConfig, out: &mut BodyFrame) -> Result<()> {
        frame.validate()?;
        let result = self.find(frame, *wanted);
        if result.is_err() {
            self.pose_rect = None;
            self.hand_rects = [None; 2];
            self.landmarks = BodyLandmarks::NONE;
        }
        self.solver.solve(&self.landmarks, time, wanted, out);
        result
    }

    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
    fn find(&mut self, frame: &RgbFrame<'_>, wanted: BodyConfig) -> Result<()> {
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        self.landmarks = BodyLandmarks::NONE;
        if !wanted.any() {
            self.pose_rect = None;
            self.hand_rects = [None; 2];
            return Ok(());
        }

        // The person: the tracked region, or the detector's when there is none or it lost them.
        let mut pose = None;
        if let Some(rect) = self.pose_rect {
            pose = self.pose_model.run(frame, &rect)?.filter(|p| p.presence >= self.config.min_pose_presence);
        }
        if pose.is_none()
            && let Some(rect) = self.pose_detector.detect(frame)?
        {
            pose = self.pose_model.run(frame, &rect)?.filter(|p| p.presence >= self.config.min_pose_presence);
        }
        self.pose_rect = pose.map(|p| p.next);
        self.landmarks.pose = pose;
        if !wanted.hands {
            self.hand_rects = [None; 2];
            return Ok(());
        }

        // The hands: each one's own tracked region first, else the pose's guess.
        let min_visibility = self.config.min_wrist_visibility;
        for side in Side::BOTH {
            let s = side.index();
            let rect = self.hand_rects[s]
                .or_else(|| pose.as_ref().and_then(|p| hand_rect_from_pose(p, side, min_visibility, fw, fh)));
            self.hand_rects[s] = None;
            let Some(rect) = rect else { continue };
            self.track_hand(frame, side, &rect)?;
        }
        if let Some(p) = &pose {
            let min = self.config.min_wrist_visibility;
            reconcile(&mut self.landmarks.hands, &mut self.hand_rects, p, min, (fw, fh));
        }

        // A missing hand: search the frame for palms every few frames.
        if self.hand_rects.iter().all(Option::is_some) {
            self.since_palm = u32::MAX;
            return Ok(());
        }
        self.since_palm = self.since_palm.saturating_add(1);
        if self.since_palm < self.config.palm_every {
            return Ok(());
        }
        self.since_palm = 0;
        self.palm.detect(frame, 2, &mut self.palms)?;
        for i in 0..self.palms.len() {
            let rect = self.palms[i];
            if self.hand_rects.iter().flatten().any(|held| same_hand(held, &rect, fw, fh)) {
                continue;
            }
            // Which side: the nearest pose wrist, else the model's handedness on a first look.
            let side = match &pose {
                Some(p) => Side::BOTH
                    .into_iter()
                    .min_by(|x, y| {
                        let d = |side: Side| {
                            let wrist = p.image[POSE_HAND_POINTS[side.index()][0]];
                            ((rect.x_center - wrist.0) * fw).hypot((rect.y_center - wrist.1) * fh)
                        };
                        d(*x).total_cmp(&d(*y))
                    })
                    .unwrap_or(Side::Left),
                None => match self.hands[0].run(frame, &rect)? {
                    Some(h) if h.presence >= self.config.min_hand_presence => side_from_handedness(h.handedness),
                    _ => continue,
                },
            };
            if self.hand_rects[side.index()].is_none() {
                self.track_hand(frame, side, &rect)?;
            }
        }
        Ok(())
    }

    /// Runs `side`'s hand model on `rect`; a hand found there is kept with its next region.
    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
    fn track_hand(&mut self, frame: &RgbFrame<'_>, side: Side, rect: &NormRect) -> Result<()> {
        let s = side.index();
        let found = self.hands[s].run(frame, rect)?.filter(|h| h.presence >= self.config.min_hand_presence);
        self.hand_rects[s] = found.map(|h| h.next_rect(frame.width as f32, frame.height as f32));
        self.landmarks.hands[s] = found;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hand_region_from_the_pose_sits_on_the_knuckles_turned_up_the_forearm() {
        let mut pose = PoseLandmarks::NONE;
        pose.visibility = [1.0; POSE_POINTS];
        // The performer's left hand upright on screen: wrist below, knuckles above.
        pose.image[15] = (0.51, 0.6);
        pose.image[17] = (0.47, 0.5);
        pose.image[19] = (0.53, 0.5);
        let rect = hand_rect_from_pose(&pose, Side::Left, 0.5, 100.0, 100.0).unwrap_or(NormRect::whole_frame());
        assert!(rect.rotation.abs() < 1e-5, "upright: {rect:?}");
        // Middle knuckle at (0.51, 0.5), 0.1 from the wrist: side 0.2, grown 2.7 times, shifted a tenth up.
        assert!((rect.width - 0.2 * 2.7).abs() < 1e-3, "{rect:?}");
        assert!((rect.y_center - (0.5 - 0.02)).abs() < 1e-3, "{rect:?}");
        // A hidden wrist places nothing.
        pose.visibility[15] = 0.1;
        assert!(hand_rect_from_pose(&pose, Side::Left, 0.5, 100.0, 100.0).is_none());
    }

    #[test]
    fn hands_go_to_the_side_whose_wrist_they_are_at() {
        let mut pose = PoseLandmarks::NONE;
        pose.visibility = [1.0; POSE_POINTS];
        // The performer's left wrist on the image's right, the right wrist on its left.
        (pose.image[15], pose.image[16]) = ((0.6, 0.5), (0.4, 0.5));
        let hand_at = |x: f32| {
            let mut h = HandLandmarks::NONE;
            h.image[0] = (x, 0.5);
            Some(h)
        };
        let rect = |x: f32| Some(NormRect { x_center: x, ..NormRect::whole_frame() });
        let size = (100.0, 100.0);
        // Swapped hands swap back, with their regions.
        let (mut hands, mut rects) = ([hand_at(0.4), hand_at(0.6)], [rect(0.4), rect(0.6)]);
        reconcile(&mut hands, &mut rects, &pose, 0.5, size);
        assert_eq!(hands.map(|h| h.map(|h| h.image[0].0)), [Some(0.6), Some(0.4)]);
        assert_eq!(rects.map(|r| r.map(|r| r.x_center)), [Some(0.6), Some(0.4)]);
        // Both on the left wrist: the farther one lets go.
        let (mut hands, mut rects) = ([hand_at(0.61), hand_at(0.58)], [rect(0.61), rect(0.58)]);
        reconcile(&mut hands, &mut rects, &pose, 0.5, size);
        assert!(hands[0].is_some() && hands[1].is_none() && rects[1].is_none());
        // A lone hand found for the wrong side moves over.
        let (mut hands, mut rects) = ([hand_at(0.42), None], [rect(0.42), None]);
        reconcile(&mut hands, &mut rects, &pose, 0.5, size);
        assert!(hands[0].is_none() && hands[1].is_some());
        // An unseen wrist: the regions are trusted.
        pose.visibility[16] = 0.1;
        let (mut hands, mut rects) = ([hand_at(0.4), hand_at(0.6)], [rect(0.4), rect(0.6)]);
        reconcile(&mut hands, &mut rects, &pose, 0.5, size);
        assert_eq!(hands.map(|h| h.map(|h| h.image[0].0)), [Some(0.4), Some(0.6)]);
    }

    #[test]
    fn regions_on_one_hand_are_the_same_hand() {
        let a = NormRect { x_center: 0.5, y_center: 0.5, width: 0.2, height: 0.2, rotation: 0.0 };
        let near = NormRect { x_center: 0.55, ..a };
        let far = NormRect { x_center: 0.8, ..a };
        assert!(same_hand(&a, &near, 100.0, 100.0));
        assert!(!same_hand(&a, &far, 100.0, 100.0));
    }
}
