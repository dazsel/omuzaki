//! The `VRMC_springBone` 1.0 extension, mock subset: colliders with sphere and capsule shapes, collider groups, and spring
//! chains with their joints and centre node. Every joint parameter is written; one that a file leaves out reads back as
//! the spec default, which is what [`SpringJoint::new`] gives. `VRMC_springBone_extended_collider` and
//! `VRMC_springBone_limit` are not modelled: they are reported as dropped, as `omuzaki-io-vrm` reports them, and the base
//! shape or the plain joint is kept.
//!
//! Chains and colliders refer to each other by index, which is the scene's own form. The mock does not check those
//! indices itself: every import runs the scene's structural checks, and they name any index that points nowhere.

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{
    Avatar, Collider, ColliderGroup, ColliderShape, Finding, IoError, Report, SpringChain, SpringJoint,
};
use serde_json::{Value, json};

use crate::json::Gltf;
use crate::vrm::{array, indices, malformed, node_ix, number, optional, text, vec3};

/// The root extension this module reads and writes.
pub(crate) const EXTENSION: &str = "VRMC_springBone";

/// The body of `VRMC_springBone` for `av`, or `None` when there are no springs and no colliders, in which case the file
/// does not list the extension. Empty lists are omitted, because the 1.0 schema wants an entry whenever a list is there.
pub(crate) fn body(av: &Avatar) -> Option<Value> {
    let sb = &av.spring_bones;
    if sb.chains.is_empty() && sb.colliders.is_empty() {
        return None;
    }
    let mut out = json!({ "specVersion": "1.0" });
    if !sb.colliders.is_empty() {
        out["colliders"] = Value::Array(sb.colliders.iter().map(collider_json).collect());
    }
    if !sb.collider_groups.is_empty() {
        out["colliderGroups"] = Value::Array(sb.collider_groups.iter().map(group_json).collect());
    }
    if !sb.chains.is_empty() {
        out["springs"] = Value::Array(sb.chains.iter().map(chain_json).collect());
    }
    Some(out)
}

fn collider_json(c: &Collider) -> Value {
    let shape = match c.shape {
        ColliderShape::Sphere { offset, radius } => {
            json!({ "sphere": { "offset": offset.to_array(), "radius": radius } })
        }
        ColliderShape::Capsule { offset, radius, tail } => {
            json!({ "capsule": { "offset": offset.to_array(), "radius": radius, "tail": tail.to_array() } })
        }
    };
    json!({ "node": c.node.index(), "shape": shape })
}

fn group_json(g: &ColliderGroup) -> Value {
    json!({ "name": g.name, "colliders": g.colliders })
}

fn chain_json(c: &SpringChain) -> Value {
    let joints: Vec<Value> = c.joints.iter().map(joint_json).collect();
    let mut out = json!({ "name": c.name, "joints": joints });
    if !c.collider_groups.is_empty() {
        out["colliderGroups"] = json!(c.collider_groups);
    }
    if let Some(center) = c.center {
        out["center"] = json!(center.index());
    }
    out
}

fn joint_json(j: &SpringJoint) -> Value {
    json!({
        "node": j.node.index(),
        "hitRadius": j.hit_radius,
        "stiffness": j.stiffness,
        "gravityPower": j.gravity_power,
        "gravityDir": j.gravity_dir.to_array(),
        "dragForce": j.drag_force,
    })
}

