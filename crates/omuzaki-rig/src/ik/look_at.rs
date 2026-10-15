//! Look-at (RG-05, FE-07): turn the chest, neck and head toward a point, and let the eyes take the rest.
//!
//! # The solve
//!
//! Everything is measured in the **body frame**: the frame of the parent of the first torso bone (the spine), aligned
//! so that at rest it is the root space (+Z ahead, +Y up, +X the character's left). The body frame follows the
//! animated torso, so a character bent forward still looks "up" relative to its own body, and bone axes that are tilted
//! in the file (Blender and VRM 0.x exports) make no difference: every frame is built as `world · rest⁻¹`.
//!
//! 1. **Where to look.** The direction from the head bone to the target gives a yaw (`atan2(x, z)`) and a pitch
//!    (`asin(y)`). Yaw is limited to ±[`YAW_LIMIT`] and pitch to [`PITCH_DOWN`]..[`PITCH_UP`] (human neck range; the
//!    VRM spec states none, so these are tuning, not rights). Past a limit the head stays **on** the limit.
//! 2. **Chest, neck, head.** The three bones share the turn, by default [`SHARES`] = 10, 30 and 60% (the head leads, the
//!    chest only follows; a person turning to look moves their shoulders least). They are applied in order, and each
//!    bone takes its share of what is *left*, aimed from where the head is **now**, so turning the chest (which carries
//!    the head sideways a few centimetres) cannot leave the last bone aiming at a stale point. The head is last, so it
//!    turns about its own origin and ends exactly on the goal. Each bone is turned by the minimal swing, so any nod
//!    or tilt (roll) the animation gave the head survives. A bone the skeleton lacks gives its share to the others.
//! 3. **Eye share.** With [`LookAt::eye_share`] `e` the chain turns the fraction `1 − e` of the way, and the eyes take
//!    what the head leaves, aimed at the *true* target (not the limited one) through the VRM look-at range maps
//!    ([`RangeMap`]: `angle / input_max_value · output_scale`, saturating at `input_max_value`):
//!    - **Bone type:** each eye bone is turned by the mapped angle. The left eye's outer side is +X; the right eye's
//!      inner side is +X. Both eyes get the same angle (parallel gaze: the eye separation is ignored).
//!    - **Expression type:** bones are not touched; the weights of `lookLeft`, `lookRight`, `lookUp` and `lookDown` are
//!      left in [`LookAtIk::eye_weights`] for the painter. The one expression pair has no inner and outer, so both
//!      directions use the outer horizontal map (VRM 1.0 `lookAt.md`).
//!
//!    At `e = 0` the eyes stay in place; the eye gain reaches full at `e =` [`EYE_RAMP`], so there is no step at zero.
//! 4. **Weight.** [`LookAt::weight`] scales the whole turn: the chain's fractions and the eyes' output. `0` leaves the
//!    pose bit for bit as it was; the result is continuous in the weight.
//!
//! Only rotations of the chest, neck, head and eye bones change, so lengths and every translation are as animated.
//!
//! # Clamps, never flips: singular sets
//!
//! Limiting a yaw to ±70° has one place where no continuous answer exists: a target **exactly behind** the body asks for
//! +70° and −70° at once. Taking the shorter way round flips the head across the front; this solver instead eases the
//! limited yaw to zero over the [`BEHIND_BLEND`] (20°) either side of straight behind, so the head holds the limit on
//! the side the target is on, then **sweeps through straight ahead inside that wedge**. The output is continuous
//! everywhere (a bisection test bounds its gain: a target circling the head at pitch `p` moves the output at most about
//! `8 / cos p` times as fast as the target, which is steep near the vertical, like the folded links of the two-bone solve), and the wedge is the only place the head is not on the limit.
//! Exactly three singular sets, each with a defined result:
//! 1. *Behind* (`|yaw| = 180°`, the wedge above): yaw 0, the head faces front.
//! 2. *Straight above or below* (the yaw is undefined): within [`POLE_SIN`] (5°) of the vertical the yaw eases to zero,
//!    so at the pole the head faces front, pitched to its limit. The target is far outside the pitch limit there, so
//!    nothing that the limits would allow is lost.
//! 3. *The target at the head's origin* (no direction): inside [`NEAR_NONE`] (1 cm) of the head the turn is zero and the
//!    pose is as animated; it fades in to full by [`NEAR_FULL`] (5 cm). The distance is measured from where the head is
//!    when each bone is turned, because turning the chest carries the head, possibly onto the target; the eyes use the
//!    final head place.
//!
//! The eyes use the same easing about their own head frame. If the animation itself holds the head almost exactly
//! opposite the goal (more than a half turn off, which the limits cannot ask for), the minimal swing has no unique axis
//! and `Quat::from_rotation_arc` picks one deterministically.
//!
//! No allocation, no stored state except the last eye weights: a pure function of the inputs.

