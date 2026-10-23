//! The arm guard (#2353): every frame, after IK and the constraints and last before the springs, keeps the forearms
//! and hands out of the body (torso, pelvis, thighs, head), whatever posed them: a clip, a gesture, a tracker, VMC.
//!
//! # The body
//!
//! A [`BodyProxy`] is a few capsules fixed to the body's bones, so they move with it: the torso (hips to neck), the
//! pelvis (hip joint to hip joint), each thigh (hip to knee) and the head (a sphere). [`BodyProxy::fit`] measures them
//! from the character's own skinned mesh, so a kit character and an imported avatar each get their own: each capsule's
//! radius is a high quantile of the distance from its axis of the vertices its bones carry, which takes in clothing
//! that is skinned to the body and leaves out hair and accessories on bones of their own. The forearm's radius and the
//! hand's centre and radius come from the same mesh.
//!
//! # The guard
//!
//! For each arm, the elbow, the forearm and the hand are tested against every capsule. Where one is inside, the wrist
//! target moves out along the contact's normal by the depth plus [`GuardConfig::margin`] (scaled up for a contact near
//! the elbow, which the wrist moves less) and the arm is solved again with the two-bone IK, the hand keeping its turn
//! in the world; an elbow inside swings the pole out. A few rounds settle a contact against two capsules at once.
//!
//! The push is then smoothed in time: it takes effect at once (a frame never shows a hand inside the body), and it
//! lets go over [`GuardConfig::release_s`], so a hand that leaves the body glides back to its animation instead of
//! snapping. An arm with nothing inside and nothing to let go of is not touched, bit for bit.
//!
//! No allocation per frame; the cost is a few capsule tests and at most a handful of two-bone solves per arm.

use omuzaki_anim_api::{BoneId, HumanoidBone, Pose, Skeleton, Transform};
use omuzaki_core::glam::Vec3;

use super::targets::{Hand, HandTarget, bone_world, closest_points, inverse};

/// `p` carried by the rigid transform `t`.
fn point(t: Transform, p: Vec3) -> Vec3 {
    t.translation + t.rotation * p
}

/// A point fixed in a bone's frame: where a capsule end sits on the body.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Anchor {
    /// The bone.
    pub bone: BoneId,
    /// The point in the bone's space, metres.
    pub offset: Vec3,
}

impl Anchor {
    /// Where the anchor is in `pose`, skeleton root space.
    fn at(self, skeleton: &Skeleton, pose: &Pose) -> Vec3 {
        point(bone_world(skeleton, pose, self.bone), self.offset)
    }
}

/// One capsule of the body, between two anchors.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Proxy {
    /// What it stands for, for reports.
    pub part: BodyPart,
    /// One end of the axis.
    pub a: Anchor,
    /// The other end (the same as `a` for a sphere).
    pub b: Anchor,
    /// The radius, metres.
    pub radius: f32,
}

/// The parts of the body the arms are kept out of.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BodyPart {
    /// Hips to neck.
    Torso,
    /// Across the hip joints.
    Pelvis,
    /// The left thigh.
    LeftThigh,
    /// The right thigh.
    RightThigh,
    /// The head.
    Head,
}

/// The body the arms are kept out of, and the arms' own sizes.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyProxy {
    /// The body's capsules.
    pub body: Vec<Proxy>,
    /// The forearm's radius, metres.
    pub forearm_radius: f32,
    /// Each hand's centre, in its hand bone's space, `[left, right]`.
    pub hands: [Vec3; 2],
    /// The hand's radius about its centre, metres.
    pub hand_radius: f32,
}

/// How far out a vertex may be and still count: the share of each part's vertices inside its capsule.
const BODY_QUANTILE: f32 = 0.95;
/// The same for the forearm and the hand, whose extremes are fingers and wrist bones rather than surface.
const LIMB_QUANTILE: f32 = 0.85;

/// The `q` quantile of `values` (which it sorts), or `None` if there are none.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // an index into a short list
fn quantile(values: &mut [f32], q: f32) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    let i = ((values.len() - 1) as f32 * q).round() as usize;
    values.get(i).copied()
}

/// The distance from `p` to the segment `a`-`b`.
fn to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() > 1e-12 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + ab * t)
}

