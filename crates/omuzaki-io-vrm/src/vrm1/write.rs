//! The scene's avatar layer as `VRMC_vrm` 1.0, `VRMC_springBone` and `VRMC_node_constraint`.
//!
//! Spec defaults are written explicitly, empty bind arrays are omitted (the schema wants at least one entry when the
//! array is present), and the humanoid, expressions and springs come out in the scene's deterministic order.

use std::collections::BTreeMap;

use omuzaki_gltf::to_json;
use omuzaki_io_api::{
    AimAxis, Avatar, ColliderShape, ColorBindKind, ConstraintKind, Expression, ExpressionName, Finding,
    FirstPersonFlag, IoScene, JointLimit, LookAt, LookAtKind, NodeIx, Override, RangeMap, Report, RollAxis, VrmVersion,
};
use serde_json::Value;

use super::wire::{
    EXTENDED_COLLIDER, LIMIT, WireAim, WireAimAxis, WireAngleLimit, WireAnnotation, WireCapsule, WireCollider,
    WireColliderGroup, WireColorBind, WireColorType, WireConstraint, WireExpression, WireExpressions, WireExtCapsule,
    WireExtShape, WireExtSphere, WireExtendedCollider, WireFirstPerson, WireFirstPersonType, WireHumanoid, WireJoint,
    WireLimit, WireLookAt, WireLookAtType, WireMorphBind, WireNodeConstraint, WireNodeRef, WireOverride, WirePlane,
    WireRangeMap, WireRoll, WireRollAxis, WireRotation, WireShape, WireSphere, WireSphericalLimit, WireSpring,
    WireSpringBone, WireSpringLimit, WireUvBind, WireVrm,
};
use super::{EXTENSION, SPRING_BONE};
use crate::avatar::on_cycle;

/// The licence ids the 0.x import records for the share-alike names (`CC_BY_SA`, `CC_BY_NC_SA`). Share-alike is a
/// licence condition VRM 1.0 has no flag for.
const SHARE_ALIKE: [&str; 2] = ["CC-BY-SA-4.0", "CC-BY-NC-SA-4.0"];

/// Warns when the model's licence has a condition the 1.0 meta cannot state, so the file does not look freer than it is.
fn licence_condition(scene: &IoScene, report: &mut Report) {
    let Some(id) = scene.provenance.as_ref().map(|p| p.licence.as_str()).filter(|id| SHARE_ALIKE.contains(id)) else {
        return;
    };
    let msg = format!(
        "the source licenseName stands for {id}, which requires share-alike; VRM 1.0 has no flag for that, so the \
         file's licenseUrl and flags do not say so"
    );
    report.push(
        Finding::warning("vrm.meta_licence_condition", msg)
            .at("meta/licenseUrl")
            .hint("keep the original licence with the model when you share it"),
    );
}

/// The root extensions of the scene's avatar, by name: `VRMC_vrm`, and `VRMC_springBone` when there is anything to put
/// in it. Empty for a scene with no avatar.
pub(crate) fn root_extensions(scene: &IoScene, report: &mut Report) -> Vec<(&'static str, Value)> {
    let Some(av) = &scene.avatar else { return Vec::new() };
    let wire = WireVrm {
        spec_version: "1.0".to_owned(),
        meta: scene.meta.clone(),
        humanoid: WireHumanoid {
            human_bones: av
                .humanoid
                .bones
                .iter()
                .map(|(bone, node)| (bone.as_str().to_owned(), WireNodeRef { node: node.index() }))
                .collect(),
        },
        first_person: av.first_person.as_ref().map(|f| WireFirstPerson {
            mesh_annotations: f
                .annotations
                .iter()
                .map(|a| WireAnnotation {
                    node: a.node.index(),
                    kind: match a.flag {
                        FirstPersonFlag::Auto => WireFirstPersonType::Auto,
                        FirstPersonFlag::Both => WireFirstPersonType::Both,
                        FirstPersonFlag::ThirdPersonOnly => WireFirstPersonType::ThirdPersonOnly,
                        FirstPersonFlag::FirstPersonOnly => WireFirstPersonType::FirstPersonOnly,
                    },
                })
                .collect(),
        }),
        look_at: av.look_at.as_ref().map(look_at),
        expressions: expressions(av),
    };
    let body = to_json(&wire);
    // The meta's unknown keys were written with it (`VrmMeta::extras`); the 0.x ones have no place in 1.0.
    report.findings.extend(scene.meta.unwritable_extras(VrmVersion::V1));
    licence_condition(scene, report);
    let mut out = vec![(EXTENSION, body)];
    if !av.spring_bones.chains.is_empty() || !av.spring_bones.colliders.is_empty() {
        out.push((SPRING_BONE, to_json(&spring_bones(av, report))));
    }
    out
}

