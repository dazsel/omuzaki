//! The payload mapped onto [`PuppetDef`]. The conventions, and the `Inochi2D` source each one rests on, are in the crate
//! docs; the comments here say only what the code can't.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::f32::consts::PI;

use omuzaki_puppet_api::{
    BindTarget, BindValues, Binding, BlendMode, Composite, Drawable, Interp, MapMode, MaskMode, MaskRef, Mesh, Node,
    NodeIx, NodeKind, Param, ParamIx, Part, Pendulum, PendulumModel, PhysicsWorld, PuppetDef, PuppetMeta, TexIx,
    Transform2,
};
use serde_json::value::RawValue;

use crate::error::{ImportError, Limits, limit, malformed};
use crate::payload::{JBinding, JMask, JMesh, JMeta, JNode, JParam, JPhysics, JPuppet, JTransform, Named, field};
use crate::report::{FindingKind, ImportReport, Mapped, rule};

/// `Inochi2D`'s "no texture" slot id (`NO_TEXTURE = uint.max`, `core/nodes/part/package.d`).
const NO_TEXTURE: u64 = u32::MAX as u64;
/// How close an x or y rotation must be to a multiple of pi to count as a flip.
const FLIP_EPSILON: f32 = 1e-4;

/// A node of the JSON tree in flat, parents-first order.
struct Flat<'j, 'a> {
    json: &'j JNode<'a>,
    path: String,
    parent: Option<usize>,
    uuid: Option<u32>,
}

/// A mask whose source is resolved once every node's kind is known.
struct PendingMask {
    uuid: u32,
    mode: MaskMode,
    path: String,
}

/// What the mapping needs while it walks the payload.
struct Ctx<'r> {
    limits: &'r Limits,
    report: &'r mut ImportReport,
    vertices: usize,
    offsets: usize,
}

fn finite(v: f32, path: &str) -> Result<f32, ImportError> {
    if v.is_finite() { Ok(v) } else { Err(malformed(rule::NUMBER, path, "not a finite number")) }
}

fn get_or(v: &[f32], i: usize, default: f32) -> f32 {
    v.get(i).copied().unwrap_or(default)
}

/// Maps the INP1 JSON payload. The texture list and the canvas are filled in by the caller, which decodes the
/// textures after the payload has proved sound.
pub(crate) fn convert(
    json: &[u8],
    texture_count: usize,
    limits: &Limits,
    report: &mut ImportReport,
) -> Result<PuppetDef, ImportError> {
    let puppet: JPuppet<'_> = crate::payload::parse_puppet(json)?;
    let Some(root) = puppet.nodes.as_ref() else {
        return Err(malformed(rule::TREE, "nodes", "the puppet has no root node"));
    };
    let mut ctx = Ctx { limits, report, vertices: 0, offsets: 0 };
    let flat = flatten(root, limits)?;
    let uuids = node_uuids(&flat, ctx.report)?;

    let mut nodes = Vec::with_capacity(flat.len());
    let mut pending_masks = Vec::new();
    let mut physics_nodes = Vec::new();
    let mut propagates = Vec::with_capacity(flat.len());
    for (i, f) in flat.iter().enumerate() {
        let (node, masks) = map_node(f, texture_count, &mut ctx)?;
        if !masks.is_empty() {
            pending_masks.push((i, masks));
        }
        if matches!(node.kind, NodeKind::Group) && type_name(f)?.as_deref() == Some("SimplePhysics") {
            physics_nodes.push(i);
        }
        propagates.push(match node.kind {
            NodeKind::Part(_) | NodeKind::Mask(_) => true,
            // A composite passes a mesh group's deformation on only when it says so; a missing flag reads false.
            NodeKind::Composite(_) => {
                field::<bool>(f.json.propagate_meshgroup, &format!("{}/propagate_meshgroup", f.path))?.unwrap_or(false)
            }
            _ => false,
        });
        nodes.push(node);
    }
    resolve_masks(&mut nodes, pending_masks, &uuids, ctx.report)?;
    note_mesh_group_reach(&flat, &nodes, &propagates, ctx.report);

    let jparams = puppet.param.as_deref().unwrap_or_default();
    if jparams.len() > limits.max_params {
        return Err(limit("max_params", "param", format!("{} parameters", jparams.len())));
    }
    let mut params = Vec::with_capacity(jparams.len());
    let mut param_uuids = BTreeMap::new();
    for (p, jp) in jparams.iter().enumerate() {
        let path = format!("param/{p}");
        if let Some(uuid) = jp.uuid {
            param_uuids.entry(uuid).or_insert(ParamIx(index_u32(p)));
        }
        params.push(map_param(jp, &path, &nodes, &uuids, &mut ctx)?);
    }

    let mut physics = Vec::new();
    for i in physics_nodes {
        if let Some(pendulum) = map_pendulum(&flat[i], i, &param_uuids, ctx.report)? {
            physics.push(pendulum);
        }
    }

    let world = map_world(puppet.physics, ctx.report)?;
    let meta = map_meta(puppet.meta, ctx.report)?;
    note_unmapped(puppet.automation, rule::AUTOMATION, "automation", ctx.report);
    note_unmapped(puppet.animations, rule::ANIMATION, "animations", ctx.report);

    Ok(PuppetDef { meta, canvas: [1.0, 1.0], textures: Vec::new(), nodes, params, physics, world })
}

#[allow(clippy::cast_possible_truncation, reason = "indices are bounded by Limits, far below u32::MAX")]
const fn index_u32(i: usize) -> u32 {
    i as u32
}

/// The tree in pre-order (a parent before its children, children in file order), with the node limit checked as
/// each node is reached, so a huge tree is refused before any of it is mapped.
fn flatten<'j, 'a>(root: &'j JNode<'a>, limits: &Limits) -> Result<Vec<Flat<'j, 'a>>, ImportError> {
    let mut out: Vec<Flat<'j, 'a>> = Vec::new();
    let mut stack = vec![(root, String::from("nodes"), None::<usize>, 1usize)];
    while let Some((json, path, parent, depth)) = stack.pop() {
        if out.len() >= limits.max_nodes {
            return Err(limit("max_nodes", path, format!("more than {} nodes", limits.max_nodes)));
        }
        if depth > limits.max_depth {
            return Err(limit("max_depth", path, format!("a tree deeper than {}", limits.max_depth)));
        }
        let uuid = field::<u32>(json.uuid, &format!("{path}/uuid"))?;
        let index = out.len();
        if let Some(children) = &json.children {
            for (c, child) in children.iter().enumerate().rev() {
                stack.push((child, format!("{path}/children/{c}"), Some(index), depth + 1));
            }
        }
        out.push(Flat { json, path, parent, uuid });
    }
    Ok(out)
}

