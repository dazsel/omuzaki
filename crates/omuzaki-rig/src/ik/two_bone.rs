//! Analytic two-bone IK for arms and legs (RG-05): bring a hand or foot to a target by bending the elbow or knee.
//!
//! # The solve
//!
//! For a chain `root → mid → tip` (shoulder, elbow, wrist or hip, knee, ankle) with segment lengths `l1`, `l2`, the
//! tip can sit at any distance `d` from the root between the hinge's fully bent and fully straight positions. The law
//! of cosines gives the mid joint's place on the root-to-target line (`along`) and its height off it (`height`); the
//! **pole** picks which way round the line that height points. Then two minimal rotations carry the animated bones
//! onto that triangle: the upper bone swings the mid joint into place, the lower bone swings the tip onto the line.
//! Only the two rotations change, so bone lengths and every translation are exactly as animated.
//!
//! - **Pole.** [`Limb::pole`] is a point the bend faces. Its component along the root-to-target line is ignored. If it
//!   sits on that line (no side to pick) the bend keeps the side the animated limb already had, then the default of the
//!   limb ([`ELBOW_BEND`], [`KNEE_BEND`]). The side is a continuous function of the pole and the target, so a target
//!   sweeping all the way round the root never flips it.
//! - **Soft reach.** The last [`SOFT_REACH`] of the extension is eased exponentially, so the distance approaches the
//!   straight limb smoothly and the limb never snaps straight as a target crosses its reach. Targets inside 98% of the
//!   reach are hit exactly; one far outside gives a straight limb pointing at the target.
//! - **Hinge limits.** The elbow or knee bends `0..=`[`MAX_FLEXION`] and never hyperextends: the tip is never closer
//!   than the fully bent hinge allows, nor farther than straight.
//! - **Weight.** [`Limb::weight`] blends the limb's *configuration* (aim direction, reach distance, bend side) from the
//!   animated one to the solved one along a rigid path, then builds the triangle, so the hinge limit holds at every
//!   weight (blending the two joint rotations separately can fold the elbow past it). `0` is the identity bit for bit,
//!   and **for any fixed target the pose is continuous in the weight over `0..=1`, outside singular set 3**, so a fade never pops.
//!   One case needs a rule: when the solved aim, or the solved bend side, is *exactly opposite* the animated one there
//!   is no shortest way round, and no blend can be continuous in both the weight and the target (a continuous family
//!   from "the animated limb" to "the solved limb" does not exist there). The aim then sweeps through the plane of the
//!   animated aim and the pole, and the bend side turns counter-clockwise. So at a fixed partial weight, a target
//!   crossing the exact opposite of the animated aim or bend (within [`OPPOSITE_SIN`]) makes the elbow jump to the
//!   other way round; the jump is bounded by the limb's own size (see the singular sets below).
//! - **Singular sets.** Exactly three, each unavoidable (a continuous family from one configuration to another that
//!   passes through a reference direction which vanishes or reverses does not exist), and each bounded by a test:
//!   1. *A target at the exact opposite of the animated aim or bend, at a fixed partial weight* (above). The elbow goes
//!      the other way round: it moves at most the chord `2·l1·sin(π·w)`, measured 0.15, 0.34, 0.49, 0.34 and 0.15 m on a
//!      0.27 m upper arm at weights 0.1, 0.25, 0.5, 0.75 and 0.9. Nearby, a bone's roll follows the target at up to
//!      `3 / sin(angle from the opposite)` times the target's own step (the sphere sweep asserts it).
//!   2. *A partly faded-in animated bend whose side is exactly opposite the solved plane.* A bone's roll is carried from
//!      a reference plane that is swept from the animated limb's bend plane toward the solved one, by `τ = (1 - fade) ·
//!      sweep`, where `fade` rises from 0 to 1 as the bend grows past [`NEAR_STRAIGHT`] (the elbow's offset from the shoulder-wrist
//!      line as a share of the shorter bone, about 6 degrees; the direction of a bend that shrinks to nothing has no limit) and `sweep` is 0 at weight
//!      0 and 1 at weight 1. The sweep goes by angle, counter-clockwise when the two planes are exactly opposite, so
//!      for a fixed target, pole and animated pose the roll is continuous in the weight over `0..=1`. As the animated
//!      bend direction turns once round the limb it crosses the solved plane's opposite once, and the roll reference
//!      jumps there by `2π · min(τ, 1 - τ)` (at most half a turn): measured 3.142, 2.211 and 0.653 rad at fade shares
//!      0.5, 0.35 and 0.10 at weight 1, matching the formula to three decimals. Everywhere else it is continuous with a
//!      gain of at most about seven (measured 0.0012 rad per 0.00017 rad of heading).
//!   3. *The animated wrist within `NO_AIM` (2 micrometres) of the shoulder* (pose space only). A limb folded flat
//!      with equal bones has its wrist on its shoulder, so the shoulder-wrist vector has no direction (below `NO_AIM`, 2 micrometres, it is rounding noise).
//!      The aim is then taken perpendicular to the upper bone (the pole's side, else the target's, else any), so the
//!      elbow starts exactly where the animation put it. With the target along the animated elbow no solution is
//!      near it at weight 0+: the wrist moves to `w · T` and `E′·W = (l1² − l2² + |W|²) / 2` puts the elbow on a
//!      circle perpendicular to `T`, about 90 degrees from `E`, so weight-zero continuity is not promised there. Any real shoulder-wrist vector above the noise, however short, is
//!      kept: bones that nearly match fold onto a line along it and take the solved plane like a straight limb. The
//!      aim therefore jumps across `NO_AIM` in the animated pose, from along the bones (unequal bones) to
//!      perpendicular to them; for any fixed pose outside `NO_AIM` the weight fade is continuous; inside it (set 3) weight-zero
//!      continuity is not promised, see above.
//! - **Folded links are steep, not singular.** With the elbow folded nearly flat and the tip within a few micrometres of
//!   the shoulder (bones that nearly match), the elbow's height is the square root of `l1² - along²` and `along` is
//!   `(l1² - l2² + d²) / 2d`: moving the tip a nanometre swings the elbow by millimetres. The pose is still
//!   continuous in the weight (the wrist is exact from the first hair of weight, and the elbow's gap shrinks as the
//!   weight interval narrows), but the slope is unbounded like `1 / sqrt(width)`; the largest measured by the
//!   randomised test is 1.7e9 per unit weight at a width of 1e-12 (a 34.5 cm / 1.9 cm arm at 177.6 degrees). The
//!   animated pose is reproduced exactly at weight zero by adding back the formula's rounding error, which fades out
//!   as the tip leaves the root and as the weight rises.
//! - **Helper bones.** Twist or other bones between the shoulder, elbow and wrist ride along rigidly.
//!
//! The hand or foot keeps its local rotation (it turns with the forearm or shin); look-at and foot placement are
//! separate parts of the stage. No allocation, no state: a pure function of the inputs.

