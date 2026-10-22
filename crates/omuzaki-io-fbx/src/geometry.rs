//! Meshes, skins and blend shapes: each ufbx mesh as one `Mesh` with a primitive per material, the skin that deforms it
//! and its blend shapes as morph targets, all in canonical space ([`Basis`]).
//!
//! **Vertices.** FBX stores attributes per polygon corner, each with its own index; a vertex of the result is one
//! distinct combination of control point, normal, UV and colour, so a seam splits a control point and nothing else does.
//! Polygons are triangulated by ufbx (ear clipping on the polygon's plane). The UV origin moves from FBX's bottom left to
//! glTF's top left (`v → 1 − v`), and a file of the other handedness gets its corners reversed so the front stays front.
//!
//! **Skins.** The first skin deformer of a mesh is its skin; its clusters are the joints, and a cluster's
//! `geometry_to_bone` is exactly glTF's inverse bind matrix (mesh space to joint space at bind time). A vertex keeps its
//! four heaviest weights, renormalised. A vertex no cluster weighs is bound to the first joint (FBX would leave it where
//! the mesh node puts it; glTF has no such vertex) and counted in a warning.
//!
//! **Faces seen from behind.** FBX records no culling a renderer can rely on (Maya writes `CullingOff` on every model,
//! and tools leave the choice to the engine), so a material is made double-sided exactly when a primitive that uses it
//! is an open surface (an edge with one triangle: hair cards, a skirt, lashes), whose back can be seen. A closed body
//! stays single-sided, since its back faces are hidden anyway.
//!
//! **Blend shapes.** Each blend channel is one morph target, named after the channel, at its full-weight shape;
//! in-between shapes are not kept (warning). Offsets are per control point, so a split vertex shares its point's offset.

use std::collections::{BTreeSet, HashMap};

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_io_api::{
    Finding, ImportCtx, IoError, IoScene, MaterialIx, Mesh, MeshData, MeshIx, MorphTarget, NodeIx, Primitive, Report,
    Skin, SkinIx,
};
use ufbx::SceneRoot;

use crate::load::fraction;
use crate::space::Basis;

/// The most joints a skin may have: `JOINTS_0` is `u16`.
const MAX_JOINTS: usize = u16::MAX as usize;

/// What the meshes became, for the nodes that draw them.
pub(crate) struct Built {
    /// By ufbx mesh `typed_id`: the mesh and the skin that deforms it.
    pub(crate) meshes: Vec<Option<(MeshIx, Option<SkinIx>)>>,
}

/// Converts every mesh some node draws, and attaches each to its nodes.
///
/// # Errors
/// [`IoError::Limit`] over `Limits::max_vertices` (counted over the whole scene), [`IoError::Cancelled`],
/// [`IoError::Malformed`] for a mesh whose numbers aren't finite or a skin with more joints than `u16` holds.
pub(crate) fn convert(
    loaded: &SceneRoot,
    basis: &Basis,
    nodes: &[Option<NodeIx>],
    materials: &[MaterialIx],
    scene: &mut IoScene,
    report: &mut Report,
    ctx: &ImportCtx<'_>,
) -> Result<Built, IoError> {
    let mut built = Built { meshes: vec![None; loaded.meshes.len()] };
    let mut budget =
        Budget { left: u64::from(ctx.options.limits.max_vertices), limit: ctx.options.limits.max_vertices };
    let total = loaded.meshes.len() as u64;
    let mut open = BTreeSet::new();
    for (k, mesh) in loaded.meshes.iter().enumerate() {
        ctx.progress.checkpoint("fbx.meshes", fraction(k as u64, total))?;
        // A mesh no node draws is not part of the model.
        if mesh.element.instances.is_empty() {
            continue;
        }
        let mut converter =
            Converter { basis, nodes, materials, report: &mut *report, budget: &mut budget, open: &mut open };
        let (made, skin) = converter.mesh(mesh)?;
        if made.primitives.is_empty() {
            continue;
        }
        let mesh_ix = scene.add_mesh(made);
        let skin_ix = skin.map(|s| scene.add_skin(s));
        if let Some(slot) = built.meshes.get_mut(mesh.element.typed_id as usize) {
            *slot = Some((mesh_ix, skin_ix));
        }
        for instance in mesh.element.instances.iter() {
            if let Some(node) = nodes.get(instance.element.typed_id as usize).copied().flatten() {
                let n = &mut scene.nodes[node.index()];
                n.mesh = Some(mesh_ix);
                n.skin = skin_ix;
            }
        }
    }
    for material in open {
        if let Some(m) = scene.materials.get_mut(material.index()) {
            m.double_sided = true;
        }
    }
    ctx.progress.checkpoint("fbx.meshes", 1.0)?;
    Ok(built)
}

