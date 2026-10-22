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
//! "Every field" is the scene as the preparation left it: an avatar whose file needed no cleanup re-exports as the file
//! was, while welded vertices, merged primitives and dropped unused materials, textures and images (reported at import)
//! export as cleaned, and a node constraint the solver refused is not written. A mesh no node draws is written from the
//! geometry it kept. A scale the import baked out of the skeleton is baked into what hangs from it instead, so
//! everything sits where the file had it: a rigid mesh two nodes draw at different scales becomes one mesh per scale,
//! and a collider on a bone that lost its scale is carried into the bone's unscaled frame.

use std::collections::BTreeMap;

use omuzaki_anim_api::spring::JointLimit;
use omuzaki_anim_api::{Axis, ConstraintKind, SignedAxis};
use omuzaki_avatar_api::avatar::AvatarFace;
use omuzaki_avatar_api::body::{BodyView, MeshView};
use omuzaki_avatar_api::def::AvatarDef;
use omuzaki_avatar_api::expression::AvatarExpression;
use omuzaki_avatar_api::retained::{Retained, RetainedInstance, RetainedMesh};
use omuzaki_char_api::{JointRef, MaterialName, PartId, TextureName};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::{
    self as io, Avatar, Blob, EncodedImage, FirstPerson, IoError, IoScene, MaterialIx, MeshAnnotation, MeshIx, Node,
    NodeIx, Report, SkinIx,
};
use omuzaki_render_api::MorphSet;
use omuzaki_store_api::ContentHash;

use super::springs::shape as io_shape;
use crate::convert::bones::to_file;
use crate::convert::material::{SceneTexture, export_material};
use crate::convert::texture::{encode_png, sampler_to_io};
use crate::prepare::avatar::apply_material_edit;
use crate::prepare::avatar::physics::in_bone_frame;

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
    let meshes = meshes(body, r)?;
    scene.nodes = nodes(body, r, &meshes.of_node);
    scene.roots = r.roots.iter().map(|&n| NodeIx(n)).collect();
    scene.meshes = meshes.list;
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
    avatar.expressions = expressions(body, &face, r);
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
    avatar.spring_bones = springs(r);
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
    add_attachments(&mut scene, body, &node_of_bone)?;
    Ok(scene)
}

