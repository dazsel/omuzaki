//! MediaPipe's palm detector (`hand_detector.tflite`, the full-range 192x192 model of `hand_landmarker.task`): the
//! whole frame letterboxed in, palms out, each turned into the region the hand landmark model crops.
//!
//! Constants are the hand detector graph's (`hand_detector_graph.cc` and its `palm_detection` options): 2016 SSD
//! anchors over strides 8, 16, 16, 16; 18 values per anchor (a box and 7 keypoints); weighted NMS at 0.3; the
//! region is the box turned so the wrist-to-middle-knuckle line points up, shifted half a box toward the fingers,
//! squared and grown 2.6 times.

use std::f32::consts::FRAC_PI_2;

use omuzaki_ai_api::{Session, Signature};

use super::graph::{ImageGraph, image_signature};
use crate::error::Result;
use crate::frame::{Border, RgbFrame};
use crate::roi::{BBox, NormRect, normalize_radians, rotation_from_points};
use crate::ssd::{Detection, SsdLayout, decode, weighted_nms};

const LAYOUT: SsdLayout = SsdLayout { input: 192, strides: &[8, 16, 16, 16], coords: 18 };
const ANCHORS: usize = 2016;
const KEYPOINTS: usize = 7;
const NMS_IOU: f32 = 0.3;
/// The keypoints that set the region's rotation: the wrist and the middle finger's knuckle.
const WRIST: usize = 0;
const MIDDLE_MCP: usize = 2;
/// The region grows the palm box this much, after moving it this many boxes toward the fingers.
const ROI_SCALE: f32 = 2.6;
const ROI_SHIFT_Y: f32 = -0.5;

/// The palm detector graph as a slot signature: a 192x192 image in 0..1, then the regressors and raw scores of the
/// 2016 anchors. Output names are the converted graph's own.
pub fn palm_signature() -> Signature {
    image_signature(192, &[("Identity", &[1, ANCHORS, 18]), ("Identity_1", &[1, ANCHORS, 1])])
}

/// The palm detector.
pub struct PalmDetector {
    graph: ImageGraph,
    anchors: Vec<(f32, f32)>,
    min_score: f32,
    candidates: Vec<Detection<KEYPOINTS>>,
    merged: Vec<Detection<KEYPOINTS>>,
}

impl PalmDetector {
    /// Wraps a session of [`palm_signature`]; palms scoring below `min_score` are ignored.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`](crate::FaceTrackError::Session) when the session's signature differs.
    pub fn new(session: Box<dyn Session>, min_score: f32) -> Result<Self> {
        let graph = ImageGraph::new("palm", session, &palm_signature(), (0.0, 1.0), Border::Zero)?;
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

    /// Finds palms in `frame` and writes the regions to crop for the landmark model into `out` (cleared first), best
    /// first, at most `max`.
    ///
    /// # Errors
    /// A session failure or an output of the wrong size.
    pub fn detect(&mut self, frame: &RgbFrame<'_>, max: usize, out: &mut Vec<NormRect>) -> Result<()> {
        out.clear();
        let Some(placement) = self.graph.run(frame, &NormRect::whole_frame())? else { return Ok(()) };
        let regressors = self.graph.output(0, ANCHORS * LAYOUT.coords)?;
        let scores = self.graph.output(1, ANCHORS)?;
        decode(&LAYOUT, &self.anchors, regressors, scores, self.min_score, &mut self.candidates);
        weighted_nms(&mut self.candidates, NMS_IOU, &mut self.merged);
        let (fw, fh) = (placement.fw, placement.fh);
        // Detections are in fractions of the input, so they map through a "side" of one.
        let to_frame = |(u, v): (f32, f32)| placement.to_frame(1, u, v);
        for palm in self.merged.iter().take(max) {
            let (x0, y0) = to_frame((palm.bbox.xmin, palm.bbox.ymin));
            let (x1, y1) = to_frame((palm.bbox.xmin + palm.bbox.width, palm.bbox.ymin + palm.bbox.height));
            let bbox = BBox { xmin: x0.min(x1), ymin: y0.min(y1), width: (x1 - x0).abs(), height: (y1 - y0).abs() };
            let (wrist, knuckle) = (to_frame(palm.keypoints[WRIST]), to_frame(palm.keypoints[MIDDLE_MCP]));
            let rect = bbox.to_rect(normalize_radians(FRAC_PI_2 + rotation_from_points(wrist, knuckle, fw, fh)));
            out.push(rect.shifted(fw, fh, 0.0, ROI_SHIFT_Y).expanded(fw, fh, ROI_SCALE, true));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_has_the_models_anchor_count() {
        assert_eq!(LAYOUT.anchor_centers().len(), ANCHORS);
    }
}
