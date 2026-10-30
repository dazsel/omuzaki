//! The tracked body (PF-06 hands, PF-07 arms): a body source's newest frame laid on the pose before the animator.
//!
//! A [`BodyFrame`] is already solved into T-space rotations and finger angles, so this layer only decides how much of
//! it to show. Each part (each arm, each hand) eases in as the source grows sure of it and out as it loses it, so a
//! hand leaving the frame drifts back to the animated pose instead of freezing or snapping. A source that goes quiet
//! counts as seeing nothing.
//!
//! - **Order.** The layer goes after the score's clips and before the animator, so the tracked arms win over a clip's
//!   arms while the animator's breath and the tracked head still move the body.
//! - **The head stays where the face put it.** The face tracker measures the head against the camera, not against the
//!   chest, so when the tracked chest turns, the neck turns back by as much and the head ends up where the face says.
//!   A mocap source sends the head relative to the chest ([`BodyFrame::head_relative`]), so there the chest carries it.
//! - **Any bone.** A webcam gives the chest, arms and wrists; a mocap source (VMC) can give the hips, spine, legs and
//!   finger bones too. Each bone goes with its part's weight: the arms', the hands' (wrists and fingers) or the rest of
//!   the body's. The head, neck, eyes and jaw are the face's and are never taken from the body.
//! - **Mirror.** With the retarget's mirror on (PF-27, a selfie preview), the frame is reflected left to right, as the
//!   face's channels are.
//! - **Fingers** go through the kit's hand shapes (`omuzaki_anim::hands`), on the nominal thumb rest the score's clips
//!   use.

use std::borrow::Cow;

use omuzaki_anim::hands::{self, FingerCurl, HandRest, HandShape};
use omuzaki_anim_api::{BoneMask, HUMANOID_COUNT, HumanoidBone, HumanoidPose, Tick};
use omuzaki_core::glam::Quat;
use omuzaki_track_api::body::{BodyFrame, BodySource, FingerAngles, Side};

/// Seconds without a frame after which the source counts as seeing nothing.
pub const STALE_BODY_S: f64 = 0.4;
/// How fast a part eases in and out, per second of full weight: in over about a tenth of a second, out over a third.
const EASE_IN_PER_S: f32 = 8.0;
const EASE_OUT_PER_S: f32 = 3.0;

/// Which part's weight a bone shows with, by the character's side.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Part {
    /// The face's: never taken from the body.
    Face,
    /// The upper chest: the webcam's one torso bone, so it follows the arms as well as the body.
    Chest,
    /// The hips, spine, chest and legs.
    Body,
    /// A shoulder, upper arm or forearm.
    Arm(usize),
    /// A wrist: needs its arm as well as its hand.
    Wrist(usize),
    /// A finger bone.
    Finger(usize),
}

/// The part of each humanoid bone, in [`HumanoidBone::ALL`] order.
fn parts() -> [Part; HUMANOID_COUNT] {
    use HumanoidBone as B;
    std::array::from_fn(|i| {
        let bone = HumanoidBone::ALL[i];
        match bone {
            B::Neck | B::Head | B::LeftEye | B::RightEye | B::Jaw => Part::Face,
            B::UpperChest => Part::Chest,
            B::LeftHand => Part::Wrist(0),
            B::RightHand => Part::Wrist(1),
            B::LeftShoulder | B::LeftUpperArm | B::LeftLowerArm => Part::Arm(0),
            B::RightShoulder | B::RightUpperArm | B::RightLowerArm => Part::Arm(1),
            _ => {
                // A finger is a bone with a hand above it.
                let mut up = bone.parent();
                while let Some(parent) = up {
                    match parent {
                        B::LeftHand => return Part::Finger(0),
                        B::RightHand => return Part::Finger(1),
                        _ => up = parent.parent(),
                    }
                }
                Part::Body
            }
        }
    })
}

/// The tracked body layer. Hot path: [`TrackedBody::step`] and [`TrackedBody::apply`] allocate and lock nothing.
pub struct TrackedBody {
    source: Option<Box<dyn BodySource>>,
    /// The newest frame, already mirrored when the mirror is on.
    frame: BodyFrame,
    /// When it arrived, on the pipeline clock.
    arrived: Option<f64>,
    /// The weight each part shows with now: arms left, right, hands left, right, then the rest of the body.
    weights: [f32; 5],
    /// Each bone's part.
    parts: [Part; HUMANOID_COUNT],
    last_tick: Option<f64>,
    rest: HandRest,
    /// The upper chest's rotation before and after the tracked chest was laid on, for the neck's counter-turn.
    chest_turn: Quat,
}

