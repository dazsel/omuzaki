//! The face-mesh model: crop the ROI to 256x256, predict 478 landmarks, project them back onto the frame, and derive
//! the ROI for the next frame so the detector can be skipped while the face keeps being found.

use std::path::Path;
use std::time::Instant;

use crate::error::{Result, TrackError};
use crate::frame::{Border, Frame, TensorSpec, warp_to_tensor};
use crate::roi::{BBox, Letterbox, NormRect, pad_to_aspect, roi_to_frame, rotation_from_points};
use crate::runtime::{Model, RuntimeOptions, output_f32};

/// Model input side in pixels.
const INPUT: usize = 256;
/// Landmarks the model predicts: 468 mesh points plus 10 iris points.
pub const NUM_LANDMARKS: usize = 478;
/// Landmark indices that level the ROI: outer corners of the two eyes.
const EYE_LANDMARKS: (usize, usize) = (33, 263);
/// The next frame's ROI is the landmark box scaled up by this much, so a moving face stays inside it.
const NEXT_ROI_SCALE: f32 = 1.5;

/// A landmark in normalised frame coordinates; `z` is depth relative to the face, scaled like `x`.
pub type Landmark = [f32; 3];

/// What the landmark model found in one ROI.
#[derive(Debug, Clone)]
pub struct FaceLandmarks {
    /// 478 landmarks.
    pub points: Vec<Landmark>,
    /// Probability that a face is really in the ROI.
    pub presence: f32,
}

/// Seconds spent in each part of one landmark call.
#[derive(Debug, Clone, Copy, Default)]
pub struct LandmarkTimings {
    /// Warping the ROI into the input tensor.
    pub preprocess: f64,
    /// The ONNX Runtime call.
    pub inference: f64,
    /// Projecting landmarks back onto the frame.
    pub decode: f64,
}

/// The landmark model with a reusable input buffer.
pub struct LandmarkModel {
    model: Model,
    input: Vec<f32>,
}

impl LandmarkModel {
    /// Loads `face_landmarks_detector.onnx`.
    ///
    /// # Errors
    /// Returns an error when the model cannot be loaded.
    pub fn load(path: &Path, options: &RuntimeOptions) -> Result<Self> {
        Ok(Self { model: Model::load(path, options)?, input: vec![0.0; INPUT * INPUT * 3] })
    }

    /// Predicts landmarks for the face inside `rect` (an ROI from the detector or from the previous frame).
    ///
    /// # Errors
    /// Returns an error when the ROI has no area, inference fails or the outputs are not the expected shape.
    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
    pub fn run(&mut self, frame: &Frame, rect: &NormRect, timings: &mut LandmarkTimings) -> Result<FaceLandmarks> {
        let (fw, fh) = (frame.width() as f32, frame.height() as f32);
        let t0 = Instant::now();
        let roi = rect.to_pixels(fw, fh);
        let mut padded = roi;
        let letterbox = pad_to_aspect(&mut padded, INPUT, INPUT)?;
        warp_to_tensor(
            frame,
            &padded,
            TensorSpec { width: INPUT, height: INPUT, range: (0.0, 1.0), border: Border::Replicate },
            &mut self.input,
        )?;
        let t1 = Instant::now();
        let outputs = self.model.run(&[1, INPUT, INPUT, 3], &self.input)?;
        let t2 = Instant::now();
        let raw = output_f32(&outputs, "Identity")?;
        let presence_logit = output_f32(&outputs, "Identity_1")?;
        if raw.len() != NUM_LANDMARKS * 3 || presence_logit.len() != 1 {
            return Err(TrackError::Model(format!(
                "landmark outputs have {} and {} values",
                raw.len(),
                presence_logit.len()
            )));
        }
        let points = project_landmarks(raw, &letterbox, &roi, fw, fh);
        timings.preprocess = (t1 - t0).as_secs_f64();
        timings.inference = (t2 - t1).as_secs_f64();
        timings.decode = t2.elapsed().as_secs_f64();
        Ok(FaceLandmarks { points, presence: 1.0 / (1.0 + (-presence_logit[0]).exp()) })
    }
}

