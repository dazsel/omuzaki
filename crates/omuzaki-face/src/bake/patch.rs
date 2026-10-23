//! Face patches: copies of the head's triangles in front of one painted feature, lifted a fraction of a millimetre off
//! the skin, with UVs that map the feature's rectangle of the face plane into an atlas cell.
//!
//! The live renderer projects the painted face onto the head (a planar overlay in the head material). A file has no such
//! overlay, so the baker draws the face on patches instead: each feature (an eye, a brow, the mouth, the static face)
//! gets its own patch and material, transparent where nothing is painted, so a texture-transform bind can flip one
//! feature's cell without touching the others. A patch is part of the head mesh, so it follows the head's skin and its
//! morph targets.

use std::collections::BTreeMap;

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::FaceRect;
use omuzaki_io_api::{Mesh, MorphTarget, Primitive};

/// The head-local frame the face plane is measured in: the head centre in bind space, bind-space metres per head-local
/// unit (the character's stature against its template's), and the head-local `z` the front starts at (the projection's
/// `front_fade[0]`).
#[derive(Copy, Clone, Debug)]
pub(super) struct HeadFrame {
    pub centre: Vec3,
    pub scale: f32,
    pub front_from: f32,
}

impl HeadFrame {
    /// `p` (bind space) in the head-local frame.
    pub fn local(&self, p: Vec3) -> Vec3 {
        (p - self.centre) / self.scale
    }
}

/// The triangles of a mesh a patch copies, and the rectangle of the face plane their corners span.
pub(super) struct Selection {
    /// `(primitive, corners)` in mesh order.
    triangles: Vec<(usize, [u32; 3])>,
    /// The bounds of every selected corner on the face plane: the patch's UV rectangle.
    pub bounds: FaceRect,
}

/// The least a triangle's normal must face forward (+z) to belong to a patch: the face, not the side of the head.
const MIN_FACING: f32 = 0.2;

/// The triangles of `mesh` whose centre lies in front, faces forward, and falls inside `rect` on the face plane; `None`
/// when there are none.
pub(super) fn select(mesh: &Mesh, frame: HeadFrame, rect: FaceRect) -> Option<Selection> {
    let mut triangles = Vec::new();
    let mut bounds = FaceRect { x0: f32::MAX, x1: f32::MIN, y0: f32::MAX, y1: f32::MIN };
    for (at, primitive) in mesh.primitives.iter().enumerate() {
        let data = &primitive.mesh;
        let count = u32::try_from(data.positions.len()).unwrap_or(u32::MAX);
        let corners: Vec<u32> = if data.indices.is_empty() { (0..count).collect() } else { data.indices.clone() };
        for t in corners.as_chunks::<3>().0 {
            let Some(p) = corner_positions(data, *t) else { continue };
            let local = p.map(|p| frame.local(p));
            let centre = (local[0] + local[1] + local[2]) / 3.0;
            let normal = (local[1] - local[0]).cross(local[2] - local[0]).normalize_or_zero();
            let inside = centre.x >= rect.x0 && centre.x <= rect.x1 && centre.y >= rect.y0 && centre.y <= rect.y1;
            if centre.z <= frame.front_from || normal.z < MIN_FACING || !inside {
                continue;
            }
            for q in local {
                bounds = FaceRect {
                    x0: bounds.x0.min(q.x),
                    x1: bounds.x1.max(q.x),
                    y0: bounds.y0.min(q.y),
                    y1: bounds.y1.max(q.y),
                };
            }
            triangles.push((at, *t));
        }
    }
    (!triangles.is_empty()).then_some(Selection { triangles, bounds })
}

fn corner_positions(data: &MeshData, t: [u32; 3]) -> Option<[Vec3; 3]> {
    let at = |i: u32| data.positions.get(i as usize).copied();
    Some([at(t[0])?, at(t[1])?, at(t[2])?])
}

/// A new primitive holding the selected triangles of `mesh`, each corner lifted `lift` metres along its normal, with UVs
/// from `uv` (given the corner's point on the face plane), and `morph_count` morph targets copied from the source
/// primitives (zero where a source has fewer). An attribute is kept when every source primitive has it.
pub(super) fn build(
    mesh: &Mesh,
    selection: &Selection,
    frame: HeadFrame,
    lift: f32,
    uv: impl Fn(Vec2) -> Vec2,
    morph_count: usize,
) -> Primitive {
    let sources: Vec<&Primitive> = {
        let mut used: Vec<usize> = selection.triangles.iter().map(|(at, _)| *at).collect();
        used.dedup();
        used.iter().filter_map(|&at| mesh.primitives.get(at)).collect()
    };
    let has = |len: fn(&MeshData) -> usize| sources.iter().all(|p| len(&p.mesh) == p.mesh.positions.len());
    let (normals, skinned) = (has(|m| m.normals.len()), has(|m| m.joints.len()) && has(|m| m.weights.len()));

    let mut out = MeshData::default();
    let mut targets = vec![Vec::new(); morph_count];
    let mut index: BTreeMap<(usize, u32), u32> = BTreeMap::new();
    for &(at, corners) in &selection.triangles {
        let Some(source) = mesh.primitives.get(at) else { continue };
        for corner in corners {
            let next = u32::try_from(out.positions.len()).unwrap_or(u32::MAX);
            let new = *index.entry((at, corner)).or_insert(next);
            if new == next {
                let i = corner as usize;
                let data = &source.mesh;
                let position = data.positions[i];
                let normal = if normals { data.normals[i].normalize_or_zero() } else { Vec3::Z };
                out.positions.push(position + normal * lift);
                if normals {
                    out.normals.push(data.normals[i]);
                }
                let local = frame.local(position);
                out.uvs.push(uv(Vec2::new(local.x, local.y)));
                if skinned {
                    out.joints.push(data.joints[i]);
                    out.weights.push(data.weights[i]);
                }
                for (t, deltas) in targets.iter_mut().enumerate() {
                    let delta = source.morph_targets.get(t).and_then(|m| m.positions.get(i)).copied();
                    deltas.push(delta.unwrap_or(Vec3::ZERO));
                }
            }
            out.indices.push(new);
        }
    }
    let mut primitive = Primitive::new(out);
    primitive.morph_targets = targets.into_iter().map(MorphTarget::new).collect();
    primitive
}
