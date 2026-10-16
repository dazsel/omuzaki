//! Spring bones, colliders and node constraints (design D4, D8; EX-11's shapes and limits).
//!
//! The runtime [`SpringSet`] is what the solver runs: colliders inside uniquely named groups, chains naming groups by
//! name. The file's records (its collider list, groups and chains by index, names that may be empty or repeated, a
//! collider shared by two groups or in none) are kept exactly in `retained.springs`, each group and chain naming the
//! runtime entry made from it, so export writes the file's own records back. Every collider shape (inside spheres and
//! capsules, planes) and every limit (cone, hinge, spherical) reaches the runtime set as the file has it.
//!
//! A chain or constraint the solver cannot run (fewer than two joints, a joint not below the one before, a cycle) is
//! reported (`avatar.spring_chain_unused`, `avatar.constraint_unused`) and left out of the runtime set; it stays in
//! the retained records, so export still writes it.

use std::collections::BTreeSet;

use omuzaki_anim_api::spring::JointLimit;
use omuzaki_anim_api::{
    Axis, BoneConstraint, Collider, ColliderGroup, ColliderShape, ConstraintKind, ConstraintSet, SignedAxis,
    SpringChain, SpringJoint, SpringParams, SpringSet,
};
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::retained::{
    RetainedCollider, RetainedColliderGroup, RetainedSpringChain, RetainedSpringJoint, RetainedSprings,
};
use omuzaki_io_api::{self as io, Finding, IoScene, Report};

use super::skeleton::Rig;

/// Fills `avatar.springs`, `avatar.constraints` and `avatar.retained.springs`.
pub fn convert(scene: &IoScene, rig: &Rig, avatar: &mut ImportedAvatar, report: &mut Report) {
    let Some(file) = &scene.avatar else { return };
    unknown_shapes(&file.spring_bones, report);
    let (springs, retained) = springs(&file.spring_bones, rig, avatar, report);
    avatar.springs = springs;
    avatar.retained.springs = retained;
    avatar.constraints = constraints(&file.constraints, rig, avatar, report);
}

/// The runtime spring set and the file's records, with the runtime name each group and chain became.
fn springs(
    file: &io::SpringBones,
    rig: &Rig,
    avatar: &ImportedAvatar,
    report: &mut Report,
) -> (SpringSet, RetainedSprings) {
    let mut retained = RetainedSprings::new();
    retained.colliders =
        file.colliders.iter().map(|c| RetainedCollider::new(c.node.0, collider_shape(c.shape))).collect();
    let mut set = SpringSet::default();
    let mut taken = BTreeSet::new();
    for (g, group) in file.collider_groups.iter().enumerate() {
        let name = unique_name(&group.name, "group", g, &mut taken);
        let colliders: Vec<Collider> = group
            .colliders
            .iter()
            .filter_map(|&c| file.colliders.get(usize::try_from(c).ok()?))
            .filter_map(|c| Some(Collider { bone: rig.bone(c.node)?, shape: collider_shape(c.shape) }))
            .collect();
        set.groups.push(ColliderGroup { name: name.clone(), colliders });
        retained
            .groups
            .push(RetainedColliderGroup::new(group.name.clone(), group.colliders.clone()).with_runtime(name));
    }
    let group_names: Vec<String> = set.groups.iter().map(|g| g.name.clone()).collect();
    let mut taken = BTreeSet::new();
    for (c, chain) in file.chains.iter().enumerate() {
        let joints: Vec<RetainedSpringJoint> = chain
            .joints
            .iter()
            .map(|j| {
                let joint = RetainedSpringJoint::new(j.node.0, params(j));
                match j.limit.and_then(joint_limit) {
                    Some(limit) => joint.with_limit(limit),
                    None => joint,
                }
            })
            .collect();
        let mut record =
            RetainedSpringChain::new(chain.name.clone(), joints).with_groups(chain.collider_groups.clone());
        if let Some(center) = chain.center {
            record = record.with_center(center.0);
        }
        let name = unique_name(&chain.name, "chain", c, &mut taken);
        match runtime_chain(chain, &name, &group_names, rig) {
            Some(runtime) => {
                let mut trial = set.clone();
                trial.chains.push(runtime);
                match trial.validate(&avatar.skeleton) {
                    Ok(()) => {
                        set = trial;
                        record = record.with_runtime(name);
                    }
                    Err(why) => unused(report, c, &chain.name, &why.to_string()),
                }
            }
            None => unused(report, c, &chain.name, "a joint or its centre is not a bone"),
        }
        retained.chains.push(record);
    }
    (set, retained)
}