/// The attachments' meshes ([`MeshView::part`], built by the kit onto the avatar), after everything the file had. A
/// mesh that follows one bone becomes a node under that bone's node, its vertices in the bone's frame at rest, as a
/// prop parented to a bone is in any 3D tool; one spread over several bones is a skinned node at the root, with the
/// inverse binds it runs with. Their materials and textures are written from the kit's specs, as a built character's
/// are; the notes that conversion makes (an approximated shading program) were the kit's to show, not repeated here.
///
/// # Errors
/// [`IoError::Internal`] for a mesh that follows a bone the file has no node for, or a material or texture that does
/// not convert (an attached avatar that validated has neither).
fn add_attachments(scene: &mut IoScene, body: &BodyView<'_>, node_of_bone: &[Option<NodeIx>]) -> Result<(), IoError> {
    let attached: Vec<MeshView<'_>> =
        (0..body.meshes.len()).filter_map(|i| body.meshes.get(i)).filter(|m| m.part.is_some()).collect();
    if attached.is_empty() {
        return Ok(());
    }
    let mut notes = Report::new();
    let internal = |what: String| IoError::Internal(what);
    let mut textures: BTreeMap<&TextureName, SceneTexture> = BTreeMap::new();
    let mut materials: BTreeMap<&MaterialName, MaterialIx> = BTreeMap::new();
    for view in &attached {
        let name = &view.mesh.material;
        if materials.contains_key(name) {
            continue;
        }
        let spec =
            body.materials.get(name).ok_or_else(|| internal(format!("attached material '{name}' is missing")))?;
        if let Some(texture) = spec.base_texture.as_ref().filter(|t| !textures.contains_key(*t)) {
            let data = body
                .textures
                .get(texture)
                .ok_or_else(|| internal(format!("attached texture '{texture}' is missing")))?;
            let uri = format!("images/{}", scene.images.len());
            let image = encode_png(texture.as_str(), data, &uri, &mut notes).map_err(|e| internal(e.to_string()))?;
            let image = scene.add_image(image);
            let tex = scene.add_texture(io::Texture { image, sampler: sampler_to_io(&data.desc) });
            textures.insert(texture, SceneTexture { tex: io::TexRef::of(tex), colour_space: data.desc.colour_space });
        }
        let at = format!("materials/{}", scene.materials.len());
        let material = export_material(name.as_str(), spec, &|t| textures.get(t).copied(), &at, &mut notes)
            .map_err(|e| internal(e.to_string()))?;
        materials.insert(name, scene.add_material(material));
    }
    let mut taken = BTreeMap::<String, usize>::new();
    for view in attached {
        let part = view.part.map(PartId::as_str).unwrap_or_default();
        let seen = taken.entry(part.to_owned()).or_default();
        *seen += 1;
        let name = if *seen == 1 { part.to_owned() } else { format!("{part}.{seen}") };
        let joints = view
            .mesh
            .joint_bones
            .iter()
            .map(|joint| {
                let bone = match joint {
                    JointRef::Humanoid(bone) => body.skeleton.humanoid_bone(*bone),
                    JointRef::Named(bone) => body.skeleton.find(bone.as_str()),
                };
                bone.and_then(|b| node_of_bone.get(b.index()).copied().flatten()).ok_or_else(|| {
                    internal(format!("attachment '{name}' follows {joint:?}, which the file has no node for"))
                })
            })
            .collect::<Result<Vec<NodeIx>, _>>()?;
        let inverse_bind = view.inverse_bind.unwrap_or_default();
        let mut geometry = view.mesh.geometry.clone();
        let parent = match (joints.as_slice(), inverse_bind) {
            ([bone], [to_bone]) => {
                for p in &mut geometry.positions {
                    *p = to_bone.transform_point3(*p);
                }
                for n in &mut geometry.normals {
                    *n = to_bone.transform_vector3(*n).normalize_or_zero();
                }
                geometry.joints.clear();
                geometry.weights.clear();
                Some(*bone)
            }
            _ => None,
        };
        let material = materials.get(&view.mesh.material).copied();
        let mut primitive = io::Primitive::new(geometry);
        primitive.material = material;
        let mut mesh = io::Mesh::new(name.clone());
        mesh.primitives.push(primitive);
        let mut node = Node::new(name.clone());
        node.mesh = Some(scene.add_mesh(mesh));
        if parent.is_none() && !joints.is_empty() {
            node.skin = Some(scene.add_skin(io::Skin::new(name, joints, inverse_bind.to_vec())));
        }
        let at = scene.add_node(node);
        match parent.and_then(|p| scene.nodes.get_mut(p.index())) {
            Some(bone) => bone.children.push(at),
            None => scene.roots.push(at),
        }
    }
    Ok(())
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

/// For each node whose world placement in the file had a scale, the matrix that carries its scaled frame onto its
/// unscaled one (`W'⁻¹ · W`, as the import's `scale_fix`); `None` for a node with no scale on it or above it.
fn scale_folds(r: &Retained) -> Vec<Option<Mat4>> {
    let mut folds = vec![None; r.nodes.len()];
    let mut stack: Vec<(usize, Mat4, bool)> =
        r.roots.iter().filter_map(|&n| Some((usize::try_from(n).ok()?, Mat4::IDENTITY, true))).collect();
    let mut seen = vec![false; r.nodes.len()];
    while let Some((i, up, unit_above)) = stack.pop() {
        let Some(node) = r.nodes.get(i).filter(|_| !seen[i]) else { continue };
        seen[i] = true;
        let t = node.transform;
        let world = up * Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        let unit = unit_above && t.scale == Vec3::ONE;
        if !unit {
            let (_, rotation, translation) = world.to_scale_rotation_translation();
            folds[i] = Some(Mat4::from_rotation_translation(rotation, translation).inverse() * world);
        }
        for &c in node.children.iter().rev() {
            stack.extend(usize::try_from(c).ok().map(|c| (c, world, unit)));
        }
    }
    folds
}

/// The file's nodes: bones at their rest (the T-pose, no scale), every other node as the file had it, except that a node
/// under a bone whose scale the import baked keeps its place in the world: its transform takes the scale its parent no
/// longer carries.
fn nodes(body: &BodyView<'_>, r: &Retained, mesh_of: &BTreeMap<u32, u32>) -> Vec<Node> {
    let folds = scale_folds(r);
    let mut parent = vec![None; r.nodes.len()];
    for (i, n) in r.nodes.iter().enumerate() {
        for &c in &n.children {
            if let Some(slot) = parent.get_mut(usize::try_from(c).unwrap_or(usize::MAX)) {
                *slot = Some(i);
            }
        }
    }
    r.nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let mut node = Node::new(n.name.clone());
            node.transform = if let Some(bone) = n.bone.and_then(|b| body.skeleton.get(b)) {
                io::Transform { translation: bone.rest.translation, rotation: bone.rest.rotation, scale: Vec3::ONE }
            } else {
                let t = n.transform;
                let fold =
                    parent[i].filter(|&p| r.nodes[p].bone.is_some()).and_then(|p| folds.get(p).copied().flatten());
                if let Some(fold) = fold {
                    let local = fold * Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
                    let (scale, rotation, translation) = local.to_scale_rotation_translation();
                    io::Transform { translation, rotation, scale }
                } else {
                    io::Transform { translation: t.translation, rotation: t.rotation, scale: t.scale }
                }
            };
            node.children = n.children.iter().map(|&c| NodeIx(c)).collect();
            node.mesh = u32::try_from(i).ok().and_then(|i| mesh_of.get(&i)).map(|&m| MeshIx(m));
            node.skin = n.skin.map(SkinIx);
            node.passthrough = n.passthrough.clone();
            node
        })
        .collect()
}

