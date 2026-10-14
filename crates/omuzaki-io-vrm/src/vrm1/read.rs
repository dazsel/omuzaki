//! `VRMC_vrm` 1.0, `VRMC_springBone` and `VRMC_node_constraint` into the scene's avatar layer.

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    AimAxis, Avatar, Collider, ColliderGroup, ColliderShape, ColorBind, ColorBindKind, ConstraintKind, Expression,
    ExpressionName, Finding, FirstPerson, FirstPersonFlag, HumanBoneName, IoError, IoScene, LookAt, LookAtKind,
    MaterialIx, MeshAnnotation, MorphBind, NodeConstraint, NodeIx, Override, PresetExpression, RangeMap, Report,
    RollAxis, SpringChain, SpringJoint, UvBind,
};
use serde::Deserialize;

use super::wire::{
    WireAimAxis, WireColorType, WireExpression, WireExpressions, WireFirstPersonType, WireLookAt, WireLookAtType,
    WireNodeConstraint, WireOverride, WireRangeMap, WireRollAxis, WireShape, WireSpringBone, WireVrm,
};
use super::{CONSTRAINT, EXTENSION, SPRING_BONE};
use crate::avatar;
use omuzaki_gltf::schema::Document;

fn invalid(what: &str, at: &str, e: &serde_json::Error) -> IoError {
    IoError::Malformed { message: format!("{what}: {e}"), at: Some(at.to_owned()) }
}

/// Reads the avatar layer of a VRM 1.0 document into `scene.meta` and `scene.avatar`.
pub(crate) fn read(gltf: &Document, scene: &mut IoScene, report: &mut Report) -> Result<(), IoError> {
    let body = gltf.extensions.get(EXTENSION).ok_or_else(|| {
        IoError::malformed(
            "the file has no VRMC_vrm extension; if it is a VRM 0.x file, import it as vrm0 (it has a `VRM` extension)",
        )
    })?;
    let at = format!("/extensions/{EXTENSION}");
    let wire = WireVrm::deserialize(body).map_err(|e| invalid("VRMC_vrm is invalid", &at, &e))?;
    if wire.spec_version != "1.0" {
        return Err(IoError::unsupported(
            format!("VRMC_vrm specVersion {:?}", wire.spec_version),
            "only the final 1.0 is supported; VRM 0.x files use the vrm0 importer",
        ));
    }
    let asset_copyright = scene.meta.copyright_information.take();
    scene.meta = wire.meta;
    if scene.meta.copyright_information.is_none() {
        scene.meta.copyright_information = asset_copyright;
    }
    let mut av = Avatar::new();
    for (name, node) in &wire.humanoid.human_bones {
        match HumanBoneName::new(name) {
            Ok(bone) => {
                av.humanoid.bones.insert(bone, NodeIx(crate::ix32(node.node)));
            }
            Err(e) => report.push(
                Finding::warning("vrm.humanoid_unknown_bone", e.to_string())
                    .at(format!("{at}/humanoid/humanBones/{name}")),
            ),
        }
    }
    expressions(&wire.expressions, &mut av, &at, report);
    av.look_at = wire.look_at.as_ref().map(look_at);
    av.first_person = wire.first_person.map(|f| FirstPerson {
        annotations: f
            .mesh_annotations
            .iter()
            .map(|a| MeshAnnotation {
                node: NodeIx(crate::ix32(a.node)),
                flag: match a.kind {
                    WireFirstPersonType::Auto => FirstPersonFlag::Auto,
                    WireFirstPersonType::Both => FirstPersonFlag::Both,
                    WireFirstPersonType::ThirdPersonOnly => FirstPersonFlag::ThirdPersonOnly,
                    WireFirstPersonType::FirstPersonOnly => FirstPersonFlag::FirstPersonOnly,
                },
            })
            .collect(),
    });
    spring_bones(gltf, &mut av, report)?;
    constraints(gltf, &mut av, report)?;
    avatar::tidy(scene, &mut av, report);
    scene.avatar = Some(av);
    Ok(())
}

fn override_of(o: WireOverride) -> Override {
    match o {
        WireOverride::None => Override::None,
        WireOverride::Block => Override::Block,
        WireOverride::Blend => Override::Blend,
    }
}