/// Reports chain `index` (called `name`) as left out of the runtime set, because of `why`.
fn unused(report: &mut Report, index: usize, name: &str, why: &str) {
    let msg = format!("spring chain {index} ({name:?}) can't be simulated: {why}; it is kept for export");
    let hint = "fix the chain in the source file: at least two joints, each below the one before";
    report.push(Finding::warning("avatar.spring_chain_unused", msg).at(format!("avatar/springs/{index}")).hint(hint));
}

/// The runtime chain of a file chain, or `None` when a joint or the centre is not a bone.
fn runtime_chain(chain: &io::SpringChain, name: &str, group_names: &[String], rig: &Rig) -> Option<SpringChain> {
    let joints = chain
        .joints
        .iter()
        .map(|j| Some(SpringJoint { bone: rig.bone(j.node)?, params: params(j), limit: j.limit.and_then(joint_limit) }))
        .collect::<Option<Vec<_>>>()?;
    let center = match chain.center {
        Some(node) => Some(rig.bone(node)?),
        None => None,
    };
    let collider_groups =
        chain.collider_groups.iter().filter_map(|&g| group_names.get(usize::try_from(g).ok()?).cloned()).collect();
    Some(SpringChain { name: name.to_owned(), joints, collider_groups, center })
}

/// A name for runtime entry `index`, unique among `taken`: the file's name when it is new and not empty, otherwise
/// `<kind>_<index>` (then `_2`, `_3`, … if even that is taken).
fn unique_name(file: &str, kind: &str, index: usize, taken: &mut BTreeSet<String>) -> String {
    let stem = if file.is_empty() { format!("{kind}_{index}") } else { file.to_owned() };
    let mut candidate = stem.clone();
    let mut k = 2usize;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{stem}_{k}");
        k += 1;
    }
    candidate
}

/// A joint's parameters as the solver takes them.
fn params(joint: &io::SpringJoint) -> SpringParams {
    SpringParams {
        hit_radius: joint.hit_radius,
        stiffness: joint.stiffness,
        gravity_power: joint.gravity_power,
        gravity_dir: joint.gravity_dir,
        drag_force: joint.drag_force,
    }
}

/// A collider shape as the solver takes it: the same variants and numbers. A shape a later io-api adds and this build
/// does not know becomes an empty sphere (radius 0, which touches nothing), so the file's collider indices still line
/// up; [`unknown_shapes`] reports it.
fn collider_shape(shape: io::ColliderShape) -> ColliderShape {
    match shape {
        io::ColliderShape::Sphere { offset, radius } => ColliderShape::Sphere { offset, radius },
        io::ColliderShape::Capsule { offset, radius, tail } => ColliderShape::Capsule { offset, radius, tail },
        io::ColliderShape::SphereInside { offset, radius } => ColliderShape::SphereInside { offset, radius },
        io::ColliderShape::CapsuleInside { offset, radius, tail } => {
            ColliderShape::CapsuleInside { offset, radius, tail }
        }
        io::ColliderShape::Plane { offset, normal } => ColliderShape::Plane { offset, normal },
        _ => ColliderShape::Sphere { offset: omuzaki_core::glam::Vec3::ZERO, radius: 0.0 },
    }
}

