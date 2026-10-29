//! Reaches: a hand told to go to a point that rides on a bone of the same body (#2414): a hand on the chest, two palms
//! meeting in front of it, an arm stretched to the lens, a finger pointing ahead.
//!
//! A pose says how the arm is *rotated*; on another body (a shorter arm, a deeper chest) the same rotations put the hand
//! somewhere else, into the chest or short of the other palm. A [`Reach`] says where the hand should *be*, relative to
//! the body itself, so it lands on any body. The rig turns each into a root-space hand goal ([`Limb`](crate::Limb))
//! after the humanoid pose and before IK: the bone's position comes from the posed skeleton, and the offset turns with
//! the bone and scales with the arm.
//!
//! # The reach frame
//!
//! - **Origin:** the bone's head (its joint), as posed.
//! - **Axes:** the T-pose's root axes turned with the bone in the normalised pose (VRM normalised space, ADR 0014): `+X`
//!   the character's left, `+Y` up, `+Z` forward. A reach on the upper chest turns as the chest leans or twists.
//! - **Units:** arm lengths, the reaching side's shoulder-to-wrist length (upper arm plus forearm) on this skeleton, so
//!   one file fits a small and a tall body.
//!
//! A target beyond the arm's length straightens the arm along the line to it (the two-bone solve's rule), so "two arm
//! lengths straight ahead of the shoulder" is an arm stretched forward, at the lens of a camera in front.
//!
//! `Copy`, fixed size, no heap, like the pose it travels with.

use omuzaki_core::glam::Vec3;

use crate::skeleton::HumanoidBone;

/// Where one hand should go: a point on a bone of the same body, with the elbow's side.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Reach {
    /// The bone the target rides on.
    pub bone: HumanoidBone,
    /// The wrist's target from the bone's joint, in the reach frame (see the module docs), arm lengths.
    pub offset: Vec3,
    /// The direction the elbow bends toward from the shoulder, in the same frame as the offset: only its side of the
    /// line from the shoulder to the target counts.
    pub pole: Vec3,
    /// How much of the reach to apply, `0..=1`.
    pub weight: f32,
}

impl Reach {
    /// Whether every field is finite.
    pub fn is_finite(&self) -> bool {
        self.offset.is_finite() && self.pole.is_finite() && self.weight.is_finite()
    }

    /// The same reach on the other side of the body: the bone mirrored and `X` negated.
    #[must_use]
    pub fn mirrored(self) -> Self {
        let flip = |v: Vec3| Vec3::new(-v.x, v.y, v.z);
        Self { bone: self.bone.mirrored(), offset: flip(self.offset), pole: flip(self.pole), ..self }
    }

    /// `self` with its weight scaled by `by` (`0..=1`).
    #[must_use]
    pub fn scaled(self, by: f32) -> Self {
        Self { weight: self.weight * by, ..self }
    }

    /// The reach a fraction `u` (`0..=1`) of the way from `from` to `to`. On the same bone the offset, pole and weight
    /// move together. Between two bones (or from or to no reach), the first fades out over the first half and the
    /// second fades in over the second, so the hand never jumps between targets: at the switch neither pulls.
    pub fn blend(from: Option<Self>, to: Option<Self>, u: f32) -> Option<Self> {
        let u = if u.is_finite() { u.clamp(0.0, 1.0) } else { 0.0 };
        match (from, to) {
            (None, None) => None,
            (Some(a), Some(b)) if a.bone == b.bone => Some(Self {
                bone: a.bone,
                offset: a.offset.lerp(b.offset, u),
                pole: a.pole.lerp(b.pole, u),
                weight: a.weight + (b.weight - a.weight) * u,
            }),
            (a, b) => {
                if u < 0.5 {
                    a.map(|a| a.scaled(1.0 - 2.0 * u))
                } else {
                    b.map(|b| b.scaled(2.0 * u - 1.0))
                }
            }
        }
    }
}

/// The reaches of both hands. `None` leaves that arm as posed.
#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub struct Reaches {
    /// The character's left hand.
    pub left: Option<Reach>,
    /// The character's right hand.
    pub right: Option<Reach>,
}

impl Reaches {
    /// No reach.
    pub const NONE: Self = Self { left: None, right: None };

