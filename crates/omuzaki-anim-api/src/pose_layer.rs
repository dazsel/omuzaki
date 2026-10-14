//! Layering humanoid poses (decision 0014 §4): the body-pose counterpart of [`LayerStack`](crate::LayerStack).
//!
//! An [`Animator`](crate::Animator) receives `out` holding the **base pose**, the character's bind, and layers onto it,
//! so a bone no layer drives stays as given. [`PoseLayers::resolve`] pins what "layers onto" means; the animator
//! implementations call it rather than each inventing a blend, as the channel side does with `LayerStack::resolve`.
//!
//! Per layer, per bone in its [`BoneMask`], lowest layer first:
//! - [`PoseMode::Override`] replaces the incoming rotation, blended in by weight: `slerp(incoming, layer, w)`. The layer
//!   is authored in T space (identity is the T-pose), so a driven bone is the layer's value, never composed with the
//!   bind. At weight 0.5 it is half way between what was there and the layer.
//! - [`PoseMode::Add`] is a delta in the T space of the bone's parent: `slerp(identity, layer, w) * incoming`. A breath
//!   or a sway therefore moves a bent arm the same way in the world whatever the bind.
//!
//! The hips offset follows the mask's [`HumanoidBone::Hips`] bit: replaced (`Override`) or summed (`Add`), by weight.

use crate::skeleton::{HUMANOID_COUNT, HumanoidBone, HumanoidPose};
use omuzaki_core::glam::Quat;

/// A set of humanoid bones, one bit per [`HumanoidBone::index`]. The pose side of [`ChannelMask`](crate::ChannelMask).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct BoneMask(u64);

// Every humanoid bone must fit in the word.
const _: () = assert!(HUMANOID_COUNT <= u64::BITS as usize);

impl BoneMask {
    /// No bone.
    pub const EMPTY: Self = Self(0);
    /// Every humanoid bone (a full-body layer).
    pub const ALL: Self = Self((1 << HUMANOID_COUNT) - 1);

    /// The mask of exactly `bones`.
    pub fn of(bones: &[HumanoidBone]) -> Self {
        bones.iter().fold(Self::EMPTY, |mask, &bone| mask.with(bone))
    }

    /// This mask plus `bone`.
    #[must_use]
    pub const fn with(self, bone: HumanoidBone) -> Self {
        Self(self.0 | 1 << bone.index())
    }

    /// Whether `bone` is in the mask.
    pub const fn contains(self, bone: HumanoidBone) -> bool {
        self.0 >> bone.index() & 1 == 1
    }

    /// Whether no bone is in the mask.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// How a [`PoseLayer`] combines with the pose below it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum PoseMode {
    /// Replace the bones in the mask, by weight (clips).
    #[default]
    Override,
    /// Add a delta onto the bones in the mask, by weight (the life layer: breath, idle, sway).
    Add,
}

/// One source's contribution to the body pose.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct PoseLayer {
    /// The values, in T space. Bones outside `mask` are ignored.
    pub pose: HumanoidPose,
    /// The bones the source has an opinion on.
    pub mask: BoneMask,
    /// `0` changes nothing, `1` is the full effect. Clamped to that range; NaN counts as 0.
    pub weight: f32,
    /// How it combines with the pose below.
    pub mode: PoseMode,
}

/// The layers of one frame, lowest first. Borrowed, so a caller keeps them wherever it likes (a fixed array on the
/// stack) and nothing allocates.
#[derive(Copy, Clone, Debug, Default)]
pub struct PoseLayers<'a>(pub &'a [PoseLayer]);

impl PoseLayers<'_> {
    /// Layers every layer onto whatever `out` holds (the base pose). Pure; hot path: 55 quaternion operations per
    /// layer, no allocation.
    pub fn resolve(&self, out: &mut HumanoidPose) {
        for layer in self.0 {
            apply(layer, out);
        }
    }
}

