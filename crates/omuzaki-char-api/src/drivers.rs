//! Bone drivers: something other than the humanoid solve poses an extra bone (wings that fold, floppy ears, a
//! corrective shoulder helper).
//!
//! A [`BoneDriver`] says "this extra bone is posed by that". The `that` is a [`Drive`]:
//!
//! - [`Drive::Channel`]: a `customNN` channel moves the bone between two local transforms (`from` at 0, `to` at 1).
//!   This is the wings' `fold`.
//! - [`Drive::Follow`]: the bone copies a share of another bone's rotation away from its rest, optionally only the
//!   twist about one axis. This is the corrective **shoulder helper bone**: a bone under the shoulder driven at about
//!   half the upper arm's rotation is `Follow { source: upper_arm, axis: None, weight: 0.5 }`.
//!
//! ## Where drivers live
//!
//! A part lists them in [`RigContribution::drivers`](crate::RigContribution) with provisional bone ids (like spring
//! chains); the builder rebases and validates them ([`validate`]) after each part, and carries the result in
//! [`CharacterRig::drivers`](crate::CharacterRig). Only **extra** bones can be driven (the animation owns the humanoid
//! ones), each at most once, and a `Follow` source must have a lower bone id than the driven bone, which makes the
//! evaluation order a plain sort and rules out cycles.
//!
//! ## Real time
//!
//! [`DriverPlan::compile`] is cold: it validates, sorts by bone id and resolves rest rotations. [`DriverPlan::apply`]
//! is the per-frame call. It allocates nothing, takes no lock, and costs a few multiplies per driver
//! (`cargo bench -p omuzaki-char-api --bench drivers`). Call it on the local [`Pose`] after the humanoid pose, IK
//! and node constraints and **before the springs**, where VRM puts node constraints (`omuzaki-rig`'s pre-spring
//! stage, `FullRig::solve_and_skin_with`), so a spring chain under a driven bone follows it in the same frame; then
//! [`Skeleton::skin_matrices`](omuzaki_anim_api::Skeleton::skin_matrices).
//!
//! ## Export (VRM 1.0 and glTF cannot carry a driver)
//!
//! [`export_plan`] maps each driver to what a file can hold:
//!
//! - A `Follow` driver is exactly a `VRMC_node_constraint` ([`NodeConstraint`]): a rotation constraint (`axis: None`)
//!   or a roll constraint (`axis: Some`), with the source node and the weight. Its maths here is the spec's, so the
//!   viewer reproduces it with no baking.
//! - A `Channel` driver has no node to be a constraint on, so it is **baked**: the node keeps its skeleton rest and the
//!   file gets a two-key animation clip named after the channel ([`BakedDriver`]), `from` at key 0 and `to` at key 1,
//!   with the channel value as the clip time. An engine can scrub it with the same `0..1` value. A viewer that ignores
//!   the clip shows the rest pose. [`Mapping::Linear`] outside `0..1` cannot be baked: the clip clamps.
//!
//! Writing these into a file is the exporter's job (`omuzaki-io-ops`), not this crate's.

use omuzaki_anim_api::{BoneId, Channel, ChannelBuf, Pose, Region, Skeleton, Transform};
use omuzaki_core::glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// How a channel value maps onto `from..to`. Non-exhaustive: later mappings (ease curves) are not breaking changes.
///
/// Outside the crate a match must have a wildcard arm. This example matches every variant, so it must not compile
/// (E0004); if `#[non_exhaustive]` is removed, it compiles and the doctest fails.
///
/// ```compile_fail,E0004
/// use omuzaki_char_api::drivers::Mapping;
///
/// fn name(mapping: Mapping) -> &'static str {
///     match mapping {
///         Mapping::Clamped => "clamped",
///         Mapping::Linear => "linear",
///     }
/// }
/// ```
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Mapping {
    /// The value is clamped to `0..1` first. The default; the only mapping that exports.
    #[default]
    Clamped,
    /// The value is used as is, so it extrapolates past `from` and `to`.
    Linear,
}

/// The axis of a roll constraint, in the driven bone's own space.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RollAxis {
    /// The bone's X axis.
    X,
    /// The bone's Y axis.
    Y,
    /// The bone's Z axis.
    Z,
}

