//! Export of an imported avatar: the `IoScene` it writes, gathered through its `BodyView` (design D8, D1 condition 3).
//!
//! The runtime data goes back first: the skeleton at its rest (the T-pose, D4) with each skin's inverse bind matrices,
//! the meshes with their own skins and their morphs made dense again, the expressions with their binds and overrides,
//! and the node constraints. Then what the runtime does not use comes from `retained`, exactly as the file had it: the
//! node names, order, transforms of nodes that are not bones, and passthrough; each source mesh once, with its
//! primitives and its target names; the materials (with the person's edits of `AvatarDef.materials` applied field by
//! field, never a value converted back from the runtime), textures and images; the meta and its thumbnail; the
//! first-person annotations; the spring records by index; the file's own look-at. So an unedited avatar re-exports with
//! every field of the original, and an edit changes only the field it names.
//!
//! What a file had that the avatar left out is not written: a mesh no node draws, a node constraint the solver
//! refused (both reported at import).

use std::collections::BTreeMap;

use omuzaki_anim_api::spring::JointLimit;
use omuzaki_anim_api::{Axis, ConstraintKind, SignedAxis};
use omuzaki_avatar_api::avatar::AvatarFace;
use omuzaki_avatar_api::body::BodyView;
use omuzaki_avatar_api::def::AvatarDef;
use omuzaki_avatar_api::expression::AvatarExpression;
use omuzaki_avatar_api::retained::{Retained, RetainedSprings};
use omuzaki_io_api::{
    self as io, Avatar, Blob, EncodedImage, FirstPerson, IoError, IoScene, MaterialIx, MeshAnnotation, MeshIx, Node,
    NodeIx, SkinIx,
};
use omuzaki_render_api::MorphSet;
use omuzaki_store_api::ContentHash;

use super::springs::shape as io_shape;
use crate::convert::bones::to_file;
use crate::prepare::avatar::apply_material_edit;

/// The scene the imported avatar `body` exports as, with the person's material edits of `def`; `image` gives the
/// stored bytes of an image by its content hash.
///
/// # Errors
/// [`IoError::Unsupported`] for a body that is not an imported avatar; [`IoError::Resource`] for an image the store
/// does not have.
pub fn gather_imported(
    body: &BodyView<'_>,
    def: &AvatarDef,
    image: &dyn Fn(&ContentHash) -> Option<Vec<u8>>,
) -> Result<IoScene, IoError> {
    let (Some(export), Some(face)) = (body.export, body.expressions) else {
        return Err(IoError::unsupported(
            "this body is built from parts",
            "export it with the built character's gather",
        ));
    };
    let r = export.retained;
    let mut scene = IoScene::new();
    let node_of_bone = node_of_bone(r, body.skeleton.len());
    let mesh_index = kept_meshes(r);
    scene.nodes = nodes(body, r, &mesh_index);
    scene.roots = r.roots.iter().map(|&n| NodeIx(n)).collect();
    scene.meshes = meshes(body, r);
    scene.skins = skins(body, r);
    scene.materials = r
        .materials
        .iter()
        .map(|m| {
            let mut source = m.source.clone();
            if let Some(edit) = def.materials.get(&m.name) {
                apply_material_edit(&mut source, edit);
            }
            source
        })
        .collect();
    scene.textures.clone_from(&r.textures);
    scene.images = r
        .images
        .iter()
        .map(|i| {
            let bytes = image(&i.hash).ok_or_else(|| IoError::Resource {
                uri: i.name.clone(),
                reason: format!("image {} isn't in the store", i.hash),
            })?;
            Ok(EncodedImage::new(i.name.clone(), i.mime, Blob::new(bytes)))
        })
        .collect::<Result<_, IoError>>()?;
    scene.meta = r.meta.clone().unwrap_or_default();
    scene.passthrough = r.passthrough.clone();
    let mut avatar = Avatar::new();
    for (bone, def_bone) in body.skeleton.bones().iter().enumerate() {
        let (Some(role), Some(Some(node))) = (def_bone.humanoid, node_of_bone.get(bone)) else { continue };
        if let Ok(name) = to_file(role) {
            avatar.humanoid.bones.insert(name, *node);
        }
    }
    avatar.expressions = expressions(&face, r);
    avatar.look_at = export.look_at.map(|l| io::LookAt {
        kind: l.kind,
        offset_from_head_bone: l.offset_from_head_bone,
        horizontal_inner: l.horizontal_inner,
        horizontal_outer: l.horizontal_outer,
        vertical_down: l.vertical_down,
        vertical_up: l.vertical_up,
    });
    avatar.first_person = r.first_person.as_ref().map(|annotations| FirstPerson {
        annotations: annotations.iter().map(|a| MeshAnnotation { node: NodeIx(a.node), flag: a.flag.into() }).collect(),
    });
    avatar.spring_bones = springs(&r.springs);
    avatar.constraints = body
        .constraints
        .constraints
        .iter()
        .filter_map(|c| {
            let node = node_of_bone.get(c.bone.index()).copied().flatten()?;
            let source = node_of_bone.get(c.source.index()).copied().flatten()?;
            Some(io::NodeConstraint { node, kind: constraint_kind(c.kind, source), weight: c.weight })
        })
        .collect();
    scene.avatar = Some(avatar);
    Ok(scene)
}

