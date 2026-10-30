//! Recorded humanoid motion (RG-08): a take read from a motion-capture file and retargeted onto the VRM humanoid, frame
//! by frame, in VRM normalised space (identity is the T-pose, the character faces `+Z`, its left is `+X`).
//!
//! | Piece | Does |
//! |---|---|
//! | [`HumanoidTake`] | a dense take: one [`HumanoidPose`] a frame at the file's rate, sampled at any time |
//! | [`asf`] | the Acclaim skeleton (`.asf`) and motion (`.amc`) files of the CMU motion-capture library, retargeted |
//!
//! A take is what the motion-parity loop measures against (the reference's poses) and what `motion.*` ops play or
//! turn into clips. Reading one never touches the filesystem: callers hand in the text.

pub mod asf;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::glam::Vec3;

/// Why a motion file could not be read.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum MotionError {
    /// The text is not what the format says, at `line` (1-based; 0 when the problem is not one line).
    #[error("{file} line {line}: {message}")]
    Parse {
        /// Which file (`skeleton` or `motion`).
        file: &'static str,
        /// Where.
        line: usize,
        /// What is wrong.
        message: String,
    },
    /// The skeleton lacks a bone the humanoid needs.
    #[error("the skeleton has no {0} bone")]
    MissingBone(String),
    /// The motion has no frames.
    #[error("the motion has no frames")]
    Empty,
}

/// A recorded take on the VRM humanoid: one pose a frame at `fps`.
#[derive(Clone, Debug, PartialEq)]
pub struct HumanoidTake {
    /// Frames a second of the recording.
    pub fps: f32,
    /// The poses, in VRM normalised space; the hips offset is metres from the first frame's hips, scaled to a body
    /// whose hips stand `hips_height` metres up.
    pub frames: Vec<HumanoidPose>,
    /// The hips height the offsets are scaled to, metres.
    pub hips_height: f32,
}

impl HumanoidTake {
    /// Seconds the take lasts.
    #[allow(clippy::cast_precision_loss)] // frame counts are far inside f32's exact range
    pub fn seconds(&self) -> f32 {
        if self.fps > 0.0 { self.frames.len().saturating_sub(1) as f32 / self.fps } else { 0.0 }
    }

    /// The pose at `t` seconds: the two frames around it blended (rotations slerped, the hips offset lerped), held at
    /// the ends.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // bounded indices
    #[allow(clippy::many_single_char_names)] // t, i, j, u, a, b: a time, two frames and the blend between them
    pub fn sample(&self, t: f32) -> HumanoidPose {
        let Some(last) = self.frames.len().checked_sub(1) else {
            return HumanoidPose::REST;
        };
        let at = if t.is_finite() { (t * self.fps).clamp(0.0, last as f32) } else { 0.0 };
        let i = (at.floor() as usize).min(last);
        let j = (i + 1).min(last);
        let u = at - i as f32;
        let (a, b) = (&self.frames[i], &self.frames[j]);
        let mut out = *a;
        for (k, q) in out.rotations.iter_mut().enumerate() {
            *q = a.rotations[k].slerp(b.rotations[k], u).normalize();
        }
        out.hips_offset = a.hips_offset.lerp(b.hips_offset, u);
        out
    }

    /// The same take with every hips offset scaled for a body whose hips stand `hips_height` metres up.
    #[must_use]
    pub fn for_hips_height(mut self, hips_height: f32) -> Self {
        if hips_height.is_finite() && hips_height > 0.0 && self.hips_height > 0.0 {
            let k = hips_height / self.hips_height;
            for f in &mut self.frames {
                f.hips_offset *= k;
            }
            self.hips_height = hips_height;
        }
        self
    }

    /// The same take with the hips held still (no travel): what a seated or desk-framed character plays.
    #[must_use]
    pub fn in_place(mut self) -> Self {
        for f in &mut self.frames {
            f.hips_offset = Vec3::new(0.0, f.hips_offset.y, 0.0);
        }
        self
    }
}

