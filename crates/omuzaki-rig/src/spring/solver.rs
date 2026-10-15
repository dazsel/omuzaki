//! The solver: `VRMC_springBone`'s reference algorithm, run in fixed [`PHYSICS_STEP`] sub-steps.
//!
//! Everything is sized in [`VrmSpringSolver::new`]; [`SpringSolver::step`] only reads and writes those buffers.

use std::collections::BTreeSet;
use std::ops::Range;

use omuzaki_anim_api::spring::DEFAULT_MAX_SUBSTEPS;
use omuzaki_anim_api::{
    BoneDef, BoneId, ColliderShape, PHYSICS_HZ, PHYSICS_STEP, Pose, Skeleton, SpringChain, SpringError, SpringJoint,
    SpringParams, SpringSet, SpringSolver, SpringStats, Tick, Transform,
};
use omuzaki_core::glam::{Quat, Vec3};

use super::collide::{Collider, DEGENERATE_DISTANCE};
use super::limit::Limiter;

/// Warm-ups longer than this are cut short: springs are damped, so more settles nothing, and a typo'd duration must
/// not stall the app.
const MAX_WARM_UP_S: f32 = 60.0;

/// A fraction of a sub-step forgiven when counting due sub-steps. A frame time like `1/60` is not exact in `f32`, and
/// without the slack a frame could owe `1.9999999` sub-steps and run one, then run three the next.
const SUBSTEP_SLACK: f64 = 1e-4;

/// A bone whose world transform is rebuilt as `world[parent] * local`.
#[derive(Copy, Clone, Debug)]
struct Node {
    bone: usize,
    parent: Option<usize>,
}

/// Rebuilds the world transform of every node (parents first) from the local transforms in `locals`.
fn rebuild(world: &mut [Transform], locals: &[Transform], nodes: &[Node]) {
    for node in nodes {
        let local = locals[node.bone];
        world[node.bone] = match node.parent {
            Some(parent) => world[parent] * local,
            None => local,
        };
    }
}

/// `frame * point`: a point given relative to `frame`, in the space `frame` is placed in.
fn place(frame: &Transform, point: Vec3) -> Vec3 {
    frame.translation + frame.rotation * point
}

/// The inverse of [`place`].
fn unplace(frame: &Transform, point: Vec3) -> Vec3 {
    frame.rotation.conjugate() * (point - frame.translation)
}

/// The shortest rotation taking unit vector `from` to unit vector `to` (the spec's `fromToQuaternion`).
///
/// Built from the cross product rather than `Quat::from_rotation_arc`, which returns the identity for angles below
/// about 5e-4 rad: invisible on screen, but it is bigger than the 1e-4 the solver is held to against the spec.
fn arc(from: Vec3, to: Vec3) -> Quat {
    let c = from.cross(to);
    let q = Quat::from_xyzw(c.x, c.y, c.z, 1.0 + from.dot(to));
    let norm_sq = q.length_squared();
    if norm_sq > 1e-12 {
        q * norm_sq.sqrt().recip()
    } else {
        // Opposite vectors: any half turn about a perpendicular axis does it.
        Quat::from_axis_angle(from.any_orthogonal_vector().normalize(), std::f32::consts::PI)
    }
}

/// What a sub-step does to a chain.
#[derive(Copy, Clone, Debug)]
enum Phase {
    /// Puts every tail where the animated pose says it rests, with no velocity.
    Rest,
    /// Moves the tails one sub-step.
    Integrate,
    /// Keeps the tails where they are and re-aims the bones at them (a frame too short to owe a sub-step).
    Hold,
}

/// A joint's parameters reduced to the numbers a sub-step uses. Built once per joint, and rebuilt by
/// [`VrmSpringSolver::set_params`]; the per-step code never sees a [`SpringParams`].
#[derive(Copy, Clone, Debug)]
struct Tuning {
    /// `1 - dragForce`: the share of velocity kept each sub-step.
    keep: f32,
    /// Stiffness force for one sub-step.
    stiffness: f32,
    /// Gravity for one sub-step.
    gravity: Vec3,
    /// The tail sphere's radius.
    hit_radius: f32,
}