use std::f32::consts::PI;

use omuzaki_anim_api::{BoneId, HumanoidBone, HumanoidMap, IkGoals, IkSolver, LookAt, Pose, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};

/// Default share of the turn taken by the chest, neck and head: 10, 30 and 60%. The head leads and the chest follows,
/// the split the prototype and the spec's look-at section use.
pub const SHARES: [f32; 3] = [0.10, 0.30, 0.60];
/// Largest head yaw from the body's front, radians: 70 degrees, a person's comfortable neck rotation.
pub const YAW_LIMIT: f32 = 70.0 * PI / 180.0;
/// Largest head pitch down, radians: 40 degrees (neck flexion).
pub const PITCH_DOWN: f32 = 40.0 * PI / 180.0;
/// Largest head pitch up, radians: 30 degrees (neck extension, less than flexion because the chin meets the shoulders).
pub const PITCH_UP: f32 = 30.0 * PI / 180.0;
/// Half the width of the wedge behind the body in which the limited yaw eases through zero, radians (20 degrees): wide
/// enough that the sweep from one limit to the other has a gain of about 3.5, narrow enough that the head holds the
/// limit everywhere else.
pub const BEHIND_BLEND: f32 = 20.0 * PI / 180.0;
/// Sine of the angle from the vertical inside which the yaw eases to zero: 5 degrees.
pub const POLE_SIN: f32 = 0.087_155_74;
/// Distance from the head (metres) below which a target has no direction and the look-at is off.
pub const NEAR_NONE: f32 = 0.01;
/// Distance from the head (metres) at which the look-at is back to full weight.
pub const NEAR_FULL: f32 = 0.05;
/// The eye share at which the eyes reach full gain; below it they fade in, so `eye_share = 0` leaves them in place
/// without a step.
pub const EYE_RAMP: f32 = 0.25;
/// Remaining-turn denominators below this are a finished chain.
const DONE: f32 = 1e-6;

/// The VRM look-at range map: a gaze angle in degrees to a bone rotation (degrees) or an expression weight.
///
/// The output is `min(angle, input_max_value) / input_max_value · output_scale`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RangeMap {
    /// The input angle, degrees, at which the output saturates.
    pub input_max_value: f32,
    /// The output at that angle: degrees for the bone type, weight for the expression type.
    pub output_scale: f32,
}

impl RangeMap {
    /// The spec's own example for the bone type: 90 degrees of gaze move the eye bone 10 degrees (`lookAt.md`).
    pub const BONE: Self = Self { input_max_value: 90.0, output_scale: 10.0 };
    /// The same input range for the expression type, weight 1 at the end.
    pub const EXPRESSION: Self = Self { input_max_value: 90.0, output_scale: 1.0 };

    /// The output for a gaze `angle` in degrees (its magnitude), zero for a map that is not positive.
    pub fn map(self, angle: f32) -> f32 {
        if self.input_max_value > 0.0 {
            angle.abs().min(self.input_max_value) / self.input_max_value * self.output_scale
        } else {
            0.0
        }
    }
}

/// How the eyes are driven.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EyeKind {
    /// Turn the eye bones.
    Bone,
    /// Leave bones alone and give the `lookLeft`, `lookRight`, `lookUp`, `lookDown` expression weights
    /// ([`LookAtIk::eye_weights`]).
    Expression,
}

/// The four VRM range maps of the eyes (`VRMC_vrm.lookAt`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EyeRanges {
    /// Looking toward the nose.
    pub horizontal_inner: RangeMap,
    /// Looking away from the nose.
    pub horizontal_outer: RangeMap,
    /// Looking down.
    pub vertical_down: RangeMap,
    /// Looking up.
    pub vertical_up: RangeMap,
}

