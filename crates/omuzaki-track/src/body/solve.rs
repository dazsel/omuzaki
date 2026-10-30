//! The body solver: landmarks in metres to the bones and fingers of a [`BodyFrame`].
//!
//! Everything is solved as **directions**, never positions, so the result fits any body: a long-armed performer and a
//! chibi character share the angles, not the reach. Points are first turned from the camera's axes into the
//! character's root axes (`+X` the performer's left, `+Y` up, `+Z` toward the camera) and One-Euro filtered.
//!
//! - **Chest.** The shoulder line gives the upper chest's turn and tilt; up is the world's up, so a person sitting at a
//!   desk (hips out of frame) still reads right.
//! - **Arms.** Each bone's rotation takes its T-pose frame to the measured one: the bone's direction, and the elbow's
//!   hinge axis (the normal of the plane of upper arm and forearm), which also fixes the upper arm's twist. A straight
//!   arm has no plane, so the hinge eases toward the last one seen as the arm straightens. The forearm takes the
//!   hand's twist (pronation), as a real forearm does, and the wrist keeps only its bend.
//! - **Hands.** The palm's frame (wrist to middle knuckle, little to index knuckle) gives the hand's rotation; each
//!   finger's knuckle, middle and tip angles are measured in that frame (the kit's [`FingerAngles`] conventions).
//!
//! Rotations are T space (VRM normalised, parent-local): a bone's local rotation is its parent's world rotation undone
//! from its own, with the spine below the upper chest taken as upright.

use omuzaki_anim_api::{BoneMask, HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{Mat3, Quat, Vec3};
use omuzaki_track_api::body::{BodyConfig, BodyFrame, FingerAngles, Fingers, Side};

use super::filter::{EuroParams, EuroPoint};
use super::hand::{HAND_POINTS, HandLandmarks};
use super::pose::{POSE_POINTS, PoseLandmarks};

/// The landmarks of one frame, by the performer's side.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BodyLandmarks {
    /// The person, if found.
    pub pose: Option<PoseLandmarks>,
    /// Each hand, if found, by the performer's [`Side`].
    pub hands: [Option<HandLandmarks>; 2],
}

impl BodyLandmarks {
    /// Nothing found.
    pub const NONE: Self = Self { pose: None, hands: [None, None] };
}

/// MediaPipe pose landmark indices (the performer's own sides).
mod at {
    pub const LEFT_SHOULDER: usize = 11;
    pub const RIGHT_SHOULDER: usize = 12;
    pub const LEFT_ELBOW: usize = 13;
    pub const RIGHT_ELBOW: usize = 14;
    pub const LEFT_WRIST: usize = 15;
    pub const RIGHT_WRIST: usize = 16;
}

/// Hand landmark indices: the wrist, then each finger's four points from base to tip, thumb first.
const WRIST: usize = 0;
const FINGER_BASES: [usize; 5] = [1, 5, 9, 13, 17];
const INDEX_MCP: usize = 5;
const MIDDLE_MCP: usize = 9;
const LITTLE_MCP: usize = 17;

/// Where the library's hand shapes put the thumb at rest: 45 degrees from the fingers toward the index side.
const THUMB_REST: f32 = std::f32::consts::FRAC_PI_4;
/// Visibility below this reads as "not seen"; above the upper value, fully seen.
const SEEN: (f32, f32) = (0.35, 0.75);
/// Bend (sine of the elbow angle) under which the arm counts as straight and its hinge is carried over.
const STRAIGHT: (f32, f32) = (0.08, 0.3);
/// How much of the last hinge is left once the upper arm has turned onto it, under which the forward hinge takes over.
const HINGE_KEPT: (f32, f32) = (0.1, 0.4);

/// Body landmark smoothing: steady at rest, quick on a wave.
const BODY_EURO: EuroParams = EuroParams { min_cutoff: 1.2, beta: 8.0 };
/// Hand landmark smoothing (metres around the hand: small numbers, so a higher beta).
const HAND_EURO: EuroParams = EuroParams { min_cutoff: 1.5, beta: 25.0 };

