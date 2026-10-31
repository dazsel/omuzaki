//! The arm guard (#2353): every frame, after IK and the constraints and last before the springs, keeps the forearms
//! and hands out of the body (torso, pelvis, thighs, head), whatever posed them: a clip, a gesture, a tracker, VMC.
//!
//! # The body
//!
//! A [`BodyProxy`] is a few capsules fixed to the body's bones, so they move with it: the torso (hips to neck), the
//! pelvis (hip joint to hip joint), each thigh (hip to knee) and the head (a sphere). [`BodyProxy::fit`] measures them
//! from the character's own skinned mesh, so a kit character and an imported avatar each get their own: each capsule's
//! radius is a high quantile of the distance from its axis of the vertices its bones carry, which takes in clothing
//! that is skinned to the body and leaves out hair and accessories on bones of their own. The torso is wider than it
//! is deep, so its cross-section is an ellipse: a half width and a half depth, measured the same way, facing the way
//! the body faces (#2414: a round torso as wide as the shoulders held a hand on the chest a hand's breadth off it).
//!
//! Clothes are measured as what they are (#2688). [`BodyProxy::fit_dressed`] fits the body from its skin and the
//! garments that hug it (tights, a fitted top), and a loose garment around the hips (a skirt, a dress, a coat's tails)
//! becomes its own part: a [`BodyPart::Skirt`] hull hung from the hips in two frustums, waist to its widest and on to
//! the hem, each widening by its [`flare`](Proxy::flare). Fitted as a thigh, a flared skirt made each thigh a
//! cylinder as wide as the hem from the hip to the knee and held hanging arms 30 degrees out; against the hull a
//! hanging hand rests on the skirt where it is.
//!
//! The forearm's radius comes from the same mesh, and so does the hand: an ellipsoid about its centre, with a half
//! extent along each of the hand bone's axes, so a palm laid on the chest or on another palm is as thin as a hand
//! and not a ball.
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
use omuzaki_core::glam::{Mat4, Quat, Vec3};

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
    /// The radius, metres: the half width across the body for the torso.
    pub radius: f32,
    /// The half depth, front to back, metres: the same as `radius` for a round part.
    pub depth: f32,
    /// The way the body faces, in `a`'s bone space: which way `depth` is measured.
    pub forward: Vec3,
    /// The width and depth at `b` as a multiple of those at `a`, changing evenly between: 1 for a straight part, more
    /// for a skirt that widens toward its hem.
    pub flare: f32,
}

impl Proxy {
    /// A round part: `depth` the same as `radius`.
    pub fn round(part: BodyPart, a: Anchor, b: Anchor, radius: f32) -> Self {
        Self { part, a, b, radius, depth: radius, forward: Vec3::Z, flare: 1.0 }
    }

    /// The part as it stands in `pose`, skeleton root space.
    pub fn placed(&self, skeleton: &Skeleton, pose: &Pose) -> Obstacle {
        let forward = bone_world(skeleton, pose, self.a.bone).rotation * self.forward;
        Obstacle {
            part: self.part,
            a: self.a.at(skeleton, pose),
            b: self.b.at(skeleton, pose),
            radius: self.radius,
            depth: self.depth,
            forward,
            flare: self.flare,
        }
    }
}

/// A part of a body placed in some space: its axis ends, its half width and half depth and its facing. What the guard
/// tests an arm against: its own body's parts each frame ([`Proxy::placed`]), and another body's ([`ArmGuard::
/// set_obstacles`], #2415), so a hug keeps each forearm out of the other's back.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Obstacle {
    /// What it stands for.
    pub part: BodyPart,
    /// One end of the axis.
    pub a: Vec3,
    /// The other end (the same as `a` for a sphere).
    pub b: Vec3,
    /// The half width, metres.
    pub radius: f32,
    /// The half depth along `forward`, metres.
    pub depth: f32,
    /// The way the part faces (unit).
    pub forward: Vec3,
    /// The width and depth at `b` as a multiple of those at `a` ([`Proxy::flare`]); 1 for a straight part.
    pub flare: f32,
}

impl Obstacle {
    /// The obstacle carried by the rigid transform `m` (rotation and translation; no scale).
    #[must_use]
    pub fn moved(&self, m: Mat4) -> Self {
        Self {
            a: m.transform_point3(self.a),
            b: m.transform_point3(self.b),
            forward: m.transform_vector3(self.forward).normalize_or_zero(),
            ..*self
        }
    }