impl RollAxis {
    fn vector(self) -> Vec3 {
        match self {
            Self::X => Vec3::X,
            Self::Y => Vec3::Y,
            Self::Z => Vec3::Z,
        }
    }

    /// The name VRM 1.0 uses for it (`rollAxis`).
    pub const fn name(self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Z => "Z",
        }
    }
}

/// What poses a driven bone. Non-exhaustive: later drive kinds (VRM's aim constraint) are not breaking changes.
///
/// Outside the crate a match must have a wildcard arm. This example matches every variant, so it must not compile
/// (E0004); if `#[non_exhaustive]` is removed, it compiles and the doctest fails.
///
/// ```compile_fail,E0004
/// use omuzaki_char_api::drivers::Drive;
///
/// fn kind(drive: Drive) -> &'static str {
///     match drive {
///         Drive::Channel { .. } => "channel",
///         Drive::Follow { .. } => "follow",
///     }
/// }
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Drive {
    /// A `custom` channel moves the bone between two local transforms.
    #[serde(rename_all = "camelCase")]
    Channel {
        /// The channel: one of the `customNN` slots.
        channel: Channel,
        /// The local transform at channel value 0 (parent-relative, like [`BoneDef::rest`](omuzaki_anim_api::BoneDef)).
        from: Transform,
        /// The local transform at channel value 1.
        to: Transform,
        /// How the value maps between them.
        #[serde(default)]
        mapping: Mapping,
    },
    /// The bone copies a share of `source`'s rotation away from its rest (VRM's node constraint, so it exports as one).
    #[serde(rename_all = "camelCase")]
    Follow {
        /// The bone to follow. Its id is lower than the driven bone's.
        source: BoneId,
        /// `None`: the whole rotation (a rotation constraint). `Some`: only the twist about the driven bone's axis (a
        /// roll constraint).
        #[serde(default)]
        axis: Option<RollAxis>,
        /// The share followed, `0..=1`.
        weight: f32,
    },
}

/// One driven bone. In a [`RigContribution`](crate::RigContribution) the ids are provisional ([`extra_id`](crate::extra_id),
/// [`humanoid_id`](crate::humanoid_id)); in a [`CharacterRig`](crate::CharacterRig) they are skeleton ids.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BoneDriver {
    /// The bone that is posed. An extra bone, driven by at most one driver.
    pub bone: BoneId,
    /// What poses it.
    pub drive: Drive,
}

impl BoneDriver {
    /// A driver that moves `bone` from `from` (channel 0) to `to` (channel 1), clamped.
    pub const fn channel(bone: BoneId, channel: Channel, from: Transform, to: Transform) -> Self {
        Self { bone, drive: Drive::Channel { channel, from, to, mapping: Mapping::Clamped } }
    }

    /// A driver that makes `bone` follow `weight` of `source`'s whole rotation.
    pub const fn follow(bone: BoneId, source: BoneId, weight: f32) -> Self {
        Self { bone, drive: Drive::Follow { source, axis: None, weight } }
    }
}

/// Why drivers can't be used with a skeleton. Each variant names the bone and says what to fix.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum DriverError {
    /// The channel is not a `custom` slot, so something else (the face, the pose stage) already owns it.
    #[error("the driver of bone '{bone}' reads channel '{}', which is not a custom slot: use custom00..custom15", .channel.name())]
    NotCustomChannel {
        /// The driven bone.
        bone: String,
        /// The channel.
        channel: Channel,
    },
    /// A bone id is not in the skeleton.
    #[error("a driver names bone id {} as its {role}, which the skeleton lacks: use `extra_id` or `humanoid_id`", .id.0)]
    UnknownBone {
        /// `driven bone` or `source bone`.
        role: &'static str,
        /// The id.
        id: BoneId,
    },
    /// The driven bone is a humanoid bone, which the animation poses.
    #[error("bone '{bone}' is a humanoid bone, which the animation poses: drive an extra bone")]
    HumanoidBone {
        /// The bone.
        bone: String,
    },
    /// Two drivers pose one bone.
    #[error("bone '{bone}' has two drivers: merge them or drive different bones")]
    DuplicateBone {
        /// The bone.
        bone: String,
    },
    /// A follow source does not come before the driven bone, so the order (and acyclicity) is not guaranteed.
    #[error("bone '{bone}' follows '{source_bone}', which comes later in the skeleton: follow a bone with a lower id")]
    SourceNotEarlier {
        /// The driven bone.
        bone: String,
        /// The source.
        source_bone: String,
    },
    /// A transform, or the weight, is not finite, or a rotation is not of unit length.
    #[error("the driver of bone '{bone}' has a transform that isn't finite with a unit rotation")]
    BadTransform {
        /// The driven bone.
        bone: String,
    },
    /// The weight is outside `0..=1`, or not a number.
    #[error("the driver of bone '{bone}' has weight {weight}, which is not in 0..=1")]
    BadWeight {
        /// The driven bone.
        bone: String,
        /// The weight.
        weight: f32,
    },
}