impl Tuning {
    /// The sub-step values for `p`. The forces are per sub-step, so the authored force is scaled by [`PHYSICS_STEP`]
    /// here, once.
    fn of(p: &SpringParams) -> Self {
        Self {
            keep: 1.0 - p.drag_force,
            stiffness: p.stiffness * PHYSICS_STEP,
            gravity: p.gravity_dir * (p.gravity_power * PHYSICS_STEP),
            hit_radius: p.hit_radius,
        }
    }
}

/// One head-and-tail pair of a chain (the spec's `SpringJointPair`): a bone that swings, and the next joint it points at.
#[derive(Clone, Debug)]
struct Segment {
    // Fixed at construction.
    /// The head bone.
    bone: usize,
    /// Its parent, if any.
    parent: Option<usize>,
    /// The bones between the previous head and this one (a chain may skip bones), parents first.
    between: Range<usize>,
    /// The head's rest transform: VRM's `initialLocalMatrix` and `initialLocalRotation`.
    rest: Transform,
    /// Unit direction from the head to the tail in the head's rest frame: VRM's `boneAxis`.
    axis: Vec3,
    /// Distance from the head to the tail: VRM's `boneLength`. Zero pins the bone at its rest rotation.
    length: f32,
    /// The joint's `VRMC_springBone_limit`, if it has one.
    limit: Option<Limiter>,
    // Set at construction, or between steps by `set_params`.
    /// The joint's parameters as the sub-steps use them.
    tuning: Tuning,
    // Changes every sub-step.
    /// The tail one sub-step ago. Between steps, in the chain's centre space (world space without a centre).
    prev: Vec3,
    /// The tail now, in the same space as `prev`.
    current: Vec3,
    /// The bone's solved local rotation.
    rotation: Quat,
}

impl Segment {
    /// Moves this pair one sub-step (or places or holds it, per `phase`), given its parent's world transform. Returns
    /// the head's world transform for the next pair.
    fn advance(&mut self, parent: Transform, phase: Phase, colliders: &Colliders<'_>) -> Transform {
        let head = place(&parent, self.rest.translation);
        if self.length > 0.0 {
            // The head's rest frame in the world: its parent's rotation, then its own initial rotation.
            let frame = parent.rotation * self.rest.rotation;
            match phase {
                Phase::Rest => self.place_at_rest(head, frame * self.axis),
                Phase::Integrate => {
                    let tail = self.integrate(head, frame, colliders);
                    self.aim(frame, head, tail);
                }
                Phase::Hold => self.aim(frame, head, self.current),
            }
        }
        Transform { translation: head, rotation: parent.rotation * self.rotation }
    }

    /// The spec's "Applying rotation": turns the bone from its rest rotation to point at `tail`
    /// (`fromToQuaternion(boneAxis, to)`, with `to` the tail in the head's rest `frame`).
    fn aim(&mut self, frame: Quat, head: Vec3, tail: Vec3) {
        let to = (frame.conjugate() * (tail - head)).normalize_or(self.axis);
        self.rotation = self.rest.rotation * arc(self.axis, to);
    }

    /// Starts the tail on the bone's rest line, with no velocity, and the bone at its rest rotation.
    fn place_at_rest(&mut self, head: Vec3, rest_dir: Vec3) {
        let tail = head + rest_dir * self.length;
        self.prev = tail;
        self.current = tail;
        self.rotation = self.rest.rotation;
    }

    /// The spec's "Inertia calculation" and "Collision with colliders", for one sub-step, with the limit of
    /// `VRMC_springBone_limit` applied right after the inertia and after each push. `frame` is the head's rest frame
    /// in the world. Returns the new tail.
    fn integrate(&mut self, head: Vec3, frame: Quat, colliders: &Colliders<'_>) -> Vec3 {
        let Tuning { keep, stiffness, gravity, hit_radius } = self.tuning;
        let rest_dir = frame * self.axis;
        let inertia = (self.current - self.prev) * keep;
        let mut next = self.current + inertia + rest_dir * stiffness + gravity;
        next = self.constrain(head, next, frame);
        let outward = self.outward(head, rest_dir);
        for collider in colliders.iter() {
            if let Some(push) = collider.push_out(next, hit_radius, outward) {
                next = self.constrain(head, next + push, frame);
            }
        }
        if !next.is_finite() {
            // A bad frame (a tracker glitch gone NaN) must not poison the hair for good: restart from rest.
            next = head + rest_dir * self.length;
            self.current = next;
        }
        self.prev = self.current;
        self.current = next;
        next
    }

