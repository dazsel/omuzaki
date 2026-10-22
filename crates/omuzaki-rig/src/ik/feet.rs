//! Foot planting (RG-05): feet that stay put on the ground while the body moves over them.
//!
//! [`FootPlanting`] is an [`IkSolver`] for the two feet of a humanoid. It runs on the animated pose, before spring
//! bones, and works in four steps per frame:
//!
//! 1. **Detect.** A foot is *planted* when its ankle is within [`PLANT_HEIGHT`] of the standing height and slower than
//!    [`PLANT_SPEED`]; it is released when it rises above [`RELEASE_HEIGHT`] or moves faster than [`RELEASE_SPEED`].
//!    The release limits are looser than the plant limits (hysteresis), so a foot hovering at a threshold does not
//!    chatter. A clip's explicit contact flags ([`FootPlanting::set_contacts`]) replace the detection. Height and speed
//!    are measured in the world, so the root's placement ([`FootPlanting::set_root`]) matters: a walking character's
//!    stance foot is still in the world though it slides back in root space.
//! 2. **Lock.** On the frame a foot plants, its ankle's world position is stored with its height snapped to the
//!    ground. While planted the foot's target is that point, so the foot cannot slide. The target eases from the
//!    animated ankle to the lock over [`Config::blend_time`] when a foot plants, and back when it is released.
//! 3. **Hips.** If a planted foot is out of reach (beyond the leg's exactly reachable range, see
//!    [`SOFT_REACH`]), the hips are lowered along world down by just enough to reach it:
//!    at once on the way down, then back up at no more than [`Config::hip_rise_speed`].
//! 4. **Solve.** The legs go to their targets with [`TwoBoneIk`] (knee toward where the animation put it), then each
//!    planted foot is turned the shortest way so its sole lies flat on the ground. While a foot is released the sole
//!    is pitched heel-up by up to [`TOE_OFF_LIFT`], so the heel leaves the ground before the toe.
//!
//! The solver reads the foot goals' `weight` only (planting strength, `0..=1`); their `target` and `pole` are not
//! used, because the targets come from the pose and the locks. With both foot goals absent or weightless the pose is
//! left bit for bit as given and the state is cleared. The solve is a pure function of the inputs and the previous
//! frames, with no wall clock, and allocates nothing.

use omuzaki_anim_api::{BoneId, HumanoidBone, IkGoals, IkSolver, Limb, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};

use super::two_bone::{SOFT_REACH, TwoBoneIk};

/// Height above standing height (metres) under which a free foot may plant.
pub const PLANT_HEIGHT: f32 = 0.015;
/// Height above standing height (metres) over which a planted foot is released. Above [`PLANT_HEIGHT`] by far more than
/// the jitter of a tracked or animated foot, which is what stops chatter.
pub const RELEASE_HEIGHT: f32 = 0.025;
/// World speed (m/s) under which a free foot may plant.
pub const PLANT_SPEED: f32 = 0.3;
/// World speed (m/s) over which a planted foot is released.
pub const RELEASE_SPEED: f32 = 0.6;
/// Peak heel lift (radians) while a foot is released: the toe-off.
pub const TOE_OFF_LIFT: f32 = 0.35;
/// The most the hips are lowered (metres), whatever the feet ask.
pub const MAX_HIP_DROP: f32 = 0.25;
/// Hip drop (metres) over which a free leg is fully held at its animated place.
const HOLD_RAMP: f32 = 0.005;
/// The ankle's height above the sole (metres) when the skeleton has no toes to measure it from.
const FALLBACK_ANKLE_HEIGHT: f32 = 0.08;

/// Tuning of [`FootPlanting`]. [`Config::default`] suits a 60 Hz character.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Config {
    /// Height of the ground plane in the world (+Y up), metres.
    pub ground_height: f32,
    /// Seconds per solve: velocities and fade times are measured against it.
    pub frame_dt: f32,
    /// Seconds a foot takes to blend between the animated ankle and the lock.
    pub blend_time: f32,
    /// The fastest the hips rise back after a drop, metres per second.
    pub hip_rise_speed: f32,
    /// How much of the turn that lays the sole flat is applied, `0..=1`.
    pub flat: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self { ground_height: 0.0, frame_dt: 1.0 / 60.0, blend_time: 0.08, hip_rise_speed: 0.4, flat: 1.0 }
    }
}