/// The performer's arm bones by [`Side`]: upper arm, lower arm, hand.
const ARM_BONES: [[HumanoidBone; 3]; 2] = [
    [HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm, HumanoidBone::LeftHand],
    [HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm, HumanoidBone::RightHand],
];
/// Shoulder, elbow and wrist landmarks by [`Side`].
const ARM_POINTS: [[usize; 3]; 2] = [
    [at::LEFT_SHOULDER, at::LEFT_ELBOW, at::LEFT_WRIST],
    [at::RIGHT_SHOULDER, at::RIGHT_ELBOW, at::RIGHT_WRIST],
];

/// The camera's axes (x right, y down, z away) to the root's (x the performer's left, y up, z toward the camera).
fn to_root(v: Vec3) -> Vec3 {
    Vec3::new(v.x, -v.y, -v.z)
}

fn smoothstep(edges: (f32, f32), x: f32) -> f32 {
    let t = ((x - edges.0) / (edges.1 - edges.0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The rotation whose frame has `first` as its first axis and `second` (made perpendicular to it) as its second.
/// `None` when the two are parallel or not finite.
fn frame(first: Vec3, second: Vec3) -> Option<Mat3> {
    let x = first.try_normalize()?;
    let y = (second - x * second.dot(x)).try_normalize()?;
    Some(Mat3::from_cols(x, y, x.cross(y)))
}

/// The rotation taking the frame (`rest_first`, `rest_second`) to (`first`, `second`).
fn rotation_between(rest: (Vec3, Vec3), measured: (Vec3, Vec3)) -> Option<Quat> {
    let (from, to) = (frame(rest.0, rest.1)?, frame(measured.0, measured.1)?);
    Some(Quat::from_mat3(&(to * from.transpose())).normalize())
}

/// `q`'s twist about `axis` (unit): the part of the rotation that turns about that axis.
fn twist(q: Quat, axis: Vec3) -> Quat {
    let projected = axis * Vec3::new(q.x, q.y, q.z).dot(axis);
    let raw = Quat::from_xyzw(projected.x, projected.y, projected.z, q.w);
    let length = raw.length();
    if length > 1e-6 { raw / length } else { Quat::IDENTITY }
}

/// The unsigned angle between two vectors, radians; zero for a degenerate one.
fn angle(a: Vec3, b: Vec3) -> f32 {
    match (a.try_normalize(), b.try_normalize()) {
        (Some(a), Some(b)) => a.dot(b).clamp(-1.0, 1.0).acos(),
        _ => 0.0,
    }
}

/// The landmarks a solver keeps filtered, in root axes.
struct Filtered {
    pose: [EuroPoint; POSE_POINTS],
    hands: [[EuroPoint; HAND_POINTS]; 2],
}

/// The solver. One per tracker; it keeps the filters and each elbow's last hinge.
pub struct BodySolver {
    filtered: Filtered,
    /// Each arm's hinge axis, kept while the arm is straight, in root axes.
    hinge: [Vec3; 2],
    last_time: Option<f64>,
    /// Whether the last frame had a person (the filters restart when one returns).
    had_pose: bool,
    had_hand: [bool; 2],
}

impl Default for BodySolver {
    fn default() -> Self {
        Self::new()
    }
}

impl BodySolver {
    /// A solver with no history.
    pub fn new() -> Self {
        Self {
            filtered: Filtered {
                pose: [EuroPoint::default(); POSE_POINTS],
                hands: [[EuroPoint::default(); HAND_POINTS]; 2],
            },
            hinge: [rest_hinge(Side::Left), rest_hinge(Side::Right)],
            last_time: None,
            had_pose: false,
            had_hand: [false; 2],
        }
    }

    /// Forgets the history.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Solves `landmarks`, captured at `time` seconds, into `out` (its `seq` and `time` are left to the caller).
    /// Only the parts `config` asks for are solved.
    #[allow(clippy::cast_possible_truncation)] // a frame gap of seconds fits f32
    pub fn solve(&mut self, landmarks: &BodyLandmarks, time: f64, config: &BodyConfig, out: &mut BodyFrame) {
        let dt = self.last_time.map_or(0.0, |t| (time - t) as f32);
        self.last_time = Some(time);
        out.pose = HumanoidPose::REST;
        out.mask = BoneMask::EMPTY;
        out.fingers = [None, None];
        out.arms = [0.0; 2];
        out.hands = [0.0; 2];

        let pose = landmarks.pose.filter(|_| config.arms);
        if !pose.is_some() || !self.had_pose {
            self.filtered.pose.iter_mut().for_each(EuroPoint::reset);
        }
        self.had_pose = pose.is_some();
        let mut points = [Vec3::ZERO; POSE_POINTS];
        if let Some(pose) = &pose {
            for ((point, raw), filter) in points.iter_mut().zip(&pose.world).zip(&mut self.filtered.pose) {
                *point = filter.filter(to_root(*raw), dt, BODY_EURO);
            }
        }

        // The hands' rotations and fingers, in root axes, by side.
        let mut hand_rotation = [None; 2];
        for side in Side::BOTH {
            let s = side.index();
            let hand = landmarks.hands[s].filter(|_| config.hands);
            if !hand.is_some() || !self.had_hand[s] {
                self.filtered.hands[s].iter_mut().for_each(EuroPoint::reset);
            }
            self.had_hand[s] = hand.is_some();
            let Some(hand) = hand else { continue };
            let mut h = [Vec3::ZERO; HAND_POINTS];
            for ((point, raw), filter) in h.iter_mut().zip(&hand.world).zip(&mut self.filtered.hands[s]) {
                *point = filter.filter(to_root(*raw), dt, HAND_EURO);
            }
            let Some(solved) = solve_hand(&h, side) else { continue };
            hand_rotation[s] = Some(solved.rotation);
            out.fingers[s] = Some(solved.fingers);
            out.hands[s] = hand.presence.clamp(0.0, 1.0);
        }

        // The chest: the shoulder line, with the world's up.
        let chest = pose.and_then(|p| {
            let seen = smoothstep(SEEN, p.visibility[at::LEFT_SHOULDER].min(p.visibility[at::RIGHT_SHOULDER]));
            let across = points[at::LEFT_SHOULDER] - points[at::RIGHT_SHOULDER];
            (seen > 0.0).then(|| rotation_between((Vec3::X, Vec3::Y), (across, Vec3::Y))).flatten()
        });
        if let Some(chest) = chest {
            out.pose.set(HumanoidBone::UpperChest, chest);
            out.mask = out.mask.with(HumanoidBone::UpperChest);
        }
        let chest_world = chest.unwrap_or(Quat::IDENTITY);

        for side in Side::BOTH {
            let s = side.index();
            let [upper, lower, hand_bone] = ARM_BONES[s];
            // A hand seen without its arm still turns at the wrist, relative to the animated forearm: only an arm
            // gives the forearm's world rotation, so without one the wrist keeps the animation's.
            let Some(p) = &pose else { continue };
            let [shoulder, elbow, wrist] = ARM_POINTS[s];
            let seen = smoothstep(SEEN, p.visibility[shoulder].min(p.visibility[elbow]).min(p.visibility[wrist]));
            if seen <= 0.0 {
                continue;
            }
            let (d1, d2) = (points[elbow] - points[shoulder], points[wrist] - points[elbow]);
            let Some(arm) = self.solve_arm(side, d1, d2) else { continue };
            let (upper_world, mut lower_world) = arm;
            if let Some(hand_world) = hand_rotation[s] {
                // The forearm takes the hand's twist about the forearm; the wrist keeps the rest.
                let relative = lower_world.inverse() * hand_world;
                lower_world = (lower_world * twist(relative, sign(side) * Vec3::X)).normalize();
                out.pose.set(hand_bone, (lower_world.inverse() * hand_world).normalize());
                out.mask = out.mask.with(hand_bone);
            }
            out.pose.set(upper, (chest_world.inverse() * upper_world).normalize());
            out.pose.set(lower, (upper_world.inverse() * lower_world).normalize());
            out.mask = out.mask.with(upper).with(lower);
            out.arms[s] = seen;
        }
    }

    /// The upper arm's and forearm's world rotations from their directions, or `None` for a degenerate arm.
    fn solve_arm(&mut self, side: Side, upper: Vec3, lower: Vec3) -> Option<(Quat, Quat)> {
        let (d1, d2) = (upper.try_normalize()?, lower.try_normalize()?);
        let s = side.index();
        let measured = d1.cross(d2);
        // Carried over: the last hinge, made perpendicular to this upper arm. When the arm has swung onto the old
        // hinge (a T-pose arm dropped straight down), that has no direction left, so it eases to the hinge that bends
        // the elbow forward from where the arm points now.
        let forward = d1.cross(Vec3::Z).try_normalize().unwrap_or(rest_hinge(side));
        let projected = self.hinge[s] - d1 * self.hinge[s].dot(d1);
        let kept = smoothstep(HINGE_KEPT, projected.length());
        let carried = forward.lerp(projected.try_normalize().unwrap_or(forward), kept);
        let carried = (carried - d1 * carried.dot(d1)).try_normalize().unwrap_or(forward);
        let weight = smoothstep(STRAIGHT, measured.length());
        let hinge = measured.try_normalize().map_or(carried, |m| carried.lerp(m, weight)).try_normalize()?;
        self.hinge[s] = hinge;
        let rest = (sign(side) * Vec3::X, rest_hinge(side));
        Some((rotation_between(rest, (d1, hinge))?, rotation_between(rest, (d2, hinge))?))
    }
}

/// `+1` for the left side (the T-pose arm points along `+X`), `-1` for the right.
fn sign(side: Side) -> f32 {
    match side {
        Side::Left => 1.0,
        Side::Right => -1.0,
    }
}

/// The elbow's hinge axis in the T-pose: bending the elbow brings the forearm forward (`+Z`), so the axis is
/// `arm x forward`.
fn rest_hinge(side: Side) -> Vec3 {
    (sign(side) * Vec3::X).cross(Vec3::Z)
}

/// A hand solved from its 21 points in root axes.
struct SolvedHand {
    rotation: Quat,
    fingers: Fingers,
}

/// The hand's world rotation and finger angles, or `None` for a degenerate palm.
fn solve_hand(h: &[Vec3; HAND_POINTS], side: Side) -> Option<SolvedHand> {
    let along = (h[MIDDLE_MCP] - h[WRIST]).try_normalize()?;
    let across = h[INDEX_MCP] - h[LITTLE_MCP];
    let across = (across - along * across.dot(along)).try_normalize()?;
    // In the T-pose the fingers point along the arm and the index is on the `+Z` side, for either hand.
    let rotation = rotation_between((sign(side) * Vec3::X, Vec3::Z), (along, across))?;
    // The palm's normal (toward the palm): `-Y` in the T-pose for either hand.
    let palm = -sign(side) * across.cross(along);
    let mut fingers = [FingerAngles::default(); 5];
    for (finger, (out, &base)) in fingers.iter_mut().zip(&FINGER_BASES).enumerate() {
        let (b0, b1, b2, b3) = (h[base], h[base + 1], h[base + 2], h[base + 3]);
        let first = b1 - b0;
        let (x, y, z) = (first.dot(along), first.dot(across), first.dot(palm));
        let in_plane = x.hypot(y);
        *out = if finger == 0 {
            // The thumb: its metacarpal's swing away from the index (past the 45-degree rest) and its fold across
            // the palm, then the two joints above it.
            FingerAngles {
                spread: y.atan2(x) - THUMB_REST,
                mcp: z.atan2(in_plane),
                pip: angle(first, b2 - b1),
                dip: angle(b2 - b1, b3 - b2),
            }
        } else {
            FingerAngles {
                spread: y.atan2(x),
                mcp: z.atan2(in_plane),
                pip: angle(first, b2 - b1),
                dip: angle(b2 - b1, b3 - b2),
            }
        };
    }
    Some(SolvedHand { rotation, fingers })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // a panic is a test failure
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use super::*;

    /// A T-posed person in the camera's axes: shoulders 0.4 m apart, arms straight out, seen from the front (the
    /// performer's left on the image's right).
    fn t_pose() -> PoseLandmarks {
        let mut p = PoseLandmarks::NONE;
        p.visibility = [1.0; POSE_POINTS];
        p.presence = 1.0;
        let cam = |v: Vec3| Vec3::new(v.x, -v.y, -v.z); // root to camera axes (its own inverse)
        p.world[at::LEFT_SHOULDER] = cam(Vec3::new(0.2, 0.5, 0.0));
        p.world[at::LEFT_ELBOW] = cam(Vec3::new(0.45, 0.5, 0.0));
        p.world[at::LEFT_WRIST] = cam(Vec3::new(0.7, 0.5, 0.0));
        p.world[at::RIGHT_SHOULDER] = cam(Vec3::new(-0.2, 0.5, 0.0));
        p.world[at::RIGHT_ELBOW] = cam(Vec3::new(-0.45, 0.5, 0.0));
        p.world[at::RIGHT_WRIST] = cam(Vec3::new(-0.7, 0.5, 0.0));
        p
    }

    /// A flat T-pose hand for `side` in root axes: fingers along the arm, palm down, index toward `+Z`.
    fn flat_hand(side: Side) -> HandLandmarks {
        let mut hand = HandLandmarks { presence: 1.0, ..HandLandmarks::NONE };
        let s = sign(side);
        let cam = |v: Vec3| Vec3::new(v.x, -v.y, -v.z);
        // Knuckles in a row across the palm (index at +Z), each finger straight out along the arm.
        for (finger, z) in [(1usize, 0.03f32), (2, 0.0), (3, -0.02), (4, -0.04)] {
            let base = FINGER_BASES[finger];
            for joint in 0..4 {
                #[allow(clippy::cast_precision_loss)] // joint counters
                let x = 0.08 + 0.025 * joint as f32;
                hand.world[base + joint] = cam(Vec3::new(s * x, 0.0, z));
            }
        }
        // The thumb at its 45-degree rest, in the palm's plane.
        for joint in 0..4 {
            #[allow(clippy::cast_precision_loss)] // joint counters
            let r = 0.02 + 0.025 * joint as f32;
            hand.world[1 + joint] = cam(Vec3::new(s * r * 0.707, 0.0, 0.04 + r * 0.707));
        }
        hand
    }

    fn solve(landmarks: &BodyLandmarks) -> BodyFrame {
        let mut out = BodyFrame::empty(0, omuzaki_core::time::Seconds(0.0));
        BodySolver::new().solve(landmarks, 0.0, &BodyConfig::default(), &mut out);
        out
    }

    #[test]
    fn a_t_pose_solves_to_the_rest_pose() {
        let hands = [Some(flat_hand(Side::Left)), Some(flat_hand(Side::Right))];
        let out = solve(&BodyLandmarks { pose: Some(t_pose()), hands });
        for bone in [HumanoidBone::UpperChest, HumanoidBone::LeftUpperArm, HumanoidBone::RightLowerArm, HumanoidBone::LeftHand] {
            assert!(out.mask.contains(bone), "{bone:?} is driven");
            assert!(out.pose.get(bone).angle_between(Quat::IDENTITY) < 1e-4, "{bone:?}: {}", out.pose.get(bone));
        }
        assert_eq!(out.arms, [1.0, 1.0]);
        for fingers in out.fingers {
            for f in fingers.unwrap() {
                assert!(f.spread.abs() < 0.05 && f.mcp.abs() < 1e-4 && f.pip < 1e-4 && f.dip < 1e-4, "{f:?}");
            }
        }
    }

    #[test]
    fn an_arm_down_at_the_side_turns_the_upper_arm_down() {
        let mut pose = t_pose();
        // The left arm hangs straight down (camera axes: y down).
        pose.world[at::LEFT_ELBOW] = Vec3::new(0.2, -0.25, 0.0);
        pose.world[at::LEFT_WRIST] = Vec3::new(0.2, 0.0, 0.0);
        let out = solve(&BodyLandmarks { pose: Some(pose), hands: [None, None] });
        // The arm's +X now points down (-Y): a quarter turn about +Z.
        let q = out.pose.get(HumanoidBone::LeftUpperArm);
        assert!((q * Vec3::X).abs_diff_eq(-Vec3::Y, 1e-4), "{}", q * Vec3::X);
        assert!(out.pose.get(HumanoidBone::LeftLowerArm).angle_between(Quat::IDENTITY) < 1e-4);
        // No twist: the elbow still bends forward, so the whole turn is about the forward axis.
        assert!(q.angle_between(Quat::from_rotation_z(-FRAC_PI_2)) < 1e-4, "{q}");
    }

    #[test]
    fn a_bent_elbow_bends_the_forearm_forward() {
        let mut pose = t_pose();
        // Left forearm straight toward the camera (+Z in root axes, -z in the camera's).
        pose.world[at::LEFT_WRIST] = Vec3::new(0.45, -0.5, -0.25);
        let out = solve(&BodyLandmarks { pose: Some(pose), hands: [None, None] });
        let lower = out.pose.get(HumanoidBone::LeftLowerArm);
        assert!((lower * Vec3::X).abs_diff_eq(Vec3::Z, 1e-4), "{}", lower * Vec3::X);
        assert!((lower.angle_between(Quat::IDENTITY) - FRAC_PI_2).abs() < 1e-4);
        assert!(out.pose.get(HumanoidBone::LeftUpperArm).angle_between(Quat::IDENTITY) < 1e-4);
    }

    #[test]
    fn a_turned_chest_carries_the_arms_and_mirrors_by_side() {
        let mut pose = t_pose();
        // Turn the whole upper body 30 degrees about up.
        let turn = Quat::from_rotation_y(0.5);
        let cam = |v: Vec3| Vec3::new(v.x, -v.y, -v.z);
        for i in [11, 12, 13, 14, 15, 16] {
            pose.world[i] = cam(turn * cam(pose.world[i]));
        }
        let out = solve(&BodyLandmarks { pose: Some(pose), hands: [None, None] });
        assert!(out.pose.get(HumanoidBone::UpperChest).angle_between(turn) < 1e-4);
        // The arms are straight out from the turned chest: identity relative to it.
        assert!(out.pose.get(HumanoidBone::LeftUpperArm).angle_between(Quat::IDENTITY) < 1e-4);
        assert!(out.pose.get(HumanoidBone::RightUpperArm).angle_between(Quat::IDENTITY) < 1e-4);
    }

    #[test]
    fn a_fist_flexes_every_finger_toward_the_palm() {
        let mut hand = flat_hand(Side::Left);
        let cam = |v: Vec3| Vec3::new(v.x, -v.y, -v.z);
        // Curl each finger: the knuckle segment goes down (palm is -Y), the rest folds back toward the wrist.
        for (finger, z) in [(1usize, 0.03f32), (2, 0.0), (3, -0.02), (4, -0.04)] {
            let base = FINGER_BASES[finger];
            hand.world[base + 1] = cam(Vec3::new(0.08, -0.025, z));
            hand.world[base + 2] = cam(Vec3::new(0.06, -0.035, z));
            hand.world[base + 3] = cam(Vec3::new(0.05, -0.02, z));
        }
        let out = solve(&BodyLandmarks { pose: Some(t_pose()), hands: [Some(hand), None] });
        let fingers = out.fingers[0].unwrap();
        for f in &fingers[1..] {
            assert!((f.mcp - FRAC_PI_2).abs() < 1e-3, "{f:?}");
            assert!(f.pip > 0.5 && f.dip > 0.5, "{f:?}");
        }
        assert!(out.fingers[1].is_none() && out.hands == [1.0, 0.0]);
    }

    #[test]
    fn unseen_arms_and_unasked_parts_are_left_alone() {
        let mut pose = t_pose();
        pose.visibility[at::RIGHT_WRIST] = 0.1;
        let out = solve(&BodyLandmarks { pose: Some(pose), hands: [Some(flat_hand(Side::Left)), None] });
        assert!(out.mask.contains(HumanoidBone::LeftUpperArm) && !out.mask.contains(HumanoidBone::RightUpperArm));
        assert_eq!(out.arms[1], 0.0);
        let mut frame = BodyFrame::empty(0, omuzaki_core::time::Seconds(0.0));
        let config = BodyConfig { arms: false, hands: true, precise: false };
        BodySolver::new().solve(&BodyLandmarks { pose: Some(t_pose()), hands: [None, None] }, 0.0, &config, &mut frame);
        assert!(frame.mask.is_empty() && !frame.sees_anything());
    }
}