/// The uuid of every node, the first in tree order winning (`Inochi2D`'s `find` walks the tree the same way). A node
/// with an ancestor's uuid makes the tree a cycle for anything that follows uuids, and is refused.
fn node_uuids(flat: &[Flat<'_, '_>], report: &mut ImportReport) -> Result<BTreeMap<u32, usize>, ImportError> {
    let mut map = BTreeMap::new();
    for (i, f) in flat.iter().enumerate() {
        let Some(uuid) = f.uuid else { continue };
        let mut up = f.parent;
        while let Some(a) = up {
            if flat[a].uuid == Some(uuid) {
                return Err(malformed(
                    rule::TREE_CYCLE,
                    format!("{}/uuid", f.path),
                    format!("uuid {uuid} is also an ancestor's ({})", flat[a].path),
                ));
            }
            up = flat[a].parent;
        }
        match map.entry(uuid) {
            Entry::Vacant(e) => {
                e.insert(i);
            }
            Entry::Occupied(e) => report.add(
                rule::DUPLICATE_UUID,
                FindingKind::Note,
                format!("{}/uuid", f.path),
                format!("uuid {uuid} is also {}'s; references go to that one", flat[*e.get()].path),
            ),
        }
    }
    Ok(map)
}

fn type_name(f: &Flat<'_, '_>) -> Result<Option<String>, ImportError> {
    field::<String>(f.json.ty, &format!("{}/type", f.path))
}

fn map_node(
    f: &Flat<'_, '_>,
    texture_count: usize,
    ctx: &mut Ctx<'_>,
) -> Result<(Node, Vec<PendingMask>), ImportError> {
    let path = &f.path;
    let j = f.json;
    let name = field::<String>(j.name, &format!("{path}/name"))?.unwrap_or_default();
    let enabled = field::<bool>(j.enabled, &format!("{path}/enabled"))?.unwrap_or(true);
    let zsort = finite(field::<f32>(j.zsort, &format!("{path}/zsort"))?.unwrap_or(0.0), &format!("{path}/zsort"))?;
    let transform = map_transform(j.transform, &format!("{path}/transform"), ctx.report)?;
    if field::<bool>(j.lockToRoot, &format!("{path}/lockToRoot"))?.unwrap_or(false) {
        ctx.report.add(
            rule::LOCK_TO_ROOT,
            FindingKind::Approximated,
            format!("{path}/lockToRoot"),
            "the node follows the root, skipping its parents' transforms, in Inochi2D; here it follows its parent",
        );
    }
    let ty = type_name(f)?.unwrap_or_default();
    let mut masks = Vec::new();
    let kind = match ty.as_str() {
        "Node" | "SimplePhysics" => NodeKind::Group,
        "Part" => map_part(j, path, texture_count, ctx, &mut masks)?,
        "Mask" => NodeKind::Mask(map_mesh(j.mesh, path, MeshUse::Mask, ctx)?),
        "Composite" => {
            let (draw, m) = map_drawable(j, path, ctx.report)?;
            masks = m;
            NodeKind::Composite(Composite { draw })
        }
        "MeshGroup" => map_mesh_group(j, path, ctx)?,
        _ => {
            ctx.report.add(
                rule::UNKNOWN_NODE,
                FindingKind::Note,
                format!("{path}/type"),
                format!(
                    "node type {ty:?} is not an Inochi2D 0.8 type we map: kept as an unknown node that acts as a group \
                     (Inochi2D 0.8 skips such a node and everything under it)"
                ),
            );
            NodeKind::Unknown { type_name: ty }
        }
    };
    let parent = f.parent.map(|p| NodeIx(index_u32(p)));
    Ok((Node { name, parent, enabled, transform, zsort, kind }, masks))
}