    /// The direction a tail sitting exactly on a collider's axis is pushed out along: the bone's direction as it stood
    /// before this sub-step, or its rest direction if the bone is too short to point. Fixing it here, rather than
    /// leaving it to rounding residue, makes the push the same on every run.
    fn outward(&self, head: Vec3, rest_dir: Vec3) -> Vec3 {
        let bone = self.current - head;
        let length = bone.length();
        if length >= DEGENERATE_DISTANCE { bone / length } else { rest_dir }
    }

    /// Puts `tail` back at `length` from `head` ("constrain the length"), then holds its direction inside the joint's
    /// limit, which keeps the length. A tail on the head has no direction, so it falls back to the rest direction.
    fn constrain(&self, head: Vec3, tail: Vec3, frame: Quat) -> Vec3 {
        let mut dir = (tail - head).normalize_or(frame * self.axis);
        if let Some(limit) = &self.limit {
            dir = limit.apply(frame, dir);
        }
        head + dir * self.length
    }
}

/// The colliders one chain collides with, placed in world space for this step.
#[derive(Copy, Clone)]
struct Colliders<'a> {
    placed: &'a [Collider],
    /// One span of `placed` per collider group the chain uses.
    groups: &'a [Range<usize>],
}

impl Colliders<'_> {
    fn iter(&self) -> impl Iterator<Item = &Collider> {
        self.groups.iter().flat_map(|group| &self.placed[group.clone()])
    }
}

/// A collider as authored: a bone and a shape in its space.
#[derive(Copy, Clone, Debug)]
struct ColliderDef {
    bone: usize,
    shape: ColliderShape,
}

impl ColliderDef {
    fn place(&self, world: &[Transform]) -> Collider {
        let frame = &world[self.bone];
        match self.shape {
            ColliderShape::Sphere { offset, radius } => Collider::sphere(place(frame, offset), radius),
            ColliderShape::Capsule { offset, radius, tail } => {
                Collider::capsule(place(frame, offset), place(frame, tail), radius)
            }
            ColliderShape::SphereInside { offset, radius } => Collider::sphere_inside(place(frame, offset), radius),
            ColliderShape::CapsuleInside { offset, radius, tail } => {
                Collider::capsule_inside(place(frame, offset), place(frame, tail), radius)
            }
            ColliderShape::Plane { offset, normal } => {
                // `validate` accepts a normal within a tolerance of unit length; the collision maths needs it exact.
                Collider::plane(place(frame, offset), (frame.rotation * normal).normalize_or(Vec3::Y))
            }
            // A shape this solver predates: no collision, never a panic. `validate` cannot know which shapes a
            // solver supports, so the gap shows up in review of the new variant, not at run time.
            _ => Collider::inert(),
        }
    }
}

/// One spring chain's place in the solver's flat buffers.
#[derive(Clone, Debug)]
struct Chain {
    /// The chain's name in the set, which is how [`VrmSpringSolver::set_params`] finds it.
    name: String,
    /// The centre bone, if the chain is simulated relative to one.
    center: Option<usize>,
    /// The centre's world transform at the start of the step (what `prev` and `current` are relative to).
    frame: Transform,
    /// A chain hanging off another chain's joint rebuilds the bones between that joint and its own first joint
    /// each sub-step, so it follows the other chain's swing.
    above: Range<usize>,
    /// The chain's [`Segment`]s.
    segments: Range<usize>,
    /// The bones from the last swinging joint down to the chain's tail bone, rebuilt each sub-step so chains hanging
    /// off it follow.
    tail: Range<usize>,
    /// The chain's collider groups, as spans of [`VrmSpringSolver::group_spans`].
    groups: Range<usize>,
}

