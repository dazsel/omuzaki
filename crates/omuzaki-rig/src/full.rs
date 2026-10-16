//! The full rig: humanoid pose, then IK, then node constraints, then spring bones, then one forward-kinematics pass for
//! skinning (spec §8 order, anim-api design §8; RG-05, RG-03; VRM 1.0 puts constraints after look-at and IK, before
//! springs).
//!
//! [`FullRig`] implements [`Rig`] by running, in this order:
//!
//! 1. [`HumanoidRig::apply_humanoid`]: the animated pose onto the skeleton's rest;
//! 2. the IK stage ([`IkSolver`], by default [`TwoBoneIk`]) on the goals set with [`FullRig::set_goals`];
//! 3. the constraint stage ([`VrmConstraintSolver`]), only when one was set with [`FullRig::with_constraints`];
//! 4. the caller's own pre-spring stage, only through [`FullRig::solve_and_skin_with`]: whatever else poses bones the
//!    springs must follow in the same frame (a character's bone drivers, #800);
//! 5. the spring stage ([`SpringSolver`], by default [`VrmSpringSolver`]) for hair, cloth and accessory chains,
//!    stepped by the `dt` of the [`Tick`].
//!
//! The order is what makes the stages compose: constraints read their sources as IK left them (a bone constrained to
//! an IK-moved bone follows the IK result), springs see the constrained rotations (a chain under a constrained bone
//! hangs from where the constraint put it), and a goal sees the animated pose. With idle goals, no constraints and no
//! spring solver the result is the humanoid solve bit for bit.
//!
//! # Goals
//!
//! [`Rig::solve`] carries no goals, so they are state: [`FullRig::set_goals`] before the frame. They persist until
//! replaced and survive [`Rig::reset`] (they are the caller's input, not carried state).
//!
//! # Forward kinematics
//!
//! [`FullRig::solve_and_skin`] solves, then runs [`Skeleton::world_pose`] **once**, over the final local pose, and hands
//! that world pose to [`Skinner::skin_from_world`]. [`FullRig::world`] exposes it for anything else that needs bone
//! positions this frame (attachments, debug draw), so none of them runs a pass of its own.
//!
//! What is *not* shared, and why: the IK solver and the spring solver each walk the part of the skeleton they need
//! inside their own `solve`/`step` (the contract passes them local poses only), and the springs rotate bones, so every
//! descendant's world transform changes after them. A world pose computed before the springs would be stale for the
//! hair and everything under it. The sound share is therefore the final pass: one full pass per frame, used by both
//! skinning and the caller.
//!
//! # Cost
//!
//! No allocation or locking after construction; the cost is the stages plus one forward-kinematics pass.

use std::sync::Arc;

use omuzaki_anim_api::{
    HumanoidPose, IkGoals, IkSolver, Pose, Rig, Skeleton, SpringError, SpringSet, SpringSolver, Tick,
};
use omuzaki_core::glam::Mat4;

use crate::constraint::VrmConstraintSolver;
use crate::ik::TwoBoneIk;
use crate::pose::HumanoidRig;
use crate::skeleton::RigSkeleton;
use crate::skin::Skinner;
use crate::spring::VrmSpringSolver;

/// Humanoid pose, IK, optional node constraints and spring bones on one skeleton, with the skinning pass folded in. One
/// per character.
#[derive(Debug)]
pub struct FullRig<I = TwoBoneIk, S = VrmSpringSolver> {
    humanoid: HumanoidRig,
    ik: I,
    /// `None` for a character without spring bones: the stage is skipped, not run on an empty set.
    springs: Option<S>,
    /// `None` for a character without node constraints: the stage is skipped, as for the springs.
    constraints: Option<VrmConstraintSolver>,
    goals: IkGoals,
    skinner: Skinner,
    /// Root-relative transforms of the last [`solve_and_skin`](Self::solve_and_skin) (boxed: 16 KiB).
    world: Box<Pose>,
}

impl FullRig {
    /// A rig with the stock two-bone IK and the VRM spring solver for `springs`. Cold: allocates.
    ///
    /// # Errors
    /// Whatever [`SpringSet::validate`] finds wrong with the set for this skeleton.
    pub fn new(skeleton: Arc<RigSkeleton>, springs: &SpringSet) -> Result<Self, SpringError> {
        let solver = VrmSpringSolver::new(springs, skeleton.skeleton())?;
        Ok(Self::from_parts(skeleton, TwoBoneIk::new(), Some(solver)))
    }

    /// A rig without spring bones: humanoid pose and IK only.
    pub fn without_springs(skeleton: Arc<RigSkeleton>) -> Self {
        Self::from_parts(skeleton, TwoBoneIk::new(), None)
    }
}

