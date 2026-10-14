//! The blendshape model: 146 landmarks in, 52 ARKit-style coefficients out.

use std::path::Path;

use crate::error::{Result, TrackError};
use crate::landmarks::{Landmark, NUM_LANDMARKS};
use crate::runtime::{Model, RuntimeOptions, output_f32};

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

/// The blendshape model with a reusable input buffer.
pub struct BlendshapeModel {
    model: Model,
    input: Vec<f32>,
}

impl BlendshapeModel {
    /// Loads `face_blendshapes.onnx`.
    ///
    /// # Errors
    /// Returns an error when the model cannot be loaded.
    pub fn load(path: &Path, options: &RuntimeOptions) -> Result<Self> {
        Ok(Self { model: Model::load(path, options)?, input: vec![0.0; SUBSET.len() * 2] })
    }

    /// Predicts the coefficients from the landmarks of a `frame_w` x `frame_h` frame. The model takes landmark x and y
    /// in frame pixels, not normalised units.
    ///
    /// # Errors
    /// Returns an error when too few landmarks are given, inference fails or the output is not 52 values.
    #[allow(clippy::cast_precision_loss)] // frame sizes are far below f32's 24 bits
    pub fn run(&mut self, landmarks: &[Landmark], frame_w: usize, frame_h: usize) -> Result<Blendshapes> {
        if landmarks.len() != NUM_LANDMARKS {
            return Err(TrackError::Input(format!("{} landmarks given, {NUM_LANDMARKS} needed", landmarks.len())));
        }
        let (slots, _) = self.input.as_chunks_mut::<2>();
        for (slot, &index) in slots.iter_mut().zip(SUBSET.iter()) {
            *slot = [landmarks[index][0] * frame_w as f32, landmarks[index][1] * frame_h as f32];
        }
        let outputs = self.model.run(&[1, SUBSET.len(), 2], &self.input)?;
        let name = outputs.keys().next().ok_or_else(|| TrackError::Model("blendshape model has no output".into()))?;
        let scores = output_f32(&outputs, name)?;
        let coefficients: Blendshapes = scores.get(1..).and_then(|s| s.try_into().ok()).ok_or_else(|| {
            TrackError::Model(format!("blendshape output has {} values, expected {}", scores.len(), NAMES.len()))
        })?;
        Ok(coefficients)
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
