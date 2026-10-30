//! Physical reactions (master checklist §6, "physical reactions"): an active ragdoll of the spine, the head and the
//! arms that a blow or a fall knocks off the animated pose and that hands the body back to it within the impact's
//! recovery time, exactly.
//!
//! Each joint is a rigid segment on a motor: a damped spring that pulls the joint's offset from the animated pose back
//! to zero. A blow is an angular impulse `r × p` on every joint between the contact and the hips, shared by the
//! inertia above each joint, so a bonk on the head rocks the head most, the neck less and the chest a little. A
//! child segment resists its parent's acceleration (its inertia), so the head lags a shoved chest and whips after it.
//! A fall turns the motors almost off for a while: gravity pulls the upper body over the way it was pushed until the
//! joint limits stop it, the arms dangle and the head droops; then the motors come back and the body pulls itself up.
//!
//! The reaction is an offset layered over everything the body animates, tracking included: the face's channels never
//! change, and the head is back where tracking puts it at [`Impact::recover_s`]. After that the layer is off and
//! the pose is the animated pose bit for bit. No randomness and no clock: the same impact on the same take replays.

use std::f32::consts::TAU;

use omuzaki_anim_api::{BodyPart, HumanoidBone as B, HumanoidPose, Impact, ImpactKind};
use omuzaki_core::glam::{Quat, Vec3};

use super::chain;

/// Longest sub-step, seconds (480 Hz): the stiffest motor stays well inside the integrator's stability bound.
const SUBSTEP_S: f32 = 1.0 / 480.0;
/// Longest frame step honoured, seconds.
const MAX_DT: f32 = 0.1;
/// Momentum of a strength-1 blow, in the units of [`Joint::inertia`].
const BLOW: f32 = 60.0;
/// The motors' strength at the bottom of a fall, `0..=1`.
const LIMP: f32 = 0.05;
/// How hard gravity pulls a tilted body over while the motors are off, rad/s² per radian of tilt.
const GRAVITY: f32 = 60.0;
/// How hard gravity holds a dangling arm to the vertical while the motors are off, rad/s² per radian.
const DANGLE: f32 = 90.0;
/// How much of a blow reaches each joint further down the chain from the one it lands on: the joints between soak
/// it up, so a bonk on the head rocks the head and barely the hips.
const SOAK: f32 = 0.45;
/// How fast a blow coming down on the head nods it forward, rad/s per unit of downward strength: the neck gives
/// under a bonk from above even though the blow points through it.
const NOD: f32 = 6.0;
/// The head's droop while the motors are off, rad/s² (forward pitch).
const DROOP: f32 = 30.0;
/// Shares of the recovery: the fall's limp phase ends, the motors are back, the offset starts to blend out.
const LIMP_UNTIL: f32 = 0.35;
const MOTORS_BACK: f32 = 0.75;
const BLEND_FROM: f32 = 0.7;

/// The joints, parents before children. A fall pivots mostly at the hips (the legs keep pointing down) and bends the
/// spine only a little: a spine bent further than garments are weighted for opens gaps in them.
const JOINTS: [B; 10] = [
    B::Hips,
    B::Spine,
    B::Chest,
    B::UpperChest,
    B::Neck,
    B::Head,
    B::LeftUpperArm,
    B::LeftLowerArm,
    B::RightUpperArm,
    B::RightLowerArm,
];
const N: usize = JOINTS.len();
/// Index of each joint's parent in [`JOINTS`] (`N` for none). The upper arms hang from the upper chest (the shoulder
/// bones in between are not simulated).
const PARENT: [usize; N] = [N, 0, 1, 2, 3, 4, 3, 6, 3, 8];
const HEAD: usize = 5;
const NECK: usize = 4;

/// One simulated joint.
#[derive(Copy, Clone, Debug)]
struct Joint {
    /// Where the joint is in a standing character, metres (character space).
    pivot: Vec3,
    /// The inertia of everything it carries, for sharing a blow.
    inertia: f32,
    /// The motor: natural frequency in Hz and damping ratio.
    hz: f32,
    damping: f32,
    /// How much of the parent's angular acceleration it resists (`1`: keeps its world orientation).
    lag: f32,
    /// The largest offset the joint takes, radians.
    limit: f32,
    /// Whether gravity pulls it over (the torso and the head) or holds it to the vertical (the arms) in a fall.
    arm: bool,
}