/// Simulates a [`SpringSet`] with `VRMC_springBone` 1.0's reference algorithm
/// (<https://github.com/vrm-c/vrm-specification/tree/master/specification/VRMC_springBone-1.0#springbone-algorithm>).
///
/// See the [module docs](super) for what is simulated and the choices made where the spec leaves room.
#[derive(Clone, Debug)]
pub struct VrmSpringSolver {
    bone_count: usize,
    /// The bones whose world transform the step needs from the animated pose: every ancestor of a chain, centre or
    /// collider. Parents first.
    animated: Vec<Node>,
    /// Pool of the bones each chain rebuilds (see [`Chain::above`], [`Segment::between`], [`Chain::tail`]).
    nodes: Vec<Node>,
    chains: Vec<Chain>,
    segments: Vec<Segment>,
    collider_defs: Vec<ColliderDef>,
    /// The colliders in world space for the step in progress (scratch, same order as `collider_defs`).
    placed: Vec<Collider>,
    group_spans: Vec<Range<usize>>,
    /// World transforms (scratch).
    world: Box<Pose>,
    /// Time not yet consumed by a sub-step, seconds.
    owed_s: f64,
    max_substeps: u32,
    stats: SpringStats,
    /// Set at construction and by [`SpringSolver::reset`]: the next step starts every spring at rest.
    needs_rest: bool,
    /// Whether a frame's sub-steps see the animated pose interpolated from the previous frame's
    /// ([`VrmSpringSolver::set_pose_interpolation`]).
    interpolate: bool,
    /// The bones whose local transform the step reads from the animated pose: every bone of `animated` and of `nodes`,
    /// ascending. Only these are interpolated.
    read: Vec<usize>,
    /// The previous frame's local transforms of the `read` bones, same order; meaningful while `has_previous`.
    previous: Vec<Transform>,
    /// This frame's local transforms of the `read` bones as they came in, so the pose is left as it was given (scratch).
    incoming: Vec<Transform>,
    /// Whether `previous` holds a frame: false at construction, after a reset and when interpolation is switched on.
    has_previous: bool,
}

impl VrmSpringSolver {
    /// Builds the solver for `set` on `skeleton`.
    ///
    /// # Errors
    /// Whatever [`SpringSet::validate`] finds wrong with the set.
    pub fn new(set: &SpringSet, skeleton: &Skeleton) -> Result<Self, SpringError> {
        set.validate(skeleton)?;
        Ok(Builder::new(set, skeleton).solver)
    }

    /// Whether `skeleton` is the one this solver was built for. The trait passes it on every call but the solver
    /// already holds everything it needs, so a different skeleton is a caller bug; it is refused (and trips a debug
    /// assertion) rather than indexed out of bounds.
    fn is_for(&self, skeleton: &Skeleton) -> bool {
        let same = skeleton.len() == self.bone_count;
        debug_assert!(same, "the solver was built for a skeleton of {} bones, not {}", self.bone_count, skeleton.len());
        same
    }

    /// Sub-steps due for a frame of `dt` seconds, after the cap. Records what was dropped.
    fn take_substeps(&mut self, dt: f32) -> u32 {
        let step_s = 1.0 / f64::from(PHYSICS_HZ);
        if dt.is_finite() && dt > 0.0 {
            self.owed_s += f64::from(dt);
        }
        let due = (self.owed_s / step_s + SUBSTEP_SLACK).floor();
        self.owed_s = (self.owed_s - due * step_s).clamp(0.0, step_s);
        let run = due.min(f64::from(self.max_substeps));
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0 <= run <= max_substeps
        let substeps = run as u32;
        #[allow(clippy::cast_possible_truncation)] // a few seconds at most, far inside f32
        let dropped_s = ((due - run) * step_s) as f32;
        self.stats = SpringStats { substeps, dropped_s };
        substeps
    }

    /// One frame's work: place the animated bones, run `substeps` sub-steps (or just re-aim the bones when there are
    /// none), and write the solved rotations into `pose`. With interpolation on, a frame of two or more sub-steps
    /// runs them one at a time, sub-step `i` of `n` seeing the pose `i/n` of the way from the previous frame's.
    fn advance(&mut self, pose: &mut Pose, substeps: u32, interpolate: bool) {
        let rest = std::mem::take(&mut self.needs_rest);
        if interpolate && self.has_previous && !rest && substeps > 1 {
            self.advance_interpolated(pose, substeps);
        } else {
            // A frame too short for a sub-step still re-aims the bones at their tails, unless they were just placed.
            let (phase, count) =
                if substeps > 0 { (Phase::Integrate, substeps) } else { (Phase::Hold, u32::from(!rest)) };
            self.run_span(&pose.local, rest, phase, count);
        }
        if interpolate {
            for (slot, &bone) in self.previous.iter_mut().zip(&self.read) {
                *slot = pose.local[bone];
            }
            self.has_previous = true;
        }
        for segment in &self.segments {
            pose.local[segment.bone].rotation = segment.rotation;
        }
    }