impl EyeRanges {
    /// All four maps the same.
    pub const fn uniform(map: RangeMap) -> Self {
        Self { horizontal_inner: map, horizontal_outer: map, vertical_down: map, vertical_up: map }
    }
}

/// Which way an eye's horizontal gaze is mapped.
#[derive(Copy, Clone)]
enum Side {
    Left,
    Right,
    Expression,
}

/// The eye gaze as VRM look-at expression weights, `0..=1` each (zero for the bone type).
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct EyeWeights {
    /// `lookLeft` (toward +X).
    pub look_left: f32,
    /// `lookRight`.
    pub look_right: f32,
    /// `lookUp`.
    pub look_up: f32,
    /// `lookDown`.
    pub look_down: f32,
}

impl EyeWeights {
    /// All four at zero.
    pub const ZERO: Self = Self { look_left: 0.0, look_right: 0.0, look_up: 0.0, look_down: 0.0 };
}

/// The tunables of the look-at, with the defaults above.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LookAtParams {
    /// Share of the turn for chest, neck and head (relative: they are normalised over the bones the skeleton has).
    pub shares: [f32; 3],
    /// Largest head yaw, radians.
    pub yaw_limit: f32,
    /// Largest head pitch down, radians.
    pub pitch_down: f32,
    /// Largest head pitch up, radians.
    pub pitch_up: f32,
    /// Bone or expression eyes.
    pub eye_kind: EyeKind,
    /// The eyes' range maps.
    pub eyes: EyeRanges,
}

impl Default for LookAtParams {
    fn default() -> Self {
        Self {
            shares: SHARES,
            yaw_limit: YAW_LIMIT,
            pitch_down: PITCH_DOWN,
            pitch_up: PITCH_UP,
            eye_kind: EyeKind::Bone,
            eyes: EyeRanges::uniform(RangeMap::BONE),
        }
    }
}

/// `x²(3 − 2x)` on `0..=1`: zero slope at both ends, so an ease built from it has no kink.
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// A yaw (`-π..=π`) limited to `±limit` that stays **on** the limit, except inside [`BEHIND_BLEND`] of straight behind,
/// where it eases to zero so the result is continuous across `±π`.
fn limited_yaw(yaw: f32, limit: f32) -> f32 {
    let blend = BEHIND_BLEND.min(PI - limit);
    let angle = yaw.abs();
    if angle <= limit || blend <= 0.0 {
        yaw
    } else if angle <= PI - blend {
        limit.copysign(yaw)
    } else {
        (limit * smoothstep((PI - angle) / blend)).copysign(yaw)
    }
}

/// How much of a turn to keep for a target `distance` metres from the head: none inside [`NEAR_NONE`], all from
/// [`NEAR_FULL`], smooth between, so a target closing in on the head (or a head carried onto the target) fades out.
fn near_fade(distance: f32) -> f32 {
    smoothstep((distance - NEAR_NONE) / (NEAR_FULL - NEAR_NONE))
}

/// The yaw and pitch of the unit direction `d` (+Z ahead, +Y up), the yaw eased out near the vertical where it has no
/// meaning.
fn yaw_pitch(d: Vec3) -> (f32, f32) {
    (d.x.atan2(d.z), d.y.clamp(-1.0, 1.0).asin())
}

/// How much of a yaw to keep for the unit direction `d`: all of it away from the vertical, none at the pole.
fn pole_keep(d: Vec3) -> f32 {
    smoothstep(d.x.hypot(d.z) / POLE_SIN)
}

/// The transform of `bone` in root space, built from `local`.
fn world(skeleton: &Skeleton, bone: BoneId, local: impl Fn(BoneId) -> Transform) -> Transform {
    let mut world = local(bone);
    let mut at = skeleton.bones()[bone.index()].parent;
    while let Some(parent) = at {
        world = local(parent) * world;
        at = skeleton.bones()[parent.index()].parent;
    }
    world
}