/// `Inochi2D`'s transform is `T * Rz * Ry * Rx * S` in 3D (`core/math/transform.d`); points have z = 0. With one of the x
/// and y angles zero, the plane part is exactly `Rz * diag(cos ry, cos rx) * S`, so the x and y angles fold into the
/// scale (the evaluator does the same for bound angles).
fn map_transform(raw: Option<&RawValue>, path: &str, report: &mut ImportReport) -> Result<Transform2, ImportError> {
    let t = field::<JTransform>(raw, path)?.unwrap_or_default();
    let trans = |i: usize| finite(get_or(&t.trans, i, 0.0), &format!("{path}/trans/{i}"));
    let rot = |i: usize| finite(get_or(&t.rot, i, 0.0), &format!("{path}/rot/{i}"));
    let scale = |i: usize| finite(get_or(&t.scale, i, 1.0), &format!("{path}/scale/{i}"));
    let (tx, ty, tz) = (trans(0)?, trans(1)?, trans(2)?);
    let (rx, ry, rz) = (rot(0)?, rot(1)?, rot(2)?);
    let (sx, sy) = (scale(0)?, scale(1)?);
    if tz != 0.0 {
        report.add(rule::TRANSLATE_Z, FindingKind::Dropped, format!("{path}/trans/2"), format!("z translation {tz}"));
    }
    let flip = |a: f32| (a - (a / PI).round() * PI).abs() <= FLIP_EPSILON;
    if !flip(rx) || !flip(ry) {
        report.add(
            rule::ROTATION_XY,
            FindingKind::Approximated,
            format!("{path}/rot"),
            format!(
                "x and y rotations ({rx}, {ry}) fold into scale by their cosines: exact for one of them alone, but the \
                 depth they give points is dropped{}",
                if flip(rx) || flip(ry) { "" } else { ", and with both set their shear is dropped too" }
            ),
        );
    }
    Ok(Transform2 { translation: [tx, ty], rotation: rz, scale: [sx * ry.cos(), sy * rx.cos()] })
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum MeshUse {
    /// A textured part: uvs required, drawn at `verts - origin`.
    Part,
    /// A mask mesh: drawn at `verts - origin`, uvs kept when they match.
    Mask,
    /// A mesh group: its points are used as they are (its `origin` takes no part in the deformation).
    Deformer,
}

fn map_mesh(raw: Option<&RawValue>, node_path: &str, use_: MeshUse, ctx: &mut Ctx<'_>) -> Result<Mesh, ImportError> {
    let path = format!("{node_path}/mesh");
    let Some(m) = field::<JMesh>(raw, &path)? else {
        return Ok(Mesh::default());
    };
    if m.verts.len() % 2 != 0 {
        return Err(malformed(rule::MESH, format!("{path}/verts"), "an odd number of coordinates"));
    }
    let n = m.verts.len() / 2;
    ctx.vertices = ctx.vertices.saturating_add(n);
    if ctx.vertices > ctx.limits.max_vertices {
        return Err(limit("max_vertices", path, format!("more than {} vertices", ctx.limits.max_vertices)));
    }
    let origin = if use_ == MeshUse::Deformer {
        [0.0, 0.0]
    } else {
        [
            finite(get_or(&m.origin, 0, 0.0), &format!("{path}/origin/0"))?,
            finite(get_or(&m.origin, 1, 0.0), &format!("{path}/origin/1"))?,
        ]
    };
    let mut vertices = Vec::with_capacity(n);
    for (i, v) in m.verts.as_chunks::<2>().0.iter().enumerate() {
        let p = format!("{path}/verts/{}", 2 * i);
        vertices.push([finite(v[0], &p)? - origin[0], finite(v[1], &p)? - origin[1]]);
    }
    let uvs = map_uvs(m.uvs.as_deref(), n, &path, use_)?;
    let triangles = if m.indices.len() % 3 != 0 {
        ctx.report.add(
            rule::INDICES,
            FindingKind::Dropped,
            format!("{path}/indices"),
            format!("{} indices are not whole triangles; Inochi2D doesn't draw such a mesh", m.indices.len()),
        );
        Vec::new()
    } else {
        if let Some(k) = m.indices.iter().position(|&i| i as usize >= n) {
            return Err(malformed(
                rule::MESH,
                format!("{path}/indices/{k}"),
                format!("index {} is past the {n} vertices", m.indices[k]),
            ));
        }
        m.indices.as_chunks::<3>().0.to_vec()
    };
    Ok(Mesh { vertices, uvs, triangles })
}

fn map_uvs(uvs: Option<&[f32]>, n: usize, path: &str, use_: MeshUse) -> Result<Vec<[f32; 2]>, ImportError> {
    let uvs = uvs.unwrap_or_default();
    let fits = uvs.len() == 2 * n;
    match use_ {
        MeshUse::Deformer => return Ok(Vec::new()),
        MeshUse::Mask if !fits => return Ok(Vec::new()),
        MeshUse::Part if !fits => {
            return Err(malformed(
                rule::MESH,
                format!("{path}/uvs"),
                format!("{} uv coordinates for {n} vertices", uvs.len()),
            ));
        }
        MeshUse::Mask | MeshUse::Part => {}
    }
    let mut out = Vec::with_capacity(n);
    for (i, uv) in uvs.as_chunks::<2>().0.iter().enumerate() {
        let p = format!("{path}/uvs/{}", 2 * i);
        out.push([finite(uv[0], &p)?, finite(uv[1], &p)?]);
    }
    Ok(out)
}

fn map_part(
    j: &JNode<'_>,
    path: &str,
    texture_count: usize,
    ctx: &mut Ctx<'_>,
    masks: &mut Vec<PendingMask>,
) -> Result<NodeKind, ImportError> {
    // Inochi2D fills its albedo, emissive and bump slots from the ids that aren't NO_TEXTURE, in order.
    let ids = field::<Vec<u64>>(j.textures, &format!("{path}/textures"))?.unwrap_or_default();
    let mut used = ids.iter().enumerate().filter(|(_, id)| **id < NO_TEXTURE);
    let albedo = used.next();
    if used.next().is_some() {
        ctx.report.add(
            rule::PART_EXTRA_TEXTURES,
            FindingKind::Dropped,
            format!("{path}/textures"),
            "emission and bump-map textures",
        );
    }
    if let Some(strength) = field::<f32>(j.emissionStrength, &format!("{path}/emissionStrength"))?
        && (strength - 1.0).abs() > f32::EPSILON
    {
        ctx.report.add(
            rule::PART_EXTRA_TEXTURES,
            FindingKind::Dropped,
            format!("{path}/emissionStrength"),
            format!("emission strength {strength}"),
        );
    }
    let Some((k, &id)) = albedo else {
        ctx.report.add(
            rule::PART_NO_TEXTURE,
            FindingKind::Approximated,
            format!("{path}/textures"),
            "a part with no albedo texture: kept as a mask mesh, which never draws",
        );
        return Ok(NodeKind::Mask(map_mesh(j.mesh, path, MeshUse::Mask, ctx)?));
    };
    let texture = usize::try_from(id).ok().filter(|&t| t < texture_count).ok_or_else(|| {
        malformed(rule::TEXTURE_REF, format!("{path}/textures/{k}"), format!("texture {id} of {texture_count}"))
    })?;
    let mesh = map_mesh(j.mesh, path, MeshUse::Part, ctx)?;
    let (draw, m) = map_drawable(j, path, ctx.report)?;
    *masks = m;
    Ok(NodeKind::Part(Part { mesh, texture: TexIx(index_u32(texture)), draw }))
}

fn map_mesh_group(j: &JNode<'_>, path: &str, ctx: &mut Ctx<'_>) -> Result<NodeKind, ImportError> {
    let mesh = map_mesh(j.mesh, path, MeshUse::Deformer, ctx)?;
    for (raw, key) in [(j.dynamic_deformation, "dynamic_deformation"), (j.translate_children, "translate_children")] {
        if field::<bool>(raw, &format!("{path}/{key}"))?.unwrap_or(false) {
            ctx.report.add(
                rule::MESHGROUP_MODE,
                FindingKind::Approximated,
                format!("{path}/{key}"),
                format!("{key} is set; our mesh deformer carries the points of every part and mask under it"),
            );
        }
    }
    if mesh.triangles.is_empty() {
        ctx.report.add(
            rule::MESHGROUP_EMPTY,
            FindingKind::Approximated,
            format!("{path}/mesh"),
            "a mesh group with no triangles deforms nothing: kept as a plain group",
        );
        return Ok(NodeKind::Group);
    }
    Ok(NodeKind::MeshDeformer(mesh))
}

/// A mesh group deforms the drawables under it only through drawables and propagating composites
/// (`core/nodes/meshgroup/package.d`, `setupChild`): a part under a plain node under the group stays where it is. Our
/// deformer carries every point under it, so each part `Inochi2D` would leave alone is counted and reported.
fn note_mesh_group_reach(flat: &[Flat<'_, '_>], nodes: &[Node], propagates: &[bool], report: &mut ImportReport) {
    let mut reach: Vec<Option<(usize, bool)>> = Vec::with_capacity(nodes.len());
    let mut blocked: BTreeMap<usize, usize> = BTreeMap::new();
    for node in nodes {
        let r = node.parent.map(NodeIx::get).and_then(|p| {
            if matches!(nodes[p].kind, NodeKind::MeshDeformer(_)) {
                Some((p, true))
            } else {
                reach.get(p).copied().flatten().map(|(d, ok)| (d, ok && propagates[p]))
            }
        });
        if let Some((d, false)) = r
            && node.kind.deform_points().is_some()
        {
            *blocked.entry(d).or_default() += 1;
        }
        reach.push(r);
    }
    for (d, count) in blocked {
        report.add(
            rule::MESHGROUP_MODE,
            FindingKind::Approximated,
            format!("{}/children", flat[d].path),
            format!(
                "{count} meshes sit under a plain node or a non-propagating composite inside this mesh group: Inochi2D \
                 leaves them undeformed, our mesh deformer moves them"
            ),
        );
    }
}

fn map_drawable(
    j: &JNode<'_>,
    path: &str,
    report: &mut ImportReport,
) -> Result<(Drawable, Vec<PendingMask>), ImportError> {
    let mut draw = Drawable::default();
    if let Some(mode) = field::<Named>(j.blend_mode, &format!("{path}/blend_mode"))? {
        draw.blend = map_blend(&mode, &format!("{path}/blend_mode"), report);
    }
    let num = |raw, key: &str, default: f32| -> Result<f32, ImportError> {
        let p = format!("{path}/{key}");
        finite(field::<f32>(raw, &p)?.unwrap_or(default), &p)
    };
    draw.opacity = num(j.opacity, "opacity", 1.0)?;
    draw.mask_threshold = num(j.mask_threshold, "mask_threshold", 0.5)?;
    let rgb = |raw, key: &str, default: f32| -> Result<[f32; 3], ImportError> {
        let p = format!("{path}/{key}");
        let v = field::<Vec<f32>>(raw, &p)?.unwrap_or_default();
        Ok([
            finite(get_or(&v, 0, default), &p)?,
            finite(get_or(&v, 1, default), &p)?,
            finite(get_or(&v, 2, default), &p)?,
        ])
    };
    draw.tint = rgb(j.tint, "tint", 1.0)?;
    draw.screen_tint = rgb(j.screenTint, "screenTint", 0.0)?;

    let mut masks = Vec::new();
    let mpath = format!("{path}/masks");
    for (m, mask) in field::<Vec<JMask>>(j.masks, &mpath)?.unwrap_or_default().into_iter().enumerate() {
        let p = format!("{mpath}/{m}");
        let uuid = mask.source.ok_or_else(|| malformed(rule::MASK_SOURCE, format!("{p}/source"), "no source"))?;
        let mode = map_mask_mode(mask.mode.as_ref(), &format!("{p}/mode"), report);
        masks.push(PendingMask { uuid, mode, path: format!("{p}/source") });
    }
    // Before 0.8, masks were a list of uuids with one mode for all (still read by Inochi2D 0.8).
    let legacy = field::<Vec<u32>>(j.masked_by, &format!("{path}/masked_by"))?.unwrap_or_default();
    if !legacy.is_empty() {
        let mode = field::<Named>(j.mask_mode, &format!("{path}/mask_mode"))?;
        let mode = map_mask_mode(mode.as_ref(), &format!("{path}/mask_mode"), report);
        for (m, uuid) in legacy.into_iter().enumerate() {
            masks.push(PendingMask { uuid, mode, path: format!("{path}/masked_by/{m}") });
        }
    }
    Ok((draw, masks))
}

/// `Inochi2D` 0.8's blend modes by name, or by their position in its `BlendMode` enum (`core/nodes/common.d`).
fn map_blend(mode: &Named, path: &str, report: &mut ImportReport) -> BlendMode {
    const ORDER: [&str; 19] = [
        "Normal",
        "Multiply",
        "Screen",
        "Overlay",
        "Darken",
        "Lighten",
        "ColorDodge",
        "LinearDodge",
        "AddGlow",
        "ColorBurn",
        "HardLight",
        "SoftLight",
        "Difference",
        "Exclusion",
        "Subtract",
        "Inverse",
        "DestinationIn",
        "ClipToLower",
        "SliceFromLower",
    ];
    let name = match mode {
        Named::Name(s) => s.as_str(),
        Named::Number(n) => usize::try_from(*n).ok().and_then(|i| ORDER.get(i)).copied().unwrap_or(""),
    };
    let (blend, approx) = match name {
        "Normal" => (BlendMode::Normal, None),
        "Multiply" => (BlendMode::Multiply, None),
        "Screen" => (BlendMode::Screen, None),
        "Overlay" => (BlendMode::Overlay, None),
        "Darken" => (BlendMode::Darken, None),
        "Lighten" => (BlendMode::Lighten, None),
        "ColorDodge" => (BlendMode::ColorDodge, None),
        "LinearDodge" => (BlendMode::LinearDodge, None),
        "ColorBurn" => (BlendMode::ColorBurn, None),
        "HardLight" => (BlendMode::HardLight, None),
        "SoftLight" => (BlendMode::SoftLight, None),
        "Difference" => (BlendMode::Difference, None),
        "Exclusion" => (BlendMode::Exclusion, None),
        "Subtract" => (BlendMode::Subtract, None),
        "ClipToLower" => (BlendMode::ClipToLower, None),
        "SliceFromLower" => (BlendMode::SliceFromLower, None),
        "AddGlow" => (BlendMode::LinearDodge, Some("Add (Glow) read as linear dodge (it also adds to alpha)")),
        "Inverse" => (BlendMode::Normal, Some("Inverse has no equivalent: read as normal")),
        "DestinationIn" => (BlendMode::Normal, Some("Destination In has no equivalent: read as normal")),
        _ => (BlendMode::Normal, Some("an unknown blend mode: read as normal")),
    };
    if let Some(detail) = approx {
        report.add(rule::BLEND_MODE, FindingKind::Approximated, path, format!("{} ({detail})", mode.text()));
    }
    blend
}

fn map_mask_mode(mode: Option<&Named>, path: &str, report: &mut ImportReport) -> MaskMode {
    match mode {
        None | Some(Named::Number(0)) => MaskMode::Mask,
        Some(Named::Name(s)) if s == "Mask" => MaskMode::Mask,
        Some(Named::Number(1)) => MaskMode::Dodge,
        Some(Named::Name(s)) if s == "DodgeMask" => MaskMode::Dodge,
        Some(other) => {
            report.add(rule::MASK_MODE, FindingKind::Approximated, path, format!("{}: read as Mask", other.text()));
            MaskMode::Mask
        }
    }
}

/// A mask names its source by uuid. A uuid no node has is refused (`Inochi2D` drops such a mask silently); a source our
/// model can't mask with is dropped with a finding.
fn resolve_masks(
    nodes: &mut [Node],
    pending: Vec<(usize, Vec<PendingMask>)>,
    uuids: &BTreeMap<u32, usize>,
    report: &mut ImportReport,
) -> Result<(), ImportError> {
    for (i, masks) in pending {
        let mut refs = Vec::with_capacity(masks.len());
        for m in masks {
            let Some(&src) = uuids.get(&m.uuid) else {
                return Err(malformed(rule::MASK_SOURCE, m.path, format!("no node has uuid {}", m.uuid)));
            };
            let can_mask = matches!(nodes[src].kind, NodeKind::Part(_) | NodeKind::Mask(_));
            if src == i || !can_mask {
                report.add(
                    rule::MASK_SOURCE_KIND,
                    FindingKind::Dropped,
                    m.path,
                    if src == i {
                        "a node that masks itself".to_owned()
                    } else {
                        format!("{:?} can't mask", nodes[src].name)
                    },
                );
                continue;
            }
            refs.push(MaskRef { source: NodeIx(index_u32(src)), mode: m.mode });
        }
        match &mut nodes[i].kind {
            NodeKind::Part(part) => part.draw.masks = refs,
            NodeKind::Composite(c) => c.draw.masks = refs,
            _ => debug_assert!(refs.is_empty(), "only parts and composites collect masks"),
        }
    }
    Ok(())
}

/// One parameter axis, from `Inochi2D`'s normalised key positions to positions in the parameter's units.
struct Axis {
    keys: Vec<f32>,
    lo: f32,
    hi: f32,
    /// The keys run against the file's order (the file's max is below its min).
    reversed: bool,
}

fn map_axis(ts: &[f32], min: f32, max: f32, path: &str, report: &mut ImportReport) -> Result<Axis, ImportError> {
    if ts.is_empty() {
        return Err(malformed(rule::PARAM, path, "an axis with no key points"));
    }
    for (k, t) in ts.iter().enumerate() {
        finite(*t, &format!("{path}/{k}"))?;
    }
    if ts.windows(2).any(|w| w[0] >= w[1]) {
        return Err(malformed(rule::PARAM, path, "key points are not strictly ascending"));
    }
    let mut span = max - min;
    let (lo, mut hi) = (min.min(max), min.max(max));
    if span == 0.0 && ts.len() > 1 {
        report.add(
            rule::PARAM_RANGE,
            FindingKind::Synthesised,
            path,
            format!("min and max are both {min}: the key points are spread over [{min}, {}]", min + 1.0),
        );
        span = 1.0;
        hi = lo + 1.0;
    }
    let reversed = span < 0.0;
    let mut keys: Vec<f32> = ts.iter().map(|t| min + t * span).collect();
    if reversed {
        keys.reverse();
    }
    if keys.windows(2).any(|w| w[0] >= w[1]) || !span.is_finite() {
        return Err(malformed(rule::PARAM, path, "key points collapse together in the parameter's range"));
    }
    Ok(Axis { keys, lo, hi, reversed })
}

fn map_param(
    jp: &JParam<'_>,
    path: &str,
    nodes: &[Node],
    uuids: &BTreeMap<u32, usize>,
    ctx: &mut Ctx<'_>,
) -> Result<Param, ImportError> {
    // Inochi2D 0.8's defaults for a parameter's missing keys (`core/param/package.d`).
    let two_axes = jp.is_vec2.unwrap_or(false);
    let vec2 = |v: &Option<Vec<f32>>, key: &str, d: [f32; 2]| -> Result<[f32; 2], ImportError> {
        let v = v.as_deref().unwrap_or_default();
        let p = format!("{path}/{key}");
        Ok([finite(get_or(v, 0, d[0]), &p)?, finite(get_or(v, 1, d[1]), &p)?])
    };
    let min = vec2(&jp.min, "min", [0.0, 0.0])?;
    let max = vec2(&jp.max, "max", [1.0, 1.0])?;
    let default = vec2(&jp.defaults, "defaults", [0.0, 0.0])?;
    let default_points = [vec![0.0, 1.0], vec![0.0, 1.0]];
    let points = jp.axis_points.as_deref().unwrap_or(&default_points);
    let (Some(px), Some(py)) = (points.first(), points.get(1)) else {
        return Err(malformed(rule::PARAM, format!("{path}/axis_points"), "two axes of key points are required"));
    };
    let x = map_axis(px, min[0], max[0], &format!("{path}/axis_points/0"), ctx.report)?;
    let y = if two_axes || py.len() <= 1 {
        map_axis(py, min[1], max[1], &format!("{path}/axis_points/1"), ctx.report)?
    } else {
        ctx.report.add(
            rule::PARAM_AXIS,
            FindingKind::Dropped,
            format!("{path}/axis_points/1"),
            format!("a one-axis parameter with {} y key points: only the first is used, as Inochi2D does", py.len()),
        );
        map_axis(&py[..1], min[1], max[1], &format!("{path}/axis_points/1"), ctx.report)?
    };
    if let Some(mode) = &jp.merge_mode
        && !matches!(mode, Named::Name(s) if s.eq_ignore_ascii_case("additive"))
        && !matches!(mode, Named::Number(0))
    {
        ctx.report.add(rule::PARAM_MERGE_MODE, FindingKind::Dropped, format!("{path}/merge_mode"), mode.text());
    }
    let grid = Grid { nx_file: px.len(), ny_file: py.len(), x: &x, y: &y, two_axes };
    let mut bindings = Vec::new();
    for (b, jb) in jp.bindings.as_deref().unwrap_or_default().iter().enumerate() {
        if let Some(binding) = map_binding(jb, &format!("{path}/bindings/{b}"), &grid, nodes, uuids, ctx)? {
            bindings.push(binding);
        }
    }
    Ok(Param {
        name: jp.name.clone().unwrap_or_default(),
        two_axes,
        min: [x.lo, y.lo],
        max: [x.hi, y.hi],
        default,
        keys: [x.keys.clone(), y.keys.clone()],
        bindings,
    })
}

/// A parameter's key grid as the file lays it out (`values[x][y]`) and as ours is (x-major, ascending keys).
struct Grid<'g> {
    nx_file: usize,
    ny_file: usize,
    x: &'g Axis,
    y: &'g Axis,
    two_axes: bool,
}