    /// The sub-steps of one frame, each with the animated bones of `pose` blended from the previous frame's: the
    /// translations lerped and the rotations slerped, `i/n` of the way for sub-step `i` of `n`. The blend is written
    /// into `pose` for the sub-step and the incoming values are put back after, so the caller's pose is unchanged.
    fn advance_interpolated(&mut self, pose: &mut Pose, substeps: u32) {
        for (slot, &bone) in self.incoming.iter_mut().zip(&self.read) {
            *slot = pose.local[bone];
        }
        #[allow(clippy::cast_precision_loss)] // a few sub-steps, far inside f32
        let share = 1.0 / substeps as f32;
        for i in 1..=substeps {
            #[allow(clippy::cast_precision_loss)]
            let t = i as f32 * share;
            for ((&bone, from), to) in self.read.iter().zip(&self.previous).zip(&self.incoming) {
                // A bone that did not move keeps its exact value, so a still pose steps bit for bit as a held one.
                pose.local[bone] = if i == substeps || from == to {
                    *to
                } else {
                    Transform {
                        translation: from.translation.lerp(to.translation, t),
                        rotation: from.rotation.slerp(to.rotation, t),
                    }
                };
            }
            self.run_span(&pose.local, false, Phase::Integrate, 1);
        }
    }

    /// Runs `count` sub-steps (or places the springs at rest, when `rest`) with the animated pose held at `locals`:
    /// places the animated bones and the colliders, maps every chain's tails to world space, and back after.
    fn run_span(&mut self, locals: &[Transform], rest: bool, phase: Phase, count: u32) {
        let Self { animated, nodes, chains, segments, collider_defs, placed, group_spans, world, .. } = self;
        let world = &mut world.local[..];
        rebuild(world, locals, animated);
        for (slot, def) in placed.iter_mut().zip(collider_defs.iter()) {
            *slot = def.place(world);
        }
        let scene = Scene { nodes, group_spans, placed, locals };

        for chain in chains.iter_mut() {
            let segments = &mut segments[chain.segments.clone()];
            chain.frame = chain.center.map_or(Transform::IDENTITY, |c| world[c]);
            if rest {
                run_chain(chain, segments, &scene, world, Phase::Rest);
            } else {
                chain.to_world(segments);
            }
        }
        for _ in 0..count {
            for chain in chains.iter() {
                run_chain(chain, &mut segments[chain.segments.clone()], &scene, world, phase);
            }
        }
        for chain in chains.iter() {
            chain.to_centre(&mut segments[chain.segments.clone()]);
        }
    }

    /// Whether the sub-steps of a frame see the animated pose interpolated from the previous frame's (on, the default)
    /// or held at this frame's (off, as the spec's single step per frame and three-vrm do). See the module docs'
    /// measurement: on, the hair of a 30 fps render follows a 120 fps run to about 1 mm instead of 47 mm, at the cost
    /// of re-placing the animated bones and colliders every sub-step. The first frame after switching it on, after
    /// [`SpringSolver::reset`] or after a warm-up has no previous frame and is held. Cold: never allocates.
    pub fn set_pose_interpolation(&mut self, on: bool) {
        if on != self.interpolate {
            self.interpolate = on;
            self.has_previous = false;
        }
    }

    /// Whether the animated pose is interpolated across a frame's sub-steps ([`Self::set_pose_interpolation`]).
    pub fn pose_interpolation(&self) -> bool {
        self.interpolate
    }

