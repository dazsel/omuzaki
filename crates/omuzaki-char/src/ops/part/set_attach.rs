//! `part.set_attach`: where a part is fixed (CB-43), changed after it was added: what the viewport's move, rotate and
//! scale handles write (#2590), and what an agent writes to place an accessory.

use omuzaki_char_api::{Attach, AttachOffset, BuiltCharacter, CharId, CharacterDef, JointRef, PartId, paths};
use omuzaki_core::glam::{DMat4, DQuat, DVec3, Vec3};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::accessory::attach::euler_xyz_deg;
use crate::ops::character::access;
use crate::ops::weights::build_character;

/// How far a pinned part may land from where it was, in metres, before the pin is refused.
const PIN_TOLERANCE: f64 = 1e-4;

/// Sets where a part is fixed: a bone, or another part's anchor, plus an offset from it.
///
/// Give exactly one of:
/// - `attach`: the new place, as `part.add` takes it (`{"to": "bone", "bone": {"humanoid": "head"}, "offset": {..}}` or
///   `{"to": "anchor", "part": "shirt", "anchor": "collar_ring"}`). The offset is metres, then degrees about x, y and z
///   in that order, then a uniform scale, in the bone's or anchor's frame (`accessory.list_anchors` lists both).
/// - `pin: true`: writes the place the part's kind puts it by itself as an explicit attachment on the bone it follows,
///   with the offset that keeps it exactly where it is, so it can be moved from there. Refused for a part that has
///   no single bone to follow (hair, a garment) or whose kind builds differently once attached.
/// - `clear: true`: removes the attachment, so the kind places the part by itself again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hat", "pin": true},
        {"character": "kit", "part": "hat", "attach": {"to": "bone", "bone": {"humanoid": "head"}, "offset": {"translation": [0.0, 0.14, 0.01], "rotation_deg": [-8.0, 0.0, 0.0]}}},
        {"character": "kit", "part": "hat", "clear": true}
    ],
    "x-omuzaki-fixture" = "character-hat"
))]
pub struct PartSetAttach {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part to place.
    pub part: PartId,
    /// The new place.
    #[serde(default)]
    pub attach: Option<Attach>,
    /// Write the kind's own place as an explicit attachment, keeping the part where it is.
    #[serde(default)]
    pub pin: bool,
    /// Remove the attachment.
    #[serde(default)]
    pub clear: bool,
}

impl Op for PartSetAttach {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.set_attach",
            summary: "Set where a part is fixed (a bone or an anchor plus an offset), pin it where it is, or clear it.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let current = access::part(&def, &self.character, &self.part, "part")?.attach.clone();
        let asked = [self.attach.is_some(), self.pin, self.clear].into_iter().filter(|a| *a).count();
        if asked != 1 {
            return Err(OpError::invalid("attach", "give exactly one of `attach`, `pin: true` or `clear: true`")
                .hint("`attach` places the part, `pin` keeps it where it is as an attachment, `clear` removes it"));
        }
        let next = match (&self.attach, self.pin) {
            (Some(attach), _) => {
                if let Attach::Anchor { part, .. } = attach {
                    access::part(&def, &self.character, part, "attach/part")?;
                }
                Some(attach.clone())
            }
            (None, true) => match &current {
                Some(already) => Some(already.clone()),
                None => Some(pin(&self.character, &def, &self.part)?),
            },
            (None, false) => None,
        };
        let differs = next != current;
        let path = paths::part(&self.character, &self.part).join("attach");
        let value = next
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| OpError::internal(format!("serialising the attachment failed: {e}")))?;
        let changes = if differs { vec![access::change(ctx, path, value)] } else { Vec::new() };
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "attach": next,
            "changed": differs,
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartSetAttach);