fn override_of(o: Override) -> WireOverride {
    match o {
        Override::None => WireOverride::None,
        Override::Block => WireOverride::Block,
        Override::Blend => WireOverride::Blend,
    }
}

fn expressions(av: &Avatar) -> WireExpressions {
    let mut out = WireExpressions::default();
    for (name, e) in &av.expressions {
        let target = if matches!(name, ExpressionName::Preset(_)) { &mut out.preset } else { &mut out.custom };
        target.insert(name.as_str().to_owned(), expression(e));
    }
    out
}

fn expression(e: &Expression) -> WireExpression {
    WireExpression {
        morph_target_binds: e
            .morph_binds
            .iter()
            .map(|b| WireMorphBind { node: b.node.index(), index: b.index as usize, weight: b.weight })
            .collect(),
        material_color_binds: e
            .color_binds
            .iter()
            .map(|b| WireColorBind {
                material: b.material.index(),
                kind: match b.kind {
                    ColorBindKind::Color => WireColorType::Color,
                    ColorBindKind::EmissionColor => WireColorType::EmissionColor,
                    ColorBindKind::ShadeColor => WireColorType::ShadeColor,
                    ColorBindKind::MatcapColor => WireColorType::MatcapColor,
                    ColorBindKind::RimColor => WireColorType::RimColor,
                    ColorBindKind::OutlineColor => WireColorType::OutlineColor,
                },
                target_value: b.target.to_array(),
            })
            .collect(),
        texture_transform_binds: e
            .uv_binds
            .iter()
            .map(|b| WireUvBind {
                material: b.material.index(),
                scale: Some(b.scale.to_array()),
                offset: Some(b.offset.to_array()),
            })
            .collect(),
        is_binary: e.is_binary,
        override_blink: override_of(e.override_blink),
        override_look_at: override_of(e.override_look_at),
        override_mouth: override_of(e.override_mouth),
    }
}

fn range(r: RangeMap) -> WireRangeMap {
    WireRangeMap { input_max_value: Some(r.input_max_value), output_scale: Some(r.output_scale) }
}

fn look_at(l: &LookAt) -> WireLookAt {
    WireLookAt {
        kind: match l.kind {
            LookAtKind::Bone => WireLookAtType::Bone,
            LookAtKind::Expression => WireLookAtType::Expression,
        },
        offset_from_head_bone: Some(l.offset_from_head_bone.to_array()),
        range_map_horizontal_inner: Some(range(l.horizontal_inner)),
        range_map_horizontal_outer: Some(range(l.horizontal_outer)),
        range_map_vertical_down: Some(range(l.vertical_down)),
        range_map_vertical_up: Some(range(l.vertical_up)),
    }
}