    /// Whether every field is finite.
    pub fn is_finite(&self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.forward.is_finite()
            && self.radius.is_finite()
            && self.depth.is_finite()
            && self.flare.is_finite()
    }

    /// The gap between `p` (a point with `own` radius toward the obstacle) and the surface, and the normal out of it.
    pub fn gap_to(&self, p: Vec3, own: f32) -> (f32, Vec3) {
        let (_, t) = closest_points(p, p, self.a, self.b);
        let on = self.a + (self.b - self.a) * t;
        let normal = (p - on).try_normalize().unwrap_or(self.forward);
        (p.distance(on) - own - self.across(normal, self.b - self.a, self.forward, t), normal)
    }

    /// How far the surface is from the axis toward `out` (a unit direction from the axis) at `t` along it (0 at `a`, 1
    /// at `b`), metres: on the ellipse of `radius` across and `depth` along `forward`, for the part of `out` square to
    /// `axis`, widened by the [`flare`](Self::flare) that far along.
    fn across(&self, out: Vec3, axis: Vec3, forward: Vec3, t: f32) -> f32 {
        let widen = 1.0 + (self.flare - 1.0) * t.clamp(0.0, 1.0);
        if (self.depth - self.radius).abs() <= f32::EPSILON * self.radius {
            return self.radius * widen;
        }
        let axis = axis.normalize_or_zero();
        let Some(front) = (forward - axis * forward.dot(axis)).try_normalize() else { return self.radius * widen };
        let side = axis.cross(front);
        let (f, s) = (out.dot(front), out.dot(side));
        let square = (f * f + s * s).sqrt();
        if square < 1e-6 {
            return self.radius.min(self.depth) * widen;
        }
        let (f, s) = (f / square, s / square);
        self.radius * self.depth / ((self.depth * s).powi(2) + (self.radius * f).powi(2)).sqrt() * widen
    }
}

/// How far the surface of an ellipsoid with half extents `axes` (in the frame `rotation`) is from its centre toward
/// `out` (a unit direction, root space), metres.
fn ellipsoid_toward(axes: Vec3, rotation: Quat, out: Vec3) -> f32 {
    let local = rotation.inverse() * out;
    let scaled = local / axes.max(Vec3::splat(1e-4));
    let length = scaled.length();
    if length > 1e-6 { 1.0 / length } else { axes.max_element() }
}

/// The parts of the body the arms are kept out of.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BodyPart {
    /// The upper torso, from where it bends (the waist) to the neck.
    Torso,
    /// The lower torso, from the hips to where it bends.
    Waist,
    /// Across the hip joints.
    Pelvis,
    /// The left thigh.
    LeftThigh,
    /// The right thigh.
    RightThigh,
    /// The head.
    Head,
    /// A loose garment hung from the hips (a skirt, a dress): one of its frustums ([`BodyProxy::fit_dressed`]).
    Skirt,
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
    /// Each hand's half extents about its centre along its hand bone's axes, metres, `[left, right]`.
    pub hand_axes: [Vec3; 2],
}

