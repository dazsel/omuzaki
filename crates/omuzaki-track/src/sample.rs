//! What the tracker reports for one frame, and its conversion to the animation pipeline's [`TrackFrame`].

use omuzaki_anim_api::{ARKIT_COUNT, Channel, ChannelMask, DeviceProfile, HeadPose, TrackFrame};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;

use crate::landmarks::NUM_LANDMARKS;
use crate::roi::NormRect;

/// One tracked face. Fixed-size: no heap.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceSample {
    /// The `ARKit` blendshapes by channel index; entries not in [`present`](Self::present) are `0.0`.
    pub blendshapes: [f32; ARKIT_COUNT],
    /// The channels the tracker measures: 51 of the 52, as MediaPipe has no `tongueOut`.
    pub present: ChannelMask,
    /// The facial transformation matrix (canonical face to camera), column-major, translation in centimetres in
    /// elements 12 to 14, with a uniform scale (about 1) in the rotation block.
    pub pose: [f32; 16],
    /// 478 landmarks in normalised image coordinates (smoothed in video mode); `z` is depth relative to the face,
    /// scaled like `x`.
    pub landmarks: [[f32; 3]; NUM_LANDMARKS],
    /// Face presence probability from the landmark model.
    pub presence: f32,
    /// The ROI the landmark model was run on (from the detector, or tracked from the previous frame).
    pub roi: NormRect,
}

/// Microseconds spent per stage on one frame. Stages that did not run are zero.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct StageTimings {
    /// The detector: crop, session, decode. Zero when the face was tracked from the previous frame.
    pub detect_us: u32,
    /// The landmark model: crop, session, projection back onto the frame. The sum of [`warp_us`](Self::warp_us),
    /// [`landmark_run_us`](Self::landmark_run_us) and the decode that follows them.
    pub landmarks_us: u32,
    /// Of `landmarks_us`: the crop warped into the landmark model's input tensor.
    pub warp_us: u32,
    /// Of `landmarks_us`: the landmark session's `run`, the inference itself.
    pub landmark_run_us: u32,
    /// The blendshape model, including gathering its input.
    pub blendshapes_us: u32,
    /// Landmark smoothing and the head-pose fit.
    pub geometry_us: u32,
}

impl StageTimings {
    /// Of `landmarks_us`: what follows the run, reading its outputs and projecting the landmarks onto the frame.
    pub fn landmark_decode_us(&self) -> u32 {
        self.landmarks_us.saturating_sub(self.warp_us).saturating_sub(self.landmark_run_us)
    }

    /// Every stage together.
    pub fn total_us(&self) -> u32 {
        [self.detect_us, self.landmarks_us, self.blendshapes_us, self.geometry_us]
            .into_iter()
            .fold(0, u32::saturating_add)
    }
}

impl FaceSample {
    /// The head pose in the `anim-api` conventions: `yaw` positive when the nose turns toward the image's right,
    /// `pitch` positive when it turns down, `roll` positive counter-clockwise in the image, and the position in
    /// metres in the camera frame (x right, y up, z toward the camera, so a face in front of the lens has `z < 0`).
    ///
    /// The rotation is read as yaw about y, then pitch about x, then roll about z (the camera's axes), after
    /// removing the matrix's uniform scale.
    pub fn head_pose(&self) -> HeadPose {
        let p = &self.pose;
        let scale = f32::hypot(f32::hypot(p[0], p[1]), p[2]);
        let inv = if scale > f32::EPSILON { scale.recip() } else { 1.0 };
        // Rotation element at (row, col) of the column-major matrix.
        let r = |row: usize, col: usize| p[col * 4 + row] * inv;
        let cos_pitch = f32::hypot(r(1, 0), r(1, 1));
        let pitch = f32::atan2(-r(1, 2), cos_pitch);
        let (yaw, roll) = if cos_pitch > 1e-6 {
            (f32::atan2(r(0, 2), r(2, 2)), f32::atan2(r(1, 0), r(1, 1)))
        } else {
            // Looking straight up or down: yaw and roll turn the same axis, so give it all to yaw.
            (f32::atan2(-r(2, 0), r(0, 0)), 0.0)
        };
        let position = Vec3::new(p[12], p[13], p[14]) / CM_PER_METRE;
        HeadPose { yaw, pitch, roll, position: Some(position) }
    }
}

/// Centimetres in a metre: the canonical face, and so the pose translation, is in centimetres.
const CM_PER_METRE: f32 = 100.0;

/// `anim-api`'s frame for one tracker result: the 51 channels, the head pose, and `TrackStatus::Lost` when `sample`
/// is `None`. `seq` and `time` are the caller's (the camera thread's sequence number and capture time).
pub fn to_track_frame(sample: Option<&FaceSample>, seq: u64, time: Seconds) -> TrackFrame {
    let Some(sample) = sample else { return TrackFrame::lost(seq, time, DeviceProfile::Webcam) };
    TrackFrame {
        blend: sample.blendshapes,
        present: sample.present,
        head: Some(sample.head_pose()),
        ..TrackFrame::empty(seq, time, DeviceProfile::Webcam)
    }
}

/// The `ARKit` channel of each of the model's 51 coefficients (its output without `_neutral`), by name.
///
/// `None` if a name is not an `ARKit` channel, which the tests rule out for the table in
/// [`blendshapes::NAMES`](crate::blendshapes::NAMES).
pub(crate) fn coefficient_channels() -> Option<[Channel; ARKIT_COUNT - 1]> {
    let mut channels = [Channel::TONGUE_OUT; ARKIT_COUNT - 1];
    for (slot, name) in channels.iter_mut().zip(&crate::blendshapes::NAMES[1..]) {
        *slot = Channel::from_name(name).filter(|c| c.is_arkit())?;
    }
    Some(channels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_names_are_arkit_channels_in_arkit_order_without_tongue_out() {
        let channels = coefficient_channels().expect("every name is an ARKit channel");
        assert!(channels.iter().enumerate().all(|(i, c)| c.index() == i), "same order as the ARKit table");
        assert!(!channels.contains(&Channel::TONGUE_OUT));
        assert_eq!(channels.len(), 51);
    }

    #[test]
    fn a_lost_frame_carries_no_values() {
        let lost = to_track_frame(None, 7, Seconds(1.5));
        assert_eq!((lost.seq, lost.time, lost.head), (7, Seconds(1.5), None));
        assert!(lost.present.is_empty());
    }
}
