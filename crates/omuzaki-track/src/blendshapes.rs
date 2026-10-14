//! The blendshape model: 146 landmarks in, 52 ARKit-style coefficients out.

use omuzaki_ai_api::{DType, Dim, Session, Signature, TensorBuf, TensorRef, TensorSpec};

use crate::error::{FaceTrackError, Result};
use crate::landmarks::{Landmark, NUM_LANDMARKS};

/// The landmarks the blendshape model reads, by index into the 478 (MediaPipe's `kLandmarksSubsetIdxs`: the lips,
/// eyes, brows, nose and face contour points that carry expression, plus the iris points).
pub const SUBSET: [usize; 146] = [
    0, 1, 4, 5, 6, 7, 8, 10, 13, 14, 17, 21, 33, 37, 39, 40, 46, 52, 53, 54, 55, 58, 61, 63, 65, 66, 67, 70, 78, 80,
    81, 82, 84, 87, 88, 91, 93, 95, 103, 105, 107, 109, 127, 132, 133, 136, 144, 145, 146, 148, 149, 150, 152, 153,
    154, 155, 157, 158, 159, 160, 161, 162, 163, 168, 172, 173, 176, 178, 181, 185, 191, 195, 197, 234, 246, 249, 251,
    263, 267, 269, 270, 276, 282, 283, 284, 285, 288, 291, 293, 295, 296, 297, 300, 308, 310, 311, 312, 314, 317, 318,
    321, 323, 324, 332, 334, 336, 338, 356, 361, 362, 365, 373, 374, 375, 377, 378, 379, 380, 381, 382, 384, 385, 386,
    387, 388, 389, 390, 397, 398, 400, 402, 405, 409, 415, 454, 466, 468, 469, 470, 471, 472, 473, 474, 475, 476, 477,
];

/// The 52 outputs in model order. Index 0 (`_neutral`) is not a blendshape and is dropped from results.
pub const NAMES: [&str; 52] = [
    "_neutral",
    "browDownLeft",
    "browDownRight",
    "browInnerUp",
    "browOuterUpLeft",
    "browOuterUpRight",
    "cheekPuff",
    "cheekSquintLeft",
    "cheekSquintRight",
    "eyeBlinkLeft",
    "eyeBlinkRight",
    "eyeLookDownLeft",
    "eyeLookDownRight",
    "eyeLookInLeft",
    "eyeLookInRight",
    "eyeLookOutLeft",
    "eyeLookOutRight",
    "eyeLookUpLeft",
    "eyeLookUpRight",
    "eyeSquintLeft",
    "eyeSquintRight",
    "eyeWideLeft",
    "eyeWideRight",
    "jawForward",
    "jawLeft",
    "jawOpen",
    "jawRight",
    "mouthClose",
    "mouthDimpleLeft",
    "mouthDimpleRight",
    "mouthFrownLeft",
    "mouthFrownRight",
    "mouthFunnel",
    "mouthLeft",
    "mouthLowerDownLeft",
    "mouthLowerDownRight",
    "mouthPressLeft",
    "mouthPressRight",
    "mouthPucker",
    "mouthRight",
    "mouthRollLower",
    "mouthRollUpper",
    "mouthShrugLower",
    "mouthShrugUpper",
    "mouthSmileLeft",
    "mouthSmileRight",
    "mouthStretchLeft",
    "mouthStretchRight",
    "mouthUpperUpLeft",
    "mouthUpperUpRight",
    "noseSneerLeft",
    "noseSneerRight",
];

/// The 51 ARKit blendshape coefficients (`_neutral` removed), in [`NAMES`] order after the first.
pub type Blendshapes = [f32; 51];

/// The blendshape graph (`face_blendshapes`) as a slot signature: 146 landmarks as x, y in frame pixels in, 52
/// scores out (`_neutral` first, see [`NAMES`]).
pub fn blendshapes_signature() -> Signature {
    Signature {
        inputs: vec![TensorSpec::new("landmarks", DType::F32, [1, SUBSET.len(), 2].map(Dim::Fixed))],
        outputs: vec![TensorSpec::new("scores", DType::F32, [NAMES.len()].map(Dim::Fixed))],
    }
}

/// The blendshape model with reusable buffers.
pub struct BlendshapeModel {
    session: Box<dyn Session>,
    input: [f32; SUBSET.len() * 2],
    outputs: Vec<TensorBuf>,
}

impl BlendshapeModel {
    /// Wraps a session of the blendshape graph.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`] when the session's signature is not [`blendshapes_signature`].
    pub fn new(session: Box<dyn Session>) -> Result<Self> {
        crate::error::check_signature("blendshapes", &blendshapes_signature(), session.signature())?;
        let outputs = TensorBuf::for_outputs(session.signature());
        Ok(Self { session, input: [0.0; SUBSET.len() * 2], outputs })
    }

    /// Forgets the session's carried state (see [`Session::reset`]).
    pub fn reset(&mut self) {
        self.session.reset();
    }

    /// Predicts the coefficients from the landmarks of a `frame_w` x `frame_h` frame. The model takes landmark x and y
    /// in frame pixels, not normalised units.
    ///
    /// # Errors
    /// [`FaceTrackError::Session`] when inference fails and [`FaceTrackError::Shape`] when the output is not 52 values.
    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
    pub fn run(&mut self, landmarks: &[Landmark; NUM_LANDMARKS], frame_w: u32, frame_h: u32) -> Result<Blendshapes> {
        let (fw, fh) = (frame_w as f32, frame_h as f32);
        let (slots, _) = self.input.as_chunks_mut::<2>();
        for (slot, &index) in slots.iter_mut().zip(SUBSET.iter()) {
            *slot = [landmarks[index][0] * fw, landmarks[index][1] * fh];
        }
        self.session.run(&[TensorRef::f32(&self.input, &[1, SUBSET.len(), 2])], &mut self.outputs)?;
        self.outputs[0]
            .as_f32()
            .and_then(|scores| scores.get(1..))
            .and_then(|coefficients| coefficients.try_into().ok())
            .ok_or(FaceTrackError::Shape { graph: "blendshapes" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subset_is_sorted_unique_and_in_range() {
        assert!(SUBSET.windows(2).all(|w| w[0] < w[1]));
        assert!(SUBSET.iter().all(|&i| i < NUM_LANDMARKS));
    }

    #[test]
    fn names_follow_the_arkit_order() {
        assert_eq!(NAMES[0], "_neutral");
        assert_eq!(NAMES[25], "jawOpen");
        assert_eq!(NAMES[51], "noseSneerRight");
        let mut sorted: Vec<_> = NAMES[1..].to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted.len(), 51);
        sorted.dedup();
        assert_eq!(sorted.len(), 51, "names are unique");
    }
}