/// Turns the model's raw output (x, y, z in input pixels) into normalised frame coordinates: scale to the unit
/// square, remove the letterbox padding, then map through the ROI. `z` is scaled with the ROI width, as in MediaPipe.
#[allow(clippy::cast_precision_loss)] // INPUT is a small constant
fn project_landmarks(
    raw: &[f32],
    letterbox: &Letterbox,
    roi: &crate::roi::RotatedRect,
    fw: f32,
    fh: f32,
) -> Vec<Landmark> {
    let side = INPUT as f32;
    let (horizontal, vertical) = (1.0 - letterbox.left - letterbox.right, 1.0 - letterbox.top - letterbox.bottom);
    // How long the ROI's unit x step is in the frame: the scale applied to z.
    let (origin, unit_x) = (roi_to_frame(roi, fw, fh, 0.0, 0.0), roi_to_frame(roi, fw, fh, 1.0, 0.0));
    let z_scale = (unit_x.0 - origin.0).hypot(unit_x.1 - origin.1);
    raw.as_chunks::<3>()
        .0
        .iter()
        .map(|p| {
            let (u, v) = ((p[0] / side - letterbox.left) / horizontal, (p[1] / side - letterbox.top) / vertical);
            let (x, y) = roi_to_frame(roi, fw, fh, u, v);
            [x, y, z_scale * (p[2] / side / horizontal)]
        })
        .collect()
}

/// The ROI for the next frame, from this frame's landmarks: their bounding box, rotated to level the eyes, made
/// square and scaled up. Returns `None` for too few landmarks.
#[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
pub fn next_frame_rect(points: &[Landmark], frame_w: usize, frame_h: usize) -> Option<NormRect> {
    let (fw, fh) = (frame_w as f32, frame_h as f32);
    let bbox = BBox::around(points.iter().map(|p| (p[0], p[1])))?;
    let (left, right) = (points.get(EYE_LANDMARKS.0)?, points.get(EYE_LANDMARKS.1)?);
    let rotation = rotation_from_points((left[0], left[1]), (right[0], right[1]), fw, fh);
    Some(bbox.to_rect(rotation).expanded(fw, fh, NEXT_ROI_SCALE, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roi::RotatedRect;

    #[test]
    fn centre_of_the_input_maps_to_the_roi_centre_and_padding_is_removed() {
        let roi = RotatedRect { cx: 300.0, cy: 200.0, width: 100.0, height: 50.0, rotation: 0.0 };
        // A 100x50 ROI padded into a square input: 25% of the height is padding top and bottom.
        let lb = Letterbox { left: 0.0, top: 0.25, right: 0.0, bottom: 0.25 };
        let mut raw = vec![0.0; 3 * 2];
        raw[0] = 128.0; // centre of the input
        raw[1] = 128.0;
        raw[3] = 0.0; // top-left of the input is in the top padding: above the ROI
        raw[4] = 0.0;
        let pts = project_landmarks(&raw, &lb, &roi, 600.0, 400.0);
        assert!((pts[0][0] - 0.5).abs() < 1e-6 && (pts[0][1] - 0.5).abs() < 1e-6);
        // v = (0 - 0.25) / 0.5 = -0.5 above the ROI's top edge: y = (200 - 25 - 25) / 400.
        assert!((pts[1][1] - 150.0 / 400.0).abs() < 1e-6 && (pts[1][0] - 250.0 / 600.0).abs() < 1e-6);
    }

    #[test]
    fn next_rect_is_square_scaled_and_follows_the_eye_line() {
        let mut pts = vec![[0.5, 0.5, 0.0]; NUM_LANDMARKS];
        pts[0] = [0.3, 0.2, 0.0];
        pts[1] = [0.7, 0.8, 0.0];
        pts[33] = [0.4, 0.5, 0.0];
        pts[263] = [0.6, 0.5, 0.0];
        let r = next_frame_rect(&pts, 100, 100).unwrap();
        // Box is 0.4 x 0.6 of a square frame: the long side 0.6, scaled by 1.5.
        assert!((r.width - 0.9).abs() < 1e-5 && (r.height - 0.9).abs() < 1e-5);
        assert!(r.rotation.abs() < 1e-6);
        assert!(next_frame_rect(&[], 100, 100).is_none());
    }
}