/// The meshes an export writes, and the mesh each drawing node draws.
struct Meshes {
    /// The meshes, in the file's order, then the copies (see [`meshes`]).
    list: Vec<io::Mesh>,
    /// The index in `list` of the mesh each node draws, by node.
    of_node: BTreeMap<u32, u32>,
}

/// Each source mesh a node draws, with its primitives from the runtime meshes, its morphs made dense, and its target
/// names. Every drawing node needs the mesh as it sees it: a skinned node as the runtime holds it, a node that draws it
/// rigidly with the scale its one-joint skin carried baked into the vertices (that skin's joints were made up at
/// import, so they come off again). Nodes that need the same are one mesh, which keeps the file's place in the list,
/// so an unscaled file exports its meshes as they were. A rigid node that needs another scale gets a copy, written
/// after every file mesh: two rigid nodes drawing one mesh at 2× and 3× both keep their own size.
///
/// # Errors
/// [`IoError::Internal`] when an instance names a runtime mesh the body does not have (a validated avatar never does).
fn meshes(body: &BodyView<'_>, r: &Retained) -> Result<Meshes, IoError> {
    let skinned =
        |node: u32| r.nodes.get(usize::try_from(node).unwrap_or(usize::MAX)).is_some_and(|n| n.skin.is_some());
    let mut out = Meshes { list: Vec::with_capacity(r.meshes.len()), of_node: BTreeMap::new() };
    let mut copies: Vec<(io::Mesh, Vec<u32>)> = Vec::new();
    for source in &r.meshes {
        if source.instances.is_empty() {
            out.list.push(undrawn(source)?);
            continue;
        }
        // The instances grouped by the matrix their geometry needs (identity for a skinned node), skinned groups first,
        // each group with whether it keeps the skin's joints.
        let mut groups: Vec<(Mat4, bool, Vec<&RetainedInstance>)> = Vec::new();
        for instance in &source.instances {
            let rigid = !skinned(instance.node);
            let fold = if rigid { rigid_fold(body, instance) } else { Mat4::IDENTITY };
            match groups.iter_mut().find(|(m, ..)| *m == fold) {
                Some((_, keeps_joints, members)) => {
                    *keeps_joints |= !rigid;
                    members.push(instance);
                }
                None => groups.push((fold, !rigid, vec![instance])),
            }
        }
        groups.sort_by_key(|(_, keeps_joints, _)| !keeps_joints);
        let mut groups = groups.into_iter();
        let Some((fold, joints, members)) = groups.next() else { continue };
        let index = u32::try_from(out.list.len()).unwrap_or(u32::MAX);
        out.list.push(drawn(body, source, representative(joints, &members, &skinned), fold, joints)?);
        out.of_node.extend(members.iter().map(|i| (i.node, index)));
        for (fold, joints, members) in groups {
            let mesh = drawn(body, source, representative(joints, &members, &skinned), fold, joints)?;
            copies.push((mesh, members.iter().map(|i| i.node).collect()));
        }
    }
    for (mesh, nodes) in copies {
        let index = u32::try_from(out.list.len()).unwrap_or(u32::MAX);
        out.list.push(mesh);
        out.of_node.extend(nodes.into_iter().map(|n| (n, index)));
    }
    Ok(out)
}