use std::f32::consts::{PI, TAU};

use omuzaki_anim_api::{BoneId, HumanoidBone, IkGoals, IkSolver, Limb, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Mat3, Quat, Vec3};

/// The most an elbow or knee bends: 150 degrees from straight, in radians.
pub const MAX_FLEXION: f32 = 150.0 * PI / 180.0;
/// Share of the full reach, at its far end, over which the extension is eased (2%).
pub const SOFT_REACH: f32 = 0.02;
/// Default elbow direction, root space: back and out (the sign of x is the side). Used when the pole gives none.
pub const ELBOW_BEND: Vec3 = Vec3::new(0.5, -0.5, -1.0);
/// Default knee direction, root space: forward.
pub const KNEE_BEND: Vec3 = Vec3::Z;

/// How far the animated elbow must stand off the shoulder-wrist line before its own bend plane fully replaces the solved
/// one as the reference for bone roll, as a share of the shorter bone: `ratio = |h| / min(U, F)` for the elbow's
/// perpendicular offset `h` from that line and the bone lengths `U` (upper) and `F` (fore).
///
/// `h` uses both bones and is zero exactly where the plane through root, elbow and wrist is undefined: a straight limb,
/// or one folded flat on itself. A limb folded tight with long bones still has a large `h`, and so keeps its own plane.
///
/// **Why the projected lean is always defined where the reference fades.** The wrist is on the shoulder-wrist line and
/// the elbow is `h` off it, so the upper bone makes an angle `α` with the line where `sin α = h / U`, and the forearm an
/// angle `β` with `sin β = h / F`. Both are at most `ratio`. So `ratio < NEAR_STRAIGHT` keeps both bones within
/// `asin(NEAR_STRAIGHT)` (5.7 degrees) of `±aim`, and `|aim · bone| ≥ cos(5.7°) > 0.99`: the `atan2(bend · bone,
/// aim · bone)` that measures a bone's lean never sees `(0, 0)`. (A sine of the *flexion*, used before, vanishes at a
/// fold too, with a bone across the aim: that was the fold finding.) Tests: `a_weight_fade_never_pops_with_the_elbow_
/// folded_nearly_flat` (the review's case, 170 degrees to exactly 180), the randomised weight-fade test (elbows up to
/// and including exactly folded), and `the_elbow_offset_bounds_both_bones_to_the_aim_line` below.
pub const NEAR_STRAIGHT: f32 = 0.1;
/// Segments shorter than this are degenerate and left alone, metres.
const MIN_LEN: f32 = 1e-5;
/// A bend direction off the root-to-target line by less than this (metres, squared) is noise, not a side.
const MIN_OFF_LINE_SQ: f32 = 1e-8;
/// The elbow's offset from the shoulder-wrist line, as a share of the shorter bone, below which it counts as exactly on
/// the line: the offset is then rounding noise (a few parts in 1e8 of the limb), not a direction, and the solved plane
/// stands in for the limb's own.
const STRAIGHT_NOISE: f32 = 3e-4;
/// Two segment lengths closer than this (metres) are the same length, to the precision world positions are known to.
const LENGTH_NOISE: f32 = 2e-6;
/// A shoulder-wrist distance (metres) below this is rounding noise, to the precision world positions are known to: the
/// limb has no aim of its own. Above it the direction is real, however short, and is kept.
const NO_AIM: f32 = 2e-6;
/// A projection shorter than this share of its input is rounding noise.
const PROJECTION_NOISE: f32 = 1e-3;