/// The vertices the scene may still have.
struct Budget {
    left: u64,
    limit: u32,
}

impl Budget {
    fn take(&mut self, n: usize) -> Result<(), IoError> {
        let n = n as u64;
        if n > self.left {
            let limit = u64::from(self.limit);
            return Err(IoError::Limit { what: "vertex count", limit, got: limit - self.left + n });
        }
        self.left -= n;
        Ok(())
    }
}

/// The per-control-point skinning of one mesh.
struct Skinning {
    skin: Skin,
    /// Per control point: four joints and weights.
    points: Vec<([u16; 4], [f32; 4])>,
}

struct Converter<'a, 'r> {
    basis: &'a Basis,
    nodes: &'a [Option<NodeIx>],
    materials: &'a [MaterialIx],
    report: &'r mut Report,
    budget: &'r mut Budget,
    /// The materials an open surface uses.
    open: &'r mut BTreeSet<MaterialIx>,
}

/// One vertex's attributes, as bits, so equal corners merge.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    point: u32,
    normal: [u32; 3],
    uv: [u32; 2],
    colour: [u32; 4],
}

impl Converter<'_, '_> {
    fn mesh(&mut self, mesh: &ufbx::Mesh) -> Result<(Mesh, Option<Skin>), IoError> {
        let name = mesh.element.name.to_string();
        let points = mesh.vertices.len();
        let skinning = self.skinning(mesh, &name)?;
        let (target_names, targets) = self.blend_shapes(mesh, &name);
        let mut out = Mesh::new(name.clone());
        out.morph_target_names = target_names;
        let parts: Vec<(Option<u32>, Vec<u32>)> = if mesh.material_parts.is_empty() {
            vec![(None, (0..u32::try_from(mesh.faces.len()).unwrap_or(u32::MAX)).collect())]
        } else {
            mesh.material_parts.iter().map(|p| (Some(p.index), p.face_indices.to_vec())).collect()
        };
        let mut corners = vec![0_u32; mesh.max_face_triangles.saturating_mul(3)];
        for (material, faces) in parts {
            let mut data = MeshData::default();
            let mut point_of: Vec<u32> = Vec::new();
            let mut seen: HashMap<Key, u32> = HashMap::new();
            for &f in &faces {
                let Some(&face) = mesh.faces.get(f as usize) else { continue };
                if face.num_indices < 3 {
                    continue;
                }
                let wanted = (face.num_indices as usize - 2) * 3;
                if corners.len() < wanted {
                    corners.resize(wanted, 0);
                }
                let triangles = ufbx::triangulate_face(&mut corners, mesh, face) as usize;
                for tri in corners[..triangles * 3].as_chunks::<3>().0 {
                    let order = if self.basis.mirrors() { [tri[0], tri[2], tri[1]] } else { [tri[0], tri[1], tri[2]] };
                    for corner in order {
                        let index = self.vertex(mesh, corner as usize, points, &mut data, &mut point_of, &mut seen)?;
                        data.indices.push(index);
                    }
                }
            }
            if data.indices.is_empty() {
                continue;
            }
            self.budget.take(data.positions.len())?;
            if let Some(s) = &skinning {
                (data.joints, data.weights) = point_of.iter().map(|&p| s.points[p as usize]).unzip();
            }
            let mut primitive = Primitive::new(data);
            primitive.material = material
                .and_then(|m| mesh.materials.get(m as usize))
                .and_then(|m| self.materials.get(m.element.typed_id as usize))
                .copied();
            if let Some(material) = primitive.material
                && is_open(&primitive.mesh.indices, &point_of)
            {
                self.open.insert(material);
            }
            primitive.morph_targets = targets
                .iter()
                .map(|(positions, normals)| {
                    let mut t = MorphTarget::new(point_of.iter().map(|&p| positions[p as usize]).collect());
                    if let Some(normals) = normals {
                        t.normals = point_of.iter().map(|&p| normals[p as usize]).collect();
                    }
                    t
                })
                .collect();
            // ufbx makes smooth normals for a mesh without any (`generate_missing_normals`); flat ones are the fallback.
            if mesh.vertex_normal.exists || primitive.generate_flat_normals() {
                out.primitives.push(primitive);
            }
        }
        if mesh.generated_normals && !out.primitives.is_empty() {
            self.report.push(
                Finding::info(
                    "fbx.normals_generated",
                    format!("mesh '{}' has no normals; smooth ones were made", short(&name)),
                )
                .at(format!("Objects/Geometry/{}", short(&name))),
            );
        }
        Ok((out, skinning.map(|s| s.skin)))
    }

