//! MediaPipe's hand landmark model (`hand_landmarks_detector.tflite` of `hand_landmarker.task`): a 224x224 crop of
//! one hand in, 21 landmarks on the image, 21 in metres, the presence and handedness scores out; and the region to crop
//! on the next frame, from these landmarks.
//!
//! The landmark order is MediaPipe's: 0 the wrist, then four per finger from the thumb (1 to 4: CMC, MCP, IP, tip) to
//! the little finger (17 to 20), each base to tip. The next region follows `HandLandmarksToRectCalculator`: from the
//! wrist and the knuckles, turned so the wrist-to-knuckles line points up, then shifted a tenth toward the fingers,
//! squared and doubled (`hand_landmarks_detector_graph.cc`).

use std::f32::consts::FRAC_PI_2;

use omuzaki_ai_api::{Session, Signature};
use omuzaki_core::glam::Vec3;

use super::graph::{ImageGraph, image_signature};
use crate::error::Result;
use crate::frame::{Border, RgbFrame};
use crate::roi::{NormRect, normalize_radians};

/// Landmarks per hand.
pub const HAND_POINTS: usize = 21;
const INPUT: usize = 224;
/// The landmarks the next region is fitted to: the wrist, the thumb's base, and the base two joints of each finger.
const ROI_POINTS: [usize; 12] = [0, 1, 2, 3, 5, 6, 9, 10, 13, 14, 17, 18];
const NEXT_ROI_SCALE: f32 = 2.0;
const NEXT_ROI_SHIFT_Y: f32 = -0.1;

/// The hand landmark graph as a slot signature: a 224x224 crop in 0..1, then the 21 landmarks in input pixels, the
/// presence score, the handedness score and the 21 world landmarks in metres. Output names are the converted graph's.
pub fn hand_signature() -> Signature {
    image_signature(
        INPUT,
        &[
            ("Identity", &[1, HAND_POINTS * 3]),
            ("Identity_1", &[1, 1]),
            ("Identity_2", &[1, 1]),
            ("Identity_3", &[1, HAND_POINTS * 3]),
        ],
    )
}

/// One hand's landmarks.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HandLandmarks {
    /// On the image: normalised frame coordinates.
    pub image: [(f32, f32); HAND_POINTS],
    /// In metres around the hand's centre, in the camera's axes (x to the image's right, y down, z away from the
    /// camera), turned back from the crop's rotation.
    pub world: [Vec3; HAND_POINTS],
    /// How sure the model is that a hand fills the crop, `0..=1`.
    pub presence: f32,
    /// The model's handedness score, `0..=1`: high for what it calls a right hand.
    pub handedness: f32,
}

impl HandLandmarks {
    /// No hand.
    pub const NONE: Self =
        Self { image: [(0.0, 0.0); HAND_POINTS], world: [Vec3::ZERO; HAND_POINTS], presence: 0.0, handedness: 0.5 };

    /// The region to crop on the next frame, from these landmarks.
    pub fn next_rect(&self, frame_w: f32, frame_h: f32) -> NormRect {
        let at = |i: usize| self.image[i];
        let wrist = at(0);
        // The knuckle direction: the middle knuckle averaged with the mean of the index and ring knuckles.
        let side = (f32::midpoint(at(5).0, at(13).0), f32::midpoint(at(5).1, at(13).1));
        let knuckles = (f32::midpoint(side.0, at(9).0), f32::midpoint(side.1, at(9).1));
        let rotation =
            normalize_radians(FRAC_PI_2 - (-(knuckles.1 - wrist.1) * frame_h).atan2((knuckles.0 - wrist.0) * frame_w));
        // The box of the points in the frame turned by `-rotation`, about their own box's centre.
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &i in &ROI_POINTS {
            x0 = x0.min(at(i).0);
            x1 = x1.max(at(i).0);
            y0 = y0.min(at(i).1);
            y1 = y1.max(at(i).1);
        }
        let centre = (f32::midpoint(x0, x1), f32::midpoint(y0, y1));
        let (sin, cos) = (-rotation).sin_cos();
        let (mut p0, mut q0, mut p1, mut q1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &i in &ROI_POINTS {
            let (x, y) = ((at(i).0 - centre.0) * frame_w, (at(i).1 - centre.1) * frame_h);
            let (p, q) = (x * cos - y * sin, x * sin + y * cos);
            p0 = p0.min(p);
            p1 = p1.max(p);
            q0 = q0.min(q);
            q1 = q1.max(q);
        }
        let (pc, qc) = (f32::midpoint(p0, p1), f32::midpoint(q0, q1));
        let (sin, cos) = rotation.sin_cos();
        let rect = NormRect {
            x_center: (pc * cos - qc * sin) / frame_w + centre.0,
            y_center: (pc * sin + qc * cos) / frame_h + centre.1,
            width: (p1 - p0) / frame_w,
            height: (q1 - q0) / frame_h,
            rotation,
        };
        rect.shifted(frame_w, frame_h, 0.0, NEXT_ROI_SHIFT_Y).expanded(frame_w, frame_h, NEXT_ROI_SCALE, true)
    }
}

/// The hand landmark model.
pub struct HandModel {
    graph: ImageGraph,
}

impl HandModel {
    /// Wraps a session of [`hand_signature`].
    ///
    /// # Errors
    /// [`FaceTrackError::Session`](crate::FaceTrackError::Session) when the session's signature differs.
    pub fn new(session: Box<dyn Session>) -> Result<Self> {
        Ok(Self { graph: ImageGraph::new("hand", session, &hand_signature(), (0.0, 1.0), Border::Replicate)? })
    }