/// The aim of a limb whose wrist is on its shoulder: always perpendicular to the unit `upper` bone, and deterministic.
/// The pole `pole_dir` projected off the bone, else the unit target direction `axis` projected off it, else any
/// perpendicular. A projection under `PROJECTION_NOISE` of its input is rounding noise (a pole or target along the bone)
/// and is refused rather than normalised into a direction along the bone.
fn fallback_aim(upper: Vec3, pole_dir: Vec3, axis: Vec3) -> Vec3 {
    let perpendicular = |v: Vec3| {
        let p = v - upper * v.dot(upper);
        (p.length() > PROJECTION_NOISE * v.length()).then(|| p.normalize())
    };
    perpendicular(pole_dir).or_else(|| perpendicular(axis)).unwrap_or_else(|| upper.any_orthonormal_vector())
}

/// Eases the distance `want` so it approaches `reach` without ever passing it: unchanged up to `reach·(1 − SOFT_REACH)`,
/// then an exponential approach whose slope is continuous with the straight part.
fn soft_reach(want: f64, reach: f64) -> f64 {
    let zone = reach * f64::from(SOFT_REACH);
    let start = reach - zone;
    if want <= start { want } else { start + zone * (1.0 - (-(want - start) / zone).exp()) }
}

/// The unit component of `v` perpendicular to the unit `axis`, or `None` if it is too small to name a side.
fn off_axis(v: Vec3, axis: Vec3) -> Option<Vec3> {
    let p = v - axis * v.dot(axis);
    (p.length_squared() > MIN_OFF_LINE_SQ).then(|| p.normalize())
}

/// The transform of `bone` in the frame of its ancestor `stop` (or in the skeleton's root space if `None`), found by
/// multiplying up the chain (a handful of multiplies, no storage). `stop` must be an ancestor of `bone`.
fn world_from(skeleton: &Skeleton, pose: &Pose, bone: BoneId, stop: Option<BoneId>) -> Transform {
    let mut world = pose.local[bone.index()];
    let mut at = skeleton.bones()[bone.index()].parent;
    while at != stop {
        let Some(parent) = at else { break };
        world = pose.local[parent.index()] * world;
        at = skeleton.bones()[parent.index()].parent;
    }
    world
}

