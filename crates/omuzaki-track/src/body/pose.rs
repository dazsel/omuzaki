//! MediaPipe's pose landmarker (BlazePose GHUM 3D, `pose_landmarker_{lite,full}.task`): the detector finds a person
//! and the landmark model gives 33 body landmarks on the image and in metres, with a visibility per landmark, plus two
//! auxiliary points that seed the next frame's region.
//!
//! Constants are the pose graphs' (`pose_detector_graph.cc`, `pose_landmarks_detector_graph.cc`): the detector reads a
//! 224x224 letterboxed frame in -1..1 over 2254 anchors (strides 8, 16, 32, 32, 32), 12 values each (a box and 4
//! keypoints: hip centre, full-body scale, shoulder centre, upper-body scale), weighted NMS at 0.3; the region is the
//! square around the hip centre reaching twice to the scale point, turned so that line points up and grown by 1.25.
//! The landmark model reads that square at 256x256 in 0..1 and gives 39 landmarks of 5 values (x, y, z in input
//! pixels, then the visibility and presence logits) and 39 world landmarks; 33 and 34 are the auxiliary hip centre and
//! scale point that make the next region the same way.

use std::f32::consts::FRAC_PI_2;

use omuzaki_ai_api::{Session, Signature};
use omuzaki_core::glam::Vec3;

use super::graph::{ImageGraph, image_signature};
use crate::error::Result;
use crate::frame::{Border, RgbFrame};
use crate::roi::NormRect;
use crate::ssd::{Detection, SsdLayout, decode, sigmoid, weighted_nms};

/// The body landmarks a pose has (MediaPipe's 33: face, arms, hands, legs).
pub const POSE_POINTS: usize = 33;
/// Landmarks the model predicts: the 33 and six auxiliary points.
const MODEL_POINTS: usize = 39;
/// The auxiliary hip centre and scale point.
const AUX_HIPS: usize = 33;
const AUX_SCALE: usize = 34;

const LAYOUT: SsdLayout = SsdLayout { input: 224, strides: &[8, 16, 32, 32, 32], coords: 12 };
const ANCHORS: usize = 2254;
const KEYPOINTS: usize = 4;
const NMS_IOU: f32 = 0.3;
const ROI_SCALE: f32 = 1.25;
const LANDMARK_INPUT: usize = 256;

/// The pose detector graph as a slot signature: a 224x224 image in -1..1, then the regressors and raw scores of the
/// 2254 anchors.
pub fn pose_detector_signature() -> Signature {
    image_signature(LAYOUT.input, &[("Identity", &[1, ANCHORS, 12]), ("Identity_1", &[1, ANCHORS, 1])])
}

/// The pose landmark graph as a slot signature, as `tools/models/mediapipe-body.sh` prunes it: a 256x256 crop in 0..1,
/// then the 39 landmarks (5 values each), the presence score and the 39 world landmarks.
pub fn pose_landmarks_signature() -> Signature {
    image_signature(
        LANDMARK_INPUT,
        &[("Identity", &[1, MODEL_POINTS * 5]), ("Identity_1", &[1, 1]), ("Identity_4", &[1, MODEL_POINTS * 3])],
    )
}

/// The region a pose's hip centre and scale point make.
fn pose_rect(hips: (f32, f32), scale: (f32, f32), frame_w: f32, frame_h: f32) -> NormRect {
    NormRect::aligned(hips, scale, frame_w, frame_h, FRAC_PI_2).expanded(frame_w, frame_h, ROI_SCALE, true)
}

/// The pose detector.
pub struct PoseDetector {
    graph: ImageGraph,
    anchors: Vec<(f32, f32)>,
    min_score: f32,
    candidates: Vec<Detection<KEYPOINTS>>,
    merged: Vec<Detection<KEYPOINTS>>,
}

impl PoseDetector {
    /// Wraps a session of [`pose_detector_signature`]; people scoring below `min_score` are ignored.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`](crate::FaceTrackError::Session) when the session's signature differs.
    pub fn new(session: Box<dyn Session>, min_score: f32) -> Result<Self> {
        let graph = ImageGraph::new("pose detector", session, &pose_detector_signature(), (-1.0, 1.0), Border::Zero)?;
        Ok(Self {
            graph,
            anchors: LAYOUT.anchor_centers(),
            min_score,
            candidates: Vec::with_capacity(ANCHORS),
            merged: Vec::with_capacity(ANCHORS),
        })
    }

    /// Forgets the session's carried state.
    pub fn reset(&mut self) {
        self.graph.reset();
    }