impl Default for TrackedBody {
    fn default() -> Self {
        Self {
            source: None,
            frame: BodyFrame::empty(0, omuzaki_core::time::Seconds(0.0)),
            arrived: None,
            weights: [0.0; 5],
            parts: parts(),
            last_tick: None,
            rest: HandRest::nominal(),
            chest_turn: Quat::IDENTITY,
        }
    }
}

impl TrackedBody {
    /// A layer reading `source`.
    pub fn new(source: Box<dyn BodySource>) -> Self {
        Self { source: Some(source), ..Self::default() }
    }

    /// Whether a source is attached.
    pub fn has_source(&self) -> bool {
        self.source.is_some()
    }

    /// The weight each part shows with now: arms (left, right), hands (left, right), by the character's side, then the
    /// rest of the body.
    pub fn weights(&self) -> [f32; 5] {
        self.weights
    }

    /// Forgets the frame and the weights: everything shows the animation until the source speaks again.
    pub fn reset(&mut self) {
        let source = self.source.take();
        *self = Self { source, ..Self::default() };
    }

    /// Polls the source and eases each part toward how sure the source is of it. `mirror` reflects the frame.
    #[allow(clippy::cast_possible_truncation)] // a step of seconds fits f32
    pub fn step(&mut self, tick: Tick, mirror: bool) {
        let now = tick.time.0;
        let dt = self.last_tick.map_or(0.0, |t| (now - t).clamp(0.0, 0.25) as f32);
        self.last_tick = Some(now);
        if let Some(frame) = self.source.as_mut().and_then(|source| source.poll()) {
            self.frame = if mirror { mirrored(&frame) } else { frame };
            self.arrived = Some(now);
        }
        let fresh = self.arrived.is_some_and(|at| now - at <= STALE_BODY_S);
        let f = &self.frame;
        let targets = if fresh { [f.arms[0], f.arms[1], f.hands[0], f.hands[1], f.body] } else { [0.0; 5] };
        for (weight, target) in self.weights.iter_mut().zip(targets) {
            let target = if target.is_finite() { target.clamp(0.0, 1.0) } else { 0.0 };
            *weight = if target > *weight {
                (*weight + EASE_IN_PER_S * dt).min(target)
            } else {
                (*weight - EASE_OUT_PER_S * dt).max(target)
            };
        }
    }

    /// Lays the tracked bones and fingers on `pose`, each at its part's weight. Call before the animator.
    pub fn apply(&mut self, pose: &mut HumanoidPose) {
        self.chest_turn = Quat::IDENTITY;
        let (frame, [arm_l, arm_r, hand_l, hand_r, body]) = (&self.frame, self.weights);
        let (arm, hand) = ([arm_l, arm_r], [hand_l, hand_r]);
        let before = pose.get(HumanoidBone::UpperChest);
        for (bone, part) in HumanoidBone::ALL.into_iter().zip(self.parts) {
            let weight = match part {
                Part::Face => continue,
                Part::Chest => arm_l.max(arm_r).max(body),
                Part::Body => body,
                Part::Arm(s) => arm[s],
                Part::Wrist(s) => hand[s].min(arm[s]),
                Part::Finger(s) => hand[s],
            };
            if weight > 0.0 && frame.mask.contains(bone) {
                let value = frame.pose.get(bone);
                pose.set(bone, if weight >= 1.0 { value } else { pose.get(bone).slerp(value, weight) });
            }
        }
        if !frame.head_relative {
            self.chest_turn = (pose.get(HumanoidBone::UpperChest).inverse() * before).normalize();
        }
        for (s, fingers) in frame.fingers.iter().enumerate() {
            if let Some(fingers) = fingers {
                let side = if s == 0 { hands::Side::Left } else { hands::Side::Right };
                hands::apply(pose, side, &self.rest, &shape(fingers), hand[s]);
            }
        }
    }