/// How far out a vertex may be and still count: the share of each part's vertices inside its capsule.
const BODY_QUANTILE: f32 = 0.95;
/// The same for the forearm, whose extremes are wrist bones rather than surface.
const LIMB_QUANTILE: f32 = 0.85;
/// The share of a hand's vertices within each of its half extents: the fingertips and the knuckles are the surface.
const HAND_QUANTILE: f32 = 0.97;
/// The thinnest a hand's half extent is taken to be, metres.
const MIN_HAND_AXIS: f32 = 0.004;

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
    #[allow(clippy::too_many_lines)] // one measurement per part, in the body's order
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

        let hips = joint(B::Hips)?;
        let top = joint(B::Neck).or_else(|| joint(B::Head))?;
        let spine = [map.get(B::Spine), map.get(B::Chest), map.get(B::UpperChest)];
        // The torso tapers: hips, a waist, a chest and shoulders. One ellipse as wide as the widest held an elbow at the
        // waist out by the shoulders' width (#2688), so it is two frustums bent where the body is narrowest or widest.
        // Each ring rides on the last bone of the spine at or below it, so the torso bends with the spine.
        let chain: Vec<(BoneId, Transform)> = std::iter::once(hips)
            .chain([B::Spine, B::Chest, B::UpperChest].into_iter().filter_map(joint))
            .chain(std::iter::once(top))
            .collect();
        let riding_on = |at: Vec3| {
            let up = top.1.translation - hips.1.translation;
            let height = |p: Vec3| (p - hips.1.translation).dot(up);
            chain.iter().rev().find(|(_, w)| height(w.translation) <= height(at) + 1e-5).copied().unwrap_or(hips)
        };
        let mut body = hull(
            &carried(&spine),
            (hips.1.translation, top.1.translation),
            riding_on,
            [BodyPart::Waist, BodyPart::Torso],
        );
        if body.is_empty() {
            return None;
        }
        if let (Some(l), Some(r)) = (joint(B::LeftUpperLeg), joint(B::RightUpperLeg)) {
            // The pelvis: an ellipse like the torso's, on the body's up axis between the hip joints, over the height
            // of what the hips carry (a capsule across the hip joints had to be as thick as the hips are tall, and
            // stood a hand's breadth out of their sides).
            let points = carried(&[Some(hips.0)]);
            let base = (l.1.translation + r.1.translation) * 0.5;
            let (low, high) = points.iter().fold((base.y, base.y), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
            let ends = (Vec3::new(base.x, low, base.z), Vec3::new(base.x, high, base.z));
            body.extend(elliptic(&points, ends, hips.1.rotation).map(|(radius, depth, forward)| Proxy {
                part: BodyPart::Pelvis,
                a: anchor(hips, ends.0),
                b: anchor(hips, ends.1),
                radius,
                depth,
                forward,
                flare: 1.0,
            }));
            // Each thigh tapers from the hip to the knee, and is deeper than wide where it meets the seat: two
            // frustums like the torso's (a round capsule as thick as the top of the thigh stood 5 cm off its side).
            for (part, upper, knee) in
                [(BodyPart::LeftThigh, l, B::LeftLowerLeg), (BodyPart::RightThigh, r, B::RightLowerLeg)]
            {
                if let Some(knee) = joint(knee) {
                    let ends = (upper.1.translation, knee.1.translation);
                    body.extend(hull(&carried(&[Some(upper.0)]), ends, |_| upper, [part; 2]));
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
                    body.push(Proxy::round(BodyPart::Head, a, a, r));
                }
            }
        }

        let mut forearm = Vec::new();
        let mut hands = [Vec3::ZERO; 2];
        let mut hand_axes = [Vec3::ZERO; 2];
        for (i, hand) in [Hand::Left, Hand::Right].into_iter().enumerate() {
            let [_, lower, wrist] = hand.ids(skeleton)?;
            let (elbow, wrist_world) = (bone_world(skeleton, pose, lower), bone_world(skeleton, pose, wrist));
            for p in carried(&[Some(lower)]) {
                forearm.push(to_segment(p, elbow.translation, wrist_world.translation));
            }
            // The hand: the vertices of the hand bone and every bone under it (the fingers), less the thumb, which
            // stands off the palm and would make a flat hand as thick as a fist.
            let thumb = thumb_bones(map, hand);
            let under_hand = |mut b: BoneId| {
                !thumb.contains(&Some(b))
                    && loop {
                        if b == wrist {
                            break true;
                        }
                        match skeleton.bones()[b.index()].parent {
                            Some(parent) => b = parent,
                            None => break false,
                        }
                    }
            };
            let points: Vec<Vec3> = vertices.iter().filter(|(_, b)| under_hand(*b)).map(|(p, _)| *p).collect();
            if points.is_empty() {
                continue;
            }
            #[allow(clippy::cast_precision_loss)] // a vertex count
            let centre = points.iter().sum::<Vec3>() / points.len() as f32;
            let to_hand = inverse(wrist_world);
            hands[i] = point(to_hand, centre);
            let local: Vec<Vec3> = points.iter().map(|p| to_hand.rotation * (*p - centre)).collect();
            let extent = |pick: fn(Vec3) -> f32| {
                let mut d: Vec<f32> = local.iter().map(|v| pick(*v).abs()).collect();
                quantile(&mut d, HAND_QUANTILE).unwrap_or(0.0).max(MIN_HAND_AXIS)
            };
            hand_axes[i] = Vec3::new(extent(|v| v.x), extent(|v| v.y), extent(|v| v.z));
        }
        let forearm_radius = quantile(&mut forearm, LIMB_QUANTILE)?;
        // Without hand vertices the hand is the wrist, as thick as the forearm.
        for axes in &mut hand_axes {
            if *axes == Vec3::ZERO {
                *axes = Vec3::splat(forearm_radius);
            }
        }
        Some(Self { body, forearm_radius, hands, hand_axes })
    }

    /// [`fit`](Self::fit) for a dressed body: `skin` are the body's own vertices (and anything else that is part of
    /// it: the head, hair, boots), `garments` the clothes' (#2688). A garment vertex on the hips or a thigh that stands
    /// off the bare body by more than [`LOOSE`] of the part's radius is loose (a skirt, a dress); the rest hug the body
    /// and are fitted with it (tights, a fitted top, the waistband). The loose ones become a [`BodyPart::Skirt`] hull in
    /// two frustums ([`Self::skirt`]) instead of widening the thighs. `None` as [`fit`](Self::fit) is for `skin`.
    /// Cold: allocates.
    pub fn fit_dressed(
        skeleton: &Skeleton,
        pose: &Pose,
        skin: &[(Vec3, BoneId)],
        garments: &[(Vec3, BoneId)],
    ) -> Option<Self> {
        use HumanoidBone as B;
        let bare = Self::fit(skeleton, pose, skin)?;
        let map = skeleton.humanoid_map()?;
        let lower = [map.get(B::Hips), map.get(B::LeftUpperLeg), map.get(B::RightUpperLeg)];
        // The bare parts each lower bone's garment vertices are measured against: a thigh's own, the hips' both.
        let placed =
            |part: BodyPart| bare.body.iter().filter(move |p| p.part == part).map(|p| p.placed(skeleton, pose));
        let against = [
            placed(BodyPart::Pelvis).chain(placed(BodyPart::Torso)).collect::<Vec<_>>(),
            placed(BodyPart::LeftThigh).collect(),
            placed(BodyPart::RightThigh).collect(),
        ];
        let mut fitted = skin.to_vec();
        let mut loose = Vec::new();
        for &(p, bone) in garments {
            let Some(k) = lower.iter().position(|b| *b == Some(bone)) else {
                fitted.push((p, bone));
                continue;
            };
            let off = against[k].iter().map(|o| o.gap_to(p, 0.0).0 / o.radius.max(1e-3)).fold(f32::INFINITY, f32::min);
            if off > LOOSE {
                loose.push(p);
            } else {
                fitted.push((p, bone));
            }
        }
        let mut proxy = Self::fit(skeleton, pose, &fitted)?;
        if let Some(hips) = map.get(B::Hips) {
            proxy.body.extend(Self::skirt(&loose, hips, bone_world(skeleton, pose, hips)));
        }
        Some(proxy)
    }

    /// The hull of a loose garment's `points` (root space, the hips at `hips_world`): [`hull`]'s two frustums hung
    /// from the hips along the body's down axis through the garment's middle, from its top to its hem. Nothing for
    /// fewer than [`MIN_SKIRT_POINTS`] points or a garment with no height.
    fn skirt(points: &[Vec3], hips: BoneId, hips_world: Transform) -> Vec<Proxy> {
        if points.len() < MIN_SKIRT_POINTS {
            return Vec::new();
        }
        #[allow(clippy::cast_precision_loss)] // a vertex count
        let centre = points.iter().sum::<Vec3>() / points.len() as f32;
        let (top, hem) = points.iter().fold((f32::NEG_INFINITY, f32::INFINITY), |(t, h), p| (t.max(p.y), h.min(p.y)));
        if top - hem < 1e-3 {
            return Vec::new();
        }
        let ends = (Vec3::new(centre.x, top, centre.z), Vec3::new(centre.x, hem, centre.z));
        hull(points, ends, |_| (hips, hips_world), [BodyPart::Skirt; 2])
    }

    /// The hand's radius as one number: its largest half extent, metres.
    pub fn hand_radius(&self) -> f32 {
        self.hand_axes[0].max(self.hand_axes[1]).max_element()
    }
}