fn expressions(wire: &WireExpressions, av: &mut Avatar, at: &str, report: &mut Report) {
    for (group, map) in [("preset", &wire.preset), ("custom", &wire.custom)] {
        for (name, e) in map {
            let here = format!("{at}/expressions/{group}/{name}");
            if group == "preset" && PresetExpression::parse(name).is_none() {
                let msg = format!("{name:?} is not a preset expression; kept as a custom one");
                report.push(Finding::warning("vrm.expression_name_invalid", msg).at(&here));
            }
            match ExpressionName::parse(name) {
                Ok(ExpressionName::Preset(_)) if group == "custom" => report.push(
                    Finding::warning(
                        "vrm.expression_name_invalid",
                        format!("custom expression {name:?} reuses a preset name; skipped"),
                    )
                    .at(here),
                ),
                Ok(n) => {
                    av.expressions.insert(n, expression(e));
                }
                Err(why) => report.push(Finding::warning("vrm.expression_name_invalid", why.to_string()).at(here)),
            }
        }
    }
}

fn expression(w: &WireExpression) -> Expression {
    let mut e = Expression::new();
    e.is_binary = w.is_binary;
    e.override_blink = override_of(w.override_blink);
    e.override_look_at = override_of(w.override_look_at);
    e.override_mouth = override_of(w.override_mouth);
    e.morph_binds = w
        .morph_target_binds
        .iter()
        .map(|b| MorphBind {
            node: NodeIx(crate::ix32(b.node)),
            index: u32::try_from(b.index).unwrap_or(u32::MAX),
            weight: b.weight,
        })
        .collect();
    e.color_binds = w
        .material_color_binds
        .iter()
        .map(|b| ColorBind {
            material: MaterialIx(crate::ix32(b.material)),
            kind: match b.kind {
                WireColorType::Color => ColorBindKind::Color,
                WireColorType::EmissionColor => ColorBindKind::EmissionColor,
                WireColorType::ShadeColor => ColorBindKind::ShadeColor,
                WireColorType::MatcapColor => ColorBindKind::MatcapColor,
                WireColorType::RimColor => ColorBindKind::RimColor,
                WireColorType::OutlineColor => ColorBindKind::OutlineColor,
            },
            target: omuzaki_core::glam::Vec4::from_array(b.target_value),
        })
        .collect();
    e.uv_binds = w
        .texture_transform_binds
        .iter()
        .map(|b| UvBind {
            material: MaterialIx(crate::ix32(b.material)),
            scale: omuzaki_core::glam::Vec2::from_array(b.scale.unwrap_or([1.0, 1.0])),
            offset: omuzaki_core::glam::Vec2::from_array(b.offset.unwrap_or([0.0, 0.0])),
        })
        .collect();
    e
}

/// Range maps default to 90° in and 10 out for bones, 1 out for expressions, like the 0.x migration does; the 1.0
/// schema gives no defaults.
fn range(w: Option<&WireRangeMap>, kind: LookAtKind) -> RangeMap {
    let out = if kind == LookAtKind::Bone { 10.0 } else { 1.0 };
    RangeMap {
        input_max_value: w.and_then(|r| r.input_max_value).unwrap_or(90.0),
        output_scale: w.and_then(|r| r.output_scale).unwrap_or(out),
    }
}

fn look_at(w: &WireLookAt) -> LookAt {
    let kind = match w.kind {
        WireLookAtType::Bone => LookAtKind::Bone,
        WireLookAtType::Expression => LookAtKind::Expression,
    };
    LookAt {
        kind,
        offset_from_head_bone: Vec3::from_array(w.offset_from_head_bone.unwrap_or([0.0; 3])),
        horizontal_inner: range(w.range_map_horizontal_inner.as_ref(), kind),
        horizontal_outer: range(w.range_map_horizontal_outer.as_ref(), kind),
        vertical_down: range(w.range_map_vertical_down.as_ref(), kind),
        vertical_up: range(w.range_map_vertical_up.as_ref(), kind),
    }
}