/// What the planter knows about one foot, for tests and debugging.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct FootStatus {
    /// Whether the foot is planted now.
    pub planted: bool,
    /// How far the target has blended onto the lock, `0..=1` (1 when fully locked).
    pub blend: f32,
    /// The ankle's locked world position (valid once the foot has planted).
    pub lock: Vec3,
}

/// The bones and rest facts of one leg.
#[derive(Copy, Clone)]
struct Leg {
    upper: BoneId,
    lower: BoneId,
    foot: BoneId,
    /// The sole's normal in the foot bone's frame (rest: the world up).
    sole: Vec3,
    /// The toe direction in the foot bone's frame.
    forward: Vec3,
}

#[derive(Copy, Clone, Default)]
struct FootState {
    planted: bool,
    seen: bool,
    prev: Vec3,
    lock: Vec3,
    blend: f32,
}

/// Plants the feet of a humanoid; see the module docs. One instance per character.
pub struct FootPlanting {
    config: Config,
    root: Transform,
    contacts: [Option<bool>; 2],
    bone_count: usize,
    hips: BoneId,
    legs: [Leg; 2],
    ankle_height: f32,
    feet: [FootState; 2],
    hip_drop: f32,
    ik: TwoBoneIk,
}

/// `bone`'s transform in the skeleton's root space, from the local transforms in `pose`.
fn world_of(skeleton: &Skeleton, pose: &Pose, bone: BoneId) -> Transform {
    let mut world = pose.local[bone.index()];
    let mut at = skeleton.bones()[bone.index()].parent;
    while let Some(parent) = at {
        world = pose.local[parent.index()] * world;
        at = skeleton.bones()[parent.index()].parent;
    }
    world
}

/// The same, for the rest pose.
fn rest_world_of(skeleton: &Skeleton, bone: BoneId) -> Transform {
    let mut world = skeleton.bones()[bone.index()].rest;
    let mut at = skeleton.bones()[bone.index()].parent;
    while let Some(parent) = at {
        world = skeleton.bones()[parent.index()].rest * world;
        at = skeleton.bones()[parent.index()].parent;
    }
    world
}

/// The rotation of `bone`'s parent in root space (identity for a root bone).
fn parent_rotation(skeleton: &Skeleton, pose: &Pose, bone: BoneId) -> Quat {
    skeleton.bones()[bone.index()].parent.map_or(Quat::IDENTITY, |p| world_of(skeleton, pose, p).rotation)
}

fn smoothstep(x: f32) -> f32 {
    x * x * (3.0 - 2.0 * x)
}

impl FootPlanting {
    /// A planter for `skeleton`, or `None` if it is not a humanoid with both legs (hips, upper and lower leg, foot).
    pub fn new(skeleton: &Skeleton, config: Config) -> Option<Self> {
        use HumanoidBone as B;
        let map = skeleton.humanoid_map()?;
        let leg = |upper, lower, foot, toes| {
            let (foot, rest) = (map.get(foot)?, map.get(foot).map(|f| rest_world_of(skeleton, f))?);
            let toes_at = map.get(toes).map(|t| rest_world_of(skeleton, t).translation);
            let forward = toes_at.and_then(|t| (t - rest.translation).try_normalize()).unwrap_or(Vec3::Z);
            Some((
                Leg {
                    upper: map.get(upper)?,
                    lower: map.get(lower)?,
                    foot,
                    sole: rest.rotation.inverse() * Vec3::Y,
                    forward: rest.rotation.inverse() * forward,
                },
                toes_at.map(|t| rest.translation.y - t.y).filter(|h| *h > 1e-3),
            ))
        };
        let (left, left_height) = leg(B::LeftUpperLeg, B::LeftLowerLeg, B::LeftFoot, B::LeftToes)?;
        let (right, right_height) = leg(B::RightUpperLeg, B::RightLowerLeg, B::RightFoot, B::RightToes)?;
        Some(Self {
            config,
            root: Transform::IDENTITY,
            contacts: [None; 2],
            bone_count: skeleton.len(),
            hips: map.hips(),
            legs: [left, right],
            ankle_height: left_height.or(right_height).unwrap_or(FALLBACK_ANKLE_HEIGHT),
            feet: [FootState::default(); 2],
            hip_drop: 0.0,
            ik: TwoBoneIk::new(),
        })
    }

