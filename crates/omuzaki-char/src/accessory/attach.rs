//! Attachment (CB-43): where an accessory is fixed, resolved to a bind-space transform and a joint.
//!
//! A part's [`PartDef::attach`](omuzaki_char_api::PartDef) says *to what* (a bone, or an anchor another part publishes)
//! and *how far from it* ([`AttachOffset`]). Every accessory kind calls [`attachment`] once: it gets a
//! [`ResolvedAttach`], the joint the accessory is skinned to and the transform from the accessory's **local frame**
//! to bind space. The local frame is the same for every kind: the origin is the attach point, `+Y` is up, `+Z` is the
//! front and `+X` is the character's left. A kind builds its geometry in that frame and places it with
//! [`ResolvedAttach::point`].
//!
//! **Frames.** A bone's frame has its origin at the bone's joint ([`BuildCtx::joint`], so a head the head part lowered
//! counts) and the bone's orientation in the bind pose ([`Published::bind`]); in the T-pose that is the bind-space axes,
//! in an A-pose a hand carries the hang of the whole arm (the rotations of its ancestors multiplied down from the
//! root). A bone a part contributed (`halo_acc_0`) is found at its rest position with the bind-space axes. An anchor's frame is the anchor's own position and rotation. The
//! offset is applied in that frame: translation in metres, then the rotation, then the uniform scale.
//!
//! **Offset rotation** is in degrees about x, y and z, *applied in that order*: x first, then y, then z, each about the
//! fixed frame axes. As a matrix that is `Rz * Ry * Rx` ([`offset_rotation`]); [`euler_xyz_deg`] is its inverse.
//!
//! [`attach_points`] lists the bones and anchors a part could be attached to, with positions. The
//! `accessory.list_anchors` query is the same list for a character.

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_char_api::{
    Anchor, AnchorId, AnchorsFact, Attach, AttachOffset, BoneName, BuildCtx, BuildError, BuiltCharacter, CharId,
    JointRef, PartId, Published,
};
use omuzaki_core::glam::{DMat4, DQuat, DVec3, EulerRot, Quat, Vec3};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ops::character::access;
use crate::ops::weights::build_character;

/// An accessory's attachment, resolved: the joint it is skinned to and where its local frame sits in bind space.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedAttach {
    /// The joint the accessory follows: the bone itself, or the bone the anchor follows.
    pub joint: JointRef,
    /// Local frame to bind space: the target's frame, then the offset (translation, rotation, scale).
    pub to_bind: DMat4,
}

impl ResolvedAttach {
    /// A frame at `to_bind` that follows `joint`.
    pub fn new(joint: JointRef, to_bind: DMat4) -> Self {
        Self { joint, to_bind }
    }

    /// The point `local` of the accessory's frame, in bind space.
    pub fn point(&self, local: DVec3) -> DVec3 {
        self.to_bind.transform_point3(local)
    }

    /// The direction `local` of the accessory's frame, in bind space (not normalised: a scaled offset scales it).
    pub fn direction(&self, local: DVec3) -> DVec3 {
        self.to_bind.transform_vector3(local)
    }

    /// The uniform scale of the frame: the offset's scale, which sizes everything built in it.
    pub fn scale(&self) -> f64 {
        self.to_bind.x_axis.truncate().length()
    }

    /// Where the attach point is, in bind space.
    pub fn origin(&self) -> DVec3 {
        self.to_bind.w_axis.truncate()
    }
}

/// The rotation of an offset: `rotation_deg` about x, then y, then z (each about the fixed frame axes), which as a
/// matrix is `Rz * Ry * Rx`.
pub fn offset_rotation(rotation_deg: [f64; 3]) -> DQuat {
    let [x, y, z] = rotation_deg.map(f64::to_radians);
    DQuat::from_euler(EulerRot::ZYX, z, y, x)
}

/// The inverse of [`offset_rotation`]: the degrees about x, y, z that give `rotation`. Unique for y within ±90 degrees;
/// at ±90 (gimbal lock) x is taken as 0.
pub fn euler_xyz_deg(rotation: DQuat) -> [f64; 3] {
    let (z, y, x) = rotation.to_euler(EulerRot::ZYX);
    [x, y, z].map(f64::to_degrees)
}

/// The matrix of an offset: scale, then rotation, then translation, in the target's frame.
///
/// # Errors
/// A message if a number is not finite or the scale is not positive.
pub fn offset_matrix(offset: &AttachOffset) -> Result<DMat4, String> {
    let finite = offset.translation.iter().chain(&offset.rotation_deg).chain([&offset.scale]).all(|v| v.is_finite());
    if !finite {
        return Err("the offset has a number that is not finite".to_owned());
    }
    if offset.scale <= 0.0 {
        return Err(format!("the offset scale is {}: it must be greater than 0", offset.scale));
    }
    Ok(DMat4::from_scale_rotation_translation(
        DVec3::splat(offset.scale),
        offset_rotation(offset.rotation_deg),
        DVec3::from_array(offset.translation),
    ))
}