fn spring_bones(gltf: &Document, av: &mut Avatar, report: &mut Report) -> Result<(), IoError> {
    let Some(body) = gltf.extensions.get(SPRING_BONE) else { return Ok(()) };
    let at = format!("/extensions/{SPRING_BONE}");
    let wire = WireSpringBone::deserialize(body).map_err(|e| invalid("VRMC_springBone is invalid", &at, &e))?;
    let sb = &mut av.spring_bones;
    let note = |what: &str, report: &mut Report| {
        report.push(
            Finding::info(
                "gltf.extension_dropped",
                format!("{what} is not modelled; the base shape or the plain joint is used"),
            )
            .at(&at),
        );
    };
    if wire.colliders.iter().any(|c| c.extensions.contains_key("VRMC_springBone_extended_collider")) {
        note("VRMC_springBone_extended_collider (inside-out and plane colliders)", report);
    }
    if wire.springs.iter().flat_map(|s| &s.joints).any(|j| j.extensions.contains_key("VRMC_springBone_limit")) {
        note("VRMC_springBone_limit", report);
    }
    for (i, c) in wire.colliders.iter().enumerate() {
        let shape = match &c.shape {
            Some(WireShape { sphere: Some(s), .. }) => {
                ColliderShape::Sphere { offset: Vec3::from_array(s.offset), radius: s.radius }
            }
            Some(WireShape { capsule: Some(k), .. }) => ColliderShape::Capsule {
                offset: Vec3::from_array(k.offset),
                radius: k.radius,
                tail: Vec3::from_array(k.tail),
            },
            _ => {
                report.push(
                    Finding::warning(
                        "vrm.collider_shapeless",
                        "a collider has no sphere or capsule shape; an empty sphere stands in",
                    )
                    .at(format!("{at}/colliders/{i}")),
                );
                ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.0 }
            }
        };
        sb.colliders.push(Collider { node: NodeIx(crate::ix32(c.node)), shape });
    }
    for g in &wire.collider_groups {
        sb.collider_groups.push(ColliderGroup {
            name: g.name.clone(),
            colliders: g.colliders.iter().map(|&c| crate::ix32(c)).collect(),
        });
    }
    for s in &wire.springs {
        let joints = s
            .joints
            .iter()
            .map(|j| {
                let mut out = SpringJoint::new(NodeIx(crate::ix32(j.node)));
                out.hit_radius = j.hit_radius.unwrap_or(out.hit_radius);
                out.stiffness = j.stiffness.unwrap_or(out.stiffness);
                out.gravity_power = j.gravity_power.unwrap_or(out.gravity_power);
                out.gravity_dir = j.gravity_dir.map_or(out.gravity_dir, Vec3::from_array);
                out.drag_force = j.drag_force.unwrap_or(out.drag_force);
                out
            })
            .collect();
        sb.chains.push(SpringChain {
            name: s.name.clone(),
            joints,
            collider_groups: s.collider_groups.iter().map(|&g| crate::ix32(g)).collect(),
            center: s.center.map(|c| NodeIx(crate::ix32(c))),
        });
    }
    Ok(())
}

fn constraints(gltf: &Document, av: &mut Avatar, report: &mut Report) -> Result<(), IoError> {
    for (i, node) in gltf.nodes.iter().enumerate() {
        let Some(body) = node.extensions.get(CONSTRAINT) else { continue };
        let at = format!("/nodes/{i}/extensions/{CONSTRAINT}");
        let wire =
            WireNodeConstraint::deserialize(body).map_err(|e| invalid("VRMC_node_constraint is invalid", &at, &e))?;
        let c = wire.constraint;
        let (kind, weight) = match (c.roll, c.aim, c.rotation) {
            (Some(r), None, None) => (
                ConstraintKind::Roll {
                    source: NodeIx(crate::ix32(r.source)),
                    axis: match r.roll_axis {
                        WireRollAxis::X => RollAxis::X,
                        WireRollAxis::Y => RollAxis::Y,
                        WireRollAxis::Z => RollAxis::Z,
                    },
                },
                r.weight,
            ),
            (None, Some(a), None) => (
                ConstraintKind::Aim {
                    source: NodeIx(crate::ix32(a.source)),
                    axis: match a.aim_axis {
                        WireAimAxis::PositiveX => AimAxis::PositiveX,
                        WireAimAxis::NegativeX => AimAxis::NegativeX,
                        WireAimAxis::PositiveY => AimAxis::PositiveY,
                        WireAimAxis::NegativeY => AimAxis::NegativeY,
                        WireAimAxis::PositiveZ => AimAxis::PositiveZ,
                        WireAimAxis::NegativeZ => AimAxis::NegativeZ,
                    },
                },
                a.weight,
            ),
            (None, None, Some(r)) => (ConstraintKind::Rotation { source: NodeIx(crate::ix32(r.source)) }, r.weight),
            _ => {
                report.push(
                    Finding::warning(
                        "vrm.constraint_invalid",
                        "a constraint must be exactly one of roll, aim or rotation; skipped",
                    )
                    .at(at),
                );
                continue;
            }
        };
        av.constraints.push(NodeConstraint { node: NodeIx(crate::ix32(i)), kind, weight: weight.unwrap_or(1.0) });
    }
    Ok(())
}