/// The explicit attachment that keeps `part` where its kind puts it: the bone its meshes follow most, and the offset
/// from that bone's bind frame to the part's frame, found by building the part as it is and attached to the bone with
/// no offset, and fitting the move between the two (a turn, a uniform scale and a shift).
fn pin(id: &CharId, def: &CharacterDef, part: &PartId) -> Result<Attach, OpError> {
    let refuse = |why: String| {
        OpError::invalid("pin", format!("part `{part}` can't be pinned: {why}"))
            .hint("attach it explicitly with `attach` (accessory.list_anchors lists the bones and anchors)")
    };
    let free = build_character(id, def)?;
    let bone = followed_bone(&free, part).ok_or_else(|| refuse("it follows no single bone".to_owned()))?;
    let index = free.rig.bone(&bone).ok_or_else(|| refuse(format!("the skeleton has no bone {bone:?}")))?;
    let frame = free
        .rig
        .inverse_bind()
        .get(index.index())
        .map(|inverse| inverse.inverse().as_dmat4())
        .ok_or_else(|| refuse(format!("the skeleton has no bind frame for {bone:?}")))?;
    let mut probe = def.clone();
    if let Some(p) = probe.parts.get_mut(part) {
        p.attach = Some(Attach::Bone { bone: bone.clone(), offset: AttachOffset::default() });
    }
    let on_bone = build_character(id, &probe)?;
    let (from, to) = (positions(&on_bone, part), positions(&free, part));
    if from.len() != to.len() || from.len() < 3 {
        return Err(refuse("its kind builds it differently once attached".to_owned()));
    }
    // A kind that places the part by itself whatever its attachment says (hair, a garment) builds it the same way
    // attached or not: nudge the attachment and see that the part follows, so a pin never writes a place that does
    // nothing. Only needed when the bone's own place happens to be where the kind put it.
    if drift(&from, &to) < PIN_TOLERANCE {
        let mut nudged = probe.clone();
        if let Some(p) = nudged.parts.get_mut(part) {
            p.attach = Some(Attach::Bone {
                bone: bone.clone(),
                offset: AttachOffset { translation: [0.0, 0.01, 0.0], ..AttachOffset::default() },
            });
        }
        if drift(&positions(&build_character(id, &nudged)?, part), &to) < PIN_TOLERANCE {
            return Err(refuse("its kind places it by itself, whatever it is attached to".to_owned()));
        }
    }
    let moved = similarity(&from, &to).ok_or_else(|| refuse("its mesh is too small to measure".to_owned()))?;
    // Attached with no offset the part sits at `frame * L`; as built it sits at `moved * frame * L`. The offset that
    // gives the second is `frame⁻¹ * moved * frame`.
    let offset = frame.inverse() * moved * frame;
    let (scale, rotation, translation) = offset.to_scale_rotation_translation();
    let offset = AttachOffset {
        translation: translation.to_array(),
        rotation_deg: euler_xyz_deg(rotation),
        scale: (scale.x + scale.y + scale.z) / 3.0,
    };
    let attach = Attach::Bone { bone, offset };
    // The pin must leave the part where it was: check by building it pinned.
    let mut pinned = def.clone();
    if let Some(p) = pinned.parts.get_mut(part) {
        p.attach = Some(attach.clone());
    }
    let after = positions(&build_character(id, &pinned)?, part);
    let moved = drift(&after, &to);
    if after.len() != to.len() || moved > PIN_TOLERANCE {
        return Err(refuse(format!("pinned, it would move by {:.1} mm", moved * 1000.0)));
    }
    Ok(attach)
}

/// The furthest any point of `a` is from its partner in `b`.
fn drift(a: &[DVec3], b: &[DVec3]) -> f64 {
    a.iter().zip(b).map(|(p, q)| p.distance(*q)).fold(0.0, f64::max)
}

