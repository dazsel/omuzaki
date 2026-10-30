//! An arm as a person describes it: how far it is raised from the side, swung forward, turned in, the elbow bent, the
//! forearm turned palm-down and the wrist bent, plus the shoulder's shrug. [`ArmPose`] turns those numbers into the
//! normalised rotations of the shoulder, upper arm, lower arm and hand, for either side.
//!
//! Gestures are authored and blended in these numbers rather than as rotations, because a blend of two arm poses in
//! these terms is another plausible arm pose (the elbow never flips, the hand never passes through the wrist), where a
//! blend of finished rotations is only approximately one. Every number has the same meaning on both sides; the side
//! is applied at the end with the T-pose's mirror rule (`HumanoidPose::mirrored`).
//!
//! Conventions, in the VRM normalised space (the T-pose is the identity, the character faces +Z, its left is +X):
//! - `raise` 0 hangs the arm straight down, 90 holds it out to the side (abduction);
//! - `forward` swings it forward from there (flexion), negative swings it back;
//! - `twist` turns the upper arm in about its own axis, so a bent forearm crosses toward the body's middle;
//! - `elbow` bends the forearm forward, 0 straight;
//! - `pronate` turns the forearm palm-down, 0 has the palm facing the body's middle with the thumb up, negative turns
//!   it palm-up; half the turn is the forearm's and half the wrist's, so neither twists like a wrung cloth;
//! - `wrist` bends the hand toward its palm, negative bends it back;
//! - `shrug` lifts the shoulder, `reach` brings it forward.

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::Quat;
use serde::{Deserialize, Serialize};

/// Which arm.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// The character's left (+X).
    Left,
    /// The character's right.
    Right,
}

impl Side {
    /// The other side.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    /// The side's index, `0` left and `1` right.
    pub const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    /// The four bones an arm pose sets: shoulder, upper arm, lower arm, hand.
    pub const fn bones(self) -> [HumanoidBone; 4] {
        match self {
            Self::Left => [
                HumanoidBone::LeftShoulder,
                HumanoidBone::LeftUpperArm,
                HumanoidBone::LeftLowerArm,
                HumanoidBone::LeftHand,
            ],
            Self::Right => [
                HumanoidBone::RightShoulder,
                HumanoidBone::RightUpperArm,
                HumanoidBone::RightLowerArm,
                HumanoidBone::RightHand,
            ],
        }
    }
}

/// An arm in a person's terms, every angle in degrees. See the [module](self) for what each number does.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArmPose {
    /// Raised from the side, `0` hanging, `90` straight out.
    pub raise: f32,
    /// Swung forward.
    pub forward: f32,
    /// Turned in about its own axis.
    pub twist: f32,
    /// The elbow's bend.
    pub elbow: f32,
    /// The forearm turned palm-down.
    pub pronate: f32,
    /// The wrist bent toward the palm.
    pub wrist: f32,
    /// The shoulder lifted.
    pub shrug: f32,
    /// The shoulder brought forward.
    pub reach: f32,
}

/// The rotations of one arm's four bones (shoulder, upper arm, lower arm, hand), on the left side.
type LeftBones = [Quat; 4];

impl ArmPose {
    /// The arm hanging loose at the side with a soft elbow: what the gestures leave from and return to.
    pub const HANGING: Self =
        Self { raise: 8.0, forward: 4.0, twist: 10.0, elbow: 14.0, pronate: 0.0, wrist: 4.0, shrug: 0.0, reach: 0.0 };

