//! Where hands are told to go (RG-05): goals with an orientation, for props and for crossed arms.
//!
//! The two-bone solve ([`TwoBoneIk`]) takes a *point* for the wrist. A hand holding something needs a *frame*: the
//! palm lands on the prop's grip frame in position and angle. Three goal sources build on it:
//!
//! - [`HandTarget`]: one palm frame (an umbrella handle). The wrist target is the frame carried back through the
//!   palm offset, the arm is solved onto it and the hand bone is turned to match.
//! - [`TwoHandedProp`]: a prop frame and a grip on it for each hand (a book). Both palms stay on their frames.
//! - [`CrossedArms`]: each hand lands on a frame on the opposite upper arm, the right forearm over the left, and a
//!   clearance pass swings the elbows out until both forearms clear the torso and each other.
//!
//! # The palm frame
//!
//! A palm frame has its origin at the palm, `X` along the fingers, `Y` out of the back of the hand and `Z = X × Y`.
//! In the T-pose a left hand's frame is the bone's own frame moved along the bone; a right hand's is turned half a turn
//! about `Y` (fingers along `-X`). [`Hand::palm`] builds that offset; any other can be given.
//!
//! No allocation and no state: every call is a pure function of its inputs and the pose.

use std::f32::consts::{PI, TAU};

use omuzaki_anim_api::{BoneId, HumanoidBone, IkGoals, IkSolver, Limb, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Mat3, Quat, Vec3};

use super::two_bone::TwoBoneIk;

/// Which hand. Sides are the character's own: left is `+X`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Hand {
    /// The character's left.
    Left,
    /// The character's right.
    Right,
}

impl Hand {
    /// The other hand.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    /// `[upper arm, lower arm, hand]`.
    const fn bones(self) -> [HumanoidBone; 3] {
        match self {
            Self::Left => [HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand],
            Self::Right => [HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm, HumanoidBone::RightHand],
        }
    }

    /// The palm frame in the hand bone's space, `to_palm` metres from the wrist along the fingers (see the module docs).
    pub fn palm(self, to_palm: f32) -> Transform {
        match self {
            Self::Left => Transform::from_translation(Vec3::new(to_palm, 0.0, 0.0)),
            Self::Right => {
                Transform { translation: Vec3::new(-to_palm, 0.0, 0.0), rotation: Quat::from_rotation_y(PI) }
            }
        }
    }

    /// The bone ids `[upper arm, lower arm, hand]`, if the skeleton has all three.
    fn ids(self, skeleton: &Skeleton) -> Option<[BoneId; 3]> {
        let map = skeleton.humanoid_map()?;
        let [a, b, c] = self.bones();
        Some([map.get(a)?, map.get(b)?, map.get(c)?])
    }
}

/// Whether a quaternion can be normalised and inverted: every component finite, and not (nearly) zero. A zero quaternion
/// has no direction, and normalising or inverting it produces NaNs that would reach the pose.
fn is_rotation(q: Quat) -> bool {
    const MIN_LENGTH_SQUARED: f32 = 1e-12;
    q.is_finite() && q.length_squared() > MIN_LENGTH_SQUARED
}

/// The inverse of a rigid transform.
fn inverse(t: Transform) -> Transform {
    let rotation = t.rotation.inverse();
    Transform { translation: rotation * -t.translation, rotation }
}

/// A bone's transform in the skeleton's root space, found by multiplying up its chain (a handful of multiplies, so the
/// arm code never runs forward kinematics over the whole skeleton).
fn bone_world(skeleton: &Skeleton, pose: &Pose, bone: BoneId) -> Transform {
    let mut world = pose.local[bone.index()];
    let mut at = skeleton.bones()[bone.index()].parent;
    while let Some(parent) = at {
        world = pose.local[parent.index()] * world;
        at = skeleton.bones()[parent.index()].parent;
    }
    world
}

