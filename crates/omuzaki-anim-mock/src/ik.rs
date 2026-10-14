//! A stand-in IK stage: a two-bone solver for arms and legs and a head-and-eyes look-at, on a humanoid skeleton. It is
//! the reference for what [`IkSolver`] promises (rotations only, exact at weight zero, bone lengths kept) and what
//! `omuzaki-rig`'s real solver must match on the fixtures; it is not tuned (no joint limits, no smoothing).

use omuzaki_anim_api::{BoneId, HumanoidBone, IkGoals, IkSolver, Limb, LookAt, Pose, Skeleton};
use omuzaki_core::glam::{Quat, Vec3};

/// Share of the head turn the neck takes (the animator's 35/65 split).
const NECK_SHARE: f32 = 0.35;
/// Segments shorter than this are degenerate and left alone, metres.
const MIN_LEN: f32 = 1e-5;
/// Forward axis of a head or eye bone (the character faces +Z).
const FORWARD: Vec3 = Vec3::Z;

/// Below this the sine of the angle between two bend directions is noise, so the turn is taken as antipodal.
const ANTIPODAL_SIN: f32 = 1e-6;

/// The signed angle that turns bend direction `from` into `to` about `axis` (both unit and perpendicular to it),
/// right-handed. The bend plane is weighted by scaling this angle, so it sweeps continuously with the weight instead
/// of jumping when the straight blend of the two directions passes through zero.
///
/// **Antipodal rule:** when `to` is opposite `from` the turn has no preferred side, so it is always `+PI`
/// (counter-clockwise about `axis`), independent of rounding noise.
fn bend_angle(axis: Vec3, from: Vec3, to: Vec3) -> f32 {
    let (sin, cos) = (axis.dot(from.cross(to)), from.dot(to));
    if sin.abs() < ANTIPODAL_SIN && cos < 0.0 { std::f32::consts::PI } else { sin.atan2(cos) }
}

/// See the module docs. Holds one scratch pose for forward kinematics, so [`IkSolver::solve`] never allocates.
#[derive(Clone, Debug)]
pub struct MockIk {
    world: Box<Pose>,
}

impl Default for MockIk {
    fn default() -> Self {
        Self { world: Box::new(Pose::IDENTITY) }
    }
}

impl MockIk {
    /// A solver with no state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `bone`'s local rotation so its world rotation becomes `world_rot`, given the (current) world pose.
    fn set_world_rotation(&self, skeleton: &Skeleton, pose: &mut Pose, bone: BoneId, world_rot: Quat) {
        let parent =
            skeleton.bones()[bone.index()].parent.map_or(Quat::IDENTITY, |p| self.world.local[p.index()].rotation);
        pose.local[bone.index()].rotation = (parent.inverse() * world_rot).normalize();
    }

    /// Two-bone IK for `[root, mid, tip]` (shoulder-elbow-wrist or hip-knee-ankle).
    fn solve_limb(&mut self, skeleton: &Skeleton, pose: &mut Pose, chain: [BoneId; 3], limb: Limb) {
        let weight = limb.weight.clamp(0.0, 1.0);
        let [root, mid, tip] = chain.map(BoneId::index);
        let (a, b, c) =
            (self.world.local[root].translation, self.world.local[mid].translation, self.world.local[tip].translation);
        let (l1, l2) = (a.distance(b), b.distance(c));
        if weight <= 0.0 || l1 < MIN_LEN || l2 < MIN_LEN {
            return;
        }
        // Aim at the weighted target, so the arc from the animated tip to the goal is what the weight scales.
        let goal = c.lerp(limb.target, weight);
        let to_goal = goal - a;
        let dist = to_goal.length().clamp((l1 - l2).abs() + MIN_LEN, l1 + l2 - MIN_LEN);
        let axis = to_goal.try_normalize().unwrap_or((c - a).normalize_or_zero());
        // Law of cosines: distance along the axis to the elbow's foot point, and its height off the axis.
        let along = (l1 * l1 - l2 * l2 + dist * dist) / (2.0 * dist);
        let height = (l1 * l1 - along * along).max(0.0).sqrt();
        let bend = |hint: Vec3| (hint - axis * hint.dot(axis)).try_normalize();
        let side = match (bend(b - a), bend(limb.pole - a)) {
            (Some(from), Some(to)) => Quat::from_axis_angle(axis, bend_angle(axis, from, to) * weight) * from,
            (from, to) => to.or(from).unwrap_or_else(|| axis.any_orthonormal_vector()),
        };
        let new_mid = a + axis * along + side * height;

        let upper = Quat::from_rotation_arc((b - a).normalize(), (new_mid - a).normalize());
        let upper_world = (upper * self.world.local[root].rotation).normalize();
        self.set_world_rotation(skeleton, pose, chain[0], upper_world);
        // The lower bone rides on the upper one; then turn it so the tip lands on the goal.
        let carried = (upper * self.world.local[mid].rotation).normalize();
        let lower = Quat::from_rotation_arc(upper * (c - b).normalize(), (a + axis * dist - new_mid).normalize());
        let parent = upper_world;
        pose.local[mid].rotation = (parent.inverse() * (lower * carried)).normalize();
    }