/// The node each bone came from, by bone index.
fn node_of_bone(r: &Retained, bones: usize) -> Vec<Option<NodeIx>> {
    let mut out = vec![None; bones];
    for (n, node) in r.nodes.iter().enumerate() {
        if let (Some(bone), Ok(n)) = (node.bone, u32::try_from(n))
            && let Some(slot) = out.get_mut(bone.index())
        {
            *slot = Some(NodeIx(n));
        }
    }
    out
}

/// The new index of each retained mesh: meshes no node draws are left out, and the rest close up.
fn kept_meshes(r: &Retained) -> Vec<Option<u32>> {
    let mut next = 0u32;
    r.meshes
        .iter()
        .map(|m| {
            (!m.instances.is_empty()).then(|| {
                next += 1;
                next - 1
            })
        })
        .collect()
}

/// The file's nodes: bones at their rest (the T-pose, no scale), every other node as the file had it.
fn nodes(body: &BodyView<'_>, r: &Retained, mesh_index: &[Option<u32>]) -> Vec<Node> {
    r.nodes
        .iter()
        .map(|n| {
            let mut node = Node::new(n.name.clone());
            node.transform = match n.bone.and_then(|b| body.skeleton.get(b)) {
                Some(bone) => io::Transform {
                    translation: bone.rest.translation,
                    rotation: bone.rest.rotation,
                    scale: omuzaki_core::glam::Vec3::ONE,
                },
                None => io::Transform {
                    translation: n.transform.translation,
                    rotation: n.transform.rotation,
                    scale: n.transform.scale,
                },
            };
            node.children = n.children.iter().map(|&c| NodeIx(c)).collect();
            node.mesh = n.mesh.and_then(|m| mesh_index.get(usize::try_from(m).ok()?).copied().flatten()).map(MeshIx);
            node.skin = n.skin.map(SkinIx);
            node.passthrough = n.passthrough.clone();
            node
        })
        .collect()
}

