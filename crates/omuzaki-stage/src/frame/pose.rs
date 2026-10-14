//! The frame's base pose (decision 0014 §4): with no animation the stage shows the character as sculpted, so the pose
//! an [`Animator`](omuzaki_anim_api::Animator) layers onto starts as the character's bind, not the T rest.
//!
//! The layering itself lives in `omuzaki-anim-api` ([`PoseLayers::resolve`](omuzaki_anim_api::PoseLayers::resolve)) and the
//! animators; the stage only seeds. The frame loop, the exporters and the recorder all seed through
//! [`seed_base_pose`], so what is on stage and what is exported are the same pose.

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::CharacterRig;

/// Sets `out` to the character's base pose, [`CharacterRig::bind`]. Call it before every `Animator::animate`; the
/// animator layers onto it. Hot path: one 896-byte copy.
pub fn seed_base_pose(rig: &CharacterRig, out: &mut HumanoidPose) {
    *out = rig.bind;
}