fn apply(layer: &PoseLayer, out: &mut HumanoidPose) {
    // NaN must not reach a quaternion: `clamp` would pass it on, so it counts as 0 here.
    let weight = if layer.weight.is_nan() { 0.0 } else { layer.weight.min(1.0) };
    if weight <= 0.0 || layer.mask.is_empty() {
        return;
    }
    if layer.mask.contains(HumanoidBone::Hips) {
        out.hips_offset = match layer.mode {
            PoseMode::Override => out.hips_offset.lerp(layer.pose.hips_offset, weight),
            PoseMode::Add => out.hips_offset + layer.pose.hips_offset * weight,
        };
    }
    for bone in HumanoidBone::ALL {
        if !layer.mask.contains(bone) {
            continue;
        }
        let (i, value) = (bone.index(), layer.pose.rotations[bone.index()]);
        out.rotations[i] = match layer.mode {
            // At weight 1 take the layer exactly: slerp renormalises and would move it by a rounding error.
            PoseMode::Override if weight >= 1.0 => value,
            PoseMode::Override => out.rotations[i].slerp(value, weight),
            PoseMode::Add if weight >= 1.0 => (value * out.rotations[i]).normalize(),
            PoseMode::Add => (Quat::IDENTITY.slerp(value, weight) * out.rotations[i]).normalize(),
        };
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // values copied through are compared exactly
mod tests {
    use super::*;

    fn bent(bone: HumanoidBone, z: f32) -> HumanoidPose {
        let mut p = HumanoidPose::REST;
        p.set(bone, Quat::from_rotation_z(z));
        p
    }

    fn layer(pose: &HumanoidPose, bones: &[HumanoidBone], weight: f32, mode: PoseMode) -> PoseLayer {
        PoseLayer { pose: *pose, mask: BoneMask::of(bones), weight, mode }
    }

    #[test]
    fn mask_basics() {
        assert!(BoneMask::EMPTY.is_empty());
        assert!(HumanoidBone::ALL.iter().all(|&b| BoneMask::ALL.contains(b)));
        let m = BoneMask::of(&[HumanoidBone::Head]);
        assert!(m.contains(HumanoidBone::Head) && !m.contains(HumanoidBone::Neck));
    }

    #[test]
    fn no_layers_leave_the_base_untouched() {
        let base = bent(HumanoidBone::LeftUpperArm, -1.0);
        let mut out = base;
        PoseLayers(&[]).resolve(&mut out);
        assert_eq!(out, base);
    }

    #[test]
    fn override_slerps_from_the_incoming_pose_and_skips_undriven_bones() {
        let base = bent(HumanoidBone::LeftUpperArm, -1.0);
        let clip = bent(HumanoidBone::LeftUpperArm, 0.6);
        let mut out = base;
        PoseLayers(&[layer(&clip, &[HumanoidBone::LeftUpperArm], 0.5, PoseMode::Override)]).resolve(&mut out);
        assert!(out.get(HumanoidBone::LeftUpperArm).angle_between(Quat::from_rotation_z(-0.2)) < 1e-3);
        // Full weight is exact; an undriven bone keeps the base.
        let mut out = base;
        PoseLayers(&[layer(&clip, &[HumanoidBone::RightUpperArm], 1.0, PoseMode::Override)]).resolve(&mut out);
        assert_eq!(out, base);
        PoseLayers(&[layer(&clip, &[HumanoidBone::LeftUpperArm], 1.0, PoseMode::Override)]).resolve(&mut out);
        assert_eq!(out.get(HumanoidBone::LeftUpperArm), clip.get(HumanoidBone::LeftUpperArm));
    }

    #[test]
    fn add_composes_a_delta_in_the_parent_frame_and_scales_with_weight() {
        let base = bent(HumanoidBone::Spine, 0.1);
        let delta = bent(HumanoidBone::Spine, 0.2);
        let mut out = base;
        PoseLayers(&[layer(&delta, &[HumanoidBone::Spine], 1.0, PoseMode::Add)]).resolve(&mut out);
        assert!(out.get(HumanoidBone::Spine).angle_between(Quat::from_rotation_z(0.3)) < 1e-3);
        let mut out = base;
        PoseLayers(&[layer(&delta, &[HumanoidBone::Spine], 0.5, PoseMode::Add)]).resolve(&mut out);
        assert!(out.get(HumanoidBone::Spine).angle_between(Quat::from_rotation_z(0.2)) < 1e-3);
    }

    #[test]
    fn hips_offset_follows_the_hips_bit_and_bad_weights_do_nothing() {
        let mut base = HumanoidPose::REST;
        base.hips_offset.y = 0.1;
        let mut pose = HumanoidPose::REST;
        pose.hips_offset.y = 0.3;
        let mut out = base;
        PoseLayers(&[layer(&pose, &[HumanoidBone::Hips], 0.5, PoseMode::Override)]).resolve(&mut out);
        assert!((out.hips_offset.y - 0.2).abs() < 1e-6);
        let mut out = base;
        PoseLayers(&[layer(&pose, &[HumanoidBone::Hips], 1.0, PoseMode::Add)]).resolve(&mut out);
        assert!((out.hips_offset.y - 0.4).abs() < 1e-6);
        for w in [0.0, -2.0, f32::NAN] {
            let mut out = base;
            PoseLayers(&[layer(&pose, &[HumanoidBone::Hips], w, PoseMode::Add)]).resolve(&mut out);
            assert_eq!(out, base);
        }
    }
}