/// A mesh no node draws, from the geometry its primitives kept.
///
/// # Errors
/// [`IoError::Internal`] for a primitive without geometry or with an attribute the scene cannot name (a validated
/// avatar has neither).
fn undrawn(source: &RetainedMesh) -> Result<io::Mesh, IoError> {
    let mut mesh = io::Mesh::new(source.name.clone());
    mesh.morph_target_names.clone_from(&source.morph_target_names);
    for (p, primitive) in source.primitives.iter().enumerate() {
        let geometry = primitive.geometry.as_ref().ok_or_else(|| {
            IoError::Internal(format!("primitive {p} of mesh {:?}, which no node draws, kept no geometry", source.name))
        })?;
        let built = geometry
            .to_primitive(primitive.material.map(MaterialIx))
            .map_err(|e| IoError::Internal(format!("mesh {:?}, primitive {p}: {e}", source.name)))?;
        mesh.primitives.push(built);
    }
    Ok(mesh)
}

/// The instance a group's mesh is read from: a skinned member when the group keeps the joints, since a rigid member's
/// skin was made up at import, whichever node comes first; otherwise the first.
fn representative<'a>(
    joints: bool,
    members: &[&'a RetainedInstance],
    skinned: &dyn Fn(u32) -> bool,
) -> &'a RetainedInstance {
    members.iter().copied().find(|i| !joints || skinned(i.node)).unwrap_or(members[0])
}

/// The matrix a rigid instance's made-up skin carried: the scale its node had in the file (identity for none).
fn rigid_fold(body: &BodyView<'_>, instance: &RetainedInstance) -> Mat4 {
    instance
        .meshes
        .first()
        .and_then(|&m| body.meshes.get(usize::from(m)))
        .and_then(|v| v.inverse_bind)
        .and_then(|m| m.first())
        .copied()
        .unwrap_or(Mat4::IDENTITY)
}

/// `source` as the nodes of one group see it, from the runtime meshes of `instance`: `fold` baked into the vertices,
/// and the skin's joints kept only when a skinned node draws it.
fn drawn(
    body: &BodyView<'_>,
    source: &RetainedMesh,
    instance: &RetainedInstance,
    fold: Mat4,
    joints: bool,
) -> Result<io::Mesh, IoError> {
    let mut mesh = io::Mesh::new(source.name.clone());
    mesh.morph_target_names.clone_from(&source.morph_target_names);
    for (primitive, &runtime) in source.primitives.iter().zip(&instance.meshes) {
        let view = body.meshes.get(usize::from(runtime)).ok_or_else(|| {
            IoError::Internal(format!(
                "source mesh {:?} names runtime mesh {runtime}, which the body lacks",
                source.name
            ))
        })?;
        let mut geometry = view.mesh.geometry.clone();
        let count = geometry.positions.len();
        let mut targets = view.morphs.map(|m| dense(m, count)).unwrap_or_default();
        if !joints {
            geometry.joints.clear();
            geometry.weights.clear();
        }
        if fold != Mat4::IDENTITY {
            bake(fold, &mut geometry, &mut targets);
        }
        let mut out = io::Primitive::new(geometry);
        out.material = primitive.material.map(MaterialIx);
        out.morph_targets = targets;
        mesh.primitives.push(out);
    }
    Ok(mesh)
}