fn name_of(skeleton: &Skeleton, id: BoneId) -> String {
    skeleton.get(id).map_or_else(|| format!("#{}", id.0), |def| def.name.clone())
}

fn sound(t: &Transform) -> bool {
    t.translation.is_finite() && t.rotation.is_finite() && t.rotation.is_normalized()
}

/// Checks `drivers` against `skeleton`: every channel is a `custom` slot, every bone exists, driven bones are extra
/// bones with one driver each, follow sources come earlier, and the numbers are sound.
///
/// # Errors
/// The first [`DriverError`], in list order.
pub fn validate(skeleton: &Skeleton, drivers: &[BoneDriver]) -> Result<(), DriverError> {
    let mut seen = vec![false; skeleton.len()];
    for driver in drivers {
        let def = skeleton.get(driver.bone).ok_or(DriverError::UnknownBone { role: "driven bone", id: driver.bone })?;
        let bone = def.name.clone();
        if def.humanoid.is_some() {
            return Err(DriverError::HumanoidBone { bone });
        }
        if std::mem::replace(&mut seen[driver.bone.index()], true) {
            return Err(DriverError::DuplicateBone { bone });
        }
        match driver.drive {
            Drive::Channel { channel, from, to, .. } => {
                if channel.region() != Region::Custom {
                    return Err(DriverError::NotCustomChannel { bone, channel });
                }
                if !(sound(&from) && sound(&to)) {
                    return Err(DriverError::BadTransform { bone });
                }
            }
            Drive::Follow { source, weight, .. } => {
                if skeleton.get(source).is_none() {
                    return Err(DriverError::UnknownBone { role: "source bone", id: source });
                }
                if source >= driver.bone {
                    return Err(DriverError::SourceNotEarlier { bone, source_bone: name_of(skeleton, source) });
                }
                if !(0.0..=1.0).contains(&weight) {
                    return Err(DriverError::BadWeight { bone, weight });
                }
            }
        }
    }
    Ok(())
}

/// One step of a plan: a driver with everything the frame needs resolved.
#[derive(Copy, Clone, Debug)]
struct Step {
    bone: usize,
    kind: StepKind,
}

#[derive(Copy, Clone, Debug)]
enum StepKind {
    Channel { channel: Channel, from: Transform, to: Transform, clamp: bool },
    Follow { source: usize, source_rest_inverse: Quat, rest: Quat, axis: Option<Vec3>, weight: f32 },
}

/// Drivers ready to run every frame: validated, sorted by bone id (parents and sources first) and with rest rotations
/// resolved. Build it once per character with [`DriverPlan::compile`].
#[derive(Clone, Debug, Default)]
pub struct DriverPlan {
    steps: Vec<Step>,
}