/// Reads `VRMC_springBone` into `avatar.spring_bones`. A file without it leaves the avatar's springs empty.
pub(crate) fn read(gltf: &Gltf, avatar: &mut Avatar, report: &mut Report) -> Result<(), IoError> {
    let Some(body) = gltf.extensions.get(EXTENSION) else { return Ok(()) };
    let at = format!("/extensions/{EXTENSION}");
    let version = body.get("specVersion").and_then(Value::as_str);
    if version != Some("1.0") {
        return Err(IoError::unsupported(
            format!("{EXTENSION} specVersion {version:?}"),
            "only the final 1.0 is supported",
        ));
    }
    let colliders = array(body, "colliders", &at)?;
    let groups = array(body, "colliderGroups", &at)?;
    let springs = array(body, "springs", &at)?;
    let joints = || springs.iter().filter_map(|s| s.get("joints").and_then(Value::as_array)).flatten();
    if colliders.iter().any(|c| has_extension(c, "VRMC_springBone_extended_collider")) {
        note_dropped(report, &at, "VRMC_springBone_extended_collider (inside-out and plane colliders)");
    }
    if joints().any(|j| has_extension(j, "VRMC_springBone_limit")) {
        note_dropped(report, &at, "VRMC_springBone_limit (joint rotation limits)");
    }

    let sb = &mut avatar.spring_bones;
    for (i, c) in colliders.iter().enumerate() {
        sb.colliders.push(collider(c, &format!("{at}/colliders/{i}"))?);
    }
    for (i, g) in groups.iter().enumerate() {
        let here = format!("{at}/colliderGroups/{i}");
        sb.collider_groups.push(ColliderGroup {
            name: optional(g, "name", &here, text, String::new())?,
            colliders: indices(g, "colliders", &here)?,
        });
    }
    for (i, s) in springs.iter().enumerate() {
        sb.chains.push(chain(s, &format!("{at}/springs/{i}"))?);
    }
    Ok(())
}

fn has_extension(v: &Value, name: &str) -> bool {
    v.get("extensions").and_then(|e| e.get(name)).is_some()
}

/// Says that an extension the mock doesn't model was not kept, in the same words and severity as `omuzaki-io-vrm`.
fn note_dropped(report: &mut Report, at: &str, what: &str) {
    report.push(
        Finding::info(
            "gltf.extension_dropped",
            format!("{what} is not modelled; the base shape or the plain joint is used"),
        )
        .at(at),
    );
}

fn collider(c: &Value, at: &str) -> Result<Collider, IoError> {
    let node = node_ix(c.get("node").unwrap_or(&Value::Null), &format!("{at}/node"))?;
    Ok(Collider { node, shape: collider_shape(c, at)? })
}

/// The base shape of a collider: exactly one of a sphere or a capsule. A collider with no base shape is one that only
/// `VRMC_springBone_extended_collider` describes, and the mock has no shape to give it, so the file is refused.
fn collider_shape(c: &Value, at: &str) -> Result<ColliderShape, IoError> {
    let shape = c.get("shape").ok_or_else(|| malformed(at, "a collider needs a sphere or capsule shape"))?;
    let here = format!("{at}/shape");
    match (shape.get("sphere"), shape.get("capsule")) {
        (Some(s), None) => {
            let s_at = format!("{here}/sphere");
            Ok(ColliderShape::Sphere {
                offset: optional(s, "offset", &s_at, vec3, Vec3::ZERO)?,
                radius: optional(s, "radius", &s_at, number, 0.0)?,
            })
        }
        (None, Some(k)) => {
            let k_at = format!("{here}/capsule");
            Ok(ColliderShape::Capsule {
                offset: optional(k, "offset", &k_at, vec3, Vec3::ZERO)?,
                radius: optional(k, "radius", &k_at, number, 0.0)?,
                tail: optional(k, "tail", &k_at, vec3, Vec3::ZERO)?,
            })
        }
        _ => Err(malformed(&here, "a shape is exactly one of sphere or capsule")),
    }
}

fn chain(s: &Value, at: &str) -> Result<SpringChain, IoError> {
    let joints = array(s, "joints", at)?
        .iter()
        .enumerate()
        .map(|(i, j)| joint(j, &format!("{at}/joints/{i}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SpringChain {
        name: optional(s, "name", at, text, String::new())?,
        joints,
        collider_groups: indices(s, "colliderGroups", at)?,
        center: optional(s, "center", at, |v, p| node_ix(v, p).map(Some), None)?,
    })
}

fn joint(j: &Value, at: &str) -> Result<SpringJoint, IoError> {
    let node = node_ix(j.get("node").unwrap_or(&Value::Null), &format!("{at}/node"))?;
    let spec = SpringJoint::new(node);
    Ok(SpringJoint {
        node,
        hit_radius: optional(j, "hitRadius", at, number, spec.hit_radius)?,
        stiffness: optional(j, "stiffness", at, number, spec.stiffness)?,
        gravity_power: optional(j, "gravityPower", at, number, spec.gravity_power)?,
        gravity_dir: optional(j, "gravityDir", at, vec3, spec.gravity_dir)?,
        drag_force: optional(j, "dragForce", at, number, spec.drag_force)?,
    })
}