impl<I: IkSolver, S: SpringSolver> FullRig<I, S> {
    /// A rig from its stages: any IK solver, and a spring solver built for this skeleton (or `None`). Cold: allocates.
    /// The rig has no node constraints; add them with [`with_constraints`](Self::with_constraints).
    pub fn from_parts(skeleton: Arc<RigSkeleton>, ik: I, springs: Option<S>) -> Self {
        Self {
            skinner: Skinner::new(skeleton.clone()),
            humanoid: HumanoidRig::new(skeleton),
            ik,
            springs,
            constraints: None,
            goals: IkGoals::NONE,
            world: Box::new(Pose::IDENTITY),
        }
    }

    /// Adds the node-constraint stage, which runs after IK and before the springs. The solver must have been built with
    /// [`VrmConstraintSolver::new`] against this rig's skeleton (the same [`Skeleton`] this
    /// rig was made from): the solver stores no skeleton to check against, so a solver built for another one writes to
    /// the wrong bones. Replaces any earlier set. Cold: call it before the frame loop, not in it.
    #[must_use]
    pub fn with_constraints(mut self, solver: VrmConstraintSolver) -> Self {
        self.constraints = Some(solver);
        self
    }

    /// The node-constraint stage, if one was set with [`with_constraints`](Self::with_constraints).
    pub fn constraints(&self) -> Option<&VrmConstraintSolver> {
        self.constraints.as_ref()
    }

    /// Sets the IK and look-at goals for the following solves, until replaced. [`IkGoals::NONE`] turns the stage off.
    pub fn set_goals(&mut self, goals: &IkGoals) {
        self.goals = *goals;
    }

    /// The goals in force.
    pub fn goals(&self) -> &IkGoals {
        &self.goals
    }

    /// The prepared skeleton.
    pub fn rig_skeleton(&self) -> &Arc<RigSkeleton> {
        self.humanoid.rig_skeleton()
    }

    /// The spring stage's solver, for its stats and sub-step cap.
    pub fn springs_mut(&mut self) -> Option<&mut S> {
        self.springs.as_mut()
    }

    /// Root-relative transforms of every bone as of the last [`solve_and_skin`](Self::solve_and_skin) (identity before
    /// the first).
    pub fn world(&self) -> &Pose {
        &self.world
    }

    /// [`Rig::solve`], then the skinning matrices of the result into `joints` (one per bone, indexed by
    /// [`BoneId`](omuzaki_anim_api::BoneId); a shorter slice is filled as far as it goes), with a single forward-kinematics
    /// pass shared with [`world`](Self::world). Hot path: no allocation.
    pub fn solve_and_skin(&mut self, tick: Tick, pose: &HumanoidPose, out: &mut Pose, joints: &mut [Mat4]) {
        self.solve_and_skin_with(tick, pose, out, joints, |_, _| {});
    }

    /// [`solve_and_skin`](Self::solve_and_skin) with `before_springs` run on the local pose after the constraint stage
    /// and before the springs (stage 4 of the module docs), so the springs see what it posed in the same frame. Hot
    /// path: no allocation here; `before_springs` must not allocate either.
    pub fn solve_and_skin_with(
        &mut self,
        tick: Tick,
        pose: &HumanoidPose,
        out: &mut Pose,
        joints: &mut [Mat4],
        before_springs: impl FnMut(&Skeleton, &mut Pose),
    ) {
        self.solve_with(tick, pose, out, before_springs);
        self.humanoid.rig_skeleton().skeleton().world_pose(out, &mut self.world);
        self.skinner.skin_from_world(&self.world, joints);
    }

    /// Stages 1 to 5 of the module docs, with `before_springs` as stage 4.
    fn solve_with(
        &mut self,
        tick: Tick,
        pose: &HumanoidPose,
        out: &mut Pose,
        mut before_springs: impl FnMut(&Skeleton, &mut Pose),
    ) {
        self.humanoid.apply_humanoid(pose, out);
        let skeleton = self.humanoid.rig_skeleton().skeleton();
        if !self.goals.is_idle() {
            self.ik.solve(skeleton, &self.goals, out);
        }
        if let Some(constraints) = &self.constraints {
            constraints.apply(skeleton, out);
        }
        before_springs(skeleton, out);
        if let Some(springs) = &mut self.springs {
            springs.step(tick, skeleton, out);
        }
    }
}

impl<I: IkSolver, S: SpringSolver> Rig for FullRig<I, S> {
    fn skeleton(&self) -> &Skeleton {
        self.humanoid.rig_skeleton().skeleton()
    }

    fn solve(&mut self, tick: Tick, pose: &HumanoidPose, out: &mut Pose) {
        self.solve_with(tick, pose, out, |_, _| {});
    }

    /// Clears the IK solver's and the springs' carried state; the goals stay. The constraint stage carries none, so it
    /// has nothing to clear.
    fn reset(&mut self) {
        self.ik.reset();
        if let Some(springs) = &mut self.springs {
            springs.reset();
        }
    }
}
