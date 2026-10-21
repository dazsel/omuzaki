//! Spring bones for the VRM avatar block: the rig's spring chains and colliders as VRM 1.0 groups, colliders and joints.
//!
//! The runtime's [`SpringSet`] already speaks VRM 1.0 (field names, units and defaults), so this is a change of
//! address, not of meaning. Three things differ and are all handled here:
//! - **Bones are nodes.** Bone `i` is node `i` ([`bone_node`]), so no lookup table is needed.
//! - **Groups are indexed.** The runtime nests colliders inside their group and names groups from chains; the file keeps
//!   one flat collider list that groups and chains refer to by index.
//! - **Lengths scale.** The gather scaled the skeleton to a target height; hit radii, collider offsets, radii and tails
//!   are lengths and follow it (`Gathered::scale`). Stiffness, gravity and drag are not lengths and stay as authored.

use omuzaki_anim_api::{self as anim, SpringChain, SpringSet};
use omuzaki_io_api::{self as io, Finding, Report, SpringBones};

use super::skeleton::bone_node;

/// The spring bones of `springs` for a scene scaled by `scale`.
///
/// `springs` must be the validated set of a built character (`SpringSet::validate` ran when it was assembled), which is
/// what makes every bone a node and every group name resolve. Deterministic: groups, colliders and chains keep the order
/// of the set.
///
/// A collider whose shape the file cannot carry yet (a shape added to the rig's `#[non_exhaustive]` enum before the
/// file's twin has it) is left out of its group and reported as a warning in `report`, never dropped silently.
pub fn to_io(springs: &SpringSet, scale: f32, report: &mut Report) -> SpringBones {
    let mut out = SpringBones::default();
    for group in &springs.groups {
        let first = index(out.colliders.len());
        for (i, c) in group.colliders.iter().enumerate() {
            if let Some(shape) = shape(c.shape, scale) {
                out.colliders.push(io::Collider { node: bone_node(c.bone), shape });
            } else {
                let msg = format!("collider {i} of group {:?} has a shape the file cannot carry yet", group.name);
                let hint = "the collider is left out of the file; its springs will not collide with it elsewhere";
                report.push(
                    Finding::warning("gather.collider_shape_unsupported", msg).at("avatar/springBones").hint(hint),
                );
            }
        }
        let colliders = (first..index(out.colliders.len())).collect();
        out.collider_groups.push(io::ColliderGroup { name: group.name.clone(), colliders });
    }
    out.chains = springs.chains.iter().map(|chain| chain_to_io(chain, springs, scale)).collect();
    out
}

fn chain_to_io(chain: &SpringChain, springs: &SpringSet, scale: f32) -> io::SpringChain {
    io::SpringChain {
        name: chain.name.clone(),
        joints: chain
            .joints
            .iter()
            .map(|joint| {
                let p = joint.params;
                io::SpringJoint {
                    node: bone_node(joint.bone),
                    hit_radius: p.hit_radius * scale,
                    stiffness: p.stiffness,
                    gravity_power: p.gravity_power,
                    gravity_dir: p.gravity_dir,
                    drag_force: p.drag_force,
                    limit: None,
                }
            })
            .collect(),
        collider_groups: chain
            .collider_groups
            .iter()
            .filter_map(|name| springs.groups.iter().position(|g| &g.name == name))
            .map(index)
            .collect(),
        center: chain.center.map(bone_node),
    }
}

/// The file's twin of `shape`, lengths scaled by `scale`. A plane's normal is a direction and does not scale.
pub(crate) fn shape(shape: anim::ColliderShape, scale: f32) -> Option<io::ColliderShape> {
    Some(match shape {
        anim::ColliderShape::Sphere { offset, radius } => {
            io::ColliderShape::Sphere { offset: offset * scale, radius: radius * scale }
        }
        anim::ColliderShape::Capsule { offset, radius, tail } => {
            io::ColliderShape::Capsule { offset: offset * scale, radius: radius * scale, tail: tail * scale }
        }
        anim::ColliderShape::SphereInside { offset, radius } => {
            io::ColliderShape::SphereInside { offset: offset * scale, radius: radius * scale }
        }
        anim::ColliderShape::CapsuleInside { offset, radius, tail } => {
            io::ColliderShape::CapsuleInside { offset: offset * scale, radius: radius * scale, tail: tail * scale }
        }
        anim::ColliderShape::Plane { offset, normal } => io::ColliderShape::Plane { offset: offset * scale, normal },
        // `ColliderShape` is `#[non_exhaustive]`: a shape the file's twin lacks is reported by the caller.
        _ => return None,
    })
}

/// A list position as the file's `u32` index. A rig has a few hundred colliders at most, so it never saturates.
fn index(position: usize) -> u32 {
    u32::try_from(position).unwrap_or(u32::MAX)
}