/// Below this sine of the angle between two directions (about 0.06 degrees) they count as opposite: there is no single
/// shortest turn between them, so the sweep is chosen by a rule instead.
pub const OPPOSITE_SIN: f32 = 1e-3;

/// The shortest rotation taking the unit direction `from` onto `to`. `Quat::from_rotation_arc` returns the identity for any
/// pair closer than about 7e-4 rad (its test is `dot > 1 - 2·EPSILON`), which here leaves a joint that far off its
/// place; the normalised `(from × to, 1 + from · to)` is accurate for small angles and is used wherever the two
/// directions are not nearly opposite, where the library's choice of axis is kept.
fn swing_between(from: Vec3, to: Vec3) -> Quat {
    let dot = from.dot(to);
    if dot < -0.999 {
        return Quat::from_rotation_arc(from, to);
    }
    let cross = from.cross(to);
    Quat::from_xyzw(cross.x, cross.y, cross.z, 1.0 + dot).normalize()
}

/// A turn taking the unit direction `from` onto `to`, as `residual * rotation(axis, angle)`, along which a blend of
/// weight `w` is `slerp(identity, residual, w) * rotation(axis, w * angle)`.
///
/// Normally that is the shortest turn. When `to` is (nearly) opposite `from` the shortest turn is not defined, so the
/// sweep is taken through the plane of `from` and `reference` (the pole), a half turn plus a tiny `residual` that
/// lands exactly on `to`. Either way the path is a continuous function of the weight for any fixed `to`.
fn aim_path(from: Vec3, to: Vec3, reference: Vec3) -> (Vec3, f32, Quat) {
    let cross = from.cross(to);
    let (sin, cos) = (cross.length(), from.dot(to));
    if cos < 0.0 && sin < OPPOSITE_SIN {
        let toward = off_axis(reference, from).unwrap_or_else(|| from.any_orthonormal_vector());
        let axis = from.cross(toward).normalize();
        let half = Quat::from_axis_angle(axis, PI);
        (axis, PI, swing_between(half * from, to))
    } else if sin > 0.0 {
        (cross / sin, sin.atan2(cos), Quat::IDENTITY)
    } else {
        (Vec3::Z, 0.0, Quat::IDENTITY) // the same direction: nothing to turn
    }
}

/// The elbow's perpendicular offset from the line through the shoulder `a` along the unit `axis` (the shoulder-wrist line):
/// zero exactly where the plane through shoulder, elbow and wrist is undefined, and its direction is the limb's bend side.
/// Perpendicular to `axis` to rounding, so frames built from it are orthonormal.
fn elbow_offset(a: Vec3, b: Vec3, axis: Vec3) -> Vec3 {
    let offset = (b - a) - axis * (b - a).dot(axis);
    offset - axis * offset.dot(axis)
}

/// Where the mid joint and tip go for a limb of segment lengths `l1`, `l2` whose tip sits `dist` along the unit `axis`
/// from `a`, bent toward `side` (unit, perpendicular to `axis`). Solved in f64: `height` is a square root of a
/// difference, which in f32 turns rounding noise into a visible bend at full extension.
#[allow(clippy::cast_possible_truncation)] // the triangle is a few decimetres: narrowing to f32 loses nothing
fn place(
    a: Vec3,
    axis: Vec3,
    side: Vec3,
    dist: f64,
    (l1, l2): (f64, f64),
    (along_bias, height_bias): (f64, f64),
) -> (Vec3, Vec3) {
    // Law of cosines: the mid joint's foot point on the axis and its height off it, plus the biases that make the
    // animated pose come out exactly (see `solve_limb`).
    let along = (l1 * l1 - l2 * l2 + dist * dist) / (2.0 * dist) + along_bias;
    let height = ((l1 * l1 - along * along).max(0.0).sqrt() + height_bias).max(0.0);
    (a + axis * along as f32 + side * height as f32, a + axis * dist as f32)
}