const fn joint(pivot: [f32; 3], inertia: f32, hz: f32, damping: f32, lag: f32, limit_deg: f32, arm: bool) -> Joint {
    Joint {
        pivot: Vec3::new(pivot[0], pivot[1], pivot[2]),
        inertia,
        hz,
        damping,
        lag,
        limit: limit_deg * std::f32::consts::PI / 180.0,
        arm,
    }
}

const BODY: [Joint; N] = [
    joint([0.0, 0.95, 0.0], 40.0, 2.6, 0.75, 0.0, 18.0, false),
    joint([0.0, 1.03, 0.0], 22.0, 3.2, 0.62, 0.25, 8.0, false),
    joint([0.0, 1.15, 0.0], 15.0, 3.6, 0.58, 0.3, 7.0, false),
    joint([0.0, 1.27, 0.0], 10.0, 4.0, 0.55, 0.3, 6.0, false),
    joint([0.0, 1.42, 0.0], 2.4, 3.8, 0.48, 0.55, 22.0, false),
    joint([0.0, 1.5, 0.0], 1.0, 4.4, 0.46, 0.65, 22.0, false),
    joint([0.18, 1.42, 0.0], 1.6, 2.4, 0.5, 0.6, 55.0, true),
    joint([0.2, 1.15, 0.0], 0.7, 2.6, 0.5, 0.55, 45.0, true),
    joint([-0.18, 1.42, 0.0], 1.6, 2.4, 0.5, 0.6, 55.0, true),
    joint([-0.2, 1.15, 0.0], 0.7, 2.6, 0.5, 0.55, 45.0, true),
];