    /// Gives one joint of a running chain new parameters without moving anything: the tails and velocities carry on,
    /// and the new stiffness, drag, gravity and hit radius act from the next sub-step. A solver built afresh would put
    /// every tail back at rest, a visible pop while someone drags a slider.
    ///
    /// `joint` is the joint's index in the chain as authored. The last joint is only a tail, so its parameters act
    /// nowhere and it is refused. Cold path: nothing here allocates unless it returns an error.
    ///
    /// The ranges are not checked here, because `SpringParams`' checks are private to `omuzaki-anim-api`. Pass
    /// parameters the authored set would accept.
    ///
    /// # Errors
    /// [`SpringError::BadParams`] when no chain has that name, or `joint` is not a swinging joint of it.
    pub fn set_params(&mut self, chain: &str, joint: usize, params: SpringParams) -> Result<(), SpringError> {
        let Some(owner) = self.chains.iter().find(|c| c.name == chain) else {
            return Err(SpringError::BadParams {
                what: format!("chain {chain:?}"),
                reason: "no chain of this name in the solver",
            });
        };
        if joint >= owner.segments.len() {
            return Err(SpringError::BadParams {
                what: format!("joint {joint} of chain {chain:?}"),
                reason: "no swinging joint at that index; the last joint is only a tail",
            });
        }
        let segment = owner.segments.start + joint;
        self.segments[segment].tuning = Tuning::of(&params);
        Ok(())
    }
}

/// The step's read-only inputs, shared by every chain.
struct Scene<'a> {
    /// Pool of the bones chains rebuild each sub-step.
    nodes: &'a [Node],
    /// Spans of `placed`, one per collider group a chain uses.
    group_spans: &'a [Range<usize>],
    /// The colliders in world space.
    placed: &'a [Collider],
    /// The animated pose's local transforms.
    locals: &'a [Transform],
}

/// Runs one sub-step over a chain, writing the world transforms of its joints (and what hangs off them) into `world`.
fn run_chain(chain: &Chain, segments: &mut [Segment], scene: &Scene<'_>, world: &mut [Transform], phase: Phase) {
    let colliders = Colliders { placed: scene.placed, groups: &scene.group_spans[chain.groups.clone()] };
    rebuild(world, scene.locals, &scene.nodes[chain.above.clone()]);
    for segment in segments {
        rebuild(world, scene.locals, &scene.nodes[segment.between.clone()]);
        let parent = segment.parent.map_or(Transform::IDENTITY, |p| world[p]);
        world[segment.bone] = segment.advance(parent, phase, &colliders);
    }
    rebuild(world, scene.locals, &scene.nodes[chain.tail.clone()]);
}

impl Chain {
    /// Moves the chain's tails from centre space to world space for the step. The sub-steps then run in world space,
    /// which is the same arithmetic as running them in centre space because the centre holds still within a step.
    fn to_world(&self, segments: &mut [Segment]) {
        if self.center.is_some() {
            for s in segments {
                s.prev = place(&self.frame, s.prev);
                s.current = place(&self.frame, s.current);
            }
        }
    }

    /// The inverse of [`Chain::to_world`]: stores the tails relative to the centre, so that when the centre moves
    /// the next step, they move with it.
    fn to_centre(&self, segments: &mut [Segment]) {
        if self.center.is_some() {
            for s in segments {
                s.prev = unplace(&self.frame, s.prev);
                s.current = unplace(&self.frame, s.current);
            }
        }
    }
}

impl SpringSolver for VrmSpringSolver {
    fn step(&mut self, tick: Tick, skeleton: &Skeleton, pose: &mut Pose) {
        if self.is_for(skeleton) {
            let substeps = self.take_substeps(tick.dt);
            self.advance(pose, substeps, self.interpolate);
        }
    }

    fn warm_up(&mut self, skeleton: &Skeleton, pose: &mut Pose, seconds: f32) {
        if self.is_for(skeleton) {
            let seconds = if seconds.is_finite() { seconds.clamp(0.0, MAX_WARM_UP_S) } else { 0.0 };
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0 <= seconds <= 60
            let substeps = (f64::from(seconds) * f64::from(PHYSICS_HZ)).round() as u32;
            // A warm-up settles in one held pose; the frame after it has nothing to interpolate from.
            self.advance(pose, substeps, false);
            self.has_previous = false;
        }
    }

    fn reset(&mut self) {
        self.needs_rest = true;
        self.has_previous = false;
        self.owed_s = 0.0;
        self.stats = SpringStats::default();
    }

    fn set_max_substeps(&mut self, max: u32) {
        self.max_substeps = max.max(1);
    }

    fn stats(&self) -> SpringStats {
        self.stats
    }
}