impl Grid<'_> {
    /// The file's `[x][y]` for our key point `(ix, iy)`.
    fn file_index(&self, ix: usize, iy: usize) -> (usize, usize) {
        let nx = self.x.keys.len();
        let ny = self.y.keys.len();
        let fx = if self.x.reversed { nx - 1 - ix } else { ix };
        let fy = if !self.two_axes {
            0
        } else if self.y.reversed {
            ny - 1 - iy
        } else {
            iy
        };
        (fx, fy)
    }

    /// Our key points in x-major order, as the file's `[x][y]`.
    fn order(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        let ny = self.y.keys.len();
        (0..self.x.keys.len()).flat_map(move |ix| (0..ny).map(move |iy| self.file_index(ix, iy)))
    }

    /// Checks that a `[x][y]` list has the file's grid shape.
    fn check_shape<T>(&self, v: &[Vec<T>], path: &str) -> Result<(), ImportError> {
        if v.len() != self.nx_file {
            return Err(malformed(
                rule::BINDING_VALUES,
                path,
                format!("{} columns for {} x key points", v.len(), self.nx_file),
            ));
        }
        if let Some((x, col)) = v.iter().enumerate().find(|(_, col)| col.len() != self.ny_file) {
            return Err(malformed(
                rule::BINDING_VALUES,
                format!("{path}/{x}"),
                format!("{} values for {} y key points", col.len(), self.ny_file),
            ));
        }
        Ok(())
    }
}