    /// Where the skeleton's root is placed in the world this frame. Default: the identity.
    pub fn set_root(&mut self, root: Transform) {
        self.root = root;
    }

    /// Explicit contact flags for `[left, right]` from a clip: `Some(true)` plants, `Some(false)` releases, `None`
    /// leaves that foot to detection. They stay in force until set again.
    pub fn set_contacts(&mut self, contacts: [Option<bool>; 2]) {
        self.contacts = contacts;
    }

    /// The state of the left (`0`) or right (`1`) foot.
    pub fn status(&self, side: usize) -> FootStatus {
        let foot = self.feet[side.min(1)];
        FootStatus { planted: foot.planted, blend: foot.blend, lock: foot.lock }
    }

    /// How far the hips are lowered now, metres.
    pub fn hip_drop(&self) -> f32 {
        self.hip_drop
    }

    /// Updates one foot's plant state from its animated world ankle position `at`; returns its blend weight `0..=1`.
    fn track(&mut self, side: usize, at: Vec3) -> f32 {
        let dt = self.config.frame_dt.max(1e-4);
        let standing = self.config.ground_height + self.ankle_height;
        let foot = &mut self.feet[side];
        let speed = if foot.seen { (at - foot.prev).length() / dt } else { 0.0 };
        (foot.prev, foot.seen) = (at, true);
        let height = at.y - standing;
        let planted = self.contacts[side].unwrap_or(if foot.planted {
            height <= RELEASE_HEIGHT && speed <= RELEASE_SPEED
        } else {
            height < PLANT_HEIGHT && speed < PLANT_SPEED
        });
        if planted && !foot.planted {
            foot.lock = Vec3::new(at.x, standing, at.z);
        }
        foot.planted = planted;
        let step = dt / self.config.blend_time.max(dt);
        foot.blend = if planted { (foot.blend + step).min(1.0) } else { (foot.blend - step).max(0.0) };
        smoothstep(foot.blend)
    }
}