/// Cold construction: turns the authored set into the solver's flat buffers.
struct Builder<'a> {
    bones: &'a [BoneDef],
    /// Every bone's rest transform relative to the skeleton root.
    rest_world: Box<Pose>,
    /// Whether each bone is a joint of some chain.
    is_joint: Vec<bool>,
    /// For each of the set's collider groups, its span of the solver's `collider_defs`.
    group_colliders: Vec<Range<usize>>,
    solver: VrmSpringSolver,
}

impl<'a> Builder<'a> {
    fn new(set: &SpringSet, skeleton: &'a Skeleton) -> Self {
        let mut rest_world = Box::new(Pose::IDENTITY);
        skeleton.world_pose(&skeleton.rest_pose(), &mut rest_world);
        let mut is_joint = vec![false; skeleton.len()];
        for joint in set.chains.iter().flat_map(|c| &c.joints) {
            is_joint[joint.bone.index()] = true;
        }
        let solver = VrmSpringSolver {
            bone_count: skeleton.len(),
            animated: Vec::new(),
            nodes: Vec::new(),
            chains: Vec::new(),
            segments: Vec::new(),
            collider_defs: Vec::new(),
            placed: Vec::new(),
            group_spans: Vec::new(),
            world: Box::new(Pose::IDENTITY),
            owed_s: 0.0,
            max_substeps: DEFAULT_MAX_SUBSTEPS,
            stats: SpringStats::default(),
            needs_rest: true,
            interpolate: true,
            read: Vec::new(),
            previous: Vec::new(),
            incoming: Vec::new(),
            has_previous: false,
        };
        let mut builder = Self { bones: skeleton.bones(), rest_world, is_joint, group_colliders: Vec::new(), solver };
        builder.add_colliders(set);
        builder.add_chains(set);
        builder.solver.animated = builder.animated_bones();
        let read: BTreeSet<usize> =
            builder.solver.animated.iter().chain(&builder.solver.nodes).map(|node| node.bone).collect();
        builder.solver.read = read.into_iter().collect();
        builder.solver.previous = vec![Transform::IDENTITY; builder.solver.read.len()];
        builder.solver.incoming = vec![Transform::IDENTITY; builder.solver.read.len()];
        builder.solver.placed = vec![Collider::sphere(Vec3::ZERO, 0.0); builder.solver.collider_defs.len()];
        builder
    }

    fn node(&self, bone: usize) -> Node {
        Node { bone, parent: self.bones[bone].parent.map(BoneId::index) }
    }

    /// `start` and then each ancestor up to the root; nothing for `None`.
    fn ancestors_of(&self, start: Option<usize>) -> impl Iterator<Item = usize> {
        std::iter::successors(start, |&b| self.bones[b].parent.map(BoneId::index))
    }

    /// The bones from `start` up to, not including, the first one `stop` accepts (or the root), parents first.
    fn climb(&self, start: Option<usize>, stop: impl Fn(usize) -> bool) -> Vec<Node> {
        let mut path: Vec<Node> = self.ancestors_of(start).take_while(|&b| !stop(b)).map(|b| self.node(b)).collect();
        path.reverse();
        path
    }

    fn push_nodes(&mut self, nodes: Vec<Node>) -> Range<usize> {
        let start = self.solver.nodes.len();
        self.solver.nodes.extend(nodes);
        start..self.solver.nodes.len()
    }

    /// The bones whose world transform the step reads from the animated pose: every chain's parent and centre and every
    /// collider's bone, with all their ancestors (a world transform is the parent's times the bone's own).
    fn animated_bones(&self) -> Vec<Node> {
        let solver = &self.solver;
        let chain_roots =
            solver.chains.iter().flat_map(|c| solver.segments[c.segments.start].parent.into_iter().chain(c.center));
        let needed = chain_roots.chain(solver.collider_defs.iter().map(|c| c.bone));
        let with_ancestors: BTreeSet<usize> = needed.flat_map(|bone| self.ancestors_of(Some(bone))).collect();
        with_ancestors.into_iter().map(|bone| self.node(bone)).collect()
    }