/// The formula's rounding error at the animated pose, as `(along, height)` corrections for [`place`].
///
/// The elbow's place along the aim is a difference of nearly equal squares over the tip's distance, and its height a
/// square root of one more: with the tip close to the root and bones that nearly match, or the limb nearly straight, a
/// 1e-7 rounding error in a length becomes millimetres of `along` and centimetres of height. So the animated pose is
/// reproduced exactly instead: the formula's errors at the animated distance are added back, shrinking as the tip
/// leaves the root (the error scales as one over the distance) and as the weight rises, and gone at weight one.
/// `distances` are the animated and blended tip distances, `actual` the animated elbow's `(along, height)`.
fn animated_bias(lengths: (f64, f64), distances: (f64, f64), actual: (f64, f64), weight: f32) -> (f64, f64) {
    let (animated, blended) = distances;
    if animated <= 1e-9 || blended <= 1e-9 {
        return (0.0, 0.0);
    }
    let formula = (lengths.0 * lengths.0 - lengths.1 * lengths.1 + animated * animated) / (2.0 * animated);
    let along_error = actual.0 - formula;
    let height_error = actual.1 - (lengths.0 * lengths.0 - actual.0 * actual.0).max(0.0).sqrt();
    let fade = (animated / blended).min(1.0) * f64::from(1.0 - weight);
    (along_error * fade, height_error * fade)
}

/// The stage for the four limbs of a humanoid. Stateless; see the module docs.
///
/// It solves only the hand and foot goals of [`IkGoals`]; the look-at goal is another part of the IK stage and is
/// ignored here. On a generic skeleton, or a limb whose bones the skeleton lacks, it does nothing. Bones between the
/// humanoid joints (twist or helper bones) are carried along rigidly.
#[derive(Copy, Clone, Debug, Default)]
pub struct TwoBoneIk;

impl TwoBoneIk {
    /// A solver.
    pub const fn new() -> Self {
        Self
    }