/// Each source mesh a node draws, once: its primitives from the runtime meshes of its first instance (every instance
/// holds the same geometry), with the one-joint skin of a rigid mesh taken off again, its morphs made dense, and its
/// target names.
fn meshes(body: &BodyView<'_>, r: &Retained) -> Vec<io::Mesh> {
    let skinned =
        |node: u32| r.nodes.get(usize::try_from(node).unwrap_or(usize::MAX)).is_some_and(|n| n.skin.is_some());
    r.meshes
        .iter()
        .filter_map(|source| {
            let instance = source.instances.first()?;
            let mut mesh = io::Mesh::new(source.name.clone());
            mesh.morph_target_names.clone_from(&source.morph_target_names);
            for (primitive, &runtime) in source.primitives.iter().zip(&instance.meshes) {
                let Some(view) = body.meshes.get(usize::from(runtime)) else {
                    debug_assert!(false, "a validated avatar's instances name meshes it has");
                    continue;
                };
                let mut geometry = view.mesh.geometry.clone();
                if !skinned(instance.node) {
                    geometry.joints.clear();
                    geometry.weights.clear();
                }
                let count = geometry.positions.len();
                let mut out = io::Primitive::new(geometry);
                out.material = primitive.material.map(MaterialIx);
                out.morph_targets = view.morphs.map(|m| dense(m, count)).unwrap_or_default();
                mesh.primitives.push(out);
            }
            Some(mesh)
        })
        .collect()
}

/// The dense targets of a sparse set over `count` vertices: a zero delta where a target does not move a vertex.
fn dense(morphs: &MorphSet, count: usize) -> Vec<io::MorphTarget> {
    morphs
        .targets()
        .iter()
        .map(|t| {
            let mut positions = vec![omuzaki_core::glam::Vec3::ZERO; count];
            let mut normals =
                if t.normals.is_empty() { Vec::new() } else { vec![omuzaki_core::glam::Vec3::ZERO; count] };
            for (k, &i) in t.indices.iter().enumerate() {
                let i = usize::try_from(i).unwrap_or(usize::MAX);
                if let (Some(slot), Some(&d)) = (positions.get_mut(i), t.positions.get(k)) {
                    *slot = d;
                }
                if let (Some(slot), Some(&d)) = (normals.get_mut(i), t.normals.get(k)) {
                    *slot = d;
                }
            }
            let mut target = io::MorphTarget::new(positions);
            target.normals = normals;
            target
        })
        .collect()
}

/// The file's skins: joints by node and the skeleton root as the file had them, and each skin's inverse bind matrices
/// from a mesh it deforms (the file's own matrices, with any scale the import baked folded in), or the file's when no
/// mesh uses it.
fn skins(body: &BodyView<'_>, r: &Retained) -> Vec<io::Skin> {
    r.skins
        .iter()
        .enumerate()
        .map(|(s, skin)| {
            let skin_ix = u32::try_from(s).unwrap_or(u32::MAX);
            let user = r.meshes.iter().flat_map(|m| &m.instances).find(|i| {
                r.nodes.get(usize::try_from(i.node).unwrap_or(usize::MAX)).is_some_and(|n| n.skin == Some(skin_ix))
            });
            let runtime = user
                .and_then(|i| i.meshes.first())
                .and_then(|&m| body.meshes.get(usize::from(m)))
                .and_then(|v| v.inverse_bind)
                .filter(|ib| ib.len() == skin.joints.len());
            let inverse_bind = runtime.map_or_else(|| skin.inverse_bind.clone(), <[_]>::to_vec);
            let mut out =
                io::Skin::new(skin.name.clone(), skin.joints.iter().map(|&j| NodeIx(j)).collect(), inverse_bind);
            out.skeleton = skin.skeleton.map(NodeIx);
            out
        })
        .collect()
}

/// The expressions: each with its binds and overrides, a morph bind once per node and target (the runtime binds every
/// primitive of the node's instance).
fn expressions(face: &AvatarFace<'_>, r: &Retained) -> BTreeMap<io::ExpressionName, io::Expression> {
    face.expressions.iter().map(|e| (e.name.clone(), expression(e, r))).collect()
}