/// The thumb's bones on `hand`'s side.
fn thumb_bones(map: &omuzaki_anim_api::HumanoidMap, hand: Hand) -> [Option<BoneId>; 3] {
    use HumanoidBone as B;
    let bones = match hand {
        Hand::Left => [B::LeftThumbMetacarpal, B::LeftThumbProximal, B::LeftThumbDistal],
        Hand::Right => [B::RightThumbMetacarpal, B::RightThumbProximal, B::RightThumbDistal],
    };
    bones.map(|b| map.get(b))
}

/// The torso's ellipse, fitted to `points` about the axis from `a` to `b` (the hips and the neck, posed as the points
/// are): `(half width, half depth, the body's facing in a's bone space)`, or `None` without points. The facing is the
/// root's `+Z` (VRM normalised space: the body faces `+Z`) made square to the axis. The depth-to-width ratio comes from
/// the quantiles of each, and the size from the quantile of each point's distance on the ellipse of that ratio.
fn elliptic(points: &[Vec3], (from, to): (Vec3, Vec3), a_rotation: Quat) -> Option<(f32, f32, Vec3)> {
    let axis = (to - from).try_normalize().unwrap_or(Vec3::Y);
    let front = (Vec3::Z - axis * Vec3::Z.dot(axis)).try_normalize().unwrap_or(Vec3::Z);
    let side = axis.cross(front);
    let along = to - from;
    let flat: Vec<(f32, f32)> = points
        .iter()
        .map(|p| {
            let t = if along.length_squared() > 1e-12 {
                ((*p - from).dot(along) / along.length_squared()).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let v = *p - (from + along * t);
            (v.dot(side), v.dot(front))
        })
        .collect();
    let mut wide: Vec<f32> = flat.iter().map(|(s, _)| s.abs()).collect();
    let mut deep: Vec<f32> = flat.iter().map(|(_, f)| f.abs()).collect();
    let (wide, deep) = (quantile(&mut wide, BODY_QUANTILE)?, quantile(&mut deep, BODY_QUANTILE)?);
    let ratio = if wide > 1e-6 { (deep / wide).clamp(MIN_DEPTH_RATIO, 1.0 / MIN_DEPTH_RATIO) } else { 1.0 };
    let mut on_ellipse: Vec<f32> = flat.iter().map(|(s, f)| s.hypot(f / ratio)).collect();
    let radius = quantile(&mut on_ellipse, BODY_QUANTILE)?;
    Some((radius, radius * ratio, a_rotation.inverse() * front))
}

/// Two frustums about the axis `from`-`to` (root space) that hold [`BODY_QUANTILE`] of `points`, each end anchored on
/// `carrier` (the bone a point on the axis rides on, and its transform) and labelled `parts`: from `from` to the bend
/// and from the bend to `to`. The bend is the ring that stands farthest in or out of the straight line from one end's
/// ring to the other's (a waist between hips and chest, the hips where an A-line skirt stops flaring fast); each ring
/// is the quantile of the points in a band about it, on an ellipse whose depth-to-width ratio is the points' own, and
/// each frustum is raised until the quantile of its points is inside. The hull spans only the stretch of the axis the
/// points cover. Empty without points.
fn hull(
    points: &[Vec3],
    (from, to): (Vec3, Vec3),
    carrier: impl Fn(Vec3) -> (BoneId, Transform),
    parts: [BodyPart; 2],
) -> Vec<Proxy> {
    let along = to - from;
    if along.length_squared() < 1e-8 {
        return Vec::new();
    }
    let axis = along.normalize();
    let front = (Vec3::Z - axis * Vec3::Z.dot(axis)).try_normalize().unwrap_or(Vec3::Z);
    let side = axis.cross(front);
    // Each point as (how far along, 0 to 1; across; ahead).
    let flat: Vec<(f32, f32, f32)> = points
        .iter()
        .map(|p| {
            let t = ((*p - from).dot(along) / along.length_squared()).clamp(0.0, 1.0);
            let v = *p - (from + along * t);
            (t, v.dot(side), v.dot(front))
        })
        .collect();
    let mut wide: Vec<f32> = flat.iter().map(|(_, s, _)| s.abs()).collect();
    let mut deep: Vec<f32> = flat.iter().map(|(_, _, f)| f.abs()).collect();
    let (Some(wide), Some(deep)) = (quantile(&mut wide, BODY_QUANTILE), quantile(&mut deep, BODY_QUANTILE)) else {
        return Vec::new();
    };
    let ratio = if wide > 1e-6 { (deep / wide).clamp(MIN_DEPTH_RATIO, 1.0 / MIN_DEPTH_RATIO) } else { 1.0 };
    // The hull runs over the points' own span of the axis, `lo` to `hi`; each point as (how far along that span; how
    // far out on the ellipse).
    let (lo, hi) = flat.iter().fold((1.0_f32, 0.0_f32), |(lo, hi), (t, _, _)| (lo.min(*t), hi.max(*t)));
    if hi - lo < 1e-3 {
        return Vec::new();
    }
    let profile: Vec<(f32, f32)> = flat.iter().map(|(t, s, f)| ((t - lo) / (hi - lo), s.hypot(f / ratio))).collect();
    let ring = |at: f32| {
        let mut d: Vec<f32> = profile.iter().filter(|(t, _)| (t - at).abs() <= HULL_BAND).map(|(_, d)| *d).collect();
        quantile(&mut d, BODY_QUANTILE)
    };
    let (Some(r_from), Some(r_to)) = (ring(0.0), ring(1.0)) else { return Vec::new() };
    let off_line = |(t, r): (f32, f32)| (r - r_from - (r_to - r_from) * t).abs();
    #[allow(clippy::cast_precision_loss)] // a handful of rings
    let (bend, r_bend) = (1..HULL_RINGS)
        .map(|k| k as f32 / HULL_RINGS as f32)
        .filter_map(|t| ring(t).map(|r| (t, r)))
        .max_by(|a, b| off_line(*a).total_cmp(&off_line(*b)))
        .unwrap_or((0.5, f32::midpoint(r_from, r_to)));
    let anchor = |t: f32| {
        let at = from + along * (lo + (hi - lo) * t);
        let (bone, world) = carrier(at);
        (Anchor { bone, offset: point(inverse(world), at) }, world.rotation.inverse() * front)
    };
    // A part that hardly bends is one frustum: half the tests a frame for the same fit. It goes by the upper part's
    // name (a straight torso is the torso, not the waist).
    let two: [Span; 2] = [(0.0, bend, r_from, r_bend), (bend, 1.0, r_bend, r_to)];
    let one: [Span; 1] = [(0.0, 1.0, r_from, r_to)];
    let (spans, names): (&[Span], &[BodyPart]) =
        if off_line((bend, r_bend)) < STRAIGHT { (&one, &parts[1..]) } else { (&two, &parts) };
    let mut out = Vec::with_capacity(2);
    for (&part, &(t0, t1, r0, r1)) in names.iter().zip(spans) {
        let mut over: Vec<f32> = profile
            .iter()
            .filter(|(t, _)| (t0..=t1).contains(t))
            .map(|(t, d)| d - (r0 + (r1 - r0) * (t - t0) / (t1 - t0).max(1e-6)))
            .collect();
        let raise = quantile(&mut over, BODY_QUANTILE).unwrap_or(0.0).max(0.0);
        let (r0, r1) = (r0 + raise, r1 + raise);
        if r0 < 1e-4 {
            continue;
        }
        let ((a, forward), (b, _)) = (anchor(t0), anchor(t1));
        out.push(Proxy { part, a, b, radius: r0, depth: r0 * ratio, forward, flare: r1 / r0 });
    }
    out
}

/// How far a garment vertex on the hips or a thigh may stand off the bare body, as a share of the nearest part's
/// radius, and still hug it ([`BodyProxy::fit_dressed`]).
pub const LOOSE: f32 = 0.25;
/// The fewest loose vertices that make a skirt.
const MIN_SKIRT_POINTS: usize = 16;
/// The rings tried along a [`hull`] for its bend, end to end.
const HULL_RINGS: usize = 8;
/// One frustum of a [`hull`]: where it starts and ends along the hull (0 to 1) and its radius at each end.
type Span = (f32, f32, f32, f32);

/// How far a [`hull`]'s bend may stand off the straight line between its ends, metres, and the hull still be one
/// frustum.
const STRAIGHT: f32 = 0.004;
/// The half length of the band of points a [`hull`]'s ring is measured from, as a share of its length.
const HULL_BAND: f32 = 0.125;

/// The flattest a torso's ellipse is taken to be, depth to width (and the inverse).
const MIN_DEPTH_RATIO: f32 = 0.35;

/// The least share of its fitted radius a capsule keeps when [`BodyProxy::clear_of`] shrinks it.
const MIN_SHRINK: f32 = 0.5;

impl BodyProxy {
    /// The smallest gap between either arm (forearm and hand) and `proxy` in `pose`.
    fn arm_gap(&self, skeleton: &Skeleton, pose: &Pose, proxy: &Proxy) -> f32 {
        let placed = proxy.placed(skeleton, pose);
        let mut gap = f32::INFINITY;
        for (i, hand) in [Hand::Left, Hand::Right].into_iter().enumerate() {
            let Some(ids) = hand.ids(skeleton) else { continue };
            gap = gap.min(self.touch(&placed, self.arm(skeleton, pose, i, ids), i).0);
        }
        gap
    }

    /// The elbow, the wrist bone's transform and the palm's centre of hand `i` (bones `ids`) in `pose`.
    fn arm(&self, skeleton: &Skeleton, pose: &Pose, i: usize, ids: [BoneId; 3]) -> (Vec3, Transform, Vec3) {
        let elbow = bone_world(skeleton, pose, ids[1]).translation;
        let wrist = bone_world(skeleton, pose, ids[2]);
        (elbow, wrist, point(wrist, self.hands[i]))
    }

    /// The body's parts as they stand in `pose`, skeleton root space: what another body's guard keeps its arms out
    /// of ([`ArmGuard::set_obstacles`]).
    pub fn obstacles<'a>(&'a self, skeleton: &'a Skeleton, pose: &'a Pose) -> impl Iterator<Item = Obstacle> + 'a {
        self.body.iter().map(move |p| p.placed(skeleton, pose))
    }

    /// The closest hand `i`'s arm (its [`arm`](Self::arm)) comes to `obstacle`: `(gap, the normal out of it, the
    /// fraction along the forearm, 1 for the hand)`.
    #[allow(clippy::many_single_char_names)] // segment ends a, b, p, q and the fractions s, t along them
    fn touch(&self, obstacle: &Obstacle, (elbow, wrist, palm): (Vec3, Transform, Vec3), i: usize) -> (f32, Vec3, f32) {
        let Obstacle { a, b, forward, .. } = *obstacle;
        let mut best = (f32::INFINITY, Vec3::Z, 1.0);
        for (k, (p, q)) in [(elbow, wrist.translation), (palm, palm)].into_iter().enumerate() {
            let (s, t) = closest_points(p, q, a, b);
            let (on_arm, on_body) = (p + (q - p) * s, a + (b - a) * t);
            // Out of the body; on the axis itself, forward.
            let normal = (on_arm - on_body).try_normalize().unwrap_or(forward);
            let own =
                if k == 0 { self.forearm_radius } else { ellipsoid_toward(self.hand_axes[i], wrist.rotation, normal) };
            let gap = on_arm.distance(on_body) - own - obstacle.across(normal, b - a, forward, t);
            if gap < best.0 {
                best = (gap, normal, if k == 0 { s } else { 1.0 });
            }
        }
        best
    }

    /// Shrinks every capsule the arms touch in `pose` until they clear it by `margin`, keeping at least half its fitted
    /// radius: the body as built is never in collision with itself. A quantile fit takes in the soft flesh and clothes an
    /// arm hangs against (a chibi's arms rest on its hips), so call this with the character's own bind pose, or the
    /// guard would push its resting arms away. Cold.
    pub fn clear_of(&mut self, skeleton: &Skeleton, pose: &Pose, margin: f32) {
        for k in 0..self.body.len() {
            let gap = self.arm_gap(skeleton, pose, &self.body[k]);
            if gap < margin {
                let proxy = &mut self.body[k];
                let radius = (proxy.radius - (margin - gap)).max(proxy.radius * MIN_SHRINK);
                proxy.depth *= radius / proxy.radius;
                proxy.radius = radius;
            }
        }
    }
}