    /// The index of the vertex for `corner`, adding it if no equal one exists yet.
    fn vertex(
        &mut self,
        mesh: &ufbx::Mesh,
        corner: usize,
        points: usize,
        data: &mut MeshData,
        point_of: &mut Vec<u32>,
        seen: &mut HashMap<Key, u32>,
    ) -> Result<u32, IoError> {
        let malformed = || IoError::Malformed {
            message: format!("mesh '{}' refers to a corner or point it doesn't have", short(&mesh.element.name)),
            at: Some(format!("Objects/Geometry/{}", short(&mesh.element.name))),
        };
        let point = *mesh.vertex_indices.get(corner).ok_or_else(malformed)?;
        let position =
            *mesh.vertices.get(point as usize).filter(|_| (point as usize) < points).ok_or_else(malformed)?;
        let normal = attribute3(&mesh.vertex_normal, corner).map(|n| self.basis.direction(n).normalize_or(Vec3::Y));
        let uv = attribute2(&mesh.vertex_uv, corner).map(|uv| {
            #[allow(clippy::cast_possible_truncation)]
            let flipped = Vec2::new(uv.x as f32, (1.0 - uv.y) as f32);
            flipped
        });
        let colour = attribute4(&mesh.vertex_color, corner).map(|c| {
            #[allow(clippy::cast_possible_truncation)]
            let rgba = [c.x as f32, c.y as f32, c.z as f32, c.w as f32];
            rgba
        });
        let key = Key {
            point,
            normal: normal.map_or([0; 3], |n| n.to_array().map(f32::to_bits)),
            uv: uv.map_or([0; 2], |uv| uv.to_array().map(f32::to_bits)),
            colour: colour.map_or([0; 4], |c| c.map(f32::to_bits)),
        };
        if let Some(&index) = seen.get(&key) {
            return Ok(index);
        }
        let p = self.basis.point(position);
        if !p.is_finite() || normal.is_some_and(|n| !n.is_finite()) || uv.is_some_and(|uv| !uv.is_finite()) {
            return Err(IoError::Malformed {
                message: format!("mesh '{}' has a vertex that is not a finite number", short(&mesh.element.name)),
                at: Some(format!("Objects/Geometry/{}", short(&mesh.element.name))),
            });
        }
        let index = u32::try_from(data.positions.len()).map_err(|_| malformed())?;
        data.positions.push(p);
        data.normals.extend(normal);
        data.uvs.extend(uv);
        data.colours.extend(colour);
        point_of.push(point);
        seen.insert(key, index);
        Ok(index)
    }

