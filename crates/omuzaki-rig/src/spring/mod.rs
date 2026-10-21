//! Spring bones and colliders (spec RG-03): the solver for the data model in [`omuzaki_anim_api::spring`].
//!
//! [`VrmSpringSolver`] implements the reference algorithm of `VRMC_springBone` 1.0
//! (<https://github.com/vrm-c/vrm-specification/tree/master/specification/VRMC_springBone-1.0#springbone-algorithm>,
//! the section `SpringBone Algorithm`): Verlet integration of each joint's tail with inertia, stiffness and gravity, a constraint
//! that keeps the tail at the bone's length, push-out from sphere and capsule colliders (and, from `VRMC_springBone_extended_collider`, push-in to the
//! inside of a sphere or capsule and push-out of a plane's back side), and then the bone is rotated
//! to point at the tail. A joint's `VRMC_springBone_limit` (cone, hinge or spherical, see the `limit` module) holds the tail's direction
//! inside its range right after the inertia update and again after each collider push. Chains are updated root to tip, and a chain hanging off another chain's joint after it.
//!
//! # Provenance
//!
//! The solver and its oracle (`reference.rs`) are written from the `VRMC_springBone` 1.0 specification's pseudocode
//! alone, not from three-vrm's or any other runtime's source. three-vrm 3.5.5 (MIT) is only a black-box comparison:
//! `testdata/three_vrm.txt` holds numbers it printed, made by `testdata/three_vrm_golden.mjs`, and no three-vrm code
//! or data is in the crate.
//!
//! # Fixed step
//!
//!
//! Time is consumed in whole [`PHYSICS_STEP`](omuzaki_anim_api::PHYSICS_STEP) (1/120 s) sub-steps whatever the frame
//! rate (decision 0009), so a result is a pure function of the inputs and the sub-steps run. The spec's forces are
//! per-step quantities (drag is not scaled by the frame time), which only makes sense at a fixed step, so the step
//! length is also what makes the parameters mean the same thing on every machine.
//!
//! # Choices where the spec leaves room
//!
//! - **The animated pose is interpolated across a frame's sub-steps** (#79). Sub-step `i` of a frame's `n` sees the
//!   animated bones `i/n` of the way from the previous frame's pose to this one's (translations lerped, rotations
//!   slerped), so colliders, chain roots and centres glide instead of jumping at the first sub-step. Measured on the
//!   hair fixture against a 120 fps run (one sub-step a frame, so nothing to interpolate), worst tip error (rms):
//!
//!   | motion        | fps | held            | interpolated  |
//!   |---------------|-----|-----------------|---------------|
//!   | walk and sway | 30  | 47.0 mm (17.6)  | 0.9 mm (0.5)  |
//!   | walk and sway | 60  | 19.0 mm (6.9)   | 0.2 mm (0.1)  |
//!   | head turn     | 30  | 12.5 mm (4.1)   | 1.3 mm (0.4)  |
//!   | head turn     | 60  | 4.2 mm (1.4)    | 0.3 mm (0.1)  |
//!
//!   It costs re-placing the animated bones and colliders each sub-step instead of once a frame: 2% at 60 fps and 3% at
//!   30 fps on the 300-joint bench, held pose against interpolated (96 to 98 µs, 190 to 196 µs). A bone that did not
//!   move skips the blend, so a still pose steps bit for bit as a held one at any frame rate. The first frame (and the
//!   one after a reset or a warm-up) has no previous pose and is held.
//!   [`VrmSpringSolver::set_pose_interpolation`]`(false)` holds the pose as `pose` enters
//!   [`step`](omuzaki_anim_api::SpringSolver::step), as the spec's single step per frame and three-vrm do; the
//!   comparisons with them run that way.
//! - **A frame too short for a sub-step** (above 120 fps) keeps every tail where it is and re-aims the bones at them
//!   from that frame's pose. The oracle holds such a frame the same way (#1849: comparing a held solver against an
//!   oracle given nothing that frame read the previous frame's pose, 0.03 m apart at 144 fps).
//! - **Spring bones start from their rest.** A swinging bone's `initialLocalMatrix` is its skeleton rest transform, not
//!   whatever the incoming pose holds, and only its rotation is written back. An animation clip that rotates a hair
//!   bone is overridden, as it is in other VRM runtimes.
//! - **Tails start at rest in the pose they are first seen in**, and again after [`reset`](omuzaki_anim_api::SpringSolver::reset),
//!   so a character that first appears away from the origin does not whip its hair into place.
//! - **A limit sits in the head's rest frame under its parent's current rotation**, as the limit spec's `Rotation` listing
//!   has it, so it bounds the swing rather than following the bone. Its order in the step is quoted in the `limit` module's docs. A
//!   limit on a chain's last joint is ignored, as the spec requires.
//! - **A zero-length pair does not move.** A joint on top of the next one has no direction to swing in; it keeps its
//!   rest rotation.
//! - **Colliders push, then the bone length is kept**, as the spec orders it (and three-vrm does). A push that moves a
//!   tail out of a collider is undone in part when the length is restored, so a tail whose root is closer to a plane
//!   (or to the wall of an inside shape) than a bone length can still end a step behind it. The guarantee is on the
//!   push, which leaves no joint more than rounding behind the plane or outside the shape; the end of the step keeps
//!   the bone length. A test pins the case (root 0.1 m above a plane, a 0.2 m bone hanging straight down).
//! - **Centre space** is applied at the boundary of each span of held pose: tails are stored relative to the centre
//!   bone, mapped to world space when the span begins, simulated there, and mapped back. A held frame is one span; an
//!   interpolated frame is one span per sub-step. With the centre still within a span this is the spec's per-sub-step
//!   formulation exactly, and it makes nested chains and gravity (always world space) free.
//! - **A bone that is a joint of two chains, or a centre below another chain's joint, is not rejected.** The spec
//!   forbids both but [`SpringSet::validate`](omuzaki_anim_api::SpringSet::validate) doesn't check; the later chain by
//!   bone order wins, deterministically.
//! - **Colliders on a bone below a spring joint** see that bone as the incoming pose has it.
//! - **A tail exactly on a collider's axis** (its centre, for a sphere) has no direction to be pushed in. It is pushed
//!   out along the bone's direction as it stood before the sub-step, or along the rest direction if the bone has no
//!   length, so two runs always agree. The spec's `normalized` of a zero vector would leave it where it is. An inside
//!   shape only meets this when the joint's own radius exceeds the shape's; it is then pushed back against that same
//!   direction. A plane has no such case.
//! - **Bone length is fixed at rest.** The spec fixes `boneLength` at initialisation, so the solver never re-measures
//!   it. three-vrm 3.5.5 re-measures it from the child's world matrix on every update, and a child that is itself a
//!   spring joint has not moved yet that sub-step, so the length wobbles by however far the parents just swung. Left
//!   unpinned, three-vrm differs from the pinned reference by up to 0.085 in quaternion components on a free chain,
//!   0.043 in centre space, and 0.83 with colliders (frames 62 to 80, where the swings hit them). The solver matches
//!   the pinned reference to 1.2e-6 to 4.2e-6. The golden generator pins the length: `testdata/three_vrm_golden.mjs`.
//! - **Positions are `f32` in the skeleton's space**, so tails resolve to about 1e-7 of their distance from the skeleton
//!   root. A character far from its root (tens of metres) gets visibly noisy hair; move the character with the host's
//!   transform, not the root bone.
//!
//! # Cost
//!
//! [`step`](omuzaki_anim_api::SpringSolver::step) never allocates or locks: [`VrmSpringSolver::new`] sizes every buffer.
//! The work is one forward-kinematics pass over the bones the springs hang off, then a handful of vector operations
//! per joint per sub-step (`cargo bench -p omuzaki-rig`).

mod collide;
mod limit;
mod solver;

#[cfg(test)]
mod reference;
#[cfg(test)]
mod tests;

pub use solver::VrmSpringSolver;