/// One skinned mesh, as [`BodyProxy::gather`] reads it: positions in its bind space, four joints and weights per vertex,
/// and the bone each joint index means (`None` for a joint the skeleton lacks).
#[derive(Copy, Clone, Debug)]
pub struct SkinnedMesh<'a> {
    /// Vertex positions, bind space.
    pub positions: &'a [Vec3],
    /// Four joint indices per vertex.
    pub joints: &'a [[u16; 4]],
    /// Four weights per vertex.
    pub weights: &'a [[f32; 4]],
    /// The bone of each joint index.
    pub bones: &'a [Option<BoneId>],
}

impl BodyProxy {
    /// Adds `mesh`'s vertices to `out` in the skeleton's rest pose, each with the bone that carries most of its weight:
    /// `unbind(joint, bone)` is the joint's inverse bind matrix (bind space to the bone's space; `None` drops the
    /// vertex), and the bone's rest transform takes it on to the rest pose. `rest` is the skeleton's rest pose in root
    /// space ([`Skeleton::world_pose`] of [`Skeleton::rest_pose`]). For [`fit`](Self::fit) with the rest pose. Cold.
    pub fn gather(
        rest: &Pose,
        mesh: SkinnedMesh<'_>,
        unbind: impl Fn(usize, BoneId) -> Option<Mat4>,
        out: &mut Vec<(Vec3, BoneId)>,
    ) {
        for ((p, joints), weights) in mesh.positions.iter().zip(mesh.joints).zip(mesh.weights) {
            let Some(k) = (0..4).max_by(|&a, &b| weights[a].total_cmp(&weights[b])) else { continue };
            let joint = usize::from(joints[k]);
            let Some(bone) = mesh.bones.get(joint).copied().flatten() else { continue };
            let Some(unbind) = unbind(joint, bone) else { continue };
            let t = rest.local[bone.index()];
            let to_rest = Mat4::from_rotation_translation(t.rotation, t.translation) * unbind;
            out.push((to_rest.transform_point3(*p), bone));
        }
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
/// The halvings that take back a push larger than the arm needs: the last leaves it within 1/16 of the push.
const SETTLE_STEPS: usize = 4;
/// The gap a push may leave, metres, before it is taken back: less is not seen.
const SETTLE_OVER: f32 = 0.004;
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
    /// Other bodies' parts the arms are kept out of too, root space ([`set_obstacles`](Self::set_obstacles)).
    others: [Option<Obstacle>; MAX_OBSTACLES],
}

/// How many parts of other bodies the guard takes ([`ArmGuard::set_obstacles`]): two partners' five parts and more.
pub const MAX_OBSTACLES: usize = 12;

impl ArmGuard {
    /// A guard for the body `proxy`.
    pub fn new(proxy: BodyProxy, config: GuardConfig) -> Self {
        Self { proxy, config, held: [Vec3::ZERO; 2], others: [None; MAX_OBSTACLES] }
    }