/// `Inochi2D`'s binding names (`core/nodes/package.d`, `core/nodes/part/package.d`, `core/param/binding.d`).
fn map_target(name: &str) -> Result<BindTarget, &'static str> {
    Ok(match name {
        "zSort" => BindTarget::ZSort,
        "transform.t.x" => BindTarget::TranslateX,
        "transform.t.y" => BindTarget::TranslateY,
        "transform.r.x" => BindTarget::RotateX,
        "transform.r.y" => BindTarget::RotateY,
        "transform.r.z" => BindTarget::Rotate,
        "transform.s.x" => BindTarget::ScaleX,
        "transform.s.y" => BindTarget::ScaleY,
        "deform" => BindTarget::Deform,
        "opacity" => BindTarget::Opacity,
        "tint.r" => BindTarget::TintR,
        "tint.g" => BindTarget::TintG,
        "tint.b" => BindTarget::TintB,
        "transform.t.z" => return Err("z translation"),
        "screenTint.r" | "screenTint.g" | "screenTint.b" => return Err("screen tint"),
        "emissionStrength" => return Err("emission strength"),
        "alphaThreshold" => return Err("the mask threshold"),
        "gravity" | "length" | "frequency" | "angleDamping" | "lengthDamping" | "outputScale.x" | "outputScale.y" => {
            return Err("a physics setting");
        }
        _ => return Err("an unknown target"),
    })
}