/// One expression in the file's form.
fn expression(e: &AvatarExpression, r: &Retained) -> io::Expression {
    let material = |name: &omuzaki_char_api::MaterialName| {
        r.materials.iter().position(|m| &m.name == name).and_then(|i| u32::try_from(i).ok()).map(MaterialIx)
    };
    let mut out = io::Expression::new();
    for bind in &e.morph {
        let node = bind.node.or_else(|| drawing_node(r, bind.mesh));
        let Some(node) = node.map(NodeIx) else { continue };
        let index = u32::from(bind.target);
        if !out.morph_binds.iter().any(|b| b.node == node && b.index == index) {
            out.morph_binds.push(io::MorphBind { node, index, weight: bind.weight });
        }
    }
    out.color_binds = e
        .colour
        .iter()
        .filter_map(|b| Some(io::ColorBind { material: material(&b.material)?, kind: b.kind, target: b.target }))
        .collect();
    out.uv_binds =
        e.uv.iter()
            .filter_map(|b| Some(io::UvBind { material: material(&b.material)?, scale: b.scale, offset: b.offset }))
            .collect();
    out.is_binary = e.binary;
    out.override_blink = e.overrides.blink;
    out.override_look_at = e.overrides.look_at;
    out.override_mouth = e.overrides.mouth;
    out
}

/// The node whose instance holds runtime mesh `mesh`.
fn drawing_node(r: &Retained, mesh: u16) -> Option<u32> {
    r.meshes.iter().flat_map(|m| &m.instances).find(|i| i.meshes.contains(&mesh)).map(|i| i.node)
}

/// The file's spring records, back in the file's form.
fn springs(r: &RetainedSprings) -> io::SpringBones {
    io::SpringBones {
        colliders: r
            .colliders
            .iter()
            .filter_map(|c| Some(io::Collider { node: NodeIx(c.node), shape: io_shape(c.shape, 1.0)? }))
            .collect(),
        collider_groups: r
            .groups
            .iter()
            .map(|g| io::ColliderGroup { name: g.name.clone(), colliders: g.colliders.clone() })
            .collect(),
        chains: r
            .chains
            .iter()
            .map(|c| io::SpringChain {
                name: c.name.clone(),
                joints: c
                    .joints
                    .iter()
                    .map(|j| io::SpringJoint {
                        node: NodeIx(j.node),
                        hit_radius: j.params.hit_radius,
                        stiffness: j.params.stiffness,
                        gravity_power: j.params.gravity_power,
                        gravity_dir: j.params.gravity_dir,
                        drag_force: j.params.drag_force,
                        limit: j.limit.and_then(io_limit),
                    })
                    .collect(),
                collider_groups: c.groups.clone(),
                center: c.center.map(NodeIx),
            })
            .collect(),
    }
}

/// A joint limit in the file's form; `None` for a kind the file cannot carry yet.
fn io_limit(limit: JointLimit) -> Option<io::JointLimit> {
    Some(match limit {
        JointLimit::Cone { angle, rotation } => io::JointLimit::Cone { angle, rotation },
        JointLimit::Hinge { angle, rotation } => io::JointLimit::Hinge { angle, rotation },
        JointLimit::Spherical { pitch, yaw, rotation } => io::JointLimit::Spherical { pitch, yaw, rotation },
        _ => return None,
    })
}

/// A constraint kind in the file's form, following `source`.
fn constraint_kind(kind: ConstraintKind, source: NodeIx) -> io::ConstraintKind {
    match kind {
        ConstraintKind::Roll { axis } => io::ConstraintKind::Roll {
            source,
            axis: match axis {
                Axis::X => io::RollAxis::X,
                Axis::Y => io::RollAxis::Y,
                Axis::Z => io::RollAxis::Z,
            },
        },
        ConstraintKind::Aim { axis } => io::ConstraintKind::Aim {
            source,
            axis: match axis {
                SignedAxis::PositiveX => io::AimAxis::PositiveX,
                SignedAxis::NegativeX => io::AimAxis::NegativeX,
                SignedAxis::PositiveY => io::AimAxis::PositiveY,
                SignedAxis::NegativeY => io::AimAxis::NegativeY,
                SignedAxis::PositiveZ => io::AimAxis::PositiveZ,
                SignedAxis::NegativeZ => io::AimAxis::NegativeZ,
            },
        },
        ConstraintKind::Rotation => io::ConstraintKind::Rotation { source },
    }
}