    /// Solves one limb `[root, mid, tip]`, bending toward `default_bend` when the pole names no side.
    ///
    /// The result for a weight below one is the same triangle built from a *blend of the limb's configuration* (aim
    /// direction, reach distance and bend side) between the animated one and the solved one, never a blend of the two
    /// joint rotations: the distance stays between the animated and solved ones, so the hinge limit holds throughout.
    #[allow(clippy::cast_possible_truncation)] // see `place`
    fn solve_limb(skeleton: &Skeleton, pose: &mut Pose, chain: [BoneId; 3], limb: Limb, default_bend: Vec3) {
        let weight = limb.weight.clamp(0.0, 1.0);
        let [root, mid, tip] = chain;
        let nested = |outer: BoneId, inner: BoneId| outer != inner && skeleton.is_ancestor_or_self(outer, inner);
        if weight <= 0.0 || !nested(root, mid) || !nested(mid, tip) {
            return;
        }
        let root_world = world_from(skeleton, pose, root, None);
        let mid_rel = world_from(skeleton, pose, mid, Some(root));
        let mid_world = root_world * mid_rel;
        let tip_world = mid_world * world_from(skeleton, pose, tip, Some(mid));
        let (a, b, c) = (root_world.translation, mid_world.translation, tip_world.translation);
        let (l1, l2) = (a.distance(b), b.distance(c));
        if l1 < MIN_LEN || l2 < MIN_LEN {
            return;
        }
        // Segment lengths that agree to world-coordinate precision are equal: with the tip near the root the elbow's place
        // along the aim is `d/2 + (l1² - l2²)/(2d)`, which turns rounding noise in a length (about 1e-7 m at a metre from
        // the origin) into millimetres once `d` is tiny, as when an equal-boned limb is folded flat.
        let lengths = if (l1 - l2).abs() < LENGTH_NOISE {
            let mean = f64::from(l1).midpoint(f64::from(l2));
            (mean, mean)
        } else {
            (f64::from(l1), f64::from(l2))
        };

        // The full solve: aim at the target, ease the reach, face the pole.
        let to_target = limb.target - a;
        let axis = to_target.try_normalize().or_else(|| (c - a).try_normalize()).unwrap_or((b - a) / l1);
        // The hinge opens from fully bent (interior angle π − MAX_FLEXION) to straight.
        let reach = lengths.0 + lengths.1;
        let bent = (lengths.0 * lengths.0 + lengths.1 * lengths.1
            - 2.0 * lengths.0 * lengths.1 * f64::from(PI - MAX_FLEXION).cos())
        .sqrt();
        let dist = soft_reach(f64::from(to_target.length()), reach).clamp(bent, reach);
        let side = off_axis(limb.pole - a, axis)
            .or_else(|| off_axis(b - a, axis))
            .or_else(|| off_axis(default_bend, axis))
            .unwrap_or_else(|| axis.any_orthonormal_vector());

        // The animated limb's aim, and the aim's path onto the solved one (`aim_path`): `full` is the whole turn and
        // `part` the share a weight below one applies.
        // A limb folded flat on itself with equal bones has its wrist on its shoulder and no aim of its own. The aim
        // of a nearly folded such limb is perpendicular to its bones, so that is the aim taken here, on the pole's side
        // if it can: the elbow then starts exactly where the animation put it. A shoulder-wrist vector
        // above the noise is a real direction (bones that nearly match fold onto a line along it) and is kept.
        let animated_axis = (c - a).try_normalize().filter(|_| a.distance(c) >= NO_AIM).unwrap_or_else(|| {
            let upper = (b - a) / l1;
            fallback_aim(upper, limb.pole - a, axis)
        });
        let (turn_axis, turn_angle, residual) = aim_path(animated_axis, axis, limb.pole - a);
        let full = residual * Quat::from_axis_angle(turn_axis, turn_angle);
        let part = if weight >= 1.0 {
            full
        } else {
            Quat::IDENTITY.slerp(residual, weight) * Quat::from_axis_angle(turn_axis, turn_angle * weight)
        };
        // Which way the limb already bends: the side of the plane it lies in. A straight limb has no bend, so it is
        // taken to bend the way the solved side would, seen from the animated aim (`wanted`). `swing` is the signed
        // angle about the animated aim from the one to the other; opposite sides have no shortest sweep, so they
        // always turn counter-clockwise.
        let wanted = full.inverse() * side;
        let offset = elbow_offset(a, b, animated_axis);
        let standoff = offset.length() / l1.min(l2);
        let bend_side = if standoff < STRAIGHT_NOISE {
            wanted
        } else {
            offset.try_normalize().filter(|v| v.is_finite()).unwrap_or(wanted)
        };
        let (sin, cos) = (animated_axis.dot(bend_side.cross(wanted)), bend_side.dot(wanted));
        let mut swing = sin.atan2(cos);
        if cos < 0.0 && sin.abs() < OPPOSITE_SIN && swing < 0.0 {
            swing += TAU;
        }
        // The direction of a bend that shrinks to nothing has no limit, so the joints and the roll treat it apart:
        // - the joints start from the animated bend side, however small the bend, and sweep toward the solved side by
        //   `sweep`: nothing at weight 0, all at weight 1, and all at any weight once the bend has vanished (the elbow
        //   is then on the line, so nothing moves);
        // - the roll reference fades from the animated bend plane into the solved one as the bend falls below
        //   `NEAR_STRAIGHT`, so a limb passing through straight does not flip its roll, and does so only as `sweep`
        //   allows, so a hair of weight leaves the animated bones where they are.
        let curvature = (standoff / NEAR_STRAIGHT).clamp(0.0, 1.0);
        let faded = curvature * curvature * (3.0 - 2.0 * curvature);
        let sweep = if weight >= 1.0 { 1.0 } else { weight / (weight + faded * (1.0 - weight)) };
        let reference = Quat::from_axis_angle(animated_axis, swing * (1.0 - faded) * sweep) * bend_side;
        let reference = (reference - animated_axis * reference.dot(animated_axis)).try_normalize().unwrap_or(bend_side);

        let (axis, dist, side, bias) = if weight >= 1.0 {
            (axis, dist, side, (0.0, 0.0))
        } else {
            // Blend from the animated configuration along that path, so every weight is a valid limb and the result
            // is continuous in the weight for any target: the distance moves linearly, and the bend side is swept by
            // `sweep` of its own turn, measured in the animated aim's frame and carried by the same rotation.
            let animated_dist = f64::from(a.distance(c));
            let blended_dist = animated_dist + (dist - animated_dist) * f64::from(weight);
            let bias = animated_bias(
                lengths,
                (animated_dist, blended_dist),
                (f64::from((b - a).dot(animated_axis)), f64::from(offset.length())),
                weight,
            );
            (
                part * animated_axis,
                blended_dist,
                part * (Quat::from_axis_angle(animated_axis, swing * sweep) * bend_side),
                bias,
            )
        };
        let (new_mid, new_tip) = place(a, axis, side, dist, lengths, bias);

        // Turn each bone by the rigid rotation that carries the animated limb's frame (aim, bend side, plane normal)
        // onto the new one, then by the change of its lean from the aim within the bend plane. Built from frames, never
        // from a shortest arc between bone directions (which has no answer when two are opposite, as a deeply bent
        // forearm and a flipped bend side make them), so a bone's roll follows the bend plane continuously in the weight
        // and the target. A straight limb has no plane of its own: it takes the solved one carried back along the aim
        // path, which makes the carry exactly that path. Anything between the joints rides along, so each bone's new
        // local rotation is found through its actual parent.
        let frame = |aim: Vec3, bend: Vec3| Mat3::from_cols(aim, bend, aim.cross(bend));
        let carry = Quat::from_mat3(&frame(axis, side)) * Quat::from_mat3(&frame(animated_axis, reference)).inverse();
        let lean = |direction: Vec3, aim: Vec3, bend: Vec3| bend.dot(direction).atan2(aim.dot(direction));
        let normal = axis.cross(side);
        let turned = |was: Vec3, now: Vec3| {
            let change = lean(now, axis, side) - lean(was, animated_axis, reference);
            let planar = (Quat::from_axis_angle(normal, change) * carry).normalize();
            // A bone off the reference plane (a nearly straight limb's, whose plane is only partly its own) lands
            // within a few degrees; the short swing that finishes the job has no opposite directions to meet.
            (swing_between(planar * was, now) * planar).normalize()
        };
        let upper = turned((b - a) / l1, (new_mid - a) / l1);
        let lower = turned((c - b) / l2, (new_tip - new_mid) / l2);
        let root_rot = (upper * root_world.rotation).normalize();
        let root_parent = root_world.rotation * pose.local[root.index()].rotation.inverse();
        let mid_parent = upper * (mid_world.rotation * pose.local[mid.index()].rotation.inverse());
        let mid_rot = (lower * mid_world.rotation).normalize();
        pose.local[root.index()].rotation = (root_parent.inverse() * root_rot).normalize();
        pose.local[mid.index()].rotation = (mid_parent.inverse() * mid_rot).normalize();
    }
}