    /// Neck, head and eyes toward the target.
    fn solve_look(&mut self, skeleton: &Skeleton, pose: &mut Pose, look: LookAt) {
        let (Some(map), weight) = (skeleton.humanoid_map(), look.weight.clamp(0.0, 1.0)) else { return };
        let Some(head) = map.get(HumanoidBone::Head) else { return };
        // The neck is optional in VRM; without one the head takes the whole turn.
        let neck = map.get(HumanoidBone::Neck);
        if weight <= 0.0 {
            return;
        }
        let eye_share = look.eye_share.clamp(0.0, 1.0);
        let head_rot = self.world.local[head.index()].rotation;
        let to_target = (look.target - self.world.local[head.index()].translation).try_normalize();
        if let Some(dir) = to_target {
            let full = Quat::from_rotation_arc(head_rot * FORWARD, dir);
            let turn = Quat::IDENTITY.slerp(full, weight * (1.0 - eye_share));
            if let Some(neck) = neck {
                let neck_turn = Quat::IDENTITY.slerp(turn, NECK_SHARE);
                let turned = (neck_turn * self.world.local[neck.index()].rotation).normalize();
                self.set_world_rotation(skeleton, pose, neck, turned);
                // Re-run forward kinematics so the head sits on the turned neck, then give it the rest of the turn.
                skeleton.world_pose(pose, &mut self.world);
            }
            let head_world = (turn * head_rot).normalize();
            self.set_world_rotation(skeleton, pose, head, head_world);
            skeleton.world_pose(pose, &mut self.world);
        }
        if eye_share <= 0.0 {
            return; // the eyes take no part of the turn: their rotations stay exactly as animated
        }
        for eye in [HumanoidBone::LeftEye, HumanoidBone::RightEye].into_iter().filter_map(|e| map.get(e)) {
            let at = self.world.local[eye.index()];
            let Some(dir) = (look.target - at.translation).try_normalize() else { continue };
            let aim = Quat::IDENTITY.slerp(Quat::from_rotation_arc(at.rotation * FORWARD, dir), weight);
            self.set_world_rotation(skeleton, pose, eye, (aim * at.rotation).normalize());
        }
    }
}

impl IkSolver for MockIk {
    fn solve(&mut self, skeleton: &Skeleton, goals: &IkGoals, pose: &mut Pose) {
        use HumanoidBone as B;
        let Some(map) = skeleton.humanoid_map() else { return };
        let goals = &goals.finite_only();
        if goals.is_idle() {
            return;
        }
        skeleton.world_pose(pose, &mut self.world);
        let limbs = [
            (goals.left_hand, [B::LeftUpperArm, B::LeftLowerArm, B::LeftHand]),
            (goals.right_hand, [B::RightUpperArm, B::RightLowerArm, B::RightHand]),
            (goals.left_foot, [B::LeftUpperLeg, B::LeftLowerLeg, B::LeftFoot]),
            (goals.right_foot, [B::RightUpperLeg, B::RightLowerLeg, B::RightFoot]),
        ];
        for (goal, bones) in limbs {
            let ids = bones.map(|b| map.get(b));
            if let (Some(limb), [Some(r), Some(m), Some(t)]) = (goal, ids) {
                self.solve_limb(skeleton, pose, [r, m, t], limb);
            }
        }
        if let Some(look) = goals.look_at {
            self.solve_look(skeleton, pose, look);
        }
    }