impl BodyProxy {
    /// Measures the body from its skinned mesh: `vertices` are positions in root space with the skeleton in `pose`
    /// (the pose the mesh was built in: the rest pose, or a character's bind pose), each with the bone that carries most
    /// of its weight. `None` if the skeleton is not a humanoid with arms, hips and a spine, or the mesh has no vertices
    /// on the torso or the forearms. Cold: allocates.
    pub fn fit(skeleton: &Skeleton, pose: &Pose, vertices: &[(Vec3, BoneId)]) -> Option<Self> {
        use HumanoidBone as B;
        let map = skeleton.humanoid_map()?;
        let joint = |bone: B| map.get(bone).map(|id| (id, bone_world(skeleton, pose, id)));
        // The vertices carried by any of `bones`.
        let carried = |bones: &[Option<BoneId>]| -> Vec<Vec3> {
            vertices.iter().filter(|(_, b)| bones.contains(&Some(*b))).map(|(p, _)| *p).collect()
        };
        let radius = |points: &[Vec3], a: Vec3, b: Vec3, q: f32| {
            let mut d: Vec<f32> = points.iter().map(|p| to_segment(*p, a, b)).collect();
            quantile(&mut d, q)
        };
        let anchor =
            |(id, world): (BoneId, Transform), at: Vec3| Anchor { bone: id, offset: point(inverse(world), at) };
        let capsule = |part, a: (BoneId, Transform), b: (BoneId, Transform), bones: &[Option<BoneId>]| {
            let points = carried(bones);
            let r = radius(&points, a.1.translation, b.1.translation, BODY_QUANTILE)?;
            Some(Proxy { part, a: anchor(a, a.1.translation), b: anchor(b, b.1.translation), radius: r })
        };

        let hips = joint(B::Hips)?;
        let top = joint(B::Neck).or_else(|| joint(B::Head))?;
        let spine = [map.get(B::Spine), map.get(B::Chest), map.get(B::UpperChest)];
        let mut body = vec![capsule(BodyPart::Torso, hips, top, &spine)?];
        if let (Some(l), Some(r)) = (joint(B::LeftUpperLeg), joint(B::RightUpperLeg)) {
            body.extend(capsule(BodyPart::Pelvis, l, r, &[Some(hips.0)]));
            for (part, upper, knee) in
                [(BodyPart::LeftThigh, l, B::LeftLowerLeg), (BodyPart::RightThigh, r, B::RightLowerLeg)]
            {
                if let Some(knee) = joint(knee) {
                    body.extend(capsule(part, upper, knee, &[Some(upper.0)]));
                }
            }
        }
        if let Some(head) = joint(B::Head) {
            let points = carried(&[Some(head.0)]);
            if !points.is_empty() {
                #[allow(clippy::cast_precision_loss)] // a vertex count
                let centre = points.iter().sum::<Vec3>() / points.len() as f32;
                if let Some(r) = radius(&points, centre, centre, BODY_QUANTILE) {
                    let a = anchor(head, centre);
                    body.push(Proxy { part: BodyPart::Head, a, b: a, radius: r });
                }
            }
        }

        let mut forearm = Vec::new();
        let mut hands = [Vec3::ZERO; 2];
        let mut hand_radii = Vec::new();
        for (i, hand) in [Hand::Left, Hand::Right].into_iter().enumerate() {
            let [_, lower, wrist] = hand.ids(skeleton)?;
            let (elbow, wrist_world) = (bone_world(skeleton, pose, lower), bone_world(skeleton, pose, wrist));
            for p in carried(&[Some(lower)]) {
                forearm.push(to_segment(p, elbow.translation, wrist_world.translation));
            }
            // The hand: the vertices of the hand bone and every bone under it (the fingers).
            let under_hand = |mut b: BoneId| loop {
                if b == wrist {
                    break true;
                }
                match skeleton.bones()[b.index()].parent {
                    Some(parent) => b = parent,
                    None => break false,
                }
            };
            let points: Vec<Vec3> = vertices.iter().filter(|(_, b)| under_hand(*b)).map(|(p, _)| *p).collect();
            if points.is_empty() {
                continue;
            }
            #[allow(clippy::cast_precision_loss)] // a vertex count
            let centre = points.iter().sum::<Vec3>() / points.len() as f32;
            hands[i] = point(inverse(wrist_world), centre);
            hand_radii.extend(points.iter().map(|p| p.distance(centre)));
        }
        let forearm_radius = quantile(&mut forearm, LIMB_QUANTILE)?;
        // Without hand vertices the hand is the wrist, as thick as the forearm.
        let hand_radius = quantile(&mut hand_radii, LIMB_QUANTILE).unwrap_or(forearm_radius);
        Some(Self { body, forearm_radius, hands, hand_radius })
    }
}