/// The world rotation of `bone` in `bind`. A [`HumanoidPose`] rotation is relative to the bone's parent (the skeleton
/// rests with identity rotations), so the world rotation is the product down the chain from the root: a hand inherits
/// the hang of the arm above it.
fn bind_rotation(bind: &HumanoidPose, bone: HumanoidBone) -> Quat {
    let local = bind.get(bone);
    bone.parent().map_or(local, |parent| bind_rotation(bind, parent) * local)
}

/// The frame of a joint: its position, and its orientation in the bind pose. Bones a part contributed (`halo_acc_0`)
/// have the bind-space axes.
fn joint_frame(published: &Published, joint: &JointRef) -> Option<DMat4> {
    let position = DVec3::from(published.joint(joint)?);
    let rotation = match joint {
        JointRef::Humanoid(bone) => bind_rotation(&published.bind(), *bone),
        JointRef::Named(_) => Quat::IDENTITY,
    };
    Some(DMat4::from_rotation_translation(rotation.as_dquat(), position))
}

/// The rotation of `joint` in the bind pose, world space: a humanoid bone's carries the hang of its ancestors; a bone a part
/// contributed has none.
pub(super) fn joint_rotation(published: &Published, joint: &JointRef) -> DQuat {
    match joint {
        JointRef::Humanoid(bone) => bind_rotation(&published.bind(), *bone).as_dquat(),
        JointRef::Named(_) => DQuat::IDENTITY,
    }
}

/// The frame of an anchor: its position and rotation.
fn anchor_frame(anchor: &Anchor) -> DMat4 {
    DMat4::from_rotation_translation(anchor.rotation.as_dquat(), DVec3::from(anchor.position))
}

/// The names of the anchors `owner` has published, for an error that says what is there.
fn published_by(published: &Published, owner: &PartId) -> Vec<String> {
    published.get::<AnchorsFact>().keys().filter(|(part, _)| part == owner).map(|(_, name)| name.to_string()).collect()
}

/// Resolves `attach` for `part` against what was published so far.
///
/// # Errors
/// [`BuildError::Part`] naming the part and what is missing: the joint the skeleton has no position for, the part that
/// published no anchors (it is disabled, or builds after this part), the anchor that part does not publish (with the
/// ones it does), or an offset with a number that is not finite or a scale that is not positive.
pub fn resolve(part: &PartId, attach: &Attach, published: &Published) -> Result<ResolvedAttach, BuildError> {
    let fail = |message: String| BuildError::Part { part: part.clone(), message };
    let offset = offset_matrix(attach.offset()).map_err(|m| fail(format!("attach: {m}")))?;
    let (joint, frame) = match attach {
        Attach::Bone { bone, .. } => {
            let frame = joint_frame(published, bone).ok_or_else(|| {
                fail(format!(
                    "it is attached to the bone {bone:?}, which no part has placed: attach it to a bone the body has"
                ))
            })?;
            (bone.clone(), frame)
        }
        Attach::Anchor { part: owner, anchor, .. } => {
            let found = published.get::<AnchorsFact>().get(&(owner.clone(), anchor.clone())).ok_or_else(|| {
                let names = published_by(published, owner);
                let there = if names.is_empty() {
                    format!(
                        "part '{owner}' has published no anchors (is it missing, disabled, or built after this part?)"
                    )
                } else {
                    format!("part '{owner}' publishes: {}", names.join(", "))
                };
                fail(format!("it is attached to anchor '{anchor}' of part '{owner}', which does not exist: {there}"))
            })?;
            (found.bone.clone(), anchor_frame(found))
        }
    };
    Ok(ResolvedAttach { joint, to_bind: frame * offset })
}

/// The attachment of the part being built: [`PartDef::attach`](omuzaki_char_api::PartDef) resolved, or, when the part has
/// none, what `default` makes (each kind decides where it sits when nobody said).
///
/// # Errors
/// As [`resolve`], or whatever `default` reports.
pub fn attachment(
    ctx: &BuildCtx<'_>,
    default: impl FnOnce(&BuildCtx<'_>) -> Result<ResolvedAttach, BuildError>,
) -> Result<ResolvedAttach, BuildError> {
    match ctx.attach {
        Some(attach) => resolve(ctx.part, attach, ctx.published),
        None => default(ctx),
    }
}

/// The default attachment on a bone: its frame, no offset.
///
/// # Errors
/// [`BuildError::Part`] if no part has placed `bone`.
pub fn on_bone(ctx: &BuildCtx<'_>, bone: &JointRef) -> Result<ResolvedAttach, BuildError> {
    resolve(ctx.part, &Attach::Bone { bone: bone.clone(), offset: AttachOffset::default() }, ctx.published)
}

// ------------------------------------------------------------------------------------------------ listing

/// What an attach point is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttachTarget {
    /// A bone of the skeleton: give it as `{"to": "bone", "bone": <joint>}`.
    Bone {
        /// The joint to name in `attach.bone`.
        joint: JointRef,
    },
    /// An anchor a part publishes: give it as `{"to": "anchor", "part": .., "anchor": ..}`.
    Anchor {
        /// The part that publishes it.
        part: PartId,
        /// Its name.
        anchor: AnchorId,
        /// The bone it follows.
        follows: JointRef,
    },
}