    /// `self` moved `t` of the way to `other`, every number linearly (`t` is not clamped: past 1 exaggerates).
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f32) -> Self {
        let m = |a: f32, b: f32| a + (b - a) * t;
        Self {
            raise: m(self.raise, other.raise),
            forward: m(self.forward, other.forward),
            twist: m(self.twist, other.twist),
            elbow: m(self.elbow, other.elbow),
            pronate: m(self.pronate, other.pronate),
            wrist: m(self.wrist, other.wrist),
            shrug: m(self.shrug, other.shrug),
            reach: m(self.reach, other.reach),
        }
    }

    /// The numbers as an array, in field order (for springs and checks).
    pub fn to_array(self) -> [f32; 8] {
        [self.raise, self.forward, self.twist, self.elbow, self.pronate, self.wrist, self.shrug, self.reach]
    }

    /// The pose from an array in field order.
    pub fn from_array(a: [f32; 8]) -> Self {
        Self {
            raise: a[0],
            forward: a[1],
            twist: a[2],
            elbow: a[3],
            pronate: a[4],
            wrist: a[5],
            shrug: a[6],
            reach: a[7],
        }
    }

    /// Every number finite and inside what a shoulder, elbow and wrist can do.
    pub fn is_plausible(&self) -> bool {
        let within = |v: f32, lo: f32, hi: f32| v.is_finite() && (lo..=hi).contains(&v);
        within(self.raise, -10.0, 170.0)
            && within(self.forward, -60.0, 170.0)
            && within(self.twist, -90.0, 100.0)
            && within(self.elbow, 0.0, 150.0)
            && within(self.pronate, -100.0, 100.0)
            && within(self.wrist, -70.0, 80.0)
            && within(self.shrug, -10.0, 30.0)
            && within(self.reach, -20.0, 30.0)
    }

    /// The same pose held inside [`is_plausible`](Self::is_plausible)'s ranges (NaN reads as 0): an exaggerated blend
    /// past a stroke never asks a joint for what it cannot do.
    #[must_use]
    pub fn clamped(self) -> Self {
        let c = |v: f32, lo: f32, hi: f32| if v.is_nan() { 0.0 } else { v.clamp(lo, hi) };
        Self {
            raise: c(self.raise, -10.0, 170.0),
            forward: c(self.forward, -60.0, 170.0),
            twist: c(self.twist, -90.0, 100.0),
            elbow: c(self.elbow, 0.0, 150.0),
            pronate: c(self.pronate, -100.0, 100.0),
            wrist: c(self.wrist, -70.0, 80.0),
            shrug: c(self.shrug, -10.0, 30.0),
            reach: c(self.reach, -20.0, 30.0),
        }
    }

    /// The left arm's rotations.
    fn left(&self) -> LeftBones {
        let r = f32::to_radians;
        let shoulder = Quat::from_rotation_z(r(self.shrug)) * Quat::from_rotation_y(-r(self.reach));
        // Hang the arm, swing it forward in the shoulder's frame, then turn it about its own (local X) axis.
        let upper = Quat::from_rotation_x(-r(self.forward))
            * Quat::from_rotation_z(-r(90.0 - self.raise))
            * Quat::from_rotation_x(r(self.twist));
        let half = r(self.pronate) * 0.5;
        let lower = Quat::from_rotation_y(-r(self.elbow)) * Quat::from_rotation_x(half);
        let hand = Quat::from_rotation_x(half) * Quat::from_rotation_z(-r(self.wrist));
        [shoulder, upper, lower, hand].map(Quat::normalize)
    }

    /// The four rotations (shoulder, upper arm, lower arm, hand) on `side`.
    pub fn rotations(&self, side: Side) -> [Quat; 4] {
        let left = self.left();
        match side {
            Side::Left => left,
            // The T-pose's mirror rule: X kept, Y and Z negated.
            Side::Right => left.map(|q| Quat::from_xyzw(q.x, -q.y, -q.z, q.w)),
        }
    }

    /// Writes the four rotations into `pose` on `side`.
    pub fn write(&self, side: Side, pose: &mut HumanoidPose) {
        for (bone, q) in side.bones().into_iter().zip(self.rotations(side)) {
            pose.set(bone, q);
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec3;

    use super::*;

    /// Shoulder, elbow, wrist and fingertip in the T-space, on unit-length bones from a shoulder at the origin.
    fn joints(pose: &ArmPose, side: Side) -> [Vec3; 3] {
        let [shoulder, upper, lower, hand] = pose.rotations(side);
        let out = if side == Side::Left { Vec3::X } else { Vec3::NEG_X };
        let s = shoulder;
        let u = s * upper;
        let l = u * lower;
        let h = l * hand;
        let elbow = u * out;
        let wrist = elbow + l * out;
        let tip = wrist + h * out * 0.4;
        [elbow, wrist, tip]
    }

    #[test]
    fn hanging_is_down_and_raised_is_out_to_the_side() {
        let down = ArmPose { raise: 0.0, ..ArmPose::default() };
        let [elbow, wrist, _] = joints(&down, Side::Left);
        assert!(elbow.abs_diff_eq(Vec3::NEG_Y, 1e-5), "{elbow}");
        assert!(wrist.abs_diff_eq(Vec3::new(0.0, -2.0, 0.0), 1e-5));
        let out = ArmPose { raise: 90.0, ..ArmPose::default() };
        assert!(joints(&out, Side::Left)[0].abs_diff_eq(Vec3::X, 1e-5));
        assert!(joints(&out, Side::Right)[0].abs_diff_eq(Vec3::NEG_X, 1e-5));
    }

    #[test]
    fn the_numbers_mean_what_they_say_on_both_sides() {
        for side in [Side::Left, Side::Right] {
            let medial = if side == Side::Left { -1.0 } else { 1.0 };
            let base = ArmPose { elbow: 90.0, ..ArmPose::default() };
            // A bent elbow puts the forearm forward.
            let [elbow, wrist, _] = joints(&base, side);
            assert!((wrist - elbow).abs_diff_eq(Vec3::Z, 1e-5), "{side:?}: {}", wrist - elbow);
            // Twisting in brings it across the body.
            let [elbow, wrist, _] = joints(&ArmPose { twist: 40.0, ..base }, side);
            assert!((wrist - elbow).x * medial > 0.5, "{side:?}");
            // Swinging forward brings the elbow forward.
            assert!(joints(&ArmPose { forward: 30.0, ..base }, side)[0].z > 0.4);
            // A shrug lifts the shoulder and so the elbow.
            assert!(joints(&ArmPose { shrug: 15.0, ..base }, side)[0].y > joints(&base, side)[0].y);
            // Pronating 90 turns the palm down: the bent wrist then points the fingers down.
            let flexed = ArmPose { pronate: 90.0, wrist: 60.0, ..base };
            let [_, wrist, tip] = joints(&flexed, side);
            assert!((tip - wrist).y < -0.2, "{side:?}: {}", tip - wrist);
            // Supinating 90 turns it up.
            let up = ArmPose { pronate: -90.0, wrist: 60.0, ..base };
            let [_, wrist, tip] = joints(&up, side);
            assert!((tip - wrist).y > 0.2, "{side:?}: {}", tip - wrist);
        }
    }

    #[test]
    fn the_right_arm_is_the_left_mirrored() {
        let pose = ArmPose { raise: 25.0, forward: 40.0, twist: 20.0, elbow: 70.0, pronate: -30.0, wrist: 10.0, shrug: 5.0, reach: 3.0 };
        let [l, r] = [Side::Left, Side::Right].map(|s| joints(&pose, s));
        for (a, b) in l.iter().zip(&r) {
            assert!(a.abs_diff_eq(Vec3::new(-b.x, b.y, b.z), 1e-5), "{a} vs {b}");
        }
    }
}