    /// The mesh's skin: its clusters as joints with their bind matrices, and every control point's four weights.
    fn skinning(&mut self, mesh: &ufbx::Mesh, name: &str) -> Result<Option<Skinning>, IoError> {
        let Some(deformer) = mesh.skin_deformers.first() else { return Ok(None) };
        // A cluster with no bone, or a bone that isn't a node of ours, weighs nothing.
        let mut joint_of: Vec<Option<u16>> = Vec::with_capacity(deformer.clusters.len());
        let (mut joints, mut inverse_bind) = (Vec::new(), Vec::new());
        for cluster in deformer.clusters.iter() {
            let node =
                cluster.bone_node.as_ref().and_then(|b| self.nodes.get(b.element.typed_id as usize).copied().flatten());
            let Some(node) = node else {
                joint_of.push(None);
                continue;
            };
            if joints.len() >= MAX_JOINTS {
                return Err(IoError::Malformed {
                    message: format!("the skin of mesh '{}' has more than {MAX_JOINTS} joints", short(name)),
                    at: Some(format!("Objects/Deformer/{}", short(&deformer.element.name))),
                });
            }
            joint_of.push(u16::try_from(joints.len()).ok());
            joints.push(node);
            let bind = self.basis.conjugate(&cluster.geometry_to_bone);
            if !bind.is_finite() {
                return Err(IoError::Malformed {
                    message: format!("a bind matrix of mesh '{}' is not a finite number", short(name)),
                    at: Some(format!("Objects/Deformer/{}", short(&cluster.element.name))),
                });
            }
            inverse_bind.push(bind);
        }
        if joints.is_empty() {
            return Ok(None);
        }
        let mut unweighted = 0_usize;
        let points = (0..mesh.vertices.len())
            .map(|p| {
                let mut found: Vec<(u16, f64)> = deformer
                    .vertices
                    .get(p)
                    .map(|v| {
                        let begin = v.weight_begin as usize;
                        let end = begin.saturating_add(v.num_weights as usize).min(deformer.weights.len());
                        deformer.weights.get(begin..end).unwrap_or_default()
                    })
                    .unwrap_or_default()
                    .iter()
                    .filter(|w| w.weight.is_finite() && w.weight > 0.0)
                    .filter_map(|w| Some((joint_of.get(w.cluster_index as usize).copied().flatten()?, w.weight)))
                    .collect();
                found.sort_by(|a, b| b.1.total_cmp(&a.1));
                found.truncate(4);
                let sum: f64 = found.iter().map(|w| w.1).sum();
                let (mut j, mut w) = ([0_u16; 4], [0.0_f32; 4]);
                if sum <= 0.0 {
                    unweighted += 1;
                    w[0] = 1.0;
                    return (j, w);
                }
                for (k, (joint, weight)) in found.into_iter().enumerate() {
                    j[k] = joint;
                    #[allow(clippy::cast_possible_truncation)]
                    let share = (weight / sum) as f32;
                    w[k] = share;
                }
                (j, w)
            })
            .collect();
        if unweighted > 0 {
            let message = format!(
                "{unweighted} vertices of mesh '{}' have no skin weight and follow its first joint",
                short(name)
            );
            self.report.push(
                Finding::warning("fbx.unweighted_vertices", message).at(format!("Objects/Geometry/{}", short(name))),
            );
        }
        if mesh.skin_deformers.len() > 1 {
            let message = format!("mesh '{}' has {} skins; the first is kept", short(name), mesh.skin_deformers.len());
            self.report
                .push(Finding::warning("fbx.skins_dropped", message).at(format!("Objects/Geometry/{}", short(name))));
        }
        Ok(Some(Skinning { skin: Skin::new(deformer.element.name.to_string(), joints, inverse_bind), points }))
    }