/// The base shape of a collider and, for a shape the base cannot hold, its `VRMC_springBone_extended_collider`.
///
/// Follows `VRMC_springBone_extended_collider-1.0`, "Fallback": a reader without the extension gets an inert stand-in,
/// so the collider indices in the groups stay valid. An inside sphere or capsule becomes a sphere of radius 0 far below
/// the model; a plane becomes a huge sphere whose surface passes through the plane (radius 1000 is the spec's choice,
/// accurate to about 0.1 mm in `f32`, centred at `offset - normal * radius`).
fn collider_shapes(shape: ColliderShape, at: &str, report: &mut Report) -> (WireShape, Option<WireExtendedCollider>) {
    let ordinary = |sphere, capsule| (WireShape { sphere, capsule }, None);
    let extended = |fallback: WireSphere, shape: WireExtShape| {
        let ext = WireExtendedCollider { spec_version: "1.0".to_owned(), shape: Some(shape) };
        (WireShape { sphere: Some(fallback), capsule: None }, Some(ext))
    };
    let inert = || WireSphere { offset: INERT_OFFSET, radius: 0.0 };
    match shape {
        ColliderShape::Sphere { offset, radius } => {
            ordinary(Some(WireSphere { offset: offset.to_array(), radius }), None)
        }
        ColliderShape::Capsule { offset, radius, tail } => {
            ordinary(None, Some(WireCapsule { offset: offset.to_array(), radius, tail: tail.to_array() }))
        }
        ColliderShape::SphereInside { offset, radius } => extended(
            inert(),
            WireExtShape {
                sphere: Some(WireExtSphere { offset: offset.to_array(), radius, inside: true }),
                ..WireExtShape::default()
            },
        ),
        ColliderShape::CapsuleInside { offset, radius, tail } => extended(
            inert(),
            WireExtShape {
                capsule: Some(WireExtCapsule {
                    offset: offset.to_array(),
                    radius,
                    tail: tail.to_array(),
                    inside: true,
                }),
                ..WireExtShape::default()
            },
        ),
        ColliderShape::Plane { offset, normal } => extended(
            WireSphere { offset: (offset - normal * PLANE_FALLBACK_RADIUS).to_array(), radius: PLANE_FALLBACK_RADIUS },
            WireExtShape {
                plane: Some(WirePlane { offset: offset.to_array(), normal: normal.to_array() }),
                ..WireExtShape::default()
            },
        ),
        _ => {
            // The scene type is non-exhaustive: a shape added later must get its own arm; until then the inert
            // fallback keeps the file valid and the loss is reported.
            report.push(
                Finding::warning("vrm.collider_dropped", "a collider shape this writer does not know was not written")
                    .at(at),
            );
            (WireShape { sphere: Some(inert()), capsule: None }, None)
        }
    }
}

/// Where the inert fallback sphere sits (`VRMC_springBone_extended_collider-1.0`, "Fallback").
const INERT_OFFSET: [f32; 3] = [0.0, -10_000.0, 0.0];

/// Radius of the sphere that stands in for a plane (`VRMC_springBone_extended_collider-1.0`, "Fallback").
const PLANE_FALLBACK_RADIUS: f32 = 1000.0;

/// The `VRMC_springBone_limit` body of a joint limit (`VRMC_springBone_limit-1.0`).
fn limit_body(limit: JointLimit, at: &str, report: &mut Report) -> Option<WireSpringLimit> {
    let wire = match limit {
        JointLimit::Cone { angle, rotation } => {
            WireLimit { cone: Some(WireAngleLimit { angle, rotation: rotation.to_array() }), ..WireLimit::default() }
        }
        JointLimit::Hinge { angle, rotation } => {
            WireLimit { hinge: Some(WireAngleLimit { angle, rotation: rotation.to_array() }), ..WireLimit::default() }
        }
        JointLimit::Spherical { pitch, yaw, rotation } => WireLimit {
            spherical: Some(WireSphericalLimit { pitch, yaw, rotation: rotation.to_array() }),
            ..WireLimit::default()
        },
        _ => {
            report.push(
                Finding::warning("vrm.limit_invalid", "a limit kind this writer does not know was not written").at(at),
            );
            return None;
        }
    };
    Some(WireSpringLimit { spec_version: "1.0".to_owned(), limit: wire })
}

fn spring_bones(av: &Avatar, report: &mut Report) -> WireSpringBone {
    let sb = &av.spring_bones;
    let colliders = sb
        .colliders
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let (shape, ext) = collider_shapes(c.shape, &format!("avatar/springBone/colliders/{i}"), report);
            let mut extensions = BTreeMap::new();
            if let Some(ext) = ext {
                extensions.insert(EXTENDED_COLLIDER.to_owned(), to_json(&ext));
            }
            WireCollider { node: c.node.index(), shape: Some(shape), extensions }
        })
        .collect();
    let springs = sb
        .chains
        .iter()
        .enumerate()
        .map(|(ci, c)| {
            let last = c.joints.len().saturating_sub(1);
            let joints = c
                .joints
                .iter()
                .enumerate()
                .map(|(ji, j)| {
                    let mut extensions = BTreeMap::new();
                    // `VRMC_springBone_limit-1.0`, "Last joint": exporters must not write a limit on a chain's tail.
                    let limit = j.limit.filter(|_| ji != last);
                    if let Some(body) = limit
                        .and_then(|l| limit_body(l, &format!("avatar/springBone/springs/{ci}/joints/{ji}"), report))
                    {
                        extensions.insert(LIMIT.to_owned(), to_json(&body));
                    }
                    WireJoint {
                        node: j.node.index(),
                        hit_radius: Some(j.hit_radius),
                        stiffness: Some(j.stiffness),
                        gravity_power: Some(j.gravity_power),
                        gravity_dir: Some(j.gravity_dir.to_array()),
                        drag_force: Some(j.drag_force),
                        extensions,
                    }
                })
                .collect();
            WireSpring {
                name: c.name.clone(),
                joints,
                collider_groups: c.collider_groups.iter().map(|&g| g as usize).collect(),
                center: c.center.map(NodeIx::index),
            }
        })
        .collect();
    WireSpringBone {
        spec_version: "1.0".to_owned(),
        colliders,
        collider_groups: sb
            .collider_groups
            .iter()
            .map(|g| WireColliderGroup {
                name: g.name.clone(),
                colliders: g.colliders.iter().map(|&c| c as usize).collect(),
            })
            .collect(),
        springs,
    }
}