/// The frame of `bone` aligned to the root space at rest (`world · rest⁻¹`): +Z is the bone's forward however its own
/// axes are tilted, and it turns with the animated bone. `None` is the root space itself.
fn aligned(skeleton: &Skeleton, pose: &Pose, bone: Option<BoneId>) -> Quat {
    bone.map_or(Quat::IDENTITY, |b| {
        let now = world(skeleton, b, |i| pose.local[i.index()]).rotation;
        let rest = world(skeleton, b, |i| skeleton.bones()[i.index()].rest).rotation;
        now * rest.inverse()
    })
}

/// The shortest rotation taking the unit `from` onto `to`, accurate for small angles (`Quat::from_rotation_arc` returns
/// the identity within about 7e-4 rad).
pub(crate) fn swing(from: Vec3, to: Vec3) -> Quat {
    let dot = from.dot(to);
    if dot < -0.999 {
        return Quat::from_rotation_arc(from, to);
    }
    let cross = from.cross(to);
    Quat::from_xyzw(cross.x, cross.y, cross.z, 1.0 + dot).normalize()
}

/// Applies the root-space rotation `turn` to `bone`'s world orientation, by editing its local rotation.
fn turn_bone(skeleton: &Skeleton, pose: &mut Pose, bone: BoneId, turn: Quat) {
    let parent = skeleton.bones()[bone.index()].parent;
    let parent_rot = parent.map_or(Quat::IDENTITY, |p| world(skeleton, p, |i| pose.local[i.index()]).rotation);
    let local = &mut pose.local[bone.index()].rotation;
    *local = (parent_rot.inverse() * turn * parent_rot * *local).normalize();
}

/// The look-at solver: implements [`IkSolver`] for [`IkGoals::look_at`] and ignores the limb goals (the two-bone solver's
/// job).
#[derive(Copy, Clone, Debug, Default)]
pub struct LookAtIk {
    params: LookAtParams,
    eyes: EyeWeights,
}

impl LookAtIk {
    /// A solver with the given tunables.
    pub const fn new(params: LookAtParams) -> Self {
        Self { params, eyes: EyeWeights::ZERO }
    }

    /// The expression weights of the last solve for the [`EyeKind::Expression`] type; zero for the bone type, with no
    /// look-at goal, or on a generic skeleton.
    pub const fn eye_weights(&self) -> EyeWeights {
        self.eyes
    }

    /// The unit direction `d` (body frame) limited to the head's yaw and pitch range.
    fn limit(&self, d: Vec3) -> Vec3 {
        let p = &self.params;
        let (yaw, pitch) = yaw_pitch(d);
        let yaw = limited_yaw(yaw, p.yaw_limit.max(0.0)) * pole_keep(d);
        let pitch = pitch.clamp(-p.pitch_down.max(0.0), p.pitch_up.max(0.0));
        Vec3::new(pitch.cos() * yaw.sin(), pitch.sin(), pitch.cos() * yaw.cos())
    }

    /// Turns the chest, neck and head toward the limited goal; stops early if the target loses its direction.
    fn turn_chain(
        &self,
        skeleton: &Skeleton,
        pose: &mut Pose,
        map: &HumanoidMap,
        head: BoneId,
        look: LookAt,
        gain: f32,
    ) {
        use HumanoidBone as B;
        let chest = map.get(B::Chest).or_else(|| map.get(B::UpperChest));
        let bones = [chest, map.get(B::Neck), Some(head)];
        let mut shares = [0, 1, 2].map(|i| {
            let s = self.params.shares[i];
            if bones[i].is_some() && s.is_finite() && s > 0.0 { s } else { 0.0 }
        });
        if shares.iter().sum::<f32>() <= 0.0 {
            shares = [0.0, 0.0, 1.0]; // every share zero or unusable: the head takes the turn
        }
        let total: f32 = shares.iter().sum();
        let Some(first) = bones.iter().flatten().next() else { return };
        let body = aligned(skeleton, pose, skeleton.bones()[first.index()].parent);
        let reach = gain * (1.0 - look.eye_share.clamp(0.0, 1.0));

        let (mut prefix, mut done) = (0.0, 0.0);
        for (bone, share) in bones.iter().zip(shares) {
            let Some(bone) = *bone else { continue };
            prefix += share;
            let reached = reach * (prefix / total);
            let rest = 1.0 - done;
            let step = if rest > DONE { (reached - done) / rest } else { 0.0 };
            done = reached;
            if step <= 0.0 {
                continue;
            }
            // Measured from the head's place *now*: turning the chest can carry the head onto the target.
            let from = world(skeleton, head, |b| pose.local[b.index()]).translation;
            let near = near_fade((look.target - from).length());
            let Some(dir) = (look.target - from).try_normalize().filter(|_| near > 0.0) else { continue };
            let goal = body * self.limit(body.inverse() * dir);
            let forward = aligned(skeleton, pose, Some(head)) * Vec3::Z;
            let arc = swing(forward, goal);
            turn_bone(skeleton, pose, bone, Quat::IDENTITY.slerp(arc, step * near));
        }
    }