impl IkSolver for TwoBoneIk {
    fn solve(&mut self, skeleton: &Skeleton, goals: &IkGoals, pose: &mut Pose) {
        use HumanoidBone as B;
        let Some(map) = skeleton.humanoid_map() else { return };
        let goals = goals.finite_only();
        if goals.is_idle() {
            return;
        }
        let out = |x: f32| Vec3::new(x * ELBOW_BEND.x, ELBOW_BEND.y, ELBOW_BEND.z);
        let limbs = [
            (goals.left_hand, [B::LeftUpperArm, B::LeftLowerArm, B::LeftHand], out(1.0)),
            (goals.right_hand, [B::RightUpperArm, B::RightLowerArm, B::RightHand], out(-1.0)),
            (goals.left_foot, [B::LeftUpperLeg, B::LeftLowerLeg, B::LeftFoot], KNEE_BEND),
            (goals.right_foot, [B::RightUpperLeg, B::RightLowerLeg, B::RightFoot], KNEE_BEND),
        ];
        for (goal, bones, bend) in limbs {
            if let (Some(limb), [Some(root), Some(mid), Some(tip)]) = (goal, bones.map(|b| map.get(b))) {
                Self::solve_limb(skeleton, pose, [root, mid, tip], limb, bend);
            }
        }
    }

    /// Nothing to clear: the solve carries no state between frames.
    fn reset(&mut self) {}
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact pass-through below the soft zone
mod tests {
    use omuzaki_core::rng::Rng;

    use super::*;