/// The joint `part`'s meshes follow with the most weight.
fn followed_bone(built: &BuiltCharacter, part: &PartId) -> Option<JointRef> {
    let mut weight: Vec<(JointRef, f32)> = Vec::new();
    for mesh in built.meshes.iter().filter(|m| m.part == *part && !m.mesh.dynamic) {
        let geometry = &mesh.mesh.geometry;
        for (joints, weights) in geometry.joints.iter().zip(&geometry.weights) {
            for (joint, w) in joints.iter().zip(weights) {
                let Some(name) = mesh.mesh.joint_bones.get(usize::from(*joint)) else { continue };
                match weight.iter_mut().find(|(j, _)| j == name) {
                    Some((_, total)) => *total += w,
                    None => weight.push((name.clone(), *w)),
                }
            }
        }
    }
    weight.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(joint, _)| joint)
}

/// Every vertex of `part`'s meshes, in build order.
fn positions(built: &BuiltCharacter, part: &PartId) -> Vec<DVec3> {
    built
        .meshes
        .iter()
        .filter(|m| m.part == *part)
        .flat_map(|m| m.mesh.geometry.positions.iter().map(|p: &Vec3| p.as_dvec3()))
        .collect()
}

/// The turn, uniform scale and shift that take `from` onto `to` (point for point), from three well-spread points:
/// exact for such a move, which is all an attachment's frame can do. `None` when the points don't span a plane.
fn similarity(from: &[DVec3], to: &[DVec3]) -> Option<DMat4> {
    let first = 0;
    let far = (0..from.len())
        .max_by(|&i, &j| from[i].distance_squared(from[first]).total_cmp(&from[j].distance_squared(from[first])))?;
    let line = (from[far] - from[first]).normalize_or_zero();
    let off_line = |i: usize| {
        let v = from[i] - from[first];
        (v - line * v.dot(line)).length_squared()
    };
    let wide = (0..from.len()).max_by(|&i, &j| off_line(i).total_cmp(&off_line(j)))?;
    let frame = |p: &[DVec3]| -> Option<(DMat4, f64)> {
        let along = p[far] - p[first];
        let length = along.length();
        let along = along.normalize_or_zero();
        let side = p[wide] - p[first];
        let across = (side - along * side.dot(along)).normalize_or_zero();
        (along != DVec3::ZERO && across != DVec3::ZERO).then(|| {
            let axes = DMat4::from_cols(
                along.extend(0.0),
                across.extend(0.0),
                along.cross(across).extend(0.0),
                DVec3::ZERO.extend(1.0),
            );
            (axes, length)
        })
    };
    let ((from_axes, from_length), (to_axes, to_length)) = (frame(from)?, frame(to)?);
    let rotation = DQuat::from_mat4(&(to_axes * from_axes.transpose()));
    let scale = to_length / from_length;
    let translation = to[first] - rotation * from[first] * scale;
    Some(DMat4::from_scale_rotation_translation(DVec3::splat(scale), rotation, translation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_a_scale_and_a_shift_are_recovered_from_the_points() {
        let from: Vec<DVec3> =
            (0..12).map(|i| DVec3::new(f64::from(i).sin(), f64::from(i * 3).cos(), f64::from(i) * 0.1)).collect();
        let truth = DMat4::from_scale_rotation_translation(
            DVec3::splat(1.3),
            DQuat::from_euler(omuzaki_core::glam::EulerRot::ZYX, 0.4, -0.7, 1.1),
            DVec3::new(0.2, -0.5, 0.9),
        );
        let to: Vec<DVec3> = from.iter().map(|p| truth.transform_point3(*p)).collect();
        let found = similarity(&from, &to).unwrap_or_else(|| panic!("a fit"));
        for (p, q) in from.iter().zip(&to) {
            assert!(found.transform_point3(*p).distance(*q) < 1e-9);
        }
    }

    #[test]
    fn points_on_a_line_have_no_fit() {
        let from: Vec<DVec3> = (0..5).map(|i| DVec3::X * f64::from(i)).collect();
        assert_eq!(similarity(&from, &from), None);
    }
}