    /// The mapped eye turn for the unit direction `d` in the head frame: the signed yaw (+X) and pitch (up), degrees.
    fn eye_angles(&self, d: Vec3, side: Side) -> (f32, f32) {
        let e = &self.params.eyes;
        let (yaw, pitch) = yaw_pitch(d);
        let outer = match side {
            Side::Left | Side::Expression => yaw >= 0.0,
            Side::Right => yaw < 0.0,
        };
        let map = if matches!(side, Side::Expression) || outer { e.horizontal_outer } else { e.horizontal_inner };
        let yaw = limited_yaw(yaw, map.input_max_value.max(0.0).to_radians()) * pole_keep(d);
        let vertical = if pitch >= 0.0 { e.vertical_up } else { e.vertical_down };
        let pitch = pitch.abs().min(vertical.input_max_value.max(0.0).to_radians());
        (map.map(yaw.to_degrees()).copysign(yaw), vertical.map(pitch.to_degrees()).copysign(d.y))
    }

    /// Aims the eyes at the target from wherever the head ended up.
    fn turn_eyes(
        &mut self,
        skeleton: &Skeleton,
        pose: &mut Pose,
        map: &HumanoidMap,
        head: BoneId,
        look: LookAt,
        gain: f32,
    ) {
        let from = world(skeleton, head, |b| pose.local[b.index()]).translation;
        let gain =
            gain * (look.eye_share.clamp(0.0, 1.0) / EYE_RAMP).min(1.0) * near_fade((look.target - from).length());
        if gain <= 0.0 {
            return;
        }
        let Some(dir) = (look.target - from).try_normalize() else { return };
        let frame = aligned(skeleton, pose, Some(head));
        let d = frame.inverse() * dir;
        match self.params.eye_kind {
            EyeKind::Expression => {
                let (yaw, pitch) = self.eye_angles(d, Side::Expression);
                self.eyes = EyeWeights {
                    look_left: (yaw.max(0.0) * gain).min(1.0),
                    look_right: ((-yaw).max(0.0) * gain).min(1.0),
                    look_up: (pitch.max(0.0) * gain).min(1.0),
                    look_down: ((-pitch).max(0.0) * gain).min(1.0),
                };
            }
            EyeKind::Bone => {
                for (bone, side) in
                    [(map.get(HumanoidBone::LeftEye), Side::Left), (map.get(HumanoidBone::RightEye), Side::Right)]
                {
                    let Some(bone) = bone else { continue };
                    let (yaw, pitch) = self.eye_angles(d, side);
                    // Yaw about +Y carries +Z toward +X; looking up is a turn about -X.
                    let turn = Quat::from_rotation_y(yaw.to_radians()) * Quat::from_rotation_x(-pitch.to_radians());
                    turn_bone(skeleton, pose, bone, frame * Quat::IDENTITY.slerp(turn, gain) * frame.inverse());
                }
            }
        }
    }
}

impl IkSolver for LookAtIk {
    fn solve(&mut self, skeleton: &Skeleton, goals: &IkGoals, pose: &mut Pose) {
        self.eyes = EyeWeights::ZERO;
        let Some(look) = goals.finite_only().look_at.filter(|l| l.weight > 0.0) else { return };
        let Some(map) = skeleton.humanoid_map() else { return };
        let Some(head) = map.get(HumanoidBone::Head) else { return };
        let gain = look.weight.clamp(0.0, 1.0);
        self.turn_chain(skeleton, pose, map, head, look, gain);
        self.turn_eyes(skeleton, pose, map, head, look, gain);
    }

    /// Clears the last eye weights; nothing else is carried between frames.
    fn reset(&mut self) {
        self.eyes = EyeWeights::ZERO;
    }
}