/// Moves a rigid mesh's vertices, normals and morph deltas by `fold`, the matrix its one-joint skin carried. A normal
/// turns by the inverse transpose and is normalised again; its morph deltas turn by the same matrix and are divided by
/// the same length, so a morphed normal (the normal plus its delta, which the renderer normalises) keeps the direction
/// the file gave it.
fn bake(fold: Mat4, geometry: &mut omuzaki_io_api::MeshData, targets: &mut [io::MorphTarget]) {
    let normal = fold.inverse().transpose();
    geometry.positions.iter_mut().for_each(|p| *p = fold.transform_point3(*p));
    // Each vertex's normalisation factor, shared by its normal and its normal deltas.
    let lengths: Vec<f32> = geometry
        .normals
        .iter()
        .map(|n| normal.transform_vector3(*n).length())
        .map(|l| if l > f32::EPSILON { l } else { 1.0 })
        .collect();
    for (n, length) in geometry.normals.iter_mut().zip(&lengths) {
        *n = normal.transform_vector3(*n) / *length;
    }
    for target in targets {
        target.positions.iter_mut().for_each(|d| *d = fold.transform_vector3(*d));
        for (v, d) in target.normals.iter_mut().enumerate() {
            *d = normal.transform_vector3(*d) / lengths.get(v).copied().unwrap_or(1.0);
        }
    }
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

/// The expressions: each with its binds and overrides. The runtime binds a file's morph bind once per primitive of the
/// node's instance that has the target, so each file bind is the replica on the first such primitive, and a file that
/// binds one target twice (at different weights, which add up) keeps both.
fn expressions(
    body: &BodyView<'_>,
    face: &AvatarFace<'_>,
    r: &Retained,
) -> BTreeMap<io::ExpressionName, io::Expression> {
    face.expressions.iter().map(|e| (e.name.clone(), expression(body, e, r))).collect()
}

/// One expression in the file's form.
fn expression(body: &BodyView<'_>, e: &AvatarExpression, r: &Retained) -> io::Expression {
    let material = |name: &omuzaki_char_api::MaterialName| {
        r.materials.iter().position(|m| &m.name == name).and_then(|i| u32::try_from(i).ok()).map(MaterialIx)
    };
    let mut out = io::Expression::new();
    for bind in &e.morph {
        let Some(node) = bind.node.or_else(|| drawing_node(r, bind.mesh)) else { continue };
        if first_replica(body, r, node, bind.target) == Some(bind.mesh) {
            out.morph_binds.push(io::MorphBind {
                node: NodeIx(node),
                index: u32::from(bind.target),
                weight: bind.weight,
            });
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

/// The runtime mesh of node `node`'s instance that a file bind of `target` is replicated on first: its first primitive
/// whose runtime mesh has that target.
fn first_replica(body: &BodyView<'_>, r: &Retained, node: u32, target: u16) -> Option<u16> {
    let instance = r.meshes.iter().find_map(|m| m.instance(node))?;
    instance.meshes.iter().copied().find(|&m| {
        body.meshes
            .get(usize::from(m))
            .and_then(|v| v.morphs)
            .is_some_and(|morphs| usize::from(target) < morphs.targets().len())
    })
}

/// The node whose instance holds runtime mesh `mesh`.
fn drawing_node(r: &Retained, mesh: u16) -> Option<u32> {
    r.meshes.iter().flat_map(|m| &m.instances).find(|i| i.meshes.contains(&mesh)).map(|i| i.node)
}

/// The file's spring records, back in the file's form. A collider on a bone whose scale the import baked out is
/// carried into the bone's unscaled frame, which is the frame the exported node has (bones export without scale), so
/// it keeps its place and size in the world.
fn springs(retained: &Retained) -> io::SpringBones {
    let r = &retained.springs;
    let folds = scale_folds(retained);
    let fold_of = |node: u32| {
        let i = usize::try_from(node).ok()?;
        retained.nodes.get(i).filter(|n| n.bone.is_some())?;
        folds.get(i).copied().flatten()
    };
    io::SpringBones {
        // Groups name colliders by index, so every collider is written; one whose shape the file cannot carry yet
        // becomes an empty sphere, which touches nothing.
        colliders: r
            .colliders
            .iter()
            .map(|c| io::Collider {
                node: NodeIx(c.node),
                shape: io_shape(fold_of(c.node).map_or(c.shape, |fold| in_bone_frame(c.shape, fold)), 1.0)
                    .unwrap_or(io::ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.0 }),
            })
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