    fn reset(&mut self) {}
}

#[cfg(test)]
#[allow(clippy::float_cmp, clippy::cast_precision_loss, clippy::cast_possible_truncation)] // exact copies; small loop counters
mod tests {
    use super::*;
    use crate::fixtures::{humanoid_skeleton, quadruped_rig};
    use omuzaki_anim_api::Transform;

    fn world(skeleton: &Skeleton, pose: &Pose) -> Pose {
        let mut out = Pose::IDENTITY;
        skeleton.world_pose(pose, &mut out);
        out
    }

    fn bone(skeleton: &Skeleton, b: HumanoidBone) -> BoneId {
        skeleton.humanoid_bone(b).expect("fixture humanoid has every bone")
    }

    #[allow(clippy::unnecessary_wraps)] // the goal fields are `Option<Limb>`, so every call site wants the `Some`
    fn reach(target: Vec3, pole: Vec3, weight: f32) -> Option<Limb> {
        Some(Limb { target, pole, weight })
    }

    #[test]
    fn a_reachable_hand_lands_on_its_target_and_the_arm_keeps_its_lengths() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let rest = skeleton.rest_pose();
        let before = world(&skeleton, &rest);
        let (arm, elbow, hand) = (
            bone(&skeleton, HumanoidBone::RightUpperArm),
            bone(&skeleton, HumanoidBone::RightLowerArm),
            bone(&skeleton, HumanoidBone::RightHand),
        );
        let shoulder = before.local[arm.index()].translation;
        let target = shoulder + Vec3::new(-0.2, -0.25, 0.25);
        let hint = shoulder + Vec3::new(-0.1, -0.5, -0.3);
        let goals = IkGoals { right_hand: reach(target, hint, 1.0), ..IkGoals::NONE };
        let mut pose = rest.clone();
        MockIk::new().solve(&skeleton, &goals, &mut pose);
        let after = world(&skeleton, &pose);
        assert!(after.local[hand.index()].translation.distance(target) < 1e-4);
        for (p, c) in [(arm, elbow), (elbow, hand)] {
            let len = |w: &Pose| w.local[p.index()].translation.distance(w.local[c.index()].translation);
            assert!((len(&before) - len(&after)).abs() < 1e-5);
        }
        // The elbow bent to the pole's side of the shoulder-to-target line, not the other way.
        let line = (target - shoulder).normalize();
        let off_line = |p: Vec3| (p - shoulder) - line * (p - shoulder).dot(line);
        assert!(off_line(after.local[elbow.index()].translation).dot(off_line(hint)) > 0.0);
    }

    #[test]
    fn an_unreachable_foot_straightens_the_leg_toward_the_target() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let rest = skeleton.rest_pose();
        let before = world(&skeleton, &rest);
        let (hip, foot) = (bone(&skeleton, HumanoidBone::LeftUpperLeg), bone(&skeleton, HumanoidBone::LeftFoot));
        let root = before.local[hip.index()].translation;
        let target = root + Vec3::new(0.0, -0.1, 3.0);
        let goals = IkGoals { left_foot: reach(target, root + Vec3::Z, 1.0), ..IkGoals::NONE };
        let mut pose = rest.clone();
        MockIk::new().solve(&skeleton, &goals, &mut pose);
        let tip = world(&skeleton, &pose).local[foot.index()].translation;
        let leg = before.local[hip.index()].translation.distance(before.local[foot.index()].translation);
        assert!(((tip - root).length() - leg).abs() < 1e-3, "leg is straight and at full length");
        assert!((tip - root).normalize().dot((target - root).normalize()) > 0.999);
    }

    #[test]
    fn weight_zero_is_bit_identical_and_half_is_between() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let rest = skeleton.rest_pose();
        let hand = bone(&skeleton, HumanoidBone::LeftHand);
        let start = world(&skeleton, &rest).local[hand.index()].translation;
        let target = start + Vec3::new(-0.2, -0.2, 0.2);
        let hint = start + Vec3::new(0.0, -0.3, -0.5);
        let at = |w: f32| {
            let mut pose = rest.clone();
            let goals = IkGoals { left_hand: reach(target, hint, w), look_at: None, ..IkGoals::NONE };
            MockIk::new().solve(&skeleton, &goals, &mut pose);
            pose
        };
        assert_eq!(at(0.0), rest);
        let half = world(&skeleton, &at(0.5)).local[hand.index()].translation;
        assert!(half.distance(start.lerp(target, 0.5)) < 1e-4);
    }

    #[test]
    fn only_the_chain_rotations_change() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let rest = skeleton.rest_pose();
        let (arm, elbow) = (bone(&skeleton, HumanoidBone::LeftUpperArm), bone(&skeleton, HumanoidBone::LeftLowerArm));
        let start = world(&skeleton, &rest).local[arm.index()].translation;
        let goals =
            IkGoals { left_hand: reach(start + Vec3::new(0.1, -0.3, 0.2), start - Vec3::Z, 1.0), ..IkGoals::NONE };
        let mut pose = rest.clone();
        MockIk::new().solve(&skeleton, &goals, &mut pose);
        for i in 0..skeleton.len() {
            assert_eq!(pose.local[i].translation, rest.local[i].translation, "translation of bone {i}");
            let chain = i == arm.index() || i == elbow.index();
            assert_eq!(pose.local[i].rotation != rest.local[i].rotation, chain, "rotation of bone {i}");
        }
    }

    #[test]
    fn the_head_turns_to_face_the_target_when_the_eyes_take_no_share() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let rest = skeleton.rest_pose();
        let head = bone(&skeleton, HumanoidBone::Head);
        let origin = world(&skeleton, &rest).local[head.index()].translation;
        let target = origin + Vec3::new(1.0, 0.3, 2.0);
        let goals = IkGoals { look_at: Some(LookAt { target, weight: 1.0, eye_share: 0.0 }), ..IkGoals::NONE };
        let mut pose = rest.clone();
        MockIk::new().solve(&skeleton, &goals, &mut pose);
        let w = world(&skeleton, &pose);
        let facing = w.local[head.index()].rotation * FORWARD;
        let dir = (target - w.local[head.index()].translation).normalize();
        assert!(facing.dot(dir) > 0.999, "head faces the target");
        let eye = bone(&skeleton, HumanoidBone::LeftEye);
        assert!(
            (pose.local[eye.index()].rotation.angle_between(rest.local[eye.index()].rotation)) < 0.05,
            "eyes barely move"
        );
    }

    #[test]
    fn the_eyes_alone_aim_at_the_target_when_they_take_the_whole_share() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let rest = skeleton.rest_pose();
        let head = bone(&skeleton, HumanoidBone::Head);
        let origin = world(&skeleton, &rest).local[head.index()].translation;
        let target = origin + Vec3::new(-0.5, 0.0, 2.0);
        let goals = IkGoals { look_at: Some(LookAt { target, weight: 1.0, eye_share: 1.0 }), ..IkGoals::NONE };
        let mut pose = rest.clone();
        MockIk::new().solve(&skeleton, &goals, &mut pose);
        assert!(pose.local[head.index()].rotation == rest.local[head.index()].rotation, "head stays");
        let w = world(&skeleton, &pose);
        for e in [HumanoidBone::LeftEye, HumanoidBone::RightEye] {
            let eye = w.local[bone(&skeleton, e).index()];
            let dir = (target - eye.translation).normalize();
            assert!((eye.rotation * FORWARD).dot(dir) > 0.9999);
        }
    }

    #[test]
    fn a_generic_skeleton_is_left_alone() {
        let rig = quadruped_rig().expect("fixture");
        let rest = rig.skeleton.rest_pose();
        let mut pose = rest.clone();
        let goals = IkGoals {
            look_at: Some(LookAt { target: Vec3::Z, weight: 1.0, eye_share: 0.5 }),
            left_hand: reach(Vec3::ONE, Vec3::Z, 1.0),
            ..IkGoals::NONE
        };
        MockIk::new().solve(&rig.skeleton, &goals, &mut pose);
        assert_eq!(pose, rest);
    }

    #[test]
    fn the_same_inputs_give_the_same_pose() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let goals = IkGoals {
            look_at: Some(LookAt { target: Vec3::new(0.3, 1.5, 2.0), weight: 0.7, eye_share: 0.4 }),
            right_hand: reach(Vec3::new(-0.3, 1.0, 0.3), Vec3::new(-0.5, 0.8, -0.5), 0.9),
            ..IkGoals::NONE
        };
        let run = || {
            let mut pose = skeleton.rest_pose();
            MockIk::new().solve(&skeleton, &goals, &mut pose);
            pose
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn a_vanishing_weight_with_an_opposing_pole_barely_moves_an_animated_arm() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let mut animated = skeleton.rest_pose();
        let (arm, elbow, hand) = (
            bone(&skeleton, HumanoidBone::RightUpperArm),
            bone(&skeleton, HumanoidBone::RightLowerArm),
            bone(&skeleton, HumanoidBone::RightHand),
        );
        animated.local[arm.index()].rotation = Quat::from_rotation_y(0.9) * Quat::from_rotation_z(-0.5);
        animated.local[elbow.index()].rotation = Quat::from_rotation_y(1.1);
        let w = world(&skeleton, &animated);
        let shoulder = w.local[arm.index()].translation;
        let side = w.local[elbow.index()].translation - shoulder;
        let target = w.local[hand.index()].translation + Vec3::new(0.05, 0.05, 0.0);
        // The pole sits on the opposite side of the animated bend.
        let goals = IkGoals { right_hand: reach(target, shoulder - side * 4.0, 1e-3), ..IkGoals::NONE };
        let mut pose = animated.clone();
        MockIk::new().solve(&skeleton, &goals, &mut pose);
        for b in [arm, elbow] {
            assert!(pose.local[b.index()].rotation.angle_between(animated.local[b.index()].rotation) < 0.01);
        }
    }

    #[test]
    fn zero_eye_share_leaves_the_eyes_exactly_as_animated() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let rest = skeleton.rest_pose();
        let goals = IkGoals {
            look_at: Some(LookAt { target: Vec3::new(1.0, 1.7, 2.0), weight: 1.0, eye_share: 0.0 }),
            ..IkGoals::NONE
        };
        let mut pose = rest.clone();
        MockIk::new().solve(&skeleton, &goals, &mut pose);
        for e in [HumanoidBone::LeftEye, HumanoidBone::RightEye] {
            let i = bone(&skeleton, e).index();
            assert_eq!(pose.local[i].rotation, rest.local[i].rotation);
        }
    }

    #[test]
    fn a_neckless_humanoid_still_looks_with_its_head() {
        let bones: Vec<HumanoidBone> = HumanoidBone::ALL.into_iter().filter(|b| *b != HumanoidBone::Neck).collect();
        let skeleton = Skeleton::humanoid(&bones, |_| Transform::from_translation(Vec3::new(0.0, 0.1, 0.0)))
            .expect("neck is optional");
        let rest = skeleton.rest_pose();
        let head = bone(&skeleton, HumanoidBone::Head);
        let origin = world(&skeleton, &rest).local[head.index()].translation;
        let target = origin + Vec3::new(1.0, 0.0, 2.0);
        let goals = IkGoals { look_at: Some(LookAt { target, weight: 1.0, eye_share: 0.0 }), ..IkGoals::NONE };
        let mut pose = rest.clone();
        MockIk::new().solve(&skeleton, &goals, &mut pose);
        let w = world(&skeleton, &pose);
        let facing = w.local[head.index()].rotation * FORWARD;
        assert!(facing.dot((target - w.local[head.index()].translation).normalize()) > 0.999);
    }

    /// The animated right arm used by the bend-plane tests, its reach target (the current hand) and a
    /// pole turned `degrees` about the shoulder-to-hand axis away from the animated elbow's side (180 is opposite).
    fn bent_arm(skeleton: &Skeleton, degrees: f32) -> (Pose, [BoneId; 3], Vec3, Vec3) {
        let mut animated = skeleton.rest_pose();
        let ids = [HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm, HumanoidBone::RightHand]
            .map(|b| bone(skeleton, b));
        animated.local[ids[0].index()].rotation = Quat::from_rotation_y(0.9) * Quat::from_rotation_z(-0.5);
        animated.local[ids[1].index()].rotation = Quat::from_rotation_y(std::f32::consts::FRAC_PI_2);
        let w = world(skeleton, &animated);
        let [shoulder, elbow, hand] = ids.map(|i| w.local[i.index()].translation);
        let axis = (hand - shoulder).normalize();
        let side = (elbow - shoulder) - axis * (elbow - shoulder).dot(axis);
        let pole = shoulder + Quat::from_axis_angle(axis, degrees.to_radians()) * side;
        (animated, ids, hand, pole)
    }

    fn elbow_at(skeleton: &Skeleton, animated: &Pose, ids: [BoneId; 3], goal: (Vec3, Vec3), weight: f32) -> Vec3 {
        let goals = IkGoals { right_hand: reach(goal.0, goal.1, weight), ..IkGoals::NONE };
        let mut pose = animated.clone();
        MockIk::new().solve(skeleton, &goals, &mut pose);
        world(skeleton, &pose).local[ids[1].index()].translation
    }

    #[test]
    fn the_elbow_moves_continuously_as_the_weight_crosses_one_half() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let (animated, ids, hand, pole) = bent_arm(&skeleton, 175.0);
        let mut last = elbow_at(&skeleton, &animated, ids, (hand, pole), 0.4);
        let mut total = 0.0;
        for step in 1..=100 {
            let weight = 0.4 + 0.002 * step as f32;
            let now = elbow_at(&skeleton, &animated, ids, (hand, pole), weight);
            let moved = now.distance(last);
            assert!(moved < 0.004, "elbow jumped {moved} m at weight {weight}");
            total += moved;
            last = now;
        }
        assert!(total > 0.01, "the elbow does sweep over the range");
    }

    #[test]
    fn an_antipodal_pole_always_turns_the_elbow_the_same_way() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let (animated, ids, hand, pole) = bent_arm(&skeleton, 180.0);
        let before = world(&skeleton, &animated).local[ids[1].index()].translation;
        let shoulder = world(&skeleton, &animated).local[ids[0].index()].translation;
        let axis = (hand - shoulder).normalize();
        for weight in [0.3_f32, 0.5, 0.7] {
            let elbow = elbow_at(&skeleton, &animated, ids, (hand, pole), weight);
            let expected = shoulder + Quat::from_axis_angle(axis, std::f32::consts::PI * weight) * (before - shoulder);
            assert!(elbow.distance(expected) < 1e-3, "weight {weight}: counter-clockwise by pi times weight");
            assert_eq!(elbow, elbow_at(&skeleton, &animated, ids, (hand, pole), weight));
        }
    }

    #[test]
    fn a_goal_with_a_non_finite_field_is_ignored_as_if_absent() {
        let skeleton = humanoid_skeleton().expect("fixture");
        let rest = skeleton.rest_pose();
        let good_limb = Limb { target: Vec3::new(-0.3, 1.0, 0.3), pole: Vec3::new(-0.5, 0.8, -0.5), weight: 0.9 };
        let good_look = LookAt { target: Vec3::new(0.3, 1.5, 2.0), weight: 0.8, eye_share: 0.4 };
        let solve = |goals: IkGoals| {
            let mut pose = rest.clone();
            MockIk::new().solve(&skeleton, &goals, &mut pose);
            pose
        };
        let with_good = IkGoals { right_hand: Some(good_limb), look_at: Some(good_look), ..IkGoals::NONE };
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let limbs = [
                Limb { target: Vec3::new(bad, 1.0, 0.3), ..good_limb },
                Limb { pole: Vec3::new(0.0, bad, 0.0), ..good_limb },
                Limb { weight: bad, ..good_limb },
            ];
            for limb in limbs {
                // The bad left hand drops out; the good right hand and look-at are solved as usual.
                let got = solve(IkGoals { left_hand: Some(limb), ..with_good });
                assert_eq!(got, solve(with_good), "limb {limb:?}");
                let alone = solve(IkGoals { left_foot: Some(limb), ..IkGoals::NONE });
                assert_eq!(alone, rest, "limb {limb:?}");
            }
            let looks = [
                LookAt { target: Vec3::new(0.0, 1.5, bad), ..good_look },
                LookAt { weight: bad, ..good_look },
                LookAt { eye_share: bad, ..good_look },
            ];
            for look in looks {
                let got = solve(IkGoals { look_at: Some(look), right_hand: Some(good_limb), ..IkGoals::NONE });
                let expected = solve(IkGoals { right_hand: Some(good_limb), ..IkGoals::NONE });
                assert_eq!(got, expected, "look {look:?}");
            }
        }
    }
}
