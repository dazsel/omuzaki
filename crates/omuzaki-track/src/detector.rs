//! BlazeFace short-range face detector: letterbox the frame to 128x128, run the model, decode SSD anchors, merge
//! overlapping boxes, and turn the best face into a rotated ROI for the landmark model.
//!
//! The decoding constants are MediaPipe's own for this model (`face_detector_graph.cc`, "legacy model without
//! metadata" branch, which is what the bundled `face_detector.tflite` uses).

use omuzaki_ai_api::{DType, Dim, Session, Signature, TensorBuf, TensorRef, TensorSpec};

use crate::error::{FaceTrackError, Result};
use crate::frame::{Border, ImageSpec, RgbFrame, warp_to_tensor};
use crate::roi::{BBox, NormRect, RotatedRect, pad_to_aspect, roi_to_frame, rotation_from_points};

/// Detector input side in pixels.
const INPUT: usize = 128;
/// Anchor strides of the four SSD layers; the last three share a stride and are merged into one feature map.
const STRIDES: [usize; 4] = [8, 16, 16, 16];
/// Anchors per feature-map cell and layer: aspect ratio 1.0 plus the interpolated-scale anchor.
const ANCHORS_PER_LAYER: usize = 2;
/// Number of anchors (and rows in the model's outputs).
const NUM_ANCHORS: usize = 896;
/// Values per anchor in the regression output: 4 box coordinates then 6 keypoints (x, y).
const COORDS: usize = 16;
/// Keypoints per detection (eyes, nose, mouth, ears).
const KEYPOINTS: usize = 6;
/// Raw scores are clamped before the sigmoid so it cannot overflow.
const SCORE_CLIP: f32 = 100.0;
/// Boxes overlapping the best box by more than this are merged into it (`min_suppression_threshold`, whose default in
/// MediaPipe Tasks' `FaceDetectorGraphOptions` is 0.5; `FaceLandmarker` does not override it).
const NMS_IOU: f32 = 0.5;
/// Keypoint indices MediaPipe levels to get the ROI rotation: the two eyes.
const EYE_KEYPOINTS: (usize, usize) = (0, 1);
/// The ROI handed to the landmark model is the detection scaled by this much.
const ROI_SCALE: f32 = 1.5;

/// The detector graph (`face_detector`) as a slot signature: an NHWC image in -1..1, then the box and keypoint
/// regressors and the raw scores of the 896 anchors. Output names are the graph's own.
pub fn detector_signature() -> Signature {
    Signature {
        inputs: vec![TensorSpec::new("image", DType::F32, [1, INPUT, INPUT, 3].map(Dim::Fixed))],
        outputs: vec![
            TensorSpec::new("regressors", DType::F32, [1, NUM_ANCHORS, COORDS].map(Dim::Fixed)),
            TensorSpec::new("classificators", DType::F32, [1, NUM_ANCHORS, 1].map(Dim::Fixed)),
        ],
    }
}

/// One decoded detection in normalised coordinates of the (letterboxed) model input.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Detection {
    /// Index of the anchor it came from: the tie-break that makes an unstable sort order equal scores like a stable one.
    anchor: usize,
    score: f32,
    bbox: BBox,
    keypoints: [(f32, f32); KEYPOINTS],
}