fn map_interp(mode: Option<&Named>, path: &str, report: &mut ImportReport) -> Interp {
    // Inochi2D's InterpolateMode (`core/package.d`): Nearest, Linear, Stepped, Cubic, Bezier.
    let name = match mode {
        None => return Interp::Linear,
        Some(Named::Name(s)) => s.as_str(),
        Some(Named::Number(n)) => match n {
            0 => "Nearest",
            1 => "Linear",
            2 => "Stepped",
            3 => "Cubic",
            4 => "Bezier",
            _ => "",
        },
    };
    let (interp, approx) = match name {
        "Nearest" => (Interp::Nearest, None),
        "Linear" => (Interp::Linear, None),
        "Cubic" => (Interp::Cubic, None),
        "Stepped" => (Interp::Nearest, Some("stepped read as nearest")),
        "Bezier" => (Interp::Cubic, Some("Bezier read as cubic")),
        _ => (Interp::Linear, Some("an unknown mode read as linear")),
    };
    if let (Some(detail), Some(mode)) = (approx, mode) {
        report.add(rule::INTERPOLATION, FindingKind::Approximated, path, format!("{} ({detail})", mode.text()));
    }
    interp
}

fn map_binding(
    jb: &JBinding<'_>,
    path: &str,
    grid: &Grid<'_>,
    nodes: &[Node],
    uuids: &BTreeMap<u32, usize>,
    ctx: &mut Ctx<'_>,
) -> Result<Option<Binding>, ImportError> {
    let Some(node_ix) = jb.node.and_then(|u| uuids.get(&u).copied()) else {
        let detail = jb.node.map_or_else(|| "no node".to_owned(), |u| format!("no node has uuid {u}"));
        ctx.report.add(rule::BINDING_NODE, FindingKind::Dropped, format!("{path}/node"), detail);
        return Ok(None);
    };
    let node = &nodes[node_ix];
    let name = jb.param_name.as_deref().unwrap_or("");
    let target = match map_target(name) {
        Ok(t) => t,
        Err(what) => {
            ctx.report.add(
                rule::BINDING_TARGET,
                FindingKind::Dropped,
                format!("{path}/param_name"),
                format!("{name:?}: {what}"),
            );
            return Ok(None);
        }
    };
    let fits = match target {
        BindTarget::Deform => node.kind.deform_points().is_some(),
        BindTarget::Opacity | BindTarget::TintR | BindTarget::TintG | BindTarget::TintB => {
            matches!(node.kind, NodeKind::Part(_) | NodeKind::Composite(_))
        }
        _ => true,
    };
    if !fits {
        ctx.report.add(
            rule::BINDING_TARGET,
            FindingKind::Dropped,
            format!("{path}/param_name"),
            format!("{name:?} on {:?}, which it doesn't move", node.name),
        );
        return Ok(None);
    }
    let vpath = format!("{path}/values");
    let points = grid.x.keys.len() * grid.y.keys.len();
    let values = if target == BindTarget::Deform {
        let n = node.kind.deform_points().unwrap_or(0);
        let v = field::<Vec<Vec<Vec<[f32; 2]>>>>(jb.values, &vpath)?
            .ok_or_else(|| malformed(rule::BINDING_VALUES, &vpath, "no values"))?;
        grid.check_shape(&v, &vpath)?;
        ctx.offsets = ctx.offsets.saturating_add(points.saturating_mul(n));
        if ctx.offsets > ctx.limits.max_offsets {
            return Err(limit("max_offsets", vpath, format!("more than {} deform offsets", ctx.limits.max_offsets)));
        }
        let mut sets = Vec::with_capacity(points);
        for (fx, fy) in grid.order() {
            let set = &v[fx][fy];
            let p = format!("{vpath}/{fx}/{fy}");
            if set.len() != n {
                return Err(malformed(rule::BINDING_VALUES, p, format!("{} offsets for {n} points", set.len())));
            }
            if set.iter().any(|o| !(o[0].is_finite() && o[1].is_finite())) {
                return Err(malformed(rule::NUMBER, p, "not a finite number"));
            }
            sets.push(set.clone());
        }
        BindValues::Offsets(sets)
    } else {
        let v = field::<Vec<Vec<f32>>>(jb.values, &vpath)?
            .ok_or_else(|| malformed(rule::BINDING_VALUES, &vpath, "no values"))?;
        grid.check_shape(&v, &vpath)?;
        let mut out = Vec::with_capacity(points);
        for (fx, fy) in grid.order() {
            out.push(finite(v[fx][fy], &format!("{vpath}/{fx}/{fy}"))?);
        }
        BindValues::Scalar(out)
    };
    let set = if let Some(s) = &jb.isSet {
        grid.check_shape(s, &format!("{path}/isSet"))?;
        let set: Vec<bool> = grid.order().map(|(fx, fy)| s[fx][fy]).collect();
        if set.iter().all(|b| *b) { Vec::new() } else { set }
    } else {
        ctx.report.add(rule::BINDING_ISSET, FindingKind::Synthesised, path, "no isSet: every key point is set");
        Vec::new()
    };
    let interp = map_interp(jb.interpolate_mode.as_ref(), &format!("{path}/interpolate_mode"), ctx.report);
    Ok(Some(Binding { node: NodeIx(index_u32(node_ix)), target, interp, set, values }))
}