impl DriverPlan {
    /// Validates `drivers` and builds the plan. Cold: allocates.
    ///
    /// # Errors
    /// A [`DriverError`] from [`validate`].
    pub fn compile(skeleton: &Skeleton, drivers: &[BoneDriver]) -> Result<Self, DriverError> {
        validate(skeleton, drivers)?;
        let rest_of = |id: BoneId| skeleton.get(id).map_or(Quat::IDENTITY, |def| def.rest.rotation);
        let mut steps: Vec<Step> = drivers
            .iter()
            .map(|d| Step {
                bone: d.bone.index(),
                kind: match d.drive {
                    Drive::Channel { channel, from, to, mapping } => {
                        StepKind::Channel { channel, from, to, clamp: mapping == Mapping::Clamped }
                    }
                    Drive::Follow { source, axis, weight } => StepKind::Follow {
                        source: source.index(),
                        source_rest_inverse: rest_of(source).inverse(),
                        rest: rest_of(d.bone),
                        axis: axis.map(RollAxis::vector),
                        weight,
                    },
                },
            })
            .collect();
        steps.sort_by_key(|s| s.bone);
        Ok(Self { steps })
    }

    /// How many drivers the plan runs.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// True if there is nothing to run.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Poses every driven bone of `pose` (local transforms) from `channels` and from the pose's other bones.
    ///
    /// Hot path: no allocation, no lock, deterministic. A channel value that is not finite counts as 0. A `pose` from
    /// a different skeleton than the plan was compiled for is a caller error, not a panic: bones past the pose are
    /// skipped.
    pub fn apply(&self, channels: &ChannelBuf, pose: &mut Pose) {
        for step in &self.steps {
            let local = match step.kind {
                StepKind::Channel { channel, from, to, clamp } => {
                    let value = channels.get(channel);
                    let value = if value.is_finite() { value } else { 0.0 };
                    let t = if clamp { value.clamp(0.0, 1.0) } else { value };
                    Some(Transform {
                        translation: from.translation.lerp(to.translation, t),
                        rotation: from.rotation.slerp(to.rotation, t).normalize(),
                    })
                }
                StepKind::Follow { source, source_rest_inverse, rest, axis, weight } => {
                    let current = pose.local.get(source).map(|t| t.rotation);
                    current.map(|current| {
                        let mut local = pose.local.get(step.bone).copied().unwrap_or(Transform::IDENTITY);
                        local.rotation =
                            (rest * follow_share(current, source_rest_inverse, rest, axis, weight)).normalize();
                        local
                    })
                }
            };
            if let (Some(local), Some(slot)) = (local, pose.local.get_mut(step.bone)) {
                *slot = local;
            }
        }
    }
}

/// `slerp(identity, delta, weight)` for a rotation constraint, where `delta` is the source's rotation away from its
/// rest. For a roll constraint the delta is first carried through the source's rest into the shared parent space
/// (`current * source_rest⁻¹`), then into the driven bone's space (conjugating by `rest`) and reduced to its twist
/// about `axis`: the conversion `VRMC_node_constraint` specifies, so runtime and exported VRM agree.
fn follow_share(current: Quat, source_rest_inverse: Quat, rest: Quat, axis: Option<Vec3>, weight: f32) -> Quat {
    let share = match axis {
        None => source_rest_inverse * current,
        Some(axis) => twist(rest.inverse() * (current * source_rest_inverse) * rest, axis),
    };
    Quat::IDENTITY.slerp(share, weight)
}

/// The part of `q` that turns about `axis` (swing-twist decomposition); the identity when `q` is a half turn across it.
fn twist(q: Quat, axis: Vec3) -> Quat {
    let along = axis * Vec3::new(q.x, q.y, q.z).dot(axis);
    let t = Quat::from_xyzw(along.x, along.y, along.z, q.w);
    if t.length_squared() > f32::EPSILON { t.normalize() } else { Quat::IDENTITY }
}

/// A driver exported as a VRM 1.0 `VRMC_node_constraint` on the driven node.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NodeConstraint {
    /// The constrained (driven) bone.
    pub bone: BoneId,
    /// The source bone; its glTF node index goes in the extension.
    pub source: BoneId,
    /// `None` for a rotation constraint, `Some` for a roll constraint.
    pub axis: Option<RollAxis>,
    /// The share followed, `0..=1`.
    pub weight: f32,
}

impl NodeConstraint {
    /// The `VRMC_node_constraint` extension object for the driven node, given the glTF node index of the source bone.
    pub fn extension(&self, source_node: usize) -> serde_json::Value {
        let constraint = match self.axis {
            None => serde_json::json!({ "rotation": { "source": source_node, "weight": self.weight } }),
            Some(axis) => serde_json::json!({
                "roll": { "source": source_node, "rollAxis": axis.name(), "weight": self.weight }
            }),
        };
        serde_json::json!({ "specVersion": "1.0", "constraint": constraint })
    }
}