    /// Turns the neck back by the tracked chest's turn, so the head stays where the face put it. Call after the
    /// animator.
    pub fn settle_head(&self, pose: &mut HumanoidPose) {
        if self.chest_turn != Quat::IDENTITY {
            let neck = pose.get(HumanoidBone::Neck);
            pose.set(HumanoidBone::Neck, (self.chest_turn * neck).normalize());
        }
    }
}

impl std::fmt::Debug for TrackedBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackedBody").field("weights", &self.weights).finish_non_exhaustive()
    }
}

/// A hand shape from tracked finger angles. The ids are static, so making one allocates nothing.
fn shape(fingers: &[FingerAngles; 5]) -> HandShape {
    HandShape {
        id: Cow::Borrowed("tracked"),
        summary: Cow::Borrowed("the tracked hand"),
        curls: fingers.map(|a| FingerCurl { spread: a.spread, mcp: a.mcp, pip: a.pip, dip: a.dip }),
    }
}

/// `frame` reflected left to right: the pose and its mask mirrored, the sides swapped.
fn mirrored(frame: &BodyFrame) -> BodyFrame {
    let mask = HumanoidBone::ALL
        .iter()
        .filter(|bone| frame.mask.contains(**bone))
        .fold(BoneMask::EMPTY, |mask, bone| mask.with(bone.mirrored()));
    let swap = |[a, b]: [f32; 2]| [b, a];
    BodyFrame {
        pose: frame.pose.mirrored(),
        mask,
        fingers: [frame.fingers[Side::Right.index()], frame.fingers[Side::Left.index()]],
        arms: swap(frame.arms),
        hands: swap(frame.hands),
        ..*frame
    }
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use omuzaki_core::time::Seconds;

    use super::*;

    /// A source that hands out queued frames.
    struct Script(Vec<BodyFrame>);

    impl BodySource for Script {
        fn poll(&mut self) -> Option<BodyFrame> {
            (!self.0.is_empty()).then(|| self.0.remove(0))
        }
    }

    /// The left arm hanging down, its hand in a fist, fully sure.
    fn arm_down() -> BodyFrame {
        let mut f = BodyFrame::empty(1, Seconds(0.0));
        f.pose.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(-FRAC_PI_2));
        f.pose.set(HumanoidBone::UpperChest, Quat::from_rotation_y(0.3));
        f.mask = BoneMask::of(&[HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::UpperChest]);
        f.fingers[0] = Some([FingerAngles { spread: 0.0, mcp: 1.4, pip: 1.5, dip: 0.8 }; 5]);
        f.arms = [1.0, 0.0];
        f.hands = [1.0, 0.0];
        f
    }

    fn step(body: &mut TrackedBody, i: u64, mirror: bool) -> HumanoidPose {
        body.step(Tick::at_rate(i, 60.0), mirror);
        let mut pose = HumanoidPose::REST;
        body.apply(&mut pose);
        pose
    }

    #[test]
    fn a_seen_arm_eases_in_and_holds() {
        let mut body = TrackedBody::new(Box::new(Script(vec![arm_down(); 40])));
        let first = step(&mut body, 0, false);
        assert_eq!(first.get(HumanoidBone::LeftUpperArm), Quat::IDENTITY, "nothing shows on the first step");
        let mut pose = first;
        for i in 1..30 {
            pose = step(&mut body, i, false);
        }
        assert!(pose.get(HumanoidBone::LeftUpperArm).angle_between(Quat::from_rotation_z(-FRAC_PI_2)) < 1e-3);
        assert_eq!(pose.get(HumanoidBone::RightUpperArm), Quat::IDENTITY, "the unseen arm is untouched");
        assert!(pose.get(HumanoidBone::LeftIndexProximal).angle_between(Quat::IDENTITY) > 1.0, "the fist curls");
        assert_eq!(pose.get(HumanoidBone::RightIndexProximal), Quat::IDENTITY);
    }

    #[test]
    fn a_quiet_source_lets_the_arm_go() {
        let mut body = TrackedBody::new(Box::new(Script(vec![arm_down(); 1])));
        for i in 0..20 {
            step(&mut body, i, false);
        }
        assert!(body.weights()[0] > 0.99);
        // No frame for well past the stale limit: the arm eases out.
        let mut pose = HumanoidPose::REST;
        for i in 20..120 {
            pose = step(&mut body, i, false);
        }
        assert_eq!(body.weights(), [0.0; 5]);
        assert_eq!(pose.get(HumanoidBone::LeftUpperArm), Quat::IDENTITY);
    }

    #[test]
    fn the_mirror_moves_the_arm_and_fingers_to_the_other_side() {
        let mut body = TrackedBody::new(Box::new(Script(vec![arm_down(); 40])));
        let mut pose = HumanoidPose::REST;
        for i in 0..30 {
            pose = step(&mut body, i, true);
        }
        let right = pose.get(HumanoidBone::RightUpperArm);
        assert!((right * omuzaki_core::glam::Vec3::NEG_X).abs_diff_eq(omuzaki_core::glam::Vec3::NEG_Y, 1e-4), "{right}");
        assert_eq!(pose.get(HumanoidBone::LeftUpperArm), Quat::IDENTITY);
        assert!(pose.get(HumanoidBone::RightIndexProximal).angle_between(Quat::IDENTITY) > 1.0);
    }

    #[test]
    fn the_neck_turns_back_by_the_chests_turn() {
        let mut body = TrackedBody::new(Box::new(Script(vec![arm_down(); 40])));
        let mut pose = HumanoidPose::REST;
        for i in 0..30 {
            pose = step(&mut body, i, false);
        }
        body.settle_head(&mut pose);
        let head_world = pose.get(HumanoidBone::UpperChest) * pose.get(HumanoidBone::Neck);
        assert!(head_world.angle_between(Quat::IDENTITY) < 1e-3, "{head_world}");
    }

    /// A mocap frame: the hips, spine, a leg and a finger bone, the head and neck too (which must be ignored), the
    /// head relative to the chest.
    fn mocap() -> BodyFrame {
        use HumanoidBone as B;
        let mut f = BodyFrame::empty(1, Seconds(0.0));
        for (bone, q) in [
            (B::Hips, Quat::from_rotation_y(0.4)),
            (B::Spine, Quat::from_rotation_x(0.2)),
            (B::UpperChest, Quat::from_rotation_y(0.3)),
            (B::LeftUpperLeg, Quat::from_rotation_x(-0.6)),
            (B::RightIndexIntermediate, Quat::from_rotation_z(0.9)),
            (B::Neck, Quat::from_rotation_x(0.5)),
            (B::Head, Quat::from_rotation_x(0.5)),
        ] {
            f.pose.set(bone, q);
            f.mask = f.mask.with(bone);
        }
        f.arms = [1.0; 2];
        f.hands = [1.0; 2];
        f.body = 1.0;
        f.head_relative = true;
        f
    }

    #[test]
    fn a_mocap_frame_drives_the_hips_spine_legs_and_finger_bones_but_never_the_face() {
        use HumanoidBone as B;
        let mut body = TrackedBody::new(Box::new(Script(vec![mocap(); 40])));
        let mut pose = HumanoidPose::REST;
        for i in 0..30 {
            pose = step(&mut body, i, false);
        }
        let frame = mocap();
        for bone in [B::Hips, B::Spine, B::UpperChest, B::LeftUpperLeg, B::RightIndexIntermediate] {
            assert!(pose.get(bone).angle_between(frame.pose.get(bone)) < 1e-3, "{bone:?}");
        }
        assert_eq!(pose.get(B::Neck), Quat::IDENTITY, "the neck is the face's");
        assert_eq!(pose.get(B::Head), Quat::IDENTITY, "the head is the face's");
        // The head rides on the chest: no counter-turn.
        let before = pose;
        body.settle_head(&mut pose);
        assert_eq!(pose, before);
    }

    #[test]
    fn the_rest_of_the_body_eases_out_with_its_own_confidence() {
        let mut quiet = mocap();
        quiet.body = 0.0;
        let mut frames = vec![mocap(); 30];
        frames.extend(vec![quiet; 60]);
        let mut body = TrackedBody::new(Box::new(Script(frames)));
        let mut pose = HumanoidPose::REST;
        for i in 0..90 {
            pose = step(&mut body, i, false);
        }
        assert_eq!(pose.get(HumanoidBone::Hips), Quat::IDENTITY);
        assert!(pose.get(HumanoidBone::UpperChest).angle_between(Quat::IDENTITY) > 0.2, "the chest still follows the arms");
    }
}