    /// Forgets the session's carried state.
    pub fn reset(&mut self) {
        self.graph.reset();
    }

    /// The hand in `rect` of `frame`, or `None` when the region has no area. The presence is reported whatever it is;
    /// the caller decides what counts as a hand.
    ///
    /// # Errors
    /// A session failure or an output of the wrong size.
    pub fn run(&mut self, frame: &RgbFrame<'_>, rect: &NormRect) -> Result<Option<HandLandmarks>> {
        let Some(placement) = self.graph.run(frame, rect)? else { return Ok(None) };
        let raw = self.graph.output(0, HAND_POINTS * 3)?;
        let presence = self.graph.output(1, 1)?[0];
        let handedness = self.graph.output(2, 1)?[0];
        let world = self.graph.output(3, HAND_POINTS * 3)?;
        let mut out = HandLandmarks { presence, handedness, ..HandLandmarks::NONE };
        let side = self.graph.side();
        for (point, xyz) in out.image.iter_mut().zip(raw.as_chunks::<3>().0) {
            *point = placement.frame_point(side, xyz[0], xyz[1]);
        }
        // World landmarks come in the crop's axes; turn them by the crop's rotation (`WorldLandmarkProjection`).
        let (sin, cos) = rect.rotation.sin_cos();
        for (point, xyz) in out.world.iter_mut().zip(world.as_chunks::<3>().0) {
            *point = Vec3::new(cos * xyz[0] - sin * xyz[1], sin * xyz[0] + cos * xyz[1], xyz[2]);
        }
        Ok(Some(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An upright hand: wrist at the bottom, knuckles in a row above it, fingers up.
    fn upright() -> HandLandmarks {
        let mut hand = HandLandmarks::NONE;
        hand.image[0] = (0.5, 0.7);
        for (finger, x) in [(1, 0.42), (5, 0.45), (9, 0.5), (13, 0.55), (17, 0.58)] {
            for joint in 0..4 {
                #[allow(clippy::cast_precision_loss)] // joint counters
                let y = 0.55 - 0.04 * joint as f32;
                hand.image[finger + joint] = (x, y);
            }
        }
        hand
    }

    #[test]
    fn an_upright_hand_keeps_an_upright_square_region() {
        let rect = upright().next_rect(100.0, 100.0);
        assert!(rect.rotation.abs() < 1e-5, "{rect:?}");
        assert!((rect.width - rect.height).abs() < 1e-6, "square");
        // The points span 0.16 wide and 0.23 high (from the thumb's third joint at 0.47 down to the wrist at 0.7):
        // squared to the long side and doubled.
        assert!((rect.height - 2.0 * 0.23).abs() < 1e-5, "{rect:?}");
    }

    #[test]
    fn a_sideways_hand_turns_its_region() {
        // The same hand turned a quarter clockwise on screen: fingers point to the image's right.
        let mut hand = upright();
        for p in &mut hand.image {
            *p = (0.5 + (0.7 - p.1), 0.5 + (p.0 - 0.5));
        }
        let rect = hand.next_rect(100.0, 100.0);
        assert!((rect.rotation - FRAC_PI_2).abs() < 1e-4, "{rect:?}");
    }
}