/// Anchor centres in normalised input coordinates. The model's anchors all have unit width and height
/// (`fixed_anchor_size`), so only the centres matter.
fn anchor_centers() -> Vec<(f32, f32)> {
    let mut anchors = Vec::with_capacity(NUM_ANCHORS);
    let mut layer = 0;
    while layer < STRIDES.len() {
        let stride = STRIDES[layer];
        let mut same = 0;
        while layer + same < STRIDES.len() && STRIDES[layer + same] == stride {
            same += 1;
        }
        let cells = INPUT.div_ceil(stride);
        for y in 0..cells {
            for x in 0..cells {
                for _ in 0..same * ANCHORS_PER_LAYER {
                    #[allow(clippy::cast_precision_loss)] // cell indices are tiny
                    anchors.push(((x as f32 + 0.5) / cells as f32, (y as f32 + 0.5) / cells as f32));
                }
            }
        }
        layer += same;
    }
    anchors
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// Decodes the model's raw outputs into `out` (cleared first): the detections scoring at least `min_score`.
#[allow(clippy::cast_precision_loss)] // INPUT is a small constant
fn decode(anchors: &[(f32, f32)], regressors: &[f32], scores: &[f32], min_score: f32, out: &mut Vec<Detection>) {
    let scale = INPUT as f32;
    out.clear();
    for (i, &(ax, ay)) in anchors.iter().enumerate() {
        let score = sigmoid(scores[i].clamp(-SCORE_CLIP, SCORE_CLIP));
        if score < min_score {
            continue;
        }
        let raw = &regressors[i * COORDS..(i + 1) * COORDS];
        let (xc, yc) = (raw[0] / scale + ax, raw[1] / scale + ay);
        let (w, h) = (raw[2] / scale, raw[3] / scale);
        let mut keypoints = [(0.0, 0.0); KEYPOINTS];
        for (k, kp) in keypoints.iter_mut().enumerate() {
            *kp = (raw[4 + 2 * k] / scale + ax, raw[5 + 2 * k] / scale + ay);
        }
        out.push(Detection {
            anchor: i,
            score,
            bbox: BBox { xmin: xc - w / 2.0, ymin: yc - h / 2.0, width: w, height: h },
            keypoints,
        });
    }
}

/// MediaPipe's weighted non-max suppression: take the best box, average it (and its keypoints) with every box
/// that overlaps it, weighted by score, and repeat on what is left. `kept` (cleared first) receives the merged
/// detections, best first; `detections` is used up. Neither vector allocates once it has room for every anchor.
fn weighted_nms(detections: &mut Vec<Detection>, kept: &mut Vec<Detection>) {
    detections.sort_unstable_by(|a, b| b.score.total_cmp(&a.score).then(a.anchor.cmp(&b.anchor)));
    kept.clear();
    while let Some(&best) = detections.first() {
        // One pass over the remaining boxes, best first: overlapping ones (and the best itself, whatever its area)
        // join the average in that order, the others move down to be the next round's candidates.
        let mut total = 0.0f32;
        let (mut x0, mut y0, mut x1, mut y1) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        let mut keypoints = [(0.0f32, 0.0f32); KEYPOINTS];
        let mut remaining = 0;
        for read in 0..detections.len() {
            let d = detections[read];
            if read > 0 && d.bbox.iou(&best.bbox) <= NMS_IOU {
                detections[remaining] = d;
                remaining += 1;
                continue;
            }
            total += d.score;
            x0 += d.bbox.xmin * d.score;
            y0 += d.bbox.ymin * d.score;
            x1 += (d.bbox.xmin + d.bbox.width) * d.score;
            y1 += (d.bbox.ymin + d.bbox.height) * d.score;
            for (kp, k) in keypoints.iter_mut().zip(&d.keypoints) {
                *kp = (kp.0 + k.0 * d.score, kp.1 + k.1 * d.score);
            }
        }
        detections.truncate(remaining);
        let (x0, y0, x1, y1) = (x0 / total, y0 / total, x1 / total, y1 / total);
        kept.push(Detection {
            anchor: best.anchor,
            score: best.score,
            bbox: BBox { xmin: x0, ymin: y0, width: x1 - x0, height: y1 - y0 },
            keypoints: keypoints.map(|(x, y)| (x / total, y / total)),
        });
    }
}

/// The face detector: session, anchors and reusable buffers.
pub struct FaceDetector {
    session: Box<dyn Session>,
    anchors: Vec<(f32, f32)>,
    input: Vec<f32>,
    outputs: Vec<TensorBuf>,
    min_score: f32,
    /// Decoded detections, then what is left of them while merging.
    candidates: Vec<Detection>,
    /// The merged detections, best first.
    merged: Vec<Detection>,
}

impl FaceDetector {
    /// Wraps a session of the detector graph; faces scoring below `min_score` (0..1) are ignored.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`] when the session's signature is not [`detector_signature`].
    pub fn new(session: Box<dyn Session>, min_score: f32) -> Result<Self> {
        crate::error::check_signature("detector", &detector_signature(), session.signature())?;
        let anchors = anchor_centers();
        debug_assert_eq!(anchors.len(), NUM_ANCHORS, "the layer table generates the model's anchors");
        let outputs = TensorBuf::for_outputs(session.signature());
        Ok(Self {
            session,
            anchors,
            input: vec![0.0; INPUT * INPUT * 3],
            outputs,
            min_score,
            candidates: Vec::with_capacity(NUM_ANCHORS),
            merged: Vec::with_capacity(NUM_ANCHORS),
        })
    }

    /// Forgets the session's carried state (see [`Session::reset`]).
    pub fn reset(&mut self) {
        self.session.reset();
    }

    /// Finds the best face in `frame` and returns the ROI to crop for the landmark model, or `None` for no face.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`] when inference fails and [`FaceTrackError::Shape`] when the outputs are not the
    /// expected size.
    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
    pub fn detect(&mut self, frame: &RgbFrame<'_>) -> Result<Option<NormRect>> {
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        // The whole frame, letterboxed into the square input; `padded` is the area of the frame the input covers.
        let mut padded = NormRect::whole_frame().to_pixels(fw, fh);
        pad_to_aspect(&mut padded, INPUT, INPUT)
            .ok_or(FaceTrackError::BadFrame { width: frame.width, height: frame.height })?;
        warp_to_tensor(
            frame,
            &padded,
            ImageSpec { width: INPUT, height: INPUT, range: (-1.0, 1.0), border: Border::Zero },
            &mut self.input,
        )?;
        self.session.run(&[TensorRef::f32(&self.input, &[1, INPUT, INPUT, 3])], &mut self.outputs)?;
        let (Some(regressors), Some(scores)) = (self.outputs[0].as_f32(), self.outputs[1].as_f32()) else {
            return Err(FaceTrackError::Shape { graph: "detector" });
        };
        if regressors.len() != NUM_ANCHORS * COORDS || scores.len() != NUM_ANCHORS {
            return Err(FaceTrackError::Shape { graph: "detector" });
        }
        decode(&self.anchors, regressors, scores, self.min_score, &mut self.candidates);
        weighted_nms(&mut self.candidates, &mut self.merged);
        Ok(self.merged.first().map(|best| face_roi(best, &padded, fw, fh)))
    }
}

/// Projects a detection from model-input coordinates to the frame and builds the landmark ROI: the box, rotated
/// to level the eyes, scaled up so it takes in the whole face.
fn face_roi(best: &Detection, padded: &RotatedRect, fw: f32, fh: f32) -> NormRect {
    let to_frame = |(u, v): (f32, f32)| roi_to_frame(padded, fw, fh, u, v);
    let (x0, y0) = to_frame((best.bbox.xmin, best.bbox.ymin));
    let (x1, y1) = to_frame((best.bbox.xmin + best.bbox.width, best.bbox.ymin + best.bbox.height));
    let bbox = BBox { xmin: x0, ymin: y0, width: x1 - x0, height: y1 - y0 };
    let (left_eye, right_eye) = (to_frame(best.keypoints[EYE_KEYPOINTS.0]), to_frame(best.keypoints[EYE_KEYPOINTS.1]));
    let rotation = rotation_from_points(left_eye, right_eye, fw, fh);
    bbox.to_rect(rotation).expanded(fw, fh, ROI_SCALE, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_match_the_models_layout() {
        let a = anchor_centers();
        assert_eq!(a.len(), NUM_ANCHORS);
        // First layer: 16x16 cells, two anchors each, row-major, centred in the cell.
        assert_eq!(a[0], (0.5 / 16.0, 0.5 / 16.0));
        assert_eq!(a[1], a[0]);
        assert_eq!(a[2], (1.5 / 16.0, 0.5 / 16.0));
        // Second group starts after 16*16*2 anchors: 8x8 cells, six anchors each.
        assert_eq!(a[512], (0.5 / 8.0, 0.5 / 8.0));
        assert_eq!(a[517], a[512]);
        assert_eq!(a[518], (1.5 / 8.0, 0.5 / 8.0));
    }

    fn det(score: f32, x: f32, y: f32, s: f32) -> Detection {
        Detection {
            anchor: 0,
            score,
            bbox: BBox { xmin: x, ymin: y, width: s, height: s },
            keypoints: [(x, y); KEYPOINTS],
        }
    }

    #[test]
    fn nms_merges_overlaps_by_score_and_keeps_distant_boxes() {
        let mut input = vec![det(0.9, 0.1, 0.1, 0.4), det(0.3, 0.12, 0.1, 0.4), det(0.8, 0.7, 0.7, 0.2)];
        let mut merged = Vec::new();
        weighted_nms(&mut input, &mut merged);
        assert_eq!(merged.len(), 2);
        assert!((merged[0].score - 0.9).abs() < 1e-6, "best box first");
        // Weighted average of 0.1 (weight 0.9) and 0.12 (weight 0.3).
        assert!((merged[0].bbox.xmin - (0.1 * 0.9 + 0.12 * 0.3) / 1.2).abs() < 1e-6);
        assert!((merged[1].bbox.xmin - 0.7).abs() < 1e-6);
    }

    #[test]
    fn decode_applies_anchor_offsets_and_threshold() {
        let anchors = [(0.5, 0.5), (0.25, 0.25)];
        let mut reg = vec![0.0; 2 * COORDS];
        reg[2] = 64.0; // width = 64 / 128 = 0.5
        reg[3] = 32.0; // height = 0.25
        reg[4] = 12.8; // first keypoint x offset = 0.1
        let mut found = Vec::new();
        decode(&anchors, &reg, &[10.0, -10.0], 0.5, &mut found);
        assert_eq!(found.len(), 1, "the second anchor scores below the threshold");
        let d = &found[0];
        assert!(
            (d.bbox.xmin - 0.25).abs() < 1e-6
                && (d.bbox.width - 0.5).abs() < 1e-6
                && (d.bbox.height - 0.25).abs() < 1e-6
        );
        assert!((d.keypoints[0].0 - 0.6).abs() < 1e-5);
    }

    #[test]
    fn roi_levels_the_eyes_and_grows_the_box() {
        let padded = RotatedRect { cx: 50.0, cy: 50.0, width: 100.0, height: 100.0, rotation: 0.0 };
        let mut d = det(0.9, 0.4, 0.4, 0.2);
        d.keypoints[0] = (0.45, 0.45);
        d.keypoints[1] = (0.55, 0.45);
        let r = face_roi(&d, &padded, 100.0, 100.0);
        assert!(r.rotation.abs() < 1e-6 && (r.width - 0.3).abs() < 1e-5 && (r.x_center - 0.5).abs() < 1e-5);
    }
}
