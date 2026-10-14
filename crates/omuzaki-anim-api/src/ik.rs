//! Goals for the IK stage (spec §8 order: `anim blend → IK → physics`; RG-05 limb IK, FE-07 look-at).
//!
//! The humanoid pose says how the body is *animated*; goals say what the body should *reach or watch* right now: where
//! the eyes should look, where the hands and feet should be. [`IkGoals`] is the per-frame input (`Copy`, fixed size, no
//! heap, like [`HumanoidPose`](crate::HumanoidPose)), and [`IkSolver`] is the stage that bends the solved [`Pose`] to
//! meet them. It is **additive** to the rest of this crate: nothing here changes how [`Rig::solve`](crate::Rig::solve)
//! or the [`Animator`](crate::Animator) behave until a rig opts in.
//!
//! ## Conventions
//! - **Space.** Every position is in the **skeleton's root space**, the space [`Skeleton::world_pose`] produces
//!   (metres, +Y up, the character faces +Z). A scene that places the character in a world transforms the targets by the
//!   inverse of that placement before it fills the goals; the solver never sees a world matrix.
//! - **Weight.** `0..=1`. At `0` (or `None`) the goal does nothing and the pose is left **bit-identical**; at `1` the
//!   goal is met as far as the limb can reach. In between, the result moves from the animated pose toward the solved
//!   one.
//! - **Look-at and the eye channels.** Without a look-at goal the `eyeLook*` channels are offsets from straight ahead.
//!   With one they are **offsets from the target direction**: the solver aims the eyes at the target, and the painter
//!   adds the channels on top (the prototype's `lookAdd` on top of "look at the camera"). The pose keeps the aim; the
//!   channels keep the saccades and micro-movement.
//! - **Generic skeletons.** IK needs humanoid roles (arms, legs, head, eyes); on a generic skeleton a solver does
//!   nothing ([`Skeleton::humanoid_map`](crate::Skeleton::humanoid_map) is `None`), as the rig does for the humanoid pose.
//!   Creature limbs are bone drivers' job.

use omuzaki_core::glam::Vec3;

use crate::skeleton::{Pose, Skeleton};

/// Where the character should look.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LookAt {
    /// The point to look at, skeleton root space.
    pub target: Vec3,
    /// How much of the turn to apply, `0..=1`.
    pub weight: f32,
    /// How the turn is shared, `0..=1`: `0` turns the neck and head and leaves the eyes in place, `1` turns only the
    /// eyes, in between the eyes finish what the head leaves. A glance is high; a deliberate turn is low.
    pub eye_share: f32,
}

impl LookAt {
    /// Whether every field is finite (neither NaN nor infinite).
    pub fn is_finite(&self) -> bool {
        self.target.is_finite() && self.weight.is_finite() && self.eye_share.is_finite()
    }
}

/// Where one hand or foot should go, and which way its elbow or knee should point.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Limb {
    /// The point the hand or foot should reach, skeleton root space. A target out of reach is approached as far as the
    /// straightened limb allows, along the line to it.
    pub target: Vec3,
    /// A point the elbow or knee bends toward (skeleton root space), so a solver has a bend direction that does not
    /// flip. It only picks the side of the line from the root joint to the target.
    pub pole: Vec3,
    /// How much of the reach to apply, `0..=1`.
    pub weight: f32,
}

impl Limb {
    /// Whether every field is finite (neither NaN nor infinite).
    pub fn is_finite(&self) -> bool {
        self.target.is_finite() && self.pole.is_finite() && self.weight.is_finite()
    }
}

/// Everything the IK stage should do this frame. `None` leaves that part of the body as animated.
///
/// Sides are the character's own (left is +X, see the crate conventions).
#[derive(Copy, Clone, Debug, PartialEq, Default)]
pub struct IkGoals {
    /// Look-at for the neck, head and eyes.
    pub look_at: Option<LookAt>,
    /// Left hand: upper arm, lower arm, hand.
    pub left_hand: Option<Limb>,
    /// Right hand.
    pub right_hand: Option<Limb>,
    /// Left foot: upper leg, lower leg, foot.
    pub left_foot: Option<Limb>,
    /// Right foot.
    pub right_foot: Option<Limb>,
}

impl IkGoals {
    /// No goals: a solver given this leaves the pose untouched.
    pub const NONE: Self = Self { look_at: None, left_hand: None, right_hand: None, left_foot: None, right_foot: None };

    /// These goals with every goal that has a non-finite field dropped, as if it were absent. A solver applies this
    /// first, so a NaN from a bad tracker frame or a broken script can never reach the pose.
    #[must_use]
    pub fn finite_only(&self) -> Self {
        let limb = |l: Option<Limb>| l.filter(Limb::is_finite);
        Self {
            look_at: self.look_at.filter(LookAt::is_finite),
            left_hand: limb(self.left_hand),
            right_hand: limb(self.right_hand),
            left_foot: limb(self.left_foot),
            right_foot: limb(self.right_foot),
        }
    }

    /// Whether no goal asks for anything (every goal is absent or has weight zero).
    pub fn is_idle(&self) -> bool {
        let limb_idle = |l: &Option<Limb>| l.is_none_or(|l| l.weight <= 0.0);
        self.look_at.is_none_or(|l| l.weight <= 0.0)
            && limb_idle(&self.left_hand)
            && limb_idle(&self.right_hand)
            && limb_idle(&self.left_foot)
            && limb_idle(&self.right_foot)
    }
}

/// The IK stage: edits a solved pose so the body meets the goals.
pub trait IkSolver: Send {
    /// Bends `pose` toward `goals`. Hot path: no allocation, no locking.
    ///
    /// `pose` holds the local transforms the humanoid pose produced on `skeleton`, before spring bones. The solver
    /// changes only the **rotations** of the bones in the chains it solves (arm and leg bones for limbs; neck, head
    /// and eyes for look-at), so lengths never change; every other bone and every translation stays as given.
    /// A goal with any non-finite field (target, pole, weight or `eye_share`) is ignored, as if it were absent
    /// ([`IkGoals::finite_only`]). With [`IkGoals::is_idle`] the pose is left exactly as it was. On a generic skeleton it does nothing. The result
    /// is a pure function of the inputs and prior calls.
    fn solve(&mut self, skeleton: &Skeleton, goals: &IkGoals, pose: &mut Pose);

    /// Forgets any state carried between frames.
    fn reset(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_idle_and_a_weightless_goal_is_idle() {
        assert!(IkGoals::NONE.is_idle());
        let limb = Limb { target: Vec3::X, pole: Vec3::Z, weight: 0.0 };
        assert!(IkGoals { left_hand: Some(limb), ..IkGoals::NONE }.is_idle());
        let live = Limb { weight: 0.5, ..limb };
        assert!(!IkGoals { right_foot: Some(live), ..IkGoals::NONE }.is_idle());
        let look = LookAt { target: Vec3::Z, weight: 1.0, eye_share: 0.5 };
        assert!(!IkGoals { look_at: Some(look), ..IkGoals::NONE }.is_idle());
    }
}