    /// The region of the most likely person in `frame`, or `None` for nobody.
    ///
    /// # Errors
    /// A session failure or an output of the wrong size.
    pub fn detect(&mut self, frame: &RgbFrame<'_>) -> Result<Option<NormRect>> {
        let Some(placement) = self.graph.run(frame, &NormRect::whole_frame())? else { return Ok(None) };
        let regressors = self.graph.output(0, ANCHORS * LAYOUT.coords)?;
        let scores = self.graph.output(1, ANCHORS)?;
        decode(&LAYOUT, &self.anchors, regressors, scores, self.min_score, &mut self.candidates);
        weighted_nms(&mut self.candidates, NMS_IOU, &mut self.merged);
        Ok(self.merged.first().map(|best| {
            let to_frame = |(u, v): (f32, f32)| placement.frame_point(1, u, v);
            pose_rect(to_frame(best.keypoints[0]), to_frame(best.keypoints[1]), placement.fw, placement.fh)
        }))
    }
}

/// One person's landmarks.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PoseLandmarks {
    /// On the image: normalised frame coordinates.
    pub image: [(f32, f32); POSE_POINTS],
    /// In metres around the hips' centre, in the camera's axes (x to the image's right, y down, z away from the
    /// camera).
    pub world: [Vec3; POSE_POINTS],
    /// How likely each landmark is to be visible (in the frame and not hidden), `0..=1`.
    pub visibility: [f32; POSE_POINTS],
    /// How sure the model is that a person fills the region, `0..=1`.
    pub presence: f32,
    /// The region to crop on the next frame.
    pub next: NormRect,
}

impl PoseLandmarks {
    /// No person.
    pub const NONE: Self = Self {
        image: [(0.0, 0.0); POSE_POINTS],
        world: [Vec3::ZERO; POSE_POINTS],
        visibility: [0.0; POSE_POINTS],
        presence: 0.0,
        next: NormRect { x_center: 0.5, y_center: 0.5, width: 1.0, height: 1.0, rotation: 0.0 },
    };
}

/// The pose landmark model.
pub struct PoseModel {
    graph: ImageGraph,
}

impl PoseModel {
    /// Wraps a session of [`pose_landmarks_signature`].
    ///
    /// # Errors
    /// [`FaceTrackError::Session`](crate::FaceTrackError::Session) when the session's signature differs.
    pub fn new(session: Box<dyn Session>) -> Result<Self> {
        let graph =
            ImageGraph::new("pose landmarks", session, &pose_landmarks_signature(), (0.0, 1.0), Border::Replicate)?;
        Ok(Self { graph })
    }

    /// Forgets the session's carried state.
    pub fn reset(&mut self) {
        self.graph.reset();
    }

    /// The person in `rect` of `frame`, or `None` when the region has no area. The presence is reported whatever it
    /// is; the caller decides what counts as a person.
    ///
    /// # Errors
    /// A session failure or an output of the wrong size.
    pub fn run(&mut self, frame: &RgbFrame<'_>, rect: &NormRect) -> Result<Option<PoseLandmarks>> {
        let Some(placement) = self.graph.run(frame, rect)? else { return Ok(None) };
        let raw = self.graph.output(0, MODEL_POINTS * 5)?;
        let presence = self.graph.output(1, 1)?[0];
        let world = self.graph.output(2, MODEL_POINTS * 3)?;
        let side = self.graph.side();
        let mut image = [(0.0, 0.0); MODEL_POINTS];
        let mut out = PoseLandmarks { presence, ..PoseLandmarks::NONE };
        for (i, values) in raw.as_chunks::<5>().0.iter().enumerate() {
            image[i] = placement.frame_point(side, values[0], values[1]);
            if i < POSE_POINTS {
                out.image[i] = image[i];
                out.visibility[i] = sigmoid(values[3]);
            }
        }
        let (sin, cos) = rect.rotation.sin_cos();
        for (point, xyz) in out.world.iter_mut().zip(world.as_chunks::<3>().0) {
            *point = Vec3::new(cos * xyz[0] - sin * xyz[1], sin * xyz[0] + cos * xyz[1], xyz[2]);
        }
        out.next = pose_rect(image[AUX_HIPS], image[AUX_SCALE], placement.fw, placement.fh);
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_has_the_models_anchor_count() {
        assert_eq!(LAYOUT.anchor_centers().len(), ANCHORS);
    }

    #[test]
    fn the_region_is_a_square_around_the_hips_reaching_past_the_scale_point() {
        // Scale point straight above the hips: upright, side 2 x 0.2, grown 1.25 times.
        let r = pose_rect((0.5, 0.8), (0.5, 0.6), 100.0, 100.0);
        assert!(r.rotation.abs() < 1e-6 && (r.width - 0.5).abs() < 1e-6 && (r.y_center - 0.8).abs() < 1e-6);
    }
}
