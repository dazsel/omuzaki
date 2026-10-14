//! The face-mesh model: crop the ROI to 256x256, predict 478 landmarks, project them back onto the frame, and derive
//! the ROI for the next frame so the detector can be skipped while the face keeps being found.

use omuzaki_ai_api::{DType, Dim, Session, Signature, TensorBuf, TensorRef, TensorSpec};

use crate::error::{FaceTrackError, Result};
use crate::frame::{Border, ImageSpec, RgbFrame, warp_to_tensor};
use crate::roi::{BBox, Letterbox, NormRect, pad_to_aspect, roi_to_frame, rotation_from_points};

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

/// The landmark graph (`face_landmarks_detector`) as a slot signature: an NHWC image in 0..1, the landmarks as
/// x, y, z in input pixels, and the presence logit. Output names are the graph's own.
pub fn landmarks_signature() -> Signature {
    Signature {
        inputs: vec![TensorSpec::new("image", DType::F32, [1, INPUT, INPUT, 3].map(Dim::Fixed))],
        outputs: vec![
            TensorSpec::new("Identity", DType::F32, [1, 1, 1, NUM_LANDMARKS * 3].map(Dim::Fixed)),
            TensorSpec::new("Identity_1", DType::F32, [1, 1, 1, 1].map(Dim::Fixed)),
        ],
    }
}

/// The landmark model with reusable input and output buffers.
pub struct LandmarkModel {
    session: Box<dyn Session>,
    input: Vec<f32>,
    outputs: Vec<TensorBuf>,
}

impl LandmarkModel {
    /// Wraps a session of the landmark graph.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`] when the session's signature is not [`landmarks_signature`].
    pub fn new(session: Box<dyn Session>) -> Result<Self> {
        crate::error::check_signature("landmarks", &landmarks_signature(), session.signature())?;
        let outputs = TensorBuf::for_outputs(session.signature());
        Ok(Self { session, input: vec![0.0; INPUT * INPUT * 3], outputs })
    }

    /// Forgets the session's carried state (see [`Session::reset`]).
    pub fn reset(&mut self) {
        self.session.reset();
    }

    /// Predicts landmarks for the face inside `rect` (an ROI from the detector or from the previous frame) into
    /// `points`, and returns the probability that a face is really in the ROI. `None` when the ROI has no area.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`] when inference fails and [`FaceTrackError::Shape`] when the outputs are not the
    /// expected size.
    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
    pub fn run(
        &mut self,
        frame: &RgbFrame<'_>,
        rect: &NormRect,
        points: &mut [Landmark; NUM_LANDMARKS],
    ) -> Result<Option<f32>> {
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let roi = rect.to_pixels(fw, fh);
        let mut padded = roi;
        let Some(letterbox) = pad_to_aspect(&mut padded, INPUT, INPUT) else { return Ok(None) };
        warp_to_tensor(
            frame,
            &padded,
            ImageSpec { width: INPUT, height: INPUT, range: (0.0, 1.0), border: Border::Replicate },
            &mut self.input,
        )?;
        self.session.run(&[TensorRef::f32(&self.input, &[1, INPUT, INPUT, 3])], &mut self.outputs)?;
        let (Some(raw), Some(presence_logit)) = (self.outputs[0].as_f32(), self.outputs[1].as_f32()) else {
            return Err(FaceTrackError::Shape { graph: "landmarks" });
        };
        if raw.len() != NUM_LANDMARKS * 3 || presence_logit.len() != 1 {
            return Err(FaceTrackError::Shape { graph: "landmarks" });
        }
        project_landmarks(raw, &letterbox, &roi, fw, fh, points);
        Ok(Some(1.0 / (1.0 + (-presence_logit[0]).exp())))
    }
}

/// Turns the model's raw output (x, y, z in input pixels) into normalised frame coordinates in `out`: scale to the
/// unit square, remove the letterbox padding, then map through the ROI. `z` is scaled with the ROI width, as in
/// MediaPipe.
#[allow(clippy::cast_precision_loss)] // INPUT is a small constant
fn project_landmarks(
    raw: &[f32],
    letterbox: &Letterbox,
    roi: &crate::roi::RotatedRect,
    fw: f32,
    fh: f32,
    out: &mut [Landmark],
) {
    let side = INPUT as f32;
    let (horizontal, vertical) = (1.0 - letterbox.left - letterbox.right, 1.0 - letterbox.top - letterbox.bottom);
    // How long the ROI's unit x step is in the frame: the scale applied to z.
    let (origin, unit_x) = (roi_to_frame(roi, fw, fh, 0.0, 0.0), roi_to_frame(roi, fw, fh, 1.0, 0.0));
    let z_scale = (unit_x.0 - origin.0).hypot(unit_x.1 - origin.1);
    for (p, o) in raw.as_chunks::<3>().0.iter().zip(out) {
        let (u, v) = ((p[0] / side - letterbox.left) / horizontal, (p[1] / side - letterbox.top) / vertical);
        let (x, y) = roi_to_frame(roi, fw, fh, u, v);
        *o = [x, y, z_scale * (p[2] / side / horizontal)];
    }
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
        let mut pts = [[0.0; 3]; 2];
        project_landmarks(&raw, &lb, &roi, 600.0, 400.0, &mut pts);
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