impl IkSolver for FootPlanting {
    fn solve(&mut self, skeleton: &Skeleton, goals: &IkGoals, pose: &mut Pose) {
        let goals = goals.finite_only();
        let weights = [goals.left_foot, goals.right_foot].map(|g| g.map_or(0.0, |l| l.weight.clamp(0.0, 1.0)));
        if weights == [0.0, 0.0] || skeleton.len() != self.bone_count {
            self.reset();
            return;
        }
        let (root, to_root) = (self.root, self.root.rotation.inverse());
        let up = to_root * Vec3::Y;
        let down = -up;

        // 1-2. Detect and lock; where each foot is animated and where it is locked, in root space.
        let mut animated = [Vec3::ZERO; 2];
        let mut lock = [Vec3::ZERO; 2];
        let mut mix = [0.0_f32; 2];
        let mut hip_drop_wanted = 0.0_f32;
        for side in 0..2 {
            let leg = self.legs[side];
            let ankle = world_of(skeleton, pose, leg.foot).translation;
            mix[side] = self.track(side, root.translation + root.rotation * ankle) * weights[side];
            (animated[side], lock[side]) = (ankle, to_root * (self.feet[side].lock - root.translation));

            // 3. How far the hips must drop for this leg to reach its lock, by its strength: a faint goal asks for a
            // faint correction, so the pose is continuous from weight zero.
            if mix[side] > 0.0 {
                let hip = world_of(skeleton, pose, leg.upper).translation;
                let knee = world_of(skeleton, pose, leg.lower).translation;
                let reach = (hip.distance(knee) + knee.distance(ankle)) * (1.0 - SOFT_REACH) * 0.999;
                let (r, vertical) = (lock[side] - hip, (lock[side] - hip).dot(down));
                let slack = vertical * vertical - r.length_squared() + reach * reach;
                if r.length() > reach {
                    let drop = if slack >= 0.0 { vertical - slack.sqrt() } else { vertical };
                    hip_drop_wanted = hip_drop_wanted.max(drop.clamp(0.0, MAX_HIP_DROP) * mix[side]);
                }
            }
        }
        let rise = self.config.hip_rise_speed * self.config.frame_dt;
        self.hip_drop = hip_drop_wanted.max(self.hip_drop - rise).max(0.0);
        let drop = self.hip_drop * weights[0].max(weights[1]);
        if drop > 0.0 {
            let shift = parent_rotation(skeleton, pose, self.hips).inverse() * (down * drop);
            pose.local[self.hips.index()].translation += shift;
        } else if mix == [0.0, 0.0] {
            return;
        }

        // 4. Legs, then soles. A leg nobody asked for stays exactly as animated. A planted leg goes to its place
        // between the animated ankle and the lock (`mix`), with the strength of its goal; while the hips are down every
        // leg is also held at full strength, fading in over `HOLD_RAMP` of drop, so a free foot keeps its animated
        // world place and a faint goal on one foot cannot jump the leg when the other foot holds the hips down.
        let hold = (drop / HOLD_RAMP).min(1.0);
        let leg_goal = |side: usize| {
            let weight = mix[side].max(hold);
            let target = animated[side].lerp(lock[side], mix[side]);
            let knee = world_of(skeleton, pose, self.legs[side].lower).translation;
            (weight > 0.0).then_some(Limb { target, pole: knee, weight })
        };
        let leg_goals = IkGoals { left_foot: leg_goal(0), right_foot: leg_goal(1), ..IkGoals::NONE };
        self.ik.solve(skeleton, &leg_goals, pose);
        for (side, &mix) in mix.iter().enumerate() {
            if mix > 0.0 {
                self.lay_flat(skeleton, pose, side, mix, weights[side], up);
            }
        }
    }

    fn reset(&mut self) {
        self.feet = [FootState::default(); 2];
        self.hip_drop = 0.0;
    }
}

impl FootPlanting {
    /// Turns the foot of `side` so its sole lies on the ground (normal `up`), by `mix` of the way, with the heel
    /// lifted (scaled by the goal's `weight`) while the foot is being released.
    fn lay_flat(&self, skeleton: &Skeleton, pose: &mut Pose, side: usize, mix: f32, weight: f32, up: Vec3) {
        let leg = self.legs[side];
        let world = world_of(skeleton, pose, leg.foot).rotation;
        let flat = Quat::from_rotation_arc(world * leg.sole, up);
        let mut turned = Quat::IDENTITY.slerp(flat, mix * self.config.flat.clamp(0.0, 1.0)) * world;
        if !self.feet[side].planted {
            // Heel up about the foot's lateral axis (up × forward): zero at both ends of the release, most midway.
            let ahead = turned * leg.forward;
            if let Some(forward) = (ahead - up * ahead.dot(up)).try_normalize() {
                let blend = self.feet[side].blend;
                let lift = TOE_OFF_LIFT * 4.0 * blend * (1.0 - blend) * weight;
                turned = Quat::from_axis_angle(up.cross(forward), lift) * turned;
            }
        }
        let parent = parent_rotation(skeleton, pose, leg.foot);
        pose.local[leg.foot.index()].rotation = (parent.inverse() * turned).normalize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothstep_has_flat_ends() {
        assert!((smoothstep(0.0)).abs() < f32::EPSILON);
        assert!((smoothstep(1.0) - 1.0).abs() < f32::EPSILON);
        assert!((smoothstep(0.5) - 0.5).abs() < f32::EPSILON);
    }

    // The hysteresis only works if release is looser than plant.
    const _: () = assert!(RELEASE_HEIGHT > PLANT_HEIGHT && RELEASE_SPEED > PLANT_SPEED);
}