    /// Whether neither hand reaches (absent or weight zero).
    pub fn is_idle(&self) -> bool {
        let idle = |r: &Option<Reach>| r.is_none_or(|r| r.weight <= 0.0);
        idle(&self.left) && idle(&self.right)
    }

    /// The reaches with every non-finite one dropped.
    #[must_use]
    pub fn finite_only(&self) -> Self {
        Self { left: self.left.filter(Reach::is_finite), right: self.right.filter(Reach::is_finite) }
    }

    /// Left and right swapped, each mirrored.
    #[must_use]
    pub fn mirrored(&self) -> Self {
        Self { left: self.right.map(Reach::mirrored), right: self.left.map(Reach::mirrored) }
    }

    /// Each hand [`Reach::blend`]ed a fraction `u` of the way from `from` to `to`.
    pub fn blend(from: &Self, to: &Self, u: f32) -> Self {
        Self { left: Reach::blend(from.left, to.left, u), right: Reach::blend(from.right, to.right, u) }
    }

    /// `over` laid on `self` at `weight` (`0..=1`): a hand `over` reaches with takes its reach, its weight scaled by
    /// `weight`, when that is stronger than this one's; a hand it leaves alone keeps this one's.
    #[must_use]
    pub fn layered(&self, over: &Self, weight: f32) -> Self {
        let pick = |under: Option<Reach>, over: Option<Reach>| match over.map(|o| o.scaled(weight)) {
            Some(o) if under.is_none_or(|u| o.weight >= u.weight) => Some(o),
            _ => under,
        };
        Self { left: pick(self.left, over.left), right: pick(self.right, over.right) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chest(x: f32, weight: f32) -> Reach {
        Reach { bone: HumanoidBone::UpperChest, offset: Vec3::new(x, 0.0, 0.3), pole: Vec3::NEG_Y, weight }
    }

    #[test]
    fn mirroring_swaps_the_sides_and_twice_is_the_same() {
        let r =
            Reaches { right: Some(Reach { bone: HumanoidBone::RightUpperArm, ..chest(-0.2, 1.0) }), ..Reaches::NONE };
        let m = r.mirrored();
        assert_eq!(m.right, None);
        let left = m.left.unwrap();
        assert_eq!(left.bone, HumanoidBone::LeftUpperArm);
        assert_eq!(left.offset, Vec3::new(0.2, 0.0, 0.3));
        assert_eq!(m.mirrored(), r);
    }

    #[test]
    fn blending_on_one_bone_moves_and_between_bones_never_pulls_at_the_switch() {
        let (a, b) = (chest(-0.2, 1.0), chest(0.2, 0.5));
        let mid = Reach::blend(Some(a), Some(b), 0.5).unwrap();
        assert!(mid.offset.abs_diff_eq(Vec3::new(0.0, 0.0, 0.3), 1e-6));
        assert!((mid.weight - 0.75).abs() < 1e-6);
        let head = Reach { bone: HumanoidBone::Head, ..b };
        assert_eq!(Reach::blend(Some(a), Some(head), 0.0), Some(a));
        assert!(Reach::blend(Some(a), Some(head), 0.4999).unwrap().weight < 1e-3);
        assert!(Reach::blend(Some(a), Some(head), 0.5).unwrap().weight.abs() < 1e-6);
        assert_eq!(Reach::blend(Some(a), Some(head), 1.0), Some(head));
        assert_eq!(Reach::blend(Some(a), None, 0.75), None);
        assert_eq!(Reach::blend(None, Some(a), f32::NAN), None);
    }

    #[test]
    fn idle_finite_and_layering() {
        assert!(Reaches::NONE.is_idle());
        assert!(Reaches { left: Some(chest(0.1, 0.0)), ..Reaches::NONE }.is_idle());
        let bad = Reaches { left: Some(chest(f32::NAN, 1.0)), right: Some(chest(0.1, 1.0)) }.finite_only();
        assert_eq!(bad.left, None);
        let under = Reaches { left: Some(chest(0.1, 0.6)), right: None };
        let over = Reaches { left: Some(chest(0.3, 1.0)), right: Some(chest(-0.3, 1.0)) };
        let weak = under.layered(&over, 0.5);
        assert_eq!(weak.left, under.left, "a weaker reach doesn't take over");
        assert!((weak.right.unwrap().weight - 0.5).abs() < 1e-6);
        assert_eq!(under.layered(&over, 1.0).left, over.left);
    }
}