/// A channel driver exported as a two-key animation: `from` at time 0, `to` at time 1, the channel value as the time.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BakedDriver {
    /// The animated bone.
    pub bone: BoneId,
    /// The channel; the clip is named after it.
    pub channel: Channel,
    /// The local transform at key 0.
    pub from: Transform,
    /// The local transform at key 1.
    pub to: Transform,
}

/// What one driver becomes in a VRM or glTF file.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum DriverExport {
    /// A `VRMC_node_constraint`.
    Constraint(NodeConstraint),
    /// A baked animation clip.
    Baked(BakedDriver),
}

/// Maps each driver to its export form, in list order (see the module docs). Cold: allocates.
pub fn export_plan(drivers: &[BoneDriver]) -> Vec<DriverExport> {
    drivers
        .iter()
        .map(|d| match d.drive {
            Drive::Channel { channel, from, to, .. } => {
                DriverExport::Baked(BakedDriver { bone: d.bone, channel, from, to })
            }
            Drive::Follow { source, axis, weight } => {
                DriverExport::Constraint(NodeConstraint { bone: d.bone, source, axis, weight })
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::f32::consts::FRAC_PI_2;

    use omuzaki_anim_api::{BoneDef, HumanoidBone};

    use super::*;

    const EPS: f32 = 1e-5;

    fn def(name: &str, parent: Option<u16>, rest: Transform) -> BoneDef {
        BoneDef { name: name.to_owned(), parent: parent.map(BoneId), rest, humanoid: None }
    }

    /// root, arm (rotated at rest), helper: all generic, so every bone is "extra".
    fn skeleton() -> Skeleton {
        let tilted = Transform { translation: Vec3::ZERO, rotation: Quat::from_rotation_y(0.3) };
        Skeleton::generic(vec![
            def("root", None, Transform::IDENTITY),
            def("arm", Some(0), Transform::IDENTITY),
            def("helper", Some(1), tilted),
            def("wing", Some(0), Transform::IDENTITY),
        ])
        .unwrap()
    }

    fn fold_to(angle: f32) -> Transform {
        Transform { translation: Vec3::new(0.0, 0.0, 0.4), rotation: Quat::from_rotation_z(angle) }
    }

    fn wing_driver(mapping: Mapping) -> BoneDriver {
        let channel = Channel::custom(2).unwrap();
        BoneDriver {
            bone: BoneId(3),
            drive: Drive::Channel { channel, from: Transform::IDENTITY, to: fold_to(1.0), mapping },
        }
    }

    fn run(drivers: &[BoneDriver], value: f32, edit: impl FnOnce(&mut Pose)) -> Pose {
        let skeleton = skeleton();
        let mut pose = skeleton.rest_pose();
        edit(&mut pose);
        let mut channels = ChannelBuf::NEUTRAL;
        channels.set(Channel::custom(2).unwrap(), value);
        DriverPlan::compile(&skeleton, drivers).unwrap().apply(&channels, &mut pose);
        pose
    }

    #[test]
    fn a_channel_driver_interpolates_linearly_between_from_and_to() {
        let drivers = [wing_driver(Mapping::Clamped)];
        let at = |v| run(&drivers, v, |_| {}).local[3];
        assert_eq!(at(0.0), Transform::IDENTITY);
        assert!((at(0.5).rotation.angle_between(Quat::IDENTITY) - 0.5).abs() < EPS);
        assert!((at(0.5).translation.z - 0.2).abs() < EPS);
        assert!(at(1.0).rotation.angle_between(Quat::from_rotation_z(1.0)) < EPS);
    }

    #[test]
    fn clamped_holds_the_ends_and_linear_extrapolates() {
        let clamped = [wing_driver(Mapping::Clamped)];
        let linear = [wing_driver(Mapping::Linear)];
        // Below 0, above 1 and not a number all hold an end (a non-finite value counts as 0).
        for (value, spread) in [(-3.0, false), (2.0, true), (f32::NAN, false), (f32::INFINITY, false)] {
            let got = run(&clamped, value, |_| {}).local[3];
            let want = if spread { fold_to(1.0) } else { Transform::IDENTITY };
            assert!(got.translation.distance(want.translation) < EPS, "value {value}");
            assert!(got.rotation.angle_between(want.rotation) < EPS, "value {value}");
        }
        assert!((run(&linear, 2.0, |_| {}).local[3].translation.z - 0.8).abs() < EPS, "twice the way");
    }

    #[test]
    fn a_follow_driver_copies_a_share_of_the_source_rotation_away_from_its_rest() {
        let drivers = [BoneDriver::follow(BoneId(2), BoneId(1), 0.5)];
        let pose = run(&drivers, 0.0, |p| p.local[1].rotation = Quat::from_rotation_x(1.0));
        let rest = Quat::from_rotation_y(0.3);
        let want = rest * Quat::from_rotation_x(0.5);
        assert!(pose.local[2].rotation.angle_between(want) < EPS);
        // Untouched at the source's rest.
        let at_rest = run(&drivers, 0.0, |_| {});
        assert!(at_rest.local[2].rotation.angle_between(rest) < EPS);
    }

    #[test]
    fn a_roll_driver_keeps_only_the_twist_about_its_axis() {
        let driver = BoneDriver {
            bone: BoneId(3),
            drive: Drive::Follow { source: BoneId(1), axis: Some(RollAxis::X), weight: 1.0 },
        };
        // Twist 0.8 about X plus a swing about Y: only the X part comes through.
        let source = Quat::from_rotation_y(0.4) * Quat::from_rotation_x(0.8);
        let pose = run(&[driver], 0.0, |p| p.local[1].rotation = source);
        let twist_only = pose.local[3].rotation;
        let (axis, angle) = twist_only.to_axis_angle();
        assert!(axis.x.abs() > 0.999 && angle > 0.0, "a rotation about X: {axis:?}");
        assert!(angle < 0.8 + 0.05 && angle > 0.5, "roughly the X twist, got {angle}");
    }

    #[test]
    fn a_roll_driver_converts_through_the_source_and_destination_rests() {
        // Source rest Rz(90°), destination rest Ry(0.3), roll about X. The source turns 0.8 about its own X, which in
        // the shared parent space is a turn about Y: after the destination's rest it has no X twist left to follow.
        let sk = Skeleton::generic(vec![
            def("root", None, Transform::IDENTITY),
            def("src", Some(0), Transform { translation: Vec3::ZERO, rotation: Quat::from_rotation_z(FRAC_PI_2) }),
            def("dst", Some(0), Transform { translation: Vec3::ZERO, rotation: Quat::from_rotation_y(0.3) }),
        ])
        .unwrap();
        let roll = |axis, weight| BoneDriver {
            bone: BoneId(2),
            drive: Drive::Follow { source: BoneId(1), axis: Some(axis), weight },
        };
        let solve = |driver: BoneDriver, local_turn: Quat| {
            let mut pose = sk.rest_pose();
            pose.local[1].rotation = Quat::from_rotation_z(FRAC_PI_2) * local_turn;
            DriverPlan::compile(&sk, &[driver]).unwrap().apply(&ChannelBuf::NEUTRAL, &mut pose);
            pose.local[2].rotation
        };
        let dst_rest = Quat::from_rotation_y(0.3);
        let turn = Quat::from_rotation_x(0.8);
        assert!(solve(roll(RollAxis::X, 1.0), turn).angle_between(dst_rest) < EPS, "the old frame mix-up followed 0.8");
        // The parent-space Y turn is, in the destination's space, a turn about Y: that is the roll axis here.
        let followed = solve(roll(RollAxis::Y, 1.0), turn);
        assert!(followed.angle_between(dst_rest * Quat::from_rotation_y(0.8)) < EPS);
        let half = solve(roll(RollAxis::Y, 0.5), turn);
        assert!(half.angle_between(dst_rest * Quat::from_rotation_y(0.4)) < EPS);
    }

    #[test]
    fn evaluation_is_deterministic_and_leaves_other_bones_alone() {
        let drivers = [BoneDriver::follow(BoneId(2), BoneId(1), 0.5), wing_driver(Mapping::Clamped)];
        let edit = |p: &mut Pose| p.local[1].rotation = Quat::from_rotation_x(0.7);
        let (a, b) = (run(&drivers, 0.3, edit), run(&drivers, 0.3, edit));
        assert_eq!(a, b);
        let untouched = run(&[], 0.3, edit);
        assert_eq!(a.local[0], untouched.local[0]);
        assert_eq!(a.local[1], untouched.local[1]);
        // Driver order in the list does not matter: the plan sorts.
        let reversed = [drivers[1], drivers[0]];
        assert_eq!(run(&reversed, 0.3, edit), a);
    }

    #[test]
    fn invalid_drivers_are_typed_errors() {
        let sk = skeleton();
        let err = |d: BoneDriver| validate(&sk, &[d]).unwrap_err();
        let channel = |c| BoneDriver::channel(BoneId(3), c, Transform::IDENTITY, fold_to(1.0));
        assert!(
            matches!(err(channel(Channel::JAW_OPEN)), DriverError::NotCustomChannel { bone, .. } if bone == "wing")
        );
        assert!(matches!(
            err(BoneDriver::follow(BoneId(40), BoneId(0), 0.5)),
            DriverError::UnknownBone { role: "driven bone", .. }
        ));
        assert!(matches!(
            err(BoneDriver::follow(BoneId(2), BoneId(40), 0.5)),
            DriverError::UnknownBone { role: "source bone", .. }
        ));
        assert!(matches!(err(BoneDriver::follow(BoneId(1), BoneId(1), 0.5)), DriverError::SourceNotEarlier { .. }));
        assert!(matches!(err(BoneDriver::follow(BoneId(2), BoneId(1), f32::NAN)), DriverError::BadWeight { .. }));
        let nan = Transform::from_translation(Vec3::NAN);
        assert!(matches!(
            err(BoneDriver::channel(BoneId(3), Channel::custom(0).unwrap(), nan, nan)),
            DriverError::BadTransform { .. }
        ));
        let twice = [wing_driver(Mapping::Clamped), wing_driver(Mapping::Linear)];
        assert!(matches!(validate(&sk, &twice), Err(DriverError::DuplicateBone { .. })));
        assert!(DriverPlan::compile(&sk, &[]).unwrap().is_empty());
    }

    #[test]
    fn a_humanoid_bone_cannot_be_driven() {
        let sk = Skeleton::humanoid(&required(), |_| Transform::IDENTITY).unwrap();
        let head = sk.humanoid_bone(HumanoidBone::Head).unwrap();
        let driver = BoneDriver::channel(head, Channel::custom(0).unwrap(), Transform::IDENTITY, fold_to(1.0));
        assert!(matches!(validate(&sk, &[driver]), Err(DriverError::HumanoidBone { bone }) if bone == "head"));
    }

    fn required() -> Vec<HumanoidBone> {
        HumanoidBone::ALL.into_iter().filter(|b| b.is_required()).collect()
    }

    #[test]
    fn export_maps_follow_to_a_node_constraint_and_a_channel_to_a_baked_clip() {
        let rotation = BoneDriver::follow(BoneId(2), BoneId(1), 0.5);
        let roll = BoneDriver {
            bone: BoneId(3),
            drive: Drive::Follow { source: BoneId(1), axis: Some(RollAxis::Z), weight: 0.25 },
        };
        let channel = wing_driver(Mapping::Clamped);
        let plan = export_plan(&[rotation, roll, channel]);
        let DriverExport::Constraint(c) = plan[0] else { panic!("a follow driver exports as a constraint") };
        assert_eq!(
            c.extension(7),
            serde_json::json!({
                "specVersion": "1.0",
                "constraint": { "rotation": { "source": 7, "weight": 0.5 } }
            })
        );
        let DriverExport::Constraint(c) = plan[1] else { panic!("a roll driver exports as a constraint") };
        assert_eq!(c.extension(7)["constraint"]["roll"]["rollAxis"], "Z");
        assert!(
            matches!(plan[2], DriverExport::Baked(b) if b.channel == Channel::custom(2).unwrap() && b.to == fold_to(1.0))
        );
    }
}