/// Whether this build knows `shape` (see [`collider_shape`]).
fn known_shape(shape: io::ColliderShape) -> bool {
    matches!(
        shape,
        io::ColliderShape::Sphere { .. }
            | io::ColliderShape::Capsule { .. }
            | io::ColliderShape::SphereInside { .. }
            | io::ColliderShape::CapsuleInside { .. }
            | io::ColliderShape::Plane { .. }
    )
}

/// Reports every collider whose shape this build does not know (`avatar.collider_unknown`).
fn unknown_shapes(file: &io::SpringBones, report: &mut Report) {
    for (c, collider) in file.colliders.iter().enumerate().filter(|(_, c)| !known_shape(c.shape)) {
        let msg = format!(
            "collider {c} on node {} has a shape this version doesn't know; it collides with nothing",
            collider.node.0
        );
        let hint = "update Omuzaki to use this collider";
        report.push(Finding::warning("avatar.collider_unknown", msg).at(format!("avatar/colliders/{c}")).hint(hint));
    }
}

/// A joint limit as the solver takes it: the same kinds, angles and rotation; `None` for a kind a later io-api adds
/// and this build does not know (the joint then swings freely, and the chain's report says so).
fn joint_limit(limit: io::JointLimit) -> Option<JointLimit> {
    match limit {
        io::JointLimit::Cone { angle, rotation } => Some(JointLimit::cone(angle).rotated(rotation)),
        io::JointLimit::Hinge { angle, rotation } => Some(JointLimit::hinge(angle).rotated(rotation)),
        io::JointLimit::Spherical { pitch, yaw, rotation } => Some(JointLimit::spherical(pitch, yaw).rotated(rotation)),
        _ => None,
    }
}

/// The runtime constraint set: each file constraint whose node and source are bones, in file order, kept while the
/// set stays valid (one constraint per bone, weights in range, no cycle); the rest are reported.
fn constraints(file: &[io::NodeConstraint], rig: &Rig, avatar: &ImportedAvatar, report: &mut Report) -> ConstraintSet {
    let mut set = ConstraintSet::default();
    for (index, constraint) in file.iter().enumerate() {
        let source = super::skeleton::constraint_source(constraint.kind);
        let built = rig.bone(constraint.node).zip(rig.bone(source)).map(|(bone, source)| BoneConstraint {
            bone,
            source,
            kind: constraint_kind(constraint.kind),
            weight: constraint.weight,
        });
        let why = match built {
            Some(c) => {
                let mut trial = set.clone();
                trial.constraints.push(c);
                match trial.validate(&avatar.skeleton) {
                    Ok(()) => {
                        set = trial;
                        continue;
                    }
                    Err(e) => e.to_string(),
                }
            }
            None => "its node or source is not a bone".to_owned(),
        };
        let msg = format!("constraint {index} can't run: {why}; it is kept for export");
        let hint = "fix the constraint in the source file";
        report.push(
            Finding::warning("avatar.constraint_unused", msg).at(format!("avatar/constraints/{index}")).hint(hint),
        );
    }
    set
}

/// A constraint kind as the solver takes it.
fn constraint_kind(kind: io::ConstraintKind) -> ConstraintKind {
    match kind {
        io::ConstraintKind::Roll { axis, .. } => ConstraintKind::Roll {
            axis: match axis {
                io::RollAxis::X => Axis::X,
                io::RollAxis::Y => Axis::Y,
                io::RollAxis::Z => Axis::Z,
            },
        },
        io::ConstraintKind::Aim { axis, .. } => ConstraintKind::Aim {
            axis: match axis {
                io::AimAxis::PositiveX => SignedAxis::PositiveX,
                io::AimAxis::NegativeX => SignedAxis::NegativeX,
                io::AimAxis::PositiveY => SignedAxis::PositiveY,
                io::AimAxis::NegativeY => SignedAxis::NegativeY,
                io::AimAxis::PositiveZ => SignedAxis::PositiveZ,
                io::AimAxis::NegativeZ => SignedAxis::NegativeZ,
            },
        },
        io::ConstraintKind::Rotation { .. } => ConstraintKind::Rotation,
    }
}