/// Where a blow lands in a standing character, the joint it lands on, and how much momentum a strength-1 blow there
/// carries (a shove on the body is heavier than a bonk on the head).
fn contact(part: BodyPart) -> (Vec3, usize, f32) {
    match part {
        BodyPart::Head => (Vec3::new(0.0, 1.66, -0.02), HEAD, 1.0),
        BodyPart::Face => (Vec3::new(0.0, 1.6, 0.1), HEAD, 1.0),
        BodyPart::Chest => (Vec3::new(0.0, 1.32, 0.11), 3, 3.0),
        BodyPart::Stomach => (Vec3::new(0.0, 1.1, 0.1), 1, 3.0),
        BodyPart::LeftShoulder => (Vec3::new(0.17, 1.43, 0.0), 6, 2.5),
        BodyPart::RightShoulder => (Vec3::new(-0.17, 1.43, 0.0), 8, 2.5),
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The ragdoll: each joint's offset from the animated pose (a rotation vector in character space) and its rate.
#[derive(Clone, Debug, Default)]
pub struct Reaction {
    impact: Option<Impact>,
    /// Seconds since the impact.
    t: f32,
    offset: [Vec3; N],
    rate: [Vec3; N],
}

impl Reaction {
    /// Whether a reaction is playing.
    pub fn active(&self) -> bool {
        self.impact.is_some()
    }

    /// Seconds left until the body is back on its animated pose.
    pub fn remaining_s(&self) -> f32 {
        self.impact.map_or(0.0, |i| (i.recover_s - self.t).max(0.0))
    }

    /// The offset of `bone` from its animated pose, as a rotation vector in character space (zero for a bone the
    /// ragdoll does not move).
    pub fn offset(&self, bone: B) -> Vec3 {
        JOINTS.iter().position(|b| *b == bone).map_or(Vec3::ZERO, |i| self.offset[i] * self.blend())
    }

    /// Knocks the body with `impact`. A reaction already playing carries on from where it is.
    pub fn hit(&mut self, impact: &Impact) {
        let impact = impact.clamped();
        // Bake the blend-out so far into the state, so the pose does not jump when the new impact restarts it.
        let keep = self.blend();
        for i in 0..N {
            self.offset[i] *= keep;
            self.rate[i] *= keep;
        }
        let (at, struck, weight) = contact(impact.at);
        let push = Vec3::from_array(impact.travel) * (BLOW * weight * impact.strength);
        // The struck joint and every joint below it take the blow, each about its own pivot and shared by what it
        // carries, less of it the further down the chain.
        let mut j = struck;
        let mut reach = 1.0;
        while j < N {
            let lever = at - BODY[j].pivot;
            self.rate[j] += lever.cross(push) * (reach / BODY[j].inertia);
            reach *= if j == struck { 1.0 } else { SOAK };
            j = PARENT[j];
        }
        if impact.at == BodyPart::Head {
            let down = (-impact.travel[1]).max(0.0) * impact.strength * NOD;
            self.rate[HEAD].x += down;
            self.rate[NECK].x += down * 0.6;
        }
        self.impact = Some(impact);
        self.t = 0.0;
    }

    /// Stops the reaction at once.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// The motors' strength `t` seconds into `impact`.
    fn motors(impact: &Impact, t: f32) -> f32 {
        match impact.kind {
            ImpactKind::Hit => 1.0,
            ImpactKind::Fall => {
                let r = impact.recover_s;
                let down = 1.0 + (LIMP - 1.0) * smoothstep(0.0, 0.06, t);
                down + (1.0 - down) * smoothstep(LIMP_UNTIL * r, MOTORS_BACK * r, t)
            }
        }
    }

    /// How much of the offset shows, `1` until the blend-out and `0` at the recovery time.
    fn blend(&self) -> f32 {
        self.impact.map_or(0.0, |i| 1.0 - smoothstep(BLEND_FROM * i.recover_s, i.recover_s, self.t))
    }

    /// Advances the ragdoll `dt` seconds.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // small counts
    pub fn step(&mut self, dt: f32) {
        let Some(impact) = self.impact else {
            return;
        };
        let dt = if dt.is_finite() { dt.clamp(0.0, MAX_DT) } else { 0.0 };
        let n = (dt / SUBSTEP_S - 1e-3).ceil().max(0.0) as u32;
        let h = if n == 0 { 0.0 } else { dt / n as f32 };
        for _ in 0..n {
            self.t += h;
            let motors = Self::motors(&impact, self.t);
            let slack = 1.0 - motors;
            // World tilt and acceleration so far down the chain, per joint.
            let mut tilt = [Vec3::ZERO; N];
            let mut accel = [Vec3::ZERO; N];
            for i in 0..N {
                let joint = &BODY[i];
                let parent = PARENT[i];
                let (parent_tilt, parent_accel) = if parent < N { (tilt[parent], accel[parent]) } else { (Vec3::ZERO, Vec3::ZERO) };
                let omega = TAU * joint.hz;
                let mut a = -(omega * omega * self.offset[i] + 2.0 * joint.damping * omega * self.rate[i]) * motors;
                a -= joint.lag * parent_accel;
                if slack > 0.0 {
                    let world = parent_tilt + self.offset[i];
                    let level = Vec3::new(world.x, 0.0, world.z);
                    a += if joint.arm { -DANGLE * level } else { GRAVITY * level } * slack;
                    if i == HEAD || i == NECK {
                        a.x += DROOP * slack;
                    }
                }
                self.rate[i] += a * h;
                self.offset[i] += self.rate[i] * h;
                // The joint's limit: a hard stop that takes the outward speed.
                let size = self.offset[i].length();
                if size > joint.limit {
                    let out = self.offset[i] / size;
                    self.offset[i] = out * joint.limit;
                    let outward = self.rate[i].dot(out);
                    if outward > 0.0 {
                        self.rate[i] -= out * outward;
                    }
                }
                tilt[i] = parent_tilt + self.offset[i];
                accel[i] = parent_accel + a;
            }
            if !(self.offset.iter().chain(&self.rate)).all(|v| v.is_finite()) {
                self.clear();
                return;
            }
        }
        if self.t >= impact.recover_s - 1e-4 {
            self.clear();
        }
    }

    /// Lays the reaction over `out`: each joint turned by its offset in character space, the legs kept pointing where
    /// they did so the feet stay put. Does nothing when no reaction plays.
    pub fn apply(&self, out: &mut HumanoidPose) {
        let blend = self.blend();
        if blend <= 0.0 {
            return;
        }
        for (i, bone) in JOINTS.into_iter().enumerate() {
            let turn = Quat::from_scaled_axis(self.offset[i] * blend);
            if turn == Quat::IDENTITY {
                continue;
            }
            // A turn in character space, in the parent's frame as the pose stands now.
            let parent = bone.parent().map_or(Quat::IDENTITY, |p| chain(out, p));
            let local = (parent.inverse() * turn * parent).normalize();
            out.set(bone, (local * out.get(bone)).normalize());
            if bone == B::Hips {
                let back = local.inverse();
                for leg in [B::LeftUpperLeg, B::RightUpperLeg] {
                    out.set(leg, (back * out.get(leg)).normalize());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(impact: Impact, joint: usize, frames: usize) -> (f32, Vec3) {
        let mut r = Reaction::default();
        r.hit(&impact);
        let mut best = (0.0_f32, Vec3::ZERO);
        for _ in 0..frames {
            r.step(1.0 / 60.0);
            let o = r.offset[joint] * r.blend();
            if o.length() > best.0 {
                best = (o.length(), o);
            }
        }
        (best.0.to_degrees(), best.1)
    }

    #[test]
    fn a_bonk_from_in_front_tips_the_head_back_by_about_fifteen_degrees() {
        let (deg, dir) = peak(Impact::hit(BodyPart::Face, [0.0, 0.0, -1.0], 1.0), HEAD, 72);
        assert!((8.0..=26.0).contains(&deg), "{deg}");
        assert!(dir.x < 0.0, "back is negative pitch: {dir}");
        let (chest, _) = peak(Impact::hit(BodyPart::Face, [0.0, 0.0, -1.0], 1.0), 3, 72);
        assert!(chest < deg * 0.5, "the chest moves less than the head: {chest} vs {deg}");
    }

    #[test]
    fn a_shove_on_the_left_shoulder_turns_the_chest_and_swings_that_arm() {
        let shove = Impact::hit(BodyPart::LeftShoulder, [0.0, 0.0, -1.0], 1.5);
        let (_, chest) = peak(shove, 3, 72);
        // Pushed back on the left: the chest turns toward the character's left (positive Y).
        assert!(chest.y > 0.0, "{chest}");
        let (arm, _) = peak(shove, 6, 72);
        let (other, _) = peak(shove, 8, 72);
        assert!(arm > other, "{arm} vs {other}");
    }

    #[test]
    fn a_fall_slumps_the_way_it_was_pushed_and_comes_back_exactly_on_time() {
        let fall = Impact::fall(BodyPart::Chest, [0.0, 0.0, -1.0], 1.0);
        let mut r = Reaction::default();
        r.hit(&fall);
        let mut lowest = 0.0_f32;
        let mut frames = 0;
        while r.active() {
            r.step(1.0 / 60.0);
            frames += 1;
            let back: f32 = (0..=3).map(|i| r.offset[i].x * r.blend()).sum();
            lowest = lowest.min(back);
        }
        assert!(lowest.to_degrees() < -25.0, "the torso goes over backwards: {}", lowest.to_degrees());
        assert_eq!(frames, 180, "back on the pose at 3 s exactly");
        assert!(r.offset.iter().all(|o| *o == Vec3::ZERO));
    }

    #[test]
    fn nothing_explodes_under_the_hardest_blows_and_a_stalled_frame() {
        let mut r = Reaction::default();
        for part in [BodyPart::Head, BodyPart::Stomach, BodyPart::RightShoulder] {
            r.hit(&Impact::fall(part, [1.0, -1.0, 1.0], 3.0));
            r.step(0.1);
            r.step(1e9);
            for (i, o) in r.offset.iter().enumerate() {
                assert!(o.is_finite() && o.length() <= BODY[i].limit + 1e-4, "{i}: {o}");
            }
        }
    }
}