    fn add_colliders(&mut self, set: &SpringSet) {
        for group in &set.groups {
            let start = self.solver.collider_defs.len();
            let defs = group.colliders.iter().map(|c| ColliderDef { bone: c.bone.index(), shape: c.shape });
            self.solver.collider_defs.extend(defs);
            self.group_colliders.push(start..self.solver.collider_defs.len());
        }
    }

    /// Adds the chains in the order of their first bone. Bones come parents first, so a chain hanging off another's
    /// joint always comes after it, which is the order the spec asks for.
    fn add_chains(&mut self, set: &SpringSet) {
        let mut order: Vec<_> = set.chains.iter().collect();
        order.sort_by_key(|c| c.joints[0].bone);
        for chain in order {
            let spans = chain.collider_groups.iter().filter_map(|name| {
                let group = set.groups.iter().position(|g| &g.name == name)?;
                Some(self.group_colliders[group].clone())
            });
            let start = self.solver.group_spans.len();
            let spans: Vec<_> = spans.collect();
            self.solver.group_spans.extend(spans);
            let groups = start..self.solver.group_spans.len();
            self.add_chain(chain, groups);
        }
    }

    fn add_chain(&mut self, chain: &SpringChain, groups: Range<usize>) {
        let first = chain.joints[0].bone.index();
        let parent = self.bones[first].parent.map(BoneId::index);
        // If a joint of another chain is above this one, rebuild the bones between it and this chain's parent.
        let hangs_off_a_joint = self.ancestors_of(parent).any(|b| self.is_joint[b]);
        let above_nodes = if hangs_off_a_joint { self.climb(parent, |b| self.is_joint[b]) } else { Vec::new() };
        let above = self.push_nodes(above_nodes);

        let segments_start = self.solver.segments.len();
        let mut previous_head = None;
        for pair in chain.joints.windows(2) {
            let segment = self.segment(pair[0], pair[1].bone.index(), previous_head);
            previous_head = Some(segment.bone);
            self.solver.segments.push(segment);
        }
        let segments = segments_start..self.solver.segments.len();

        let last_head = self.solver.segments[segments.end - 1].bone;
        let tail_bone = chain.joints[chain.joints.len() - 1].bone.index();
        let tail_nodes = self.climb(Some(tail_bone), |b| b == last_head);
        let tail = self.push_nodes(tail_nodes);

        let center = chain.center.map(BoneId::index);
        let name = chain.name.clone();
        self.solver.chains.push(Chain { name, center, frame: Transform::IDENTITY, above, segments, tail, groups });
    }

    /// The pair for `joint` pointing at the bone `tail`, below `previous_head` in the same chain (if any).
    fn segment(&mut self, joint: SpringJoint, tail: usize, previous_head: Option<usize>) -> Segment {
        let head = joint.bone.index();
        let parent = self.bones[head].parent.map(BoneId::index);
        let between_nodes = match previous_head {
            Some(previous) => self.climb(parent, |b| b == previous),
            None => Vec::new(),
        };
        let between = self.push_nodes(between_nodes);

        let head_world = self.rest_world.local[head];
        let to_tail =
            head_world.rotation.conjugate() * (self.rest_world.local[tail].translation - head_world.translation);
        let length = to_tail.length();
        Segment {
            bone: head,
            parent,
            between,
            rest: self.bones[head].rest,
            axis: if length > 0.0 { to_tail / length } else { Vec3::Y },
            length,
            limit: joint.limit.map(|limit| Limiter::new(limit, if length > 0.0 { to_tail / length } else { Vec3::Y })),
            tuning: Tuning::of(&joint.params),
            prev: Vec3::ZERO,
            current: Vec3::ZERO,
            rotation: self.bones[head].rest.rotation,
        }
    }
}

#[cfg(test)]
impl VrmSpringSolver {
    /// Test helper: adopts `from`'s dynamic state (tails, rotations, time owed, whether the next step rests) and keeps
    /// this solver's parameters. Lets a test start a solver built fresh from a new set at the same point as a switched
    /// one.
    pub(super) fn copy_state_from(&mut self, from: &Self) {
        for (mine, theirs) in self.segments.iter_mut().zip(&from.segments) {
            mine.prev = theirs.prev;
            mine.current = theirs.current;
            mine.rotation = theirs.rotation;
        }
        self.owed_s = from.owed_s;
        self.needs_rest = from.needs_rest;
    }
}