/// One hand's palm onto one frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HandTarget {
    /// The hand.
    pub hand: Hand,
    /// Where the palm frame must land, skeleton root space.
    pub frame: Transform,
    /// The palm frame in the hand bone's space ([`Hand::palm`]).
    pub palm: Transform,
    /// A point the elbow bends toward ([`Limb::pole`]).
    pub pole: Vec3,
    /// How much of the goal to apply, `0..=1`: `0` leaves the pose as animated.
    pub weight: f32,
}

impl HandTarget {
    /// Solves the arm and turns the hand so the palm lands on the frame. Returns `false`, leaving the pose alone, if the
    /// skeleton lacks the arm bones, any field is not finite, or either rotation is zero (see [`is_rotation`]).
    ///
    /// The position error is that of the two-bone solve (inside 98% of the reach, under half a millimetre); an
    /// out-of-reach frame is approached with a straight arm. The angle is exact at full weight.
    pub fn solve(&self, skeleton: &Skeleton, pose: &mut Pose) -> bool {
        let Some([_, _, hand]) = self.hand.ids(skeleton) else { return false };
        let ok = self.frame.translation.is_finite()
            && is_rotation(self.frame.rotation)
            && self.palm.translation.is_finite()
            && is_rotation(self.palm.rotation)
            && self.pole.is_finite()
            && self.weight.is_finite();
        if !ok {
            return false;
        }
        let frame = Transform { translation: self.frame.translation, rotation: self.frame.rotation.normalize() };
        // The hand bone's world transform that puts the palm on the frame.
        let wanted = frame * inverse(self.palm);
        let limb = Some(Limb { target: wanted.translation, pole: self.pole, weight: self.weight });
        let mut goals = IkGoals::NONE;
        match self.hand {
            Hand::Left => goals.left_hand = limb,
            Hand::Right => goals.right_hand = limb,
        }
        if self.weight <= 0.0 {
            return true;
        }
        TwoBoneIk::new().solve(skeleton, &goals, pose);

        let Some(parent) = skeleton.bones()[hand.index()].parent else { return true };
        let parent_rotation = bone_world(skeleton, pose, parent).rotation;
        let local = &mut pose.local[hand.index()].rotation;
        *local = local.slerp((parent_rotation.inverse() * wanted.rotation).normalize(), self.weight.clamp(0.0, 1.0));
        true
    }
}

/// How far a hand's palm is from a frame: `(metres, radians)`. `None` if the skeleton lacks the arm.
pub fn palm_error(
    skeleton: &Skeleton,
    pose: &Pose,
    hand: Hand,
    palm: Transform,
    frame: Transform,
) -> Option<(f32, f32)> {
    let [_, _, id] = hand.ids(skeleton)?;
    let at = bone_world(skeleton, pose, id) * palm;
    Some((at.translation.distance(frame.translation), at.rotation.angle_between(frame.rotation)))
}

/// A prop held in both hands: the prop's frame and the palm frame each hand takes on it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TwoHandedProp {
    /// The prop's frame, skeleton root space.
    pub prop: Transform,
    /// The left palm frame in the prop's space.
    pub left: Transform,
    /// The right palm frame in the prop's space.
    pub right: Transform,
    /// The palm frame in the hand bone's space, for the left and the right hand ([`Hand::palm`]).
    pub palms: [Transform; 2],
    /// Poles `[left, right]` ([`Limb::pole`]).
    pub poles: [Vec3; 2],
    /// How much of the goal to apply, `0..=1`.
    pub weight: f32,
}

impl TwoHandedProp {
    /// The goal for each hand, `[left, right]`.
    pub fn targets(&self) -> [HandTarget; 2] {
        let one = |hand: Hand, grip: Transform, i: usize| HandTarget {
            hand,
            frame: self.prop * grip,
            palm: self.palms[i],
            pole: self.poles[i],
            weight: self.weight,
        };
        [one(Hand::Left, self.left, 0), one(Hand::Right, self.right, 1)]
    }