/// The guard's settings.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GuardConfig {
    /// The gap the guard leaves between an arm and the body once it pushes, metres.
    pub margin: f32,
    /// How long a push takes to let go, seconds (the time constant of its decay).
    pub release_s: f32,
    /// How deep a contact may be before the guard acts, metres: skin that only grazes is left alone.
    pub tolerance: f32,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self { margin: 0.003, release_s: 0.08, tolerance: 0.001 }
    }
}

/// What the guard did this frame.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct GuardReport {
    /// The smallest gap between an arm and the body as posed, before the guard, metres (negative inside).
    pub before: f32,
    /// The same after the guard.
    pub after: f32,
    /// How far each wrist was moved, `[left, right]`, metres.
    pub moved: [f32; 2],
}

/// The deepest contact of one arm: its gap, the normal out of the body, and where along the forearm it is (`0` at the
/// elbow, `1` at the wrist and for the hand).
#[derive(Copy, Clone, Debug)]
struct Contact {
    gap: f32,
    normal: Vec3,
    along: f32,
}

/// The rounds of re-solving one arm takes at most.
const ROUNDS: usize = 6;
/// A contact this close to the elbow pushes the wrist as if it were here: the wrist cannot move the elbow.
const MIN_ALONG: f32 = 0.35;
/// A push shorter than this is dropped, metres: the last half millimetre of letting go is not worth a solve.
const LET_GO: f32 = 5e-4;
/// How much further than its depth an elbow contact swings the pole out.
const POLE_GAIN: f32 = 4.0;

/// Keeps the arms out of the body: a [`BodyProxy`], the settings, and each arm's push being let go.
#[derive(Clone, Debug)]
pub struct ArmGuard {
    proxy: BodyProxy,
    config: GuardConfig,
    /// Each wrist's push from the last frame, root space.
    held: [Vec3; 2],
}

impl ArmGuard {
    /// A guard for the body `proxy`.
    pub fn new(proxy: BodyProxy, config: GuardConfig) -> Self {
        Self { proxy, config, held: [Vec3::ZERO; 2] }
    }

    /// The body it guards.
    pub fn proxy(&self) -> &BodyProxy {
        &self.proxy
    }

    /// Forgets the pushes being let go.
    pub fn reset(&mut self) {
        self.held = [Vec3::ZERO; 2];
    }

    /// The deepest contact of `hand`'s arm in `pose`.
    #[allow(clippy::many_single_char_names)] // segment ends a, b, p, q and the fractions s, t along them
    fn contact(&self, skeleton: &Skeleton, pose: &Pose, hand: Hand, ids: [BoneId; 3]) -> Contact {
        let [_, lower, wrist] = ids;
        let elbow = bone_world(skeleton, pose, lower).translation;
        let wrist_world = bone_world(skeleton, pose, wrist);
        let palm = point(wrist_world, self.proxy.hands[usize::from(hand == Hand::Right)]);
        let mut deepest = Contact { gap: f32::INFINITY, normal: Vec3::ZERO, along: 1.0 };
        for proxy in &self.proxy.body {
            let (a, b) = (proxy.a.at(skeleton, pose), proxy.b.at(skeleton, pose));
            // The forearm (elbow to wrist, which takes in the elbow) and the hand.
            let parts =
                [(elbow, wrist_world.translation, self.proxy.forearm_radius), (palm, palm, self.proxy.hand_radius)];
            for (k, (p, q, radius)) in parts.into_iter().enumerate() {
                let (s, t) = closest_points(p, q, a, b);
                let (on_arm, on_body) = (p + (q - p) * s, a + (b - a) * t);
                let d = on_arm.distance(on_body);
                let gap = d - radius - proxy.radius;
                if gap < deepest.gap {
                    // Out of the body; on the axis itself, forward.
                    let normal = (on_arm - on_body).try_normalize().unwrap_or(Vec3::Z);
                    deepest = Contact { gap, normal, along: if k == 0 { s } else { 1.0 } };
                }
            }
        }
        deepest
    }

