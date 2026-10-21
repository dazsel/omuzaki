//! Hips translation between characters of different height (VRM Animation, `how_to_transform_human_pose.md`, "Hips
//! translation").
//!
//! [`HumanoidPose::hips_offset`] is metres relative to the rest hips, so a pose or clip recorded on a tall character
//! lifts a short one off the floor (or sinks it). VRMA's rule is to scale the whole translation by `y_dst / y_src`, the
//! hips' height above the floor in the T-pose of the target and of the source. Rotations need no scaling: they are
//! the same angles on any proportions.
//!
//! The scale lives here, at load time, not in the rig: whoever brings a pose or clip in from a character of another
//! height (a VRMA import, a pose recorded elsewhere) calls [`retarget_hips`] once, and the rig keeps adding the offset
//! unscaled. Library poses are authored in metres on the template character, and need it only for a character of
//! another height.

use omuzaki_anim_api::{HumanoidPose, Pose, Skeleton};

/// Why a hips scale could not be worked out.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum HipsScaleError {
    /// The skeleton is not a VRM humanoid, so it has no hips to measure.
    #[error("the skeleton is not a humanoid, so it has no hips height")]
    NotHumanoid,
    /// A height is not a finite number above zero.
    #[error("a hips height must be finite and above zero, got {height}")]
    BadHeight {
        /// The offending height, metres.
        height: f32,
    },
}

/// The hips' height above the floor in the skeleton's rest pose, metres: the `y` of the hips in skeleton space.
///
/// # Errors
/// [`HipsScaleError::NotHumanoid`] if `skeleton` has no hips role, and [`HipsScaleError::BadHeight`] if the height is
/// not finite or not above zero.
pub fn hips_rest_height(skeleton: &Skeleton) -> Result<f32, HipsScaleError> {
    let hips = skeleton.require_humanoid().map_err(|_| HipsScaleError::NotHumanoid)?.hips();
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut world);
    let height = world.local[hips.index()].translation.y;
    check(height)
}

/// Scales the pose's hips offset by `to_height / from_height`, the hips' rest heights of the character the pose was
/// made on and of the one it will play on. Rotations are untouched.
///
/// # Errors
/// [`HipsScaleError::BadHeight`] if either height is not finite and above zero. The pose is left as it was.
pub fn scale_hips(pose: &mut HumanoidPose, from_height: f32, to_height: f32) -> Result<(), HipsScaleError> {
    let ratio: f32 = check(to_height)? / check(from_height)?;
    pose.hips_offset *= ratio;
    Ok(())
}

/// [`scale_hips`] from the rest hips height of `from` to that of `to`: a pose made on the character with skeleton `from`,
/// played on the one with skeleton `to`.
///
/// # Errors
/// As [`hips_rest_height`], for either skeleton. The pose is left as it was.
pub fn retarget_hips(pose: &mut HumanoidPose, from: &Skeleton, to: &Skeleton) -> Result<(), HipsScaleError> {
    scale_hips(pose, hips_rest_height(from)?, hips_rest_height(to)?)
}

fn check(height: f32) -> Result<f32, HipsScaleError> {
    if height.is_finite() && height > 0.0 { Ok(height) } else { Err(HipsScaleError::BadHeight { height }) }
}