    /// Solves both arms. `false` if either could not be solved.
    pub fn solve(&self, skeleton: &Skeleton, pose: &mut Pose) -> bool {
        let [left, right] = self.targets();
        // Both run, whatever the first returns.
        let (left, right) = (left.solve(skeleton, pose), right.solve(skeleton, pose));
        left && right
    }
}

/// A capsule: the points within `radius` of the segment `a`-`b`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Capsule {
    /// One end of the axis.
    pub a: Vec3,
    /// The other end of the axis.
    pub b: Vec3,
    /// The radius.
    pub radius: f32,
}

impl Capsule {
    /// The gap between two capsules' surfaces; negative when they overlap.
    pub fn gap(&self, other: &Self) -> f32 {
        segment_distance(self.a, self.b, other.a, other.b) - self.radius - other.radius
    }
}

#[allow(clippy::many_single_char_names)] // the closest-points derivation names its terms a, b, c, e, f, s, t
/// The shortest distance between the segments `p1`-`q1` and `p2`-`q2` (closest points of two segments, clamped to both).
fn segment_distance(p1: Vec3, q1: Vec3, p2: Vec3, q2: Vec3) -> f32 {
    const TINY: f32 = 1e-12;
    let (d1, d2, r) = (q1 - p1, q2 - p2, p1 - p2);
    let (a, e, f) = (d1.length_squared(), d2.length_squared(), d2.dot(r));
    let (s, t) = if a <= TINY && e <= TINY {
        (0.0, 0.0)
    } else if a <= TINY {
        (0.0, (f / e).clamp(0.0, 1.0))
    } else {
        let c = d1.dot(r);
        if e <= TINY {
            ((-c / a).clamp(0.0, 1.0), 0.0)
        } else {
            let b = d1.dot(d2);
            let denominator = a * e - b * b;
            let s = if denominator > TINY { ((b * f - c * e) / denominator).clamp(0.0, 1.0) } else { 0.0 };
            let t = (b * s + f) / e;
            if t < 0.0 {
                ((-c / a).clamp(0.0, 1.0), 0.0)
            } else if t > 1.0 {
                (((b - c) / a).clamp(0.0, 1.0), 1.0)
            } else {
                (s, t)
            }
        }
    };
    (p1 + d1 * s).distance(p2 + d2 * t)
}

/// What the clearance pass of [`CrossedArms`] settled on.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ArmClearance {
    /// The smaller of the two forearms' gaps to the torso, metres.
    pub torso: f32,
    /// The gap between the two forearms, metres.
    pub forearms: f32,
    /// The larger of the two palms' distances from their contact frames on the opposite upper arms *as solved*, metres.
    pub contact: f32,
    /// How far each elbow was swung about its shoulder-to-wrist line, radians, `[left, right]`. Pass it back as the hint of
    /// [`CrossedArms::solve_from`] next frame.
    pub swings: [f32; 2],
    /// Whether both gaps reached the margin with both palms on their arms (`contact` within [`CONTACT_TOLERANCE`]).
    pub cleared: bool,
}

/// Arms crossed over the chest: each hand on the opposite upper arm, one forearm over the other.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct CrossedArms {
    /// The hand whose forearm lies on top (in front).
    pub over: Hand,
    /// Where on the opposite upper arm the palm lands: `0` at the shoulder, `1` at the elbow.
    pub fraction: f32,
    /// The wrist-to-palm distance along the fingers ([`Hand::palm`]).
    pub to_palm: f32,
    /// The upper arm's radius: the palm rests this far off its axis.
    pub arm_radius: f32,
    /// The forearm's radius, for the clearance test.
    pub forearm_radius: f32,
    /// The direction the over forearm stands off the under one (forward, up, or between), root space.
    pub stack: Vec3,
    /// The torso, root space. Forearms cross in front of the chest, so its radius is the section's half depth.
    pub torso: Capsule,
    /// The smallest gap the clearance pass accepts, metres.
    pub margin: f32,
    /// How much of the goal to apply, `0..=1`.
    pub weight: f32,
}