    #[test]
    fn the_fallback_aim_is_perpendicular_to_the_bone_however_noisy_the_projection() {
        let upper = Vec3::new(1.0, 1.0, 0.0).normalize();
        let mut rng = Rng::new(7);
        for case in 0..2000 {
            // Poles and targets along the bone, off it by a few ulps, or anywhere (including zero and non-finite).
            let noise = Vec3::new(5.96e-8, 5.96e-8, 0.0) * rng.range_f32(-1.0, 1.0);
            let pole = if case % 4 == 0 {
                upper * 0.25 + noise
            } else {
                Vec3::new(rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0))
            };
            let target = if case % 3 == 0 { upper + noise } else { Vec3::Z };
            let aim = fallback_aim(upper, pole, target);
            assert!(aim.is_finite() && (aim.length() - 1.0).abs() < 1e-5, "case {case}: {aim:?}");
            assert!(aim.dot(upper).abs() < 1e-3, "case {case}: aim {aim:?} not perpendicular");
        }
        assert!(fallback_aim(upper, Vec3::NAN, Vec3::ZERO).dot(upper).abs() < 1e-3);
    }

    #[test]
    fn soft_reach_is_exact_inside_continuous_and_never_past_the_reach() {
        let reach = 0.5_f64;
        assert_eq!(soft_reach(0.3, reach), 0.3);
        let start = reach * (1.0 - f64::from(SOFT_REACH));
        let mut last = soft_reach(start, reach);
        assert!((last - start).abs() < 1e-12);
        for i in 1..=200_u8 {
            let eased = soft_reach(start + f64::from(i) * 0.001, reach);
            assert!(eased >= last && eased <= reach, "monotonic and bounded at step {i}");
            assert!(eased - last < 0.001 + 1e-9, "slope never exceeds 1 (no jump)");
            last = eased;
        }
        assert!((soft_reach(10.0, reach) - reach).abs() < 1e-12);
    }

    #[test]
    fn off_axis_drops_the_axis_component_and_refuses_noise() {
        assert_eq!(off_axis(Vec3::new(3.0, 2.0, 0.0), Vec3::X), Some(Vec3::Y));
        assert_eq!(off_axis(Vec3::new(3.0, 1e-6, 0.0), Vec3::X), None);
    }

    #[test]
    fn the_elbow_offset_bounds_both_bones_to_the_aim_line() {
        // `ratio = |h| / min(U, F)` bounds how far either bone can lean from the shoulder-wrist line: `sin α = h / U` and
        // `sin β = h / F`, both at most `ratio` (see `NEAR_STRAIGHT`). So below `NEAR_STRAIGHT` both stay within
        // `asin(NEAR_STRAIGHT)` of `±aim`, and the projected lean is defined.
        let mut rng = Rng::new(0x4e45_4152);
        let unit = |rng: &mut Rng| loop {
            let v = Vec3::new(rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0), rng.range_f32(-1.0, 1.0));
            if v.length_squared() > 0.01 && v.length_squared() <= 1.0 {
                return v.normalize();
            }
        };
        let (mut checked, mut near) = (0_u32, 0_u32);
        for _ in 0..20_000 {
            let (shoulder, elbow, wrist) =
                (Vec3::ZERO, unit(&mut rng) * rng.range_f32(0.01, 0.5), unit(&mut rng) * 0.4);
            // Half the cases put the wrist near the line through shoulder and elbow, so the ratio is often small.
            let wrist = if rng.next_f32() < 0.5 { elbow * rng.range_f32(-1.5, 1.5) + wrist * 0.02 } else { wrist };
            let Some(aim) = (wrist - shoulder).try_normalize() else { continue };
            let (upper, fore) = (elbow.length(), (wrist - elbow).length());
            if upper < 1e-3 || fore < 1e-3 {
                continue;
            }
            let ratio = elbow_offset(shoulder, elbow, aim).length() / upper.min(fore);
            let bound = ratio.min(1.0).asin() + 1e-3;
            for bone in [elbow - shoulder, wrist - elbow] {
                let lean = bone.normalize().dot(aim).abs().clamp(0.0, 1.0).acos();
                assert!(lean <= bound, "a bone leans {lean} from the aim line, past asin({ratio}) = {bound}");
                if ratio < NEAR_STRAIGHT {
                    assert!(bone.normalize().dot(aim).abs() > 0.99, "the lean is defined below the fade threshold");
                    near += 1;
                }
            }
            checked += 1;
        }
        assert!(checked > 10_000 && near > 1_000, "the cases cover both sides of the threshold ({checked}, {near})");
    }
}