fn map_pendulum(
    f: &Flat<'_, '_>,
    i: usize,
    params: &BTreeMap<u32, ParamIx>,
    report: &mut ImportReport,
) -> Result<Option<Pendulum>, ImportError> {
    let path = &f.path;
    let j = f.json;
    let uuid = field::<u32>(j.param, &format!("{path}/param"))?;
    let Some(param) = uuid.and_then(|u| params.get(&u).copied()) else {
        let detail = uuid.map_or_else(|| "no parameter".to_owned(), |u| format!("no parameter has uuid {u}"));
        report.add(rule::PHYSICS_PARAM, FindingKind::Dropped, format!("{path}/param"), detail);
        return Ok(None);
    };
    // SimplePhysics's model and map mode, by name or by their position in its enums (`core/nodes/drivers/simplephysics.d`).
    let model = match field::<Named>(j.model_type, &format!("{path}/model_type"))? {
        None | Some(Named::Number(0)) => PendulumModel::Rigid,
        Some(Named::Name(s)) if s == "Pendulum" || s == "pendulum" => PendulumModel::Rigid,
        Some(Named::Name(s)) if s == "SpringPendulum" || s == "spring_pendulum" => PendulumModel::Spring,
        Some(Named::Number(1)) => PendulumModel::Spring,
        Some(other) => {
            report.add(rule::PHYSICS_MODE, FindingKind::Approximated, format!("{path}/model_type"), other.text());
            PendulumModel::Rigid
        }
    };
    let map = match field::<Named>(j.map_mode, &format!("{path}/map_mode"))? {
        None | Some(Named::Number(0)) => MapMode::AngleLength,
        Some(Named::Name(s)) if s == "AngleLength" || s == "angle_length" => MapMode::AngleLength,
        Some(Named::Name(s)) if s == "XY" || s == "xy" => MapMode::Xy,
        Some(Named::Number(1)) => MapMode::Xy,
        Some(Named::Name(s)) if s == "LengthAngle" || s == "length_angle" => MapMode::LengthAngle,
        Some(Named::Number(2)) => MapMode::LengthAngle,
        Some(Named::Name(s)) if s == "YX" || s == "yx" => MapMode::Yx,
        Some(Named::Number(3)) => MapMode::Yx,
        Some(other) => {
            report.add(rule::PHYSICS_MODE, FindingKind::Approximated, format!("{path}/map_mode"), other.text());
            MapMode::AngleLength
        }
    };
    let num = |raw, key: &str, default: f32| -> Result<f32, ImportError> {
        let p = format!("{path}/{key}");
        finite(field::<f32>(raw, &p)?.unwrap_or(default), &p)
    };
    let gravity = num(j.gravity, "gravity", 1.0)?;
    let length = num(j.length, "length", 100.0)?;
    let mut frequency = num(j.frequency, "frequency", 1.0)?;
    let angle_damping = num(j.angle_damping, "angle_damping", 0.5)?;
    let length_damping = num(j.length_damping, "length_damping", 0.5)?;
    let scale_path = format!("{path}/output_scale");
    let scale = field::<Vec<f32>>(j.output_scale, &scale_path)?.unwrap_or_default();
    let output_scale = [finite(get_or(&scale, 0, 1.0), &scale_path)?, finite(get_or(&scale, 1, 1.0), &scale_path)?];
    let local_only = field::<bool>(j.local_only, &format!("{path}/local_only"))?.unwrap_or(false);
    if length <= 0.0 {
        report.add(rule::PHYSICS_INVALID, FindingKind::Dropped, format!("{path}/length"), format!("length {length}"));
        return Ok(None);
    }
    if frequency <= 0.0 {
        if model == PendulumModel::Spring {
            let detail = format!("a spring with frequency {frequency}");
            report.add(rule::PHYSICS_INVALID, FindingKind::Dropped, format!("{path}/frequency"), detail);
            return Ok(None);
        }
        // A rigid pendulum never reads its frequency; any positive value keeps the contract's check.
        report.add(
            rule::PHYSICS_INVALID,
            FindingKind::Synthesised,
            format!("{path}/frequency"),
            "set to 1 Hz (unused)",
        );
        frequency = 1.0;
    }
    let name = field::<String>(j.name, &format!("{path}/name"))?.unwrap_or_default();
    Ok(Some(Pendulum {
        name,
        anchor: NodeIx(index_u32(i)),
        param,
        model,
        map,
        gravity,
        length,
        frequency,
        angle_damping,
        length_damping,
        output_scale,
        local_only,
    }))
}