    /// The mesh's blend channels as per-control-point offsets: names, and position and normal offsets per target.
    #[allow(clippy::type_complexity)]
    fn blend_shapes(&mut self, mesh: &ufbx::Mesh, name: &str) -> (Vec<String>, Vec<(Vec<Vec3>, Option<Vec<Vec3>>)>) {
        let points = mesh.vertices.len();
        let (mut names, mut targets) = (Vec::new(), Vec::new());
        let mut in_betweens = 0_usize;
        for deformer in mesh.blend_deformers.iter() {
            for channel in deformer.channels.iter() {
                let Some(shape) = channel.target_shape.as_ref() else { continue };
                in_betweens += channel.keyframes.len().saturating_sub(1);
                let mut positions = vec![Vec3::ZERO; points];
                let has_normals = shape.normal_offsets.len() == shape.offset_vertices.len()
                    && shape.normal_offsets.iter().any(|n| n.x != 0.0 || n.y != 0.0 || n.z != 0.0);
                let mut normals = has_normals.then(|| vec![Vec3::ZERO; points]);
                for (k, &p) in shape.offset_vertices.iter().enumerate() {
                    let (Some(slot), Some(&offset)) = (positions.get_mut(p as usize), shape.position_offsets.get(k))
                    else {
                        continue;
                    };
                    let delta = self.basis.point(offset);
                    if delta.is_finite() {
                        *slot = delta;
                    }
                    if let (Some(normals), Some(&n)) = (normals.as_mut(), shape.normal_offsets.get(k)) {
                        let d = self.basis.direction(n);
                        if d.is_finite()
                            && let Some(slot) = normals.get_mut(p as usize)
                        {
                            *slot = d;
                        }
                    }
                }
                names.push(channel.element.name.to_string());
                targets.push((positions, normals));
            }
        }
        if in_betweens > 0 {
            let message = format!(
                "mesh '{}' has {in_betweens} in-between blend shapes; only full-weight shapes are kept",
                short(name)
            );
            self.report.push(
                Finding::warning("fbx.inbetweens_dropped", message).at(format!("Objects/Geometry/{}", short(name))),
            );
        }
        (names, targets)
    }
}

/// True when the triangles have an edge only one of them uses: an open surface. Edges are between control points, so a
/// UV or normal seam (which splits vertices, not points) does not open a closed mesh.
fn is_open(indices: &[u32], point_of: &[u32]) -> bool {
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| point_of.get(i as usize).copied().unwrap_or(i));
        for (p, q) in [(a, b), (b, c), (c, a)] {
            *edges.entry((p.min(q), p.max(q))).or_insert(0) += 1;
        }
    }
    edges.values().any(|&n| n == 1)
}

/// A finding's copy of a name from the file: at most 80 characters.
fn short(name: &str) -> String {
    name.chars().take(80).collect()
}

// When an attribute exists, every corner has a value (a corner ufbx gives none gets zero), so the lengths stay equal.
fn attribute2(a: &ufbx::VertexVec2, corner: usize) -> Option<ufbx::Vec2> {
    a.exists.then(|| {
        a.indices.get(corner).and_then(|&i| a.values.get(i as usize)).copied().unwrap_or(ufbx::Vec2 { x: 0.0, y: 0.0 })
    })
}

fn attribute3(a: &ufbx::VertexVec3, corner: usize) -> Option<ufbx::Vec3> {
    let zero = ufbx::Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    a.exists.then(|| a.indices.get(corner).and_then(|&i| a.values.get(i as usize)).copied().unwrap_or(zero))
}

fn attribute4(a: &ufbx::VertexVec4, corner: usize) -> Option<ufbx::Vec4> {
    let zero = ufbx::Vec4 { x: 0.0, y: 0.0, z: 0.0, w: 0.0 };
    a.exists.then(|| a.indices.get(corner).and_then(|&i| a.values.get(i as usize)).copied().unwrap_or(zero))
}