/// The `VRMC_node_constraint` body for each constrained node, by node index. A node can have only one constraint, so
/// when the scene has several for one node the last is written and a finding says so.
pub(crate) fn constraint_bodies(av: &Avatar, report: &mut Report) -> BTreeMap<usize, Value> {
    let mut out = BTreeMap::new();
    let cyclic = on_cycle(&av.constraints);
    let skipped = cyclic.iter().filter(|&&c| c).count();
    if skipped > 0 {
        report.push(
            Finding::warning(
                "vrm.constraint_dropped",
                format!("{skipped} constraint(s) form a circular dependency, so they were not written"),
            )
            .at("avatar/constraints"),
        );
    }
    for (c, _) in av.constraints.iter().zip(&cyclic).filter(|(_, cyc)| !**cyc) {
        let weight = Some(c.weight);
        let constraint = match c.kind {
            ConstraintKind::Roll { source, axis } => WireConstraint {
                roll: Some(WireRoll {
                    source: source.index(),
                    roll_axis: match axis {
                        RollAxis::X => WireRollAxis::X,
                        RollAxis::Y => WireRollAxis::Y,
                        RollAxis::Z => WireRollAxis::Z,
                    },
                    weight,
                }),
                ..WireConstraint::default()
            },
            ConstraintKind::Aim { source, axis } => WireConstraint {
                aim: Some(WireAim {
                    source: source.index(),
                    aim_axis: match axis {
                        AimAxis::PositiveX => WireAimAxis::PositiveX,
                        AimAxis::NegativeX => WireAimAxis::NegativeX,
                        AimAxis::PositiveY => WireAimAxis::PositiveY,
                        AimAxis::NegativeY => WireAimAxis::NegativeY,
                        AimAxis::PositiveZ => WireAimAxis::PositiveZ,
                        AimAxis::NegativeZ => WireAimAxis::NegativeZ,
                    },
                    weight,
                }),
                ..WireConstraint::default()
            },
            ConstraintKind::Rotation { source } => WireConstraint {
                rotation: Some(WireRotation { source: source.index(), weight }),
                ..WireConstraint::default()
            },
        };
        let body = WireNodeConstraint { spec_version: "1.0".to_owned(), constraint };
        if out.insert(c.node.index(), to_json(&body)).is_some() {
            report.push(
                Finding::warning(
                    "vrm.constraint_duplicate",
                    "a node can have only one constraint; the last one was written",
                )
                .at(format!("avatar/constraints/{}", c.node.0)),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_io_api::NodeConstraint;

    fn rotation(node: u32, source: u32) -> NodeConstraint {
        NodeConstraint { node: NodeIx(node), kind: ConstraintKind::Rotation { source: NodeIx(source) }, weight: 1.0 }
    }

    /// `IoScene::check` rejects circular constraints, so no export reaches this; the writer still leaves them out so a
    /// caller that skips the check cannot produce a file the spec forbids.
    #[test]
    fn the_writer_leaves_out_circular_constraints_and_keeps_the_rest() {
        let mut av = Avatar::new();
        av.constraints = vec![rotation(1, 2), rotation(2, 1), rotation(3, 1)];
        let mut report = Report::new();
        let bodies = constraint_bodies(&av, &mut report);
        assert_eq!(bodies.keys().copied().collect::<Vec<_>>(), [3]);
        assert!(report.has_rule("vrm.constraint_dropped"), "{report:#?}");
    }
}