fn map_world(raw: Option<&RawValue>, report: &mut ImportReport) -> Result<PhysicsWorld, ImportError> {
    let defaults = PhysicsWorld::default();
    let Some(p) = field::<JPhysics>(raw, "physics")? else {
        return Ok(defaults);
    };
    let mut world = defaults;
    if let Some(ppm) = p.pixelsPerMeter {
        if ppm.is_finite() && ppm > 0.0 {
            world.pixels_per_meter = ppm;
        } else {
            let detail = format!("{ppm} pixels per metre: {} used", defaults.pixels_per_meter);
            report.add(rule::WORLD, FindingKind::Synthesised, "physics/pixelsPerMeter", detail);
        }
    }
    if let Some(g) = p.gravity {
        if g.is_finite() {
            world.gravity = g;
        } else {
            report.add(
                rule::WORLD,
                FindingKind::Synthesised,
                "physics/gravity",
                format!("gravity {g}: {} used", defaults.gravity),
            );
        }
    }
    Ok(world)
}

/// Metadata, verbatim. A right the file doesn't state is left out, never assumed: unknown means unknown.
fn map_meta(raw: Option<&RawValue>, report: &mut ImportReport) -> Result<PuppetMeta, ImportError> {
    let mut meta = PuppetMeta { source: "inochi2d".to_owned(), ..PuppetMeta::default() };
    let Some(m) = field::<JMeta>(raw, "meta")? else {
        report.add(
            rule::RIGHTS_UNKNOWN,
            FindingKind::Note,
            "meta",
            "no metadata: who made it and what they allow is unknown",
        );
        return Ok(meta);
    };
    meta.name = m.name.unwrap_or_default();
    meta.artist = m.artist;
    meta.rigger = m.rigger;
    meta.copyright = m.copyright;
    meta.licence_url = m.licenseURL;
    match m.rights {
        Some(rights) if !rights.is_empty() => {
            for (key, value) in rights {
                let text = match value {
                    serde_json::Value::Null => continue,
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                meta.rights.insert(key, text);
            }
        }
        _ => report.add(
            rule::RIGHTS_UNKNOWN,
            FindingKind::Note,
            "meta/rights",
            "the file states no usage rights: they are unknown, and nothing is assumed granted",
        ),
    }
    let notes = [
        ("version", m.version),
        ("contact", m.contact),
        ("reference", m.reference),
        ("thumbnailId", m.thumbnailId.filter(|&t| t < NO_TEXTURE).map(|t| format!("texture {t} is the thumbnail"))),
        ("preservePixels", m.preservePixels.filter(|p| *p).map(|_| "point (pixel-art) filtering requested".to_owned())),
    ];
    for (key, value) in notes {
        if let Some(value) = value {
            report.add(rule::META, FindingKind::Note, format!("meta/{key}"), value);
        }
    }
    Ok(meta)
}

fn note_unmapped(raw: Option<&RawValue>, id: &str, key: &str, report: &mut ImportReport) {
    let Some(raw) = raw else { return };
    let text = raw.get().trim();
    if !matches!(text, "null" | "[]" | "{}") {
        report.add(id, FindingKind::Dropped, key, format!("{key} are not modelled"));
    }
}

/// A 2D affine map: `[a c tx; b d ty]`.
#[derive(Copy, Clone)]
struct Affine {
    m: [f32; 6],
}

#[allow(clippy::many_single_char_names, reason = "matrix entries, named as in the formulas")]
impl Affine {
    fn local(t: &Transform2) -> Self {
        let (s, c) = t.rotation.sin_cos();
        let [sx, sy] = t.scale;
        Self { m: [c * sx, s * sx, -s * sy, c * sy, t.translation[0], t.translation[1]] }
    }

    fn then(self, child: Self) -> Self {
        let [a, b, c, d, tx, ty] = self.m;
        let [e, f, g, h, ux, uy] = child.m;
        Self {
            m: [a * e + c * f, b * e + d * f, a * g + c * h, b * g + d * h, a * ux + c * uy + tx, b * ux + d * uy + ty],
        }
    }

    fn apply(self, p: [f32; 2]) -> [f32; 2] {
        let [a, b, c, d, tx, ty] = self.m;
        [a * p[0] + c * p[1] + tx, b * p[0] + d * p[1] + ty]
    }
}

/// `Inochi2D` has no canvas: its camera centres the root. Ours is the smallest box centred on the root that holds every
/// part and mask at rest, or the largest texture when nothing has vertices.
pub(crate) fn canvas(def: &PuppetDef, report: &mut ImportReport) -> [f32; 2] {
    let mut world: Vec<Affine> = Vec::with_capacity(def.nodes.len());
    let mut half = [0.0f32, 0.0f32];
    for node in &def.nodes {
        let local = Affine::local(&node.transform);
        let w = node.parent.and_then(|p| world.get(p.get()).copied()).map_or(local, |pw| pw.then(local));
        world.push(w);
        let mesh = match &node.kind {
            NodeKind::Part(part) => &part.mesh,
            NodeKind::Mask(mesh) => mesh,
            _ => continue,
        };
        for v in &mesh.vertices {
            let p = w.apply(*v);
            half = [half[0].max(p[0].abs()), half[1].max(p[1].abs())];
        }
    }
    let mut size = [(2.0 * half[0]).ceil(), (2.0 * half[1]).ceil()];
    if !(size[0].is_finite() && size[1].is_finite() && size[0] > 0.0 && size[1] > 0.0) {
        #[allow(clippy::cast_precision_loss, reason = "texture sides are at most Limits::max_texture_dim")]
        let largest = def
            .textures
            .iter()
            .fold([1.0f32, 1.0f32], |acc, t| [acc[0].max(t.size[0] as f32), acc[1].max(t.size[1] as f32)]);
        size = largest;
    }
    report.add(
        rule::CANVAS,
        FindingKind::Synthesised,
        "",
        format!("a {} x {} canvas centred on the root, from the parts at rest", size[0], size[1]),
    );
    size
}

/// What reached the puppet, counted.
pub(crate) fn mapped(def: &PuppetDef) -> Mapped {
    let mut m = Mapped {
        nodes: def.nodes.len(),
        params: def.params.len(),
        bindings: def.params.iter().map(|p| p.bindings.len()).sum(),
        pendulums: def.physics.len(),
        textures: def.textures.len(),
        ..Mapped::default()
    };
    for node in &def.nodes {
        match node.kind {
            NodeKind::Part(_) => m.parts += 1,
            NodeKind::Mask(_) => m.masks += 1,
            NodeKind::Composite(_) => m.composites += 1,
            NodeKind::MeshDeformer(_) => m.mesh_deformers += 1,
            NodeKind::Unknown { .. } => m.unknown += 1,
            _ => m.groups += 1,
        }
    }
    m
}