    /// The bend direction of the arm in `pose`: from the shoulder-to-wrist line to the elbow.
    fn pole(skeleton: &Skeleton, pose: &Pose, ids: [BoneId; 3]) -> Vec3 {
        let at = |id| bone_world(skeleton, pose, id).translation;
        let (shoulder, elbow, wrist) = (at(ids[0]), at(ids[1]), at(ids[2]));
        let line = (wrist - shoulder).normalize_or_zero();
        let out = elbow - shoulder;
        (out - line * out.dot(line)).try_normalize().unwrap_or(Vec3::NEG_Y)
    }

    /// Solves `hand`'s arm from the animated `start` (its three local transforms) to put the wrist bone on `target` (its
    /// position, and the hand's turn in the world), bending toward `bend_toward`.
    #[allow(clippy::too_many_arguments)] // one arm's solve, all of it needed
    fn solve(
        skeleton: &Skeleton,
        pose: &mut Pose,
        hand: Hand,
        ids: [BoneId; 3],
        start: [Transform; 3],
        target: Transform,
        bend_toward: Vec3,
    ) {
        for (id, local) in ids.into_iter().zip(start) {
            pose.local[id.index()] = local;
        }
        HandTarget { hand, frame: target, palm: Transform::IDENTITY, pole: bend_toward, weight: 1.0 }
            .solve(skeleton, pose);
    }

    /// Keeps both arms of `pose` out of the body; `dt` is the time since the last frame, seconds (for letting go).
    pub fn apply(&mut self, skeleton: &Skeleton, pose: &mut Pose, dt: f32) -> GuardReport {
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        let keep = (-dt / self.config.release_s.max(1e-3)).exp();
        let mut report = GuardReport { before: f32::INFINITY, after: f32::INFINITY, moved: [0.0; 2] };
        for (i, hand) in [Hand::Left, Hand::Right].into_iter().enumerate() {
            let Some(ids) = hand.ids(skeleton) else { continue };
            let before = self.contact(skeleton, pose, hand, ids);
            report.before = report.before.min(before.gap);
            let held = self.held[i] * keep;
            if before.gap >= -self.config.tolerance && held.length() < LET_GO {
                self.held[i] = Vec3::ZERO;
                report.after = report.after.min(before.gap);
                continue;
            }
            let start = ids.map(|id| pose.local[id.index()]);
            let animated = bone_world(skeleton, pose, ids[2]);
            let bend = Self::pole(skeleton, pose, ids);
            let shoulder = bone_world(skeleton, pose, ids[0]).translation;
            let solve_to = |pose: &mut Pose, push: Vec3, bend: Vec3| {
                let target = Transform { translation: animated.translation + push, rotation: animated.rotation };
                let elbow = bone_world(skeleton, pose, ids[1]).translation;
                Self::solve(skeleton, pose, hand, ids, start, target, shoulder.lerp(elbow, 0.5) + bend);
            };

            // What this frame needs: push the wrist out until nothing is inside.
            let (mut need, mut bend_now) = (Vec3::ZERO, bend);
            let mut contact = before;
            for _ in 0..ROUNDS {
                if contact.gap >= -self.config.tolerance {
                    break;
                }
                let depth = -contact.gap + self.config.margin;
                if contact.along <= 0.0 {
                    bend_now = (bend_now + contact.normal * depth * POLE_GAIN).normalize_or_zero();
                }
                need += contact.normal * depth / contact.along.max(MIN_ALONG);
                solve_to(pose, need, bend_now);
                contact = self.contact(skeleton, pose, hand, ids);
            }
            // What is shown: the larger of the need and the push being let go, so no frame is inside the body.
            let mut shown = need;
            if held.length() > need.length() {
                solve_to(pose, held, bend_now);
                let letting_go = self.contact(skeleton, pose, hand, ids);
                if letting_go.gap >= contact.gap.min(-self.config.tolerance) {
                    shown = held;
                    contact = letting_go;
                } else {
                    solve_to(pose, need, bend_now);
                }
            } else if need == Vec3::ZERO {
                for (id, local) in ids.into_iter().zip(start) {
                    pose.local[id.index()] = local;
                }
            }
            self.held[i] = shown;
            report.moved[i] = shown.length();
            report.after = report.after.min(contact.gap);
        }
        report
    }
}