    /// The body it guards.
    pub fn proxy(&self) -> &BodyProxy {
        &self.proxy
    }

    /// Keeps the arms out of `obstacles` too (skeleton root space, from the next [`apply`](Self::apply)), as well as the
    /// body: another cast member's parts, placed this frame. The first [`MAX_OBSTACLES`] finite ones count; an empty
    /// slice clears them. Allocation-free.
    pub fn set_obstacles(&mut self, obstacles: &[Obstacle]) {
        let mut finite = obstacles.iter().filter(|o| o.is_finite());
        for slot in &mut self.others {
            *slot = finite.next().copied();
        }
    }

    /// The other bodies' parts the guard keeps the arms out of.
    pub fn obstacles(&self) -> impl Iterator<Item = &Obstacle> {
        self.others.iter().flatten()
    }

    /// Forgets the pushes being let go.
    pub fn reset(&mut self) {
        self.held = [Vec3::ZERO; 2];
    }

    /// The deepest contact of `hand`'s arm in `pose`: the forearm (elbow to wrist, which takes in the elbow) and the
    /// hand, against each part of the body.
    fn contact(&self, skeleton: &Skeleton, pose: &Pose, hand: Hand, ids: [BoneId; 3]) -> Contact {
        let i = usize::from(hand == Hand::Right);
        let arm = self.proxy.arm(skeleton, pose, i, ids);
        let mut deepest = Contact { gap: f32::INFINITY, normal: Vec3::ZERO, along: 1.0 };
        let own = self.proxy.body.iter().map(|p| p.placed(skeleton, pose));
        for obstacle in own.chain(self.others.iter().flatten().copied()) {
            let (gap, normal, along) = self.proxy.touch(&obstacle, arm, i);
            if gap < deepest.gap {
                deepest = Contact { gap, normal, along };
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
            // How far a point `along` the forearm moves for the wrist's move, the arm turning about the shoulder: a
            // push divided by it puts that point, not the wrist, out by the push (#2688: dividing by `along` itself
            // pushed a forearm contact a fifth too far, and a hanging arm stood off the hip by two centimetres).
            let upper = shoulder.distance(bone_world(skeleton, pose, ids[1]).translation);
            let reach = upper + bone_world(skeleton, pose, ids[1]).translation.distance(animated.translation);
            let fore = reach - upper;
            let lever = |along: f32| if reach > 1e-6 { (upper + along * fore) / reach } else { 1.0 };
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
                need += contact.normal * depth / lever(contact.along).max(MIN_ALONG);
                solve_to(pose, need, bend_now);
                contact = self.contact(skeleton, pose, hand, ids);
            }
            // The rounds push along each contact's normal in turn, and two contacts (a forearm on the hip, the hand on
            // the thigh) add up past what either needs: take back the share of the push that isn't needed, so the arm
            // rests on the body, not a few centimetres off it (#2688).
            if need != Vec3::ZERO && contact.gap > SETTLE_OVER {
                let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
                for _ in 0..SETTLE_STEPS {
                    let mid = f32::midpoint(lo, hi);
                    solve_to(pose, need * mid, bend_now);
                    if self.contact(skeleton, pose, hand, ids).gap >= self.config.margin * 0.5 {
                        hi = mid;
                    } else {
                        lo = mid;
                    }
                }
                need *= hi;
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