/// A place an accessory can be fixed, with where it is in bind space.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AttachPoint {
    /// A readable name: the bone's, or `<part>/<anchor>`.
    pub name: String,
    /// What it is.
    #[serde(flatten)]
    pub target: AttachTarget,
    /// Bind-space position, metres.
    pub position: [f32; 3],
}

/// Every bone of the built character's skeleton and every published anchor, with bind-space positions: bones first in
/// skeleton order, then anchors by `(part, name)`.
pub fn attach_points(built: &BuiltCharacter) -> Vec<AttachPoint> {
    let rig = &built.rig;
    let inverse_bind = rig.inverse_bind();
    let mut points: Vec<AttachPoint> = rig
        .skeleton
        .bones()
        .iter()
        .zip(&inverse_bind)
        .filter_map(|(bone, inverse)| {
            let joint = match bone.humanoid {
                Some(h) => JointRef::Humanoid(h),
                None => JointRef::Named(BoneName::new(&bone.name).ok()?),
            };
            let position = inverse.inverse().transform_point3(Vec3::ZERO);
            Some(AttachPoint {
                name: bone.name.clone(),
                target: AttachTarget::Bone { joint },
                position: position.to_array(),
            })
        })
        .collect();
    points.extend(built.anchors.iter().map(|((part, anchor), found)| AttachPoint {
        name: format!("{part}/{anchor}"),
        target: AttachTarget::Anchor { part: part.clone(), anchor: anchor.clone(), follows: found.bone.clone() },
        position: found.position.to_array(),
    }));
    points
}

/// Lists the bones and anchors accessories can be attached to, with their bind-space positions: a query, so it changes
/// nothing. Use a bone as `{"to": "bone", "bone": <joint>}` and an anchor as `{"to": "anchor", "part": .., "anchor": ..}`
/// in a part's `attach`, with an optional `offset` (metres, degrees about x then y then z, uniform scale).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct ListAnchors {
    /// The character to list.
    pub character: CharId,
}

impl Op for ListAnchors {
    fn meta() -> OpMeta {
        OpMeta {
            name: "accessory.list_anchors",
            summary: "List the bones and published anchors an accessory can be attached to, with their positions.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let built = build_character(&self.character, &def)?;
        Ok(Plan::answer(json!({ "character": self.character, "points": attach_points(&built) })))
    }
}

register_op!(ListAnchors);

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: DVec3, b: DVec3) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn the_offset_rotation_is_x_then_y_then_z_about_the_fixed_axes() {
        // x 90 turns +Z to -Y; then y 90 leaves -Y alone; z 90 turns -Y to +X.
        let q = offset_rotation([90.0, 90.0, 90.0]);
        assert!(close(q * DVec3::Z, DVec3::X), "{:?}", q * DVec3::Z);
        // The same turns applied one after the other, written out.
        let one_by_one = DQuat::from_rotation_z(90f64.to_radians())
            * DQuat::from_rotation_y(90f64.to_radians())
            * DQuat::from_rotation_x(90f64.to_radians());
        assert!(q.abs_diff_eq(one_by_one, 1e-12));
        // And the order matters: z first would give another result.
        let z_first = DQuat::from_rotation_x(90f64.to_radians())
            * DQuat::from_rotation_y(90f64.to_radians())
            * DQuat::from_rotation_z(90f64.to_radians());
        assert!(!close(z_first * DVec3::Y, q * DVec3::Y));
    }

    #[test]
    fn an_offset_round_trips_through_its_matrix() {
        for angles in
            [[0.0, 0.0, 0.0], [10.0, 20.0, 30.0], [-170.0, 45.0, 120.0], [90.0, -60.0, -10.0], [5.0, 89.0, 5.0]]
        {
            let back = euler_xyz_deg(offset_rotation(angles));
            let again = offset_rotation(back);
            assert!(
                offset_rotation(angles).abs_diff_eq(again, 1e-9) || offset_rotation(angles).abs_diff_eq(-again, 1e-9)
            );
            for (a, b) in angles.iter().zip(back) {
                assert!((a - b).abs() < 1e-6, "{angles:?} came back as {back:?}");
            }
        }
    }

    #[test]
    fn a_bad_offset_is_refused_with_the_reason() {
        let scale0 = AttachOffset { scale: 0.0, ..AttachOffset::default() };
        assert!(offset_matrix(&scale0).unwrap_err().contains("greater than 0"));
        let nan = AttachOffset { translation: [f64::NAN, 0.0, 0.0], ..AttachOffset::default() };
        assert!(offset_matrix(&nan).unwrap_err().contains("not finite"));
    }
}