/// How far a palm may sit from its contact frame on the solved opposite upper arm, metres.
pub const CONTACT_TOLERANCE: f32 = 0.001;
/// The coarse step of the clearance search, which tries every pair of swings round the whole circle.
const COARSE_STEP: f32 = 15.0 * PI / 180.0;
/// The fine step of the second search, around the best coarse pair.
const FINE_STEP: f32 = 3.0 * PI / 180.0;
/// How many fine steps the second search takes each way.
const FINE_REACH: i32 = 4;
/// The most rounds of neighbours a warm start climbs through.
const HINT_ROUNDS: usize = 3;
/// The most times the contact frames are re-derived from the arms as solved. The frames sit on the opposite upper arms,
/// which the solve itself moves, so each pass lands the palms closer to where those arms ended up.
const MAX_PASSES: usize = 8;
/// The pass loop stops once no frame has moved more than this, metres.
const SETTLED: f32 = 1e-4;

/// A swing as the size of the turn, `0..=PI`.
fn turn(swing: f32) -> f32 {
    let wrapped = swing.rem_euclid(TAU);
    wrapped.min(TAU - wrapped)
}

impl ArmClearance {
    /// Whether this beats `other` as the pose to keep: a cleared pose beats an uncleared one, the smaller turn wins between
    /// cleared ones, and between uncleared ones the pose with palms on their arms and the larger smaller-gap wins.
    fn beats(&self, other: &Self) -> bool {
        let rank = |c: &Self| (c.cleared, c.contact <= CONTACT_TOLERANCE);
        if rank(self) != rank(other) {
            return rank(self) > rank(other);
        }
        if self.cleared {
            turn(self.swings[0]) + turn(self.swings[1]) < turn(other.swings[0]) + turn(other.swings[1])
        } else {
            self.torso.min(self.forearms) > other.torso.min(other.forearms)
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // a few dozen steps
impl CrossedArms {
    /// The palm frame for `hand` on the opposite upper arm of `pose`: the back of the hand facing away from the torso,
    /// the fingers across the body. The over hand rests a forearm's width off the under one, along [`stack`](Self::stack).
    fn frame(&self, skeleton: &Skeleton, pose: &Pose, hand: Hand) -> Option<Transform> {
        let [own, _, _] = hand.ids(skeleton)?;
        let [shoulder, elbow, _] = hand.other().ids(skeleton)?;
        let at = |id: BoneId| bone_world(skeleton, pose, id).translation;
        let (shoulder, elbow) = (at(shoulder), at(elbow));
        let along = (elbow - shoulder).try_normalize()?;
        let axis = shoulder.lerp(elbow, self.fraction);
        // Out is away from the torso, square to the arm: a hand rests on the front of an arm, wherever that faces.
        let (a, b) = (self.torso.a, self.torso.b);
        let t = ((axis - a).dot(b - a) / (b - a).length_squared().max(f32::EPSILON)).clamp(0.0, 1.0);
        let from_torso = axis - a - (b - a) * t;
        let out = (from_torso - along * from_torso.dot(along))
            .try_normalize()
            .unwrap_or_else(|| along.any_orthonormal_vector());
        let lift = if hand == self.over { 2.0 * self.forearm_radius + self.margin } else { 0.0 };
        let on_arm = axis + out * self.arm_radius + self.stack.normalize_or_zero() * lift;
        let across = on_arm - at(own);
        let fingers = (across - out * across.dot(out)).try_normalize().unwrap_or_else(|| out.any_orthonormal_vector());
        let rotation = Quat::from_mat3(&Mat3::from_cols(fingers, out, fingers.cross(out)));
        Some(Transform { translation: on_arm, rotation })
    }

    /// The palm frames `[left, right]` on the upper arms of `pose`. [`solve`](Self::solve) aims for these on the arms as
    /// animated and refines them against the arms as solved, so on its result they are where the palms are.
    pub fn frames(&self, skeleton: &Skeleton, pose: &Pose) -> Option<[Transform; 2]> {
        Some([self.frame(skeleton, pose, Hand::Left)?, self.frame(skeleton, pose, Hand::Right)?])
    }

    /// Both forearms (elbow to wrist) of `pose`.
    fn forearms(&self, skeleton: &Skeleton, pose: &Pose) -> Option<[Capsule; 2]> {
        let capsule = |hand: Hand| {
            let [_, elbow, wrist] = hand.ids(skeleton)?;
            let at = |id: BoneId| bone_world(skeleton, pose, id).translation;
            Some(Capsule { a: at(elbow), b: at(wrist), radius: self.forearm_radius })
        };
        Some([capsule(Hand::Left)?, capsule(Hand::Right)?])
    }

    /// Solves both arms with each elbow turned `swings[hand]` radians about its shoulder-to-wrist line from the side
    /// `bends[hand]` it starts on, and measures the result. Each pass starts again from the animated pose `start`, with
    /// the contact frames re-derived from the arms the last pass solved, until they stop moving.
    fn attempt(
        &self,
        skeleton: &Skeleton,
        start: &Pose,
        bends: [Vec3; 2],
        swings: [f32; 2],
    ) -> Option<(Pose, ArmClearance)> {
        let mut frames = self.frames(skeleton, start)?;
        let mut pose = start.clone();
        for _ in 0..MAX_PASSES {
            pose.clone_from(start);
            for (i, hand) in [Hand::Left, Hand::Right].into_iter().enumerate() {
                let [upper, _, _] = hand.ids(skeleton)?;
                let palm = hand.palm(self.to_palm);
                let shoulder = bone_world(skeleton, start, upper).translation;
                let axis = ((frames[i] * inverse(palm)).translation - shoulder).try_normalize()?;
                let bend = Quat::from_axis_angle(axis, swings[i]) * bends[i];
                let target = HandTarget { hand, frame: frames[i], palm, pole: shoulder + bend, weight: self.weight };
                target.solve(skeleton, &mut pose);
            }
            let next = self.frames(skeleton, &pose)?;
            let moved = (0..2).map(|i| next[i].translation.distance(frames[i].translation)).fold(0.0, f32::max);
            frames = next;
            if moved < SETTLED {
                break;
            }
        }
        let [left, right] = self.forearms(skeleton, &pose)?;
        let torso = left.gap(&self.torso).min(right.gap(&self.torso));
        let forearms = left.gap(&right);
        let contact = [Hand::Left, Hand::Right]
            .into_iter()
            .zip(frames)
            .map(|(hand, frame)| palm_error(skeleton, &pose, hand, hand.palm(self.to_palm), frame).map(|e| e.0))
            .try_fold(0.0, |worst: f32, e| e.map(|e| worst.max(e)))?;
        let cleared = torso >= self.margin && forearms >= self.margin && contact <= CONTACT_TOLERANCE;
        Some((pose, ArmClearance { torso, forearms, contact, swings, cleared }))
    }

    /// Crosses the arms in `pose`, whose upper arms (as animated) start the contact frames: each hand lands on the
    /// opposite upper arm *as solved*. Then the elbows swing about their shoulder-to-wrist lines until both forearms
    /// clear the torso and each other by [`margin`](Self::margin): every pair of swings round the circle in coarse steps,
    /// then fine steps around the best, keeping the cleared pose with the smallest turns. If none clears, the pose with
    /// its palms on their arms and the largest smaller gap is kept and `cleared` is `false`. `None` (pose untouched) if
    /// the skeleton lacks the arms.
    ///
    /// The search is a one-off, a few hundred attempts (timings in the `ik_targets` test). Across frames use
    /// [`solve_from`](Self::solve_from) with the last result's swings.
    pub fn solve(&self, skeleton: &Skeleton, pose: &mut Pose) -> Option<ArmClearance> {
        self.solve_from(skeleton, pose, None)
    }

    /// As [`solve`](Self::solve) but warm: starts from the `hint` swings (the previous frame's [`ArmClearance::swings`]).
    /// When they still clear, that is the whole cost, one attempt. Otherwise it climbs through up to three rounds of
    /// neighbouring swings (at most 25 attempts) and never searches the whole circle, so call [`solve`](Self::solve) to
    /// search afresh. With no hint this is [`solve`](Self::solve).
    pub fn solve_from(&self, skeleton: &Skeleton, pose: &mut Pose, hint: Option<[f32; 2]>) -> Option<ArmClearance> {
        let frames = self.frames(skeleton, pose)?;
        // The bend each elbow starts with: the side it is animated on, else hanging down.
        let mut bends = [Vec3::NEG_Y; 2];
        for (i, hand) in [Hand::Left, Hand::Right].into_iter().enumerate() {
            let [upper, lower, _] = hand.ids(skeleton)?;
            let s = bone_world(skeleton, pose, upper).translation;
            let e = bone_world(skeleton, pose, lower).translation;
            let axis = ((frames[i] * inverse(hand.palm(self.to_palm))).translation - s).try_normalize()?;
            let perpendicular = |v: Vec3| (v - axis * v.dot(axis)).try_normalize();
            bends[i] = perpendicular(e - s)
                .or_else(|| perpendicular(Vec3::NEG_Y))
                .unwrap_or_else(|| axis.any_orthonormal_vector());
        }
        let mut best: Option<(Pose, ArmClearance)> = None;
        // Whether the attempt became the best so far.
        let consider = |swings: [f32; 2], best: &mut Option<(Pose, ArmClearance)>| -> Option<bool> {
            let trial = self.attempt(skeleton, pose, bends, swings)?;
            let better = best.as_ref().is_none_or(|(_, b)| trial.1.beats(b));
            if better {
                *best = Some(trial);
            }
            Some(better)
        };
        if let Some(hint) = hint.filter(|h| h.iter().all(|s| s.is_finite())) {
            // Warm: the hint, then a short climb through its neighbours. Never the whole circle, so a pose that cannot
            // clear costs a bounded few attempts every frame, not the cold search.
            consider(hint, &mut best)?;
            for _ in 0..HINT_ROUNDS {
                let (around, cleared) = best.as_ref().map(|(_, b)| (b.swings, b.cleared))?;
                if cleared {
                    break;
                }
                let mut moved = false;
                for (dl, dr) in [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 0), (1, 1)] {
                    let swings = [around[0] + dl as f32 * FINE_STEP, around[1] + dr as f32 * FINE_STEP];
                    moved |= consider(swings, &mut best)?;
                }
                if !moved {
                    break;
                }
            }
            return finish(pose, best);
        }
        let coarse = |k: i32| k as f32 * COARSE_STEP;
        let ring = (TAU / COARSE_STEP).round() as i32;
        for left in 0..ring {
            for right in 0..ring {
                consider([coarse(left - ring / 2), coarse(right - ring / 2)], &mut best)?;
            }
        }
        let around = best.as_ref().map(|(_, b)| b.swings)?;
        for left in -FINE_REACH..=FINE_REACH {
            for right in -FINE_REACH..=FINE_REACH {
                consider([around[0] + left as f32 * FINE_STEP, around[1] + right as f32 * FINE_STEP], &mut best)?;
            }
        }
        finish(pose, best)
    }
}

/// Writes the kept pose back and returns its measurements.
fn finish(pose: &mut Pose, best: Option<(Pose, ArmClearance)>) -> Option<ArmClearance> {
    let (result, clearance) = best?;
    *pose = result;
    Some(clearance)
}
